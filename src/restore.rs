//! Standalone restoration: validate disk state, then open only new tabs.
use crate::adapters::{TerminalAdapter, claude::ClaudeCode, codex, ghostty::Ghostty, opencode};
use crate::model::{Agent, Confidence, Session};
use crate::snapshot::{Snapshot, Store, home_dir};
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Args)]
pub struct Options {
    /// Open tabs. Without this flag, only print the restore plan.
    #[arg(long)]
    pub apply: bool,
    /// Read this snapshot instead of the most recent one.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
    /// Restore one session id. Explicit selection also allows ambiguity.
    #[arg(long)]
    pub session: Option<String>,
    /// Include ambiguous/unknown process candidates when restoring all.
    #[arg(long)]
    pub include_ambiguous: bool,
}

pub fn run(options: &Options) -> Result<()> {
    let store = Store::default_location()?;
    // Hold off hook/timer captures while launching the selected set. In
    // particular, the first resumed agent's hook must not replace this set.
    let _lock = if options.apply {
        Some(
            crate::files::try_lock(&store.dir().join(".capture.lock"))?
                .context("a capture or restore is already running; try again shortly")?,
        )
    } else {
        None
    };
    let snapshot = match &options.snapshot {
        Some(path) => Snapshot::from_toml(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )?,
        None => store
            .latest()?
            .context("no snapshot found; run mysessions capture while agents are open")?,
    };
    execute(&snapshot, options, &Ghostty, &home_dir()?)
}

fn execute(
    snapshot: &Snapshot,
    options: &Options,
    terminal: &dyn TerminalAdapter,
    home: &std::path::Path,
) -> Result<()> {
    if let Some(id) = &options.session {
        ensure!(
            snapshot.sessions.iter().any(|s| &s.session_id == id),
            "session {id} is not in the snapshot"
        );
    }
    let mut seen = HashSet::new();
    let mut restored = 0;
    let mut skipped = 0;
    let mut failures = 0;
    for session in &snapshot.sessions {
        if options
            .session
            .as_ref()
            .is_some_and(|id| id != &session.session_id)
        {
            continue;
        }
        let key = (session.agent, session.session_id.as_str());
        if !seen.insert(key) {
            continue;
        }
        if !options.include_ambiguous
            && options.session.is_none()
            && matches!(
                session.process_confidence,
                Confidence::Ambiguous | Confidence::Unknown
            )
        {
            eprintln!(
                "skip {}: uncertain selection; choose --session {} or --include-ambiguous",
                session.session_id, session.session_id
            );
            skipped += 1;
            continue;
        }
        if let Err(error) = validate(session, home) {
            eprintln!("skip {}: {error:#}", session.session_id);
            skipped += 1;
            continue;
        }
        println!(
            "# {:?} {} ({:?})\n{}",
            session.agent,
            session.session_id,
            session.process_confidence,
            session.shell_command()
        );
        if options.apply {
            match terminal.open_tab(&session.cwd, &session.shell_command()) {
                Ok(surface) => eprintln!(
                    "mysessions: submitted {} to new Ghostty surface {surface}",
                    session.session_id
                ),
                Err(error) => {
                    eprintln!("mysessions: {}: {error:#}", session.session_id);
                    failures += 1;
                    continue;
                }
            }
        }
        restored += 1;
    }
    eprintln!(
        "mysessions: {restored} {}, {skipped} skipped, {failures} failed",
        if options.apply { "submitted" } else { "ready" }
    );
    ensure!(
        failures == 0,
        "some tabs could not be opened; see the errors above"
    );
    Ok(())
}

pub(crate) fn validate(session: &Session, home: &std::path::Path) -> Result<()> {
    ensure!(
        !session.session_id.is_empty()
            && session.session_id.as_bytes()[0].is_ascii_alphanumeric()
            && session
                .session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "unsupported session id"
    );
    ensure!(
        session.cwd.is_absolute() && session.cwd.is_dir(),
        "working directory is missing: {}",
        session.cwd.display()
    );
    ensure!(
        !session.cwd.to_string_lossy().chars().any(char::is_control),
        "working directory contains terminal control characters"
    );
    match session.agent {
        Agent::ClaudeCode => {
            let path =
                ClaudeCode::new(home.into()).transcript_path(&session.cwd, &session.session_id);
            ensure!(
                path.is_file() && path.metadata()?.len() > 0,
                "Claude transcript is missing or empty: {}",
                path.display()
            );
            if let Some(recorded) = &session.transcript {
                ensure!(
                    std::fs::canonicalize(recorded)? == std::fs::canonicalize(&path)?,
                    "Claude transcript path disagrees with cwd/id"
                );
            }
            Ok(())
        }
        Agent::Codex => codex::validate_saved(session),
        Agent::Opencode => opencode::validate_saved(session),
    }
}

/// Explicitly selected entries still require validation immediately before
/// submission. The caller holds the capture/restore lock across the batch.
pub(crate) fn submit(
    session: &Session,
    home: &std::path::Path,
    terminal: &dyn TerminalAdapter,
) -> Result<String> {
    validate(session, home)?;
    terminal.open_tab(&session.cwd, &session.shell_command())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{adapters::Surface, model::Strategy, test_support::Scratch};
    use serde_json::json;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Terminal {
        calls: RefCell<Vec<String>>,
        fail: bool,
    }
    impl TerminalAdapter for Terminal {
        fn name(&self) -> &'static str {
            "test"
        }
        fn list_surfaces(&self) -> Result<Vec<Surface>> {
            anyhow::bail!("restore must work without live discovery")
        }
        fn open_tab(&self, _: &std::path::Path, command: &str) -> Result<String> {
            self.calls.borrow_mut().push(command.into());
            ensure!(!self.fail, "injected terminal failure");
            Ok("new-surface".into())
        }
    }
    fn session(root: &Scratch, id: &str, confidence: Confidence) -> Session {
        let path = root.0.join(format!("{id}.jsonl"));
        std::fs::write(
            &path,
            json!({"type":"session_meta","payload":{"id":id,"cwd":root.0,"source":"cli"}})
                .to_string(),
        )
        .unwrap();
        Session {
            agent: Agent::Codex,
            session_id: id.into(),
            cwd: root.0.clone(),
            label: None,
            title: None,
            strategy: Strategy::SelfDescribing,
            process_confidence: confidence,
            pid: None,
            transcript: Some(path),
            database: None,
            tab: None,
        }
    }
    fn options(apply: bool) -> Options {
        Options {
            apply,
            snapshot: None,
            session: None,
            include_ambiguous: false,
        }
    }
    #[test]
    fn standalone_restore_skips_missing_and_ambiguous_and_dry_run_opens_nothing() {
        let root = Scratch::new("restore");
        let ready = session(&root, "ready", Confidence::Probable);
        let gone = session(&root, "gone", Confidence::Exact);
        std::fs::remove_file(gone.transcript.as_ref().unwrap()).unwrap();
        let uncertain = session(&root, "uncertain", Confidence::Ambiguous);
        let snap = Snapshot {
            captured_at: 1,
            hostname: "test".into(),
            sessions: vec![ready.clone(), gone, uncertain, ready],
        };
        let terminal = Terminal::default();
        execute(&snap, &options(false), &terminal, &root.0).unwrap();
        assert!(terminal.calls.borrow().is_empty());
        execute(&snap, &options(true), &terminal, &root.0).unwrap();
        assert_eq!(terminal.calls.borrow().len(), 1); // Duplicate saved entries also collapse.
        let mut selected = options(true);
        selected.session = Some("uncertain".into());
        execute(&snap, &selected, &terminal, &root.0).unwrap();
        assert_eq!(terminal.calls.borrow().len(), 2);
        selected.session = Some("unknown".into());
        assert!(execute(&snap, &selected, &terminal, &root.0).is_err());
    }
    #[test]
    fn terminal_errors_fail_restore_and_invalid_identity_never_reaches_terminal() {
        let root = Scratch::new("restore-errors");
        let mut saved = session(&root, "ready", Confidence::Exact);
        let mut snap = Snapshot {
            captured_at: 1,
            hostname: "test".into(),
            sessions: vec![saved.clone()],
        };
        let terminal = Terminal {
            fail: true,
            ..Default::default()
        };
        assert!(execute(&snap, &options(true), &terminal, &root.0).is_err());
        saved.session_id = "--unexpected-option".into();
        snap.sessions = vec![saved];
        execute(&snap, &options(true), &terminal, &root.0).unwrap();
        assert_eq!(terminal.calls.borrow().len(), 1);
    }
    #[test]
    fn quoted_shell_command_changes_directory_before_resuming() {
        use std::os::unix::fs::PermissionsExt;
        let root = Scratch::new("shell-quoting");
        let cwd = root.0.join("project's work $dir");
        std::fs::create_dir(&cwd).unwrap();
        let bin = root.0.join("claude");
        std::fs::write(
            &bin,
            b"#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$MYSESSIONS_VERIFY_OUTPUT\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut saved = session(&root, "id", Confidence::Exact);
        saved.agent = Agent::ClaudeCode;
        saved.cwd = cwd.clone();
        let output = root.0.join("result");
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &saved.shell_command()])
            .env("PATH", &root.0)
            .env("MYSESSIONS_VERIFY_OUTPUT", &output)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            format!("{}\n-r\nid\n", cwd.display())
        );
    }
}
