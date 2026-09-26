//! Snapshot store.
//!
//! A snapshot is a TOML file that must stay useful without this tool: its
//! header explains recovery in prose, and every entry sits beside the command
//! that resumes it. History is kept because the most recent capture is not
//! always the one wanted; the useful snapshot is often from before whatever
//! went wrong.
//!
//! Rules enforced here: never replace a non-empty snapshot with an empty one,
//! write only when the captured set differs from the latest, and write
//! atomically.

use crate::model::Session;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn snapshot_key(path: &Path) -> Option<(u64, u128)> {
    if path.extension()? != "toml" {
        return None;
    }
    let name = path.file_stem()?.to_str()?;
    let (seconds, suffix) = name.split_once('-').unwrap_or((name, "0"));
    Some((seconds.parse().ok()?, suffix.parse().ok()?))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Seconds since the Unix epoch at capture time.
    pub captured_at: u64,
    pub hostname: String,
    #[serde(default, rename = "session")]
    pub sessions: Vec<Session>,
}

const HEADER: &str = "\
# roost snapshot — which agent sessions were open, where, and in which tab.
#
# You do not need roost to use this file. Each [[session]] below is preceded
# by the shell commands that reopen it: change into the directory first, then
# run the resume command. Do not rely on the resume command to restore the
# directory; at least one agent resumes in the launch directory instead.
#
# If this file is missing or stale, transcripts are still on disk. The
# sessions you had open are the most recently modified ones:
#   ls -t ~/.claude/projects/*/*.jsonl | head -20
#   sqlite3 ~/.local/share/opencode/opencode.db \\
#     'select id, directory from session order by time_updated desc limit 20'
#   ls -t ~/.codex/sessions/*/*/*/*.jsonl | head -20
#
# confidence: exact = direct lookup; probable = inferred with a tie-breaker;
# ambiguous = more than one candidate; entries never present a guess as fact.
# opencode logs and Codex open transcripts can retain earlier conversations.
# Probable/ambiguous entries are candidates, not proof of the current UI selection.
";

impl Snapshot {
    pub fn new(sessions: Vec<Session>) -> Self {
        let captured_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let hostname = std::process::Command::new("hostname")
            .arg("-s")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        Self {
            captured_at,
            hostname,
            sessions,
        }
    }

    /// Whether the set of sessions is the same, ignoring capture time.
    pub fn same_sessions_as(&self, other: &Snapshot) -> bool {
        self.sessions == other.sessions
    }

    /// Render with the explanatory header and a paste-able command comment
    /// above every entry.
    pub fn to_toml(&self) -> Result<String> {
        #[derive(Serialize)]
        struct Metadata<'a> {
            captured_at: u64,
            hostname: &'a str,
        }
        #[derive(Serialize)]
        struct Entry<'a> {
            session: [&'a Session; 1],
        }
        let metadata = Metadata {
            captured_at: self.captured_at,
            hostname: &self.hostname,
        };
        let mut out = String::from(HEADER);
        out.push('\n');
        out.push_str(&toml::to_string_pretty(&metadata).context("serialising snapshot metadata")?);
        // Serialize complete tables independently. Looking for [[session]] in
        // rendered TOML would also match that text inside multiline strings.
        for session in &self.sessions {
            out.push('\n');
            if session
                .cwd
                .to_string_lossy()
                .chars()
                .chain(session.session_id.chars())
                .any(char::is_control)
            {
                out.push_str("# Resume command omitted: cwd/id contains control characters.\n");
            } else {
                writeln!(out, "# cd {}", shell_quote(&session.cwd.to_string_lossy()))?;
                writeln!(out, "# {}", session.resume_command())?;
            }
            out.push_str(
                &toml::to_string_pretty(&Entry { session: [session] })
                    .context("serialising session")?,
            );
        }
        Ok(out)
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        toml::from_str(text).context("parsing snapshot")
    }
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Directory of snapshots, newest by file name.
pub struct Store {
    dir: PathBuf,
    keep: usize,
}

impl Store {
    pub fn new(dir: PathBuf, keep: usize) -> Self {
        Self { dir, keep }
    }

    /// `$XDG_STATE_HOME/roost` or `~/.local/state/roost`.
    pub fn default_location() -> Result<Self> {
        let base = match std::env::var_os("XDG_STATE_HOME") {
            Some(p) => PathBuf::from(p),
            None => home_dir()?.join(".local").join("state"),
        };
        Ok(Self::new(base.join("roost"), 20))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn snapshot_files(&self) -> Result<Vec<PathBuf>> {
        if !self.dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut v: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| snapshot_key(p).is_some())
            .collect();
        // Old snapshots used seconds only; new ones retain multiple changes
        // within a second. Compare numerically across both naming formats.
        v.sort_by_key(|p| snapshot_key(p).unwrap());
        Ok(v)
    }

    /// Saved snapshots, newest first. Installer metadata is never included.
    pub fn history(&self) -> Result<Vec<PathBuf>> {
        let mut files = self.snapshot_files()?;
        files.reverse();
        Ok(files)
    }

    pub fn latest(&self) -> Result<Option<Snapshot>> {
        match self.snapshot_files()?.last() {
            Some(p) => Ok(Some(Snapshot::from_toml(&std::fs::read_to_string(p)?)?)),
            None => Ok(None),
        }
    }

    /// Write the snapshot unless it is empty while a non-empty one exists, or
    /// identical in content to the latest. Returns the path written, if any.
    pub fn write_if_changed(&self, snap: &Snapshot) -> Result<Option<PathBuf>> {
        if let Some(prev) = self.latest()? {
            if snap.sessions.is_empty() && !prev.sessions.is_empty() {
                return Ok(None);
            }
            if snap.same_sessions_as(&prev) {
                return Ok(None);
            }
        }
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let name = format!("{}-{}.toml", snap.captured_at, crate::files::nonce());
        let final_path = self.dir.join(&name);
        crate::files::atomic_write(&final_path, snap.to_toml()?.as_bytes(), 0o600)?;
        self.prune()?;
        Ok(Some(final_path))
    }

    fn prune(&self) -> Result<()> {
        let files = self.snapshot_files()?;
        if files.len() > self.keep {
            for p in &files[..files.len() - self.keep] {
                std::fs::remove_file(p)?;
            }
        }
        Ok(())
    }
}

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(test)]
// QuickCheck requires owned arguments so it can generate and shrink them.
#[allow(clippy::needless_pass_by_value)]
mod tests {
    use super::*;
    use crate::model::{Agent, Confidence, Strategy};

    type SessionInput = (u8, String, String, Option<String>, Option<u32>);

    #[test]
    fn property_snapshot_preserves_arbitrary_session_text() {
        // Paths and titles can contain quotes, Unicode and line breaks. The
        // recovery comments must never corrupt the serialized mapping.
        fn property(inputs: Vec<SessionInput>) -> bool {
            let sessions = inputs
                .into_iter()
                .map(|(kind, id, cwd, label, pid)| {
                    let mut s = session(&id);
                    s.agent =
                        [Agent::ClaudeCode, Agent::Opencode, Agent::Codex][usize::from(kind % 3)];
                    s.cwd = PathBuf::from(cwd);
                    s.label = label.clone();
                    s.title = label;
                    s.pid = pid;
                    s.process_confidence = [
                        Confidence::Exact,
                        Confidence::Probable,
                        Confidence::Ambiguous,
                        Confidence::Unknown,
                    ][usize::from(kind % 4)];
                    s.strategy = [
                        Strategy::SelfDescribing,
                        Strategy::ProcessRegistry,
                        Strategy::ProcessLog,
                        Strategy::DirectoryRecency,
                    ][usize::from((kind / 4) % 4)];
                    s.transcript = Some(s.cwd.join("transcript.jsonl"));
                    s.database = Some(s.cwd.join("agent.db"));
                    s.tab = Some(crate::model::TabRef {
                        window_id: id.clone(),
                        tab_id: id.clone(),
                        surface_id: id.clone(),
                        title: id,
                        confidence: s.process_confidence,
                    });
                    s
                })
                .collect();
            let snapshot = Snapshot {
                captured_at: 1,
                hostname: "test".into(),
                sessions,
            };
            Snapshot::from_toml(&snapshot.to_toml().unwrap()).unwrap() == snapshot
        }
        crate::test_support::check(property as fn(Vec<SessionInput>) -> bool);
    }

    #[test]
    fn property_shell_quoting_preserves_one_literal_argument() {
        fn property(text: String) -> bool {
            // NUL cannot occur in a Unix argument; all other text is literal.
            let text = text.replace('\0', "");
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &format!("printf '%s' {}", shell_quote(&text))])
                .output()
                .unwrap();
            out.status.success() && out.stdout == text.as_bytes()
        }
        crate::test_support::check(property as fn(String) -> bool);
    }

    fn session(id: &str) -> Session {
        Session {
            agent: Agent::ClaudeCode,
            session_id: id.into(),
            cwd: PathBuf::from("/tmp/p q"),
            label: Some("x".into()),
            title: None,
            strategy: Strategy::ProcessRegistry,
            process_confidence: Confidence::Exact,
            pid: Some(1),
            transcript: None,
            database: None,
            tab: None,
        }
    }

    #[test]
    fn toml_round_trips_and_carries_commands() {
        let snap = Snapshot {
            captured_at: 1,
            hostname: "h".into(),
            sessions: vec![session("abc")],
        };
        let text = snap.to_toml().unwrap();
        assert!(text.contains("# cd '/tmp/p q'\n# claude -r abc\n[[session]]"));
        assert_eq!(Snapshot::from_toml(&text).unwrap(), snap);
    }

    #[test]
    fn control_characters_cannot_escape_recovery_comments() {
        let mut saved = session("id");
        saved.cwd = "/work/\n[[malicious]]\r\0".into();
        let snapshot = Snapshot {
            captured_at: 1,
            hostname: "test".into(),
            sessions: vec![saved],
        };
        let text = snapshot.to_toml().unwrap();
        assert!(text.contains("# Resume command omitted:"));
        assert_eq!(Snapshot::from_toml(&text).unwrap(), snapshot);
    }

    #[test]
    fn text_resembling_a_table_header_stays_literal() {
        let mut saved = session("first");
        saved.label = Some("\n[[session]]\n".into());
        let snapshot = Snapshot {
            captured_at: 1,
            hostname: "test".into(),
            sessions: vec![saved, session("second")],
        };
        assert_eq!(
            Snapshot::from_toml(&snapshot.to_toml().unwrap()).unwrap(),
            snapshot
        );
    }

    #[test]
    fn empty_capture_never_replaces_good_snapshot() {
        let root = crate::test_support::Scratch::new("snapshot-retention");
        let store = Store::new(root.0.clone(), 3);
        let good = Snapshot {
            captured_at: 10,
            hostname: "h".into(),
            sessions: vec![session("a")],
        };
        assert!(store.write_if_changed(&good).unwrap().is_some());
        let empty = Snapshot {
            captured_at: 11,
            hostname: "h".into(),
            sessions: vec![],
        };
        assert!(store.write_if_changed(&empty).unwrap().is_none());
        let same = Snapshot {
            captured_at: 12,
            ..good.clone()
        };
        assert!(store.write_if_changed(&same).unwrap().is_none());
        for t in 13..20 {
            let s = Snapshot {
                captured_at: t,
                hostname: "h".into(),
                sessions: vec![session(&t.to_string())],
            };
            store.write_if_changed(&s).unwrap();
        }
        assert_eq!(store.snapshot_files().unwrap().len(), 3);
    }

    #[test]
    fn changes_in_one_second_retain_both_and_sort_after_legacy_files() {
        let root = crate::test_support::Scratch::new("snapshot-names");
        let store = Store::new(root.0.clone(), 20);
        std::fs::write(root.0.join("installation.toml"), "active = true\n").unwrap();
        assert!(store.latest().unwrap().is_none());
        let first = Snapshot {
            captured_at: 10,
            hostname: "h".into(),
            sessions: vec![session("first")],
        };
        std::fs::write(root.0.join("10.toml"), first.to_toml().unwrap()).unwrap();
        let second = Snapshot {
            sessions: vec![session("second")],
            ..first.clone()
        };
        let third = Snapshot {
            sessions: vec![session("third")],
            ..first
        };
        store.write_if_changed(&second).unwrap();
        store.write_if_changed(&third).unwrap();
        assert_eq!(store.snapshot_files().unwrap().len(), 3);
        assert_eq!(store.latest().unwrap().unwrap(), third);
        assert!(root.0.join("installation.toml").is_file());
    }
}
