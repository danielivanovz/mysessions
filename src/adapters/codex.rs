//! Codex's index identifies conversations; an open rollout held by a terminal
//! process supplies candidates. Codex retains old handles after /new, so this
//! cannot prove which conversation is displayed. The first `session_meta` record
//! is checked independently against the index. CLI history alone is not liveness.

use super::{
    AgentAdapter,
    process::{Process, terminal_processes},
    sqlite,
};
use crate::model::{Agent, Confidence, Session, Strategy};
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::Deserialize;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

pub struct Codex {
    home: PathBuf,
}

impl Codex {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
}

#[derive(Debug)]
struct Thread {
    id: String,
    cwd: PathBuf,
    title: String,
    rollout: PathBuf,
}

impl AgentAdapter for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let processes = terminal_processes("codex")?;
        let root =
            std::env::var_os("CODEX_HOME").map_or_else(|| self.home.join(".codex"), PathBuf::from);
        let path = root.join("state_5.sqlite");
        if !path.try_exists()? {
            let other_format = root.try_exists()?
                && std::fs::read_dir(&root)?.any(|e| {
                    e.is_ok_and(|e| e.file_name().to_string_lossy().starts_with("state_"))
                });
            ensure!(
                processes.is_empty() && !other_format,
                "expected state_5.sqlite is missing; Codex storage format may have changed"
            );
            return Ok(Vec::new());
        }
        let db = sqlite::open(&path)?;
        let sessions = discover_from(&db, &processes)?;
        for p in &processes {
            ensure!(
                p.files.contains(&path),
                "Codex pid {} is not using {}; storage location or format changed",
                p.pid,
                path.display()
            );
        }
        Ok(sessions)
    }
}

fn discover_from(db: &Connection, processes: &[Process]) -> Result<Vec<Session>> {
    // Preparing this on every capture detects missing columns even with no
    // active CLI processes. Filter by source before touching any transcripts.
    let mut query = db.prepare("SELECT id, cwd, title, rollout_path FROM threads WHERE source = 'cli' AND archived = 0")
        .context("unrecognised Codex threads schema")?;
    let threads = query
        .query_map([], |r| {
            Ok(Thread {
                id: r.get(0)?,
                cwd: PathBuf::from(r.get::<_, String>(1)?),
                title: r.get(2)?,
                rollout: PathBuf::from(r.get::<_, String>(3)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let per_process: Vec<_> = processes
        .iter()
        .map(|p| {
            threads
                .iter()
                .filter(|t| p.files.contains(&t.rollout))
                .count()
        })
        .collect();
    let mut sessions = Vec::new();
    let mut matched = HashSet::new();
    for thread in threads {
        let owners: Vec<_> = processes
            .iter()
            .filter(|p| p.files.contains(&thread.rollout))
            .collect();
        if owners.is_empty() {
            continue;
        }
        validate_transcript(&thread)?;
        matched.insert(thread.rollout.clone());
        sessions.push(Session {
            agent: Agent::Codex,
            session_id: thread.id,
            cwd: thread.cwd,
            label: (!thread.title.is_empty()).then_some(thread.title),
            // The index title has not been verified as a terminal title.
            title: None,
            strategy: Strategy::SelfDescribing,
            process_confidence: if owners.len() == 1
                && processes
                    .iter()
                    .zip(&per_process)
                    .all(|(p, count)| !p.files.contains(&thread.rollout) || *count == 1)
            {
                Confidence::Probable
            } else {
                Confidence::Ambiguous
            },
            pid: (owners.len() == 1).then_some(owners[0].pid),
            transcript: Some(thread.rollout),
            database: None,
            tab: None,
        });
    }
    // An open CLI rollout missing from the index is drift or an initialization
    // race, not evidence that no sessions are open. Preserve the last snapshot.
    for path in processes.iter().flat_map(|p| &p.files).filter(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
            && p.extension().is_some_and(|e| e == "jsonl")
    }) {
        if !matched.contains(path) {
            let meta = read_meta(path)?;
            if meta.source == "cli" {
                let indexed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM threads WHERE rollout_path = ?1 AND source = 'cli' AND archived != 0)",
                    [path.to_string_lossy().as_ref()], |r| r.get(0))?;
                ensure!(
                    indexed,
                    "open CLI rollout {} is missing from the CLI index or has a conflicting source",
                    path.display()
                );
            }
        }
    }
    sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    Ok(sessions)
}

#[derive(Deserialize)]
struct Meta {
    id: Option<String>,
    session_id: Option<String>,
    cwd: PathBuf,
    source: serde_json::Value,
}

fn read_meta(path: &Path) -> Result<Meta> {
    #[derive(Deserialize)]
    struct Record {
        r#type: String,
        payload: Meta,
    }
    const LIMIT: u64 = 4 * 1024 * 1024;
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut line = String::new();
    // Metadata may include developer instructions; read only the first record,
    // capped at 4 MiB, never a whole multi-megabyte conversation.
    BufReader::new(file.take(LIMIT + 1)).read_line(&mut line)?;
    ensure!(
        line.len() as u64 <= LIMIT,
        "Codex session_meta exceeds 4 MiB"
    );
    let record: Record = serde_json::from_str(&line)
        .with_context(|| format!("unrecognised Codex first record in {}", path.display()))?;
    ensure!(
        record.r#type == "session_meta",
        "Codex first record is not session_meta"
    );
    Ok(record.payload)
}

fn validate_transcript(thread: &Thread) -> Result<()> {
    let meta = read_meta(&thread.rollout)?;
    ensure!(
        meta.id.is_some() || meta.session_id.is_some(),
        "Codex session_meta has no id"
    );
    for id in [meta.id.as_deref(), meta.session_id.as_deref()]
        .into_iter()
        .flatten()
    {
        ensure!(
            id == thread.id,
            "Codex index/transcript id mismatch for {}",
            thread.rollout.display()
        );
    }
    ensure!(
        meta.cwd == thread.cwd && thread.cwd.is_absolute(),
        "Codex index/transcript cwd mismatch"
    );
    ensure!(
        meta.source == "cli",
        "Codex index/transcript source mismatch"
    );
    Ok(())
}

pub fn validate_saved(session: &Session) -> Result<()> {
    validate_transcript(&Thread {
        id: session.session_id.clone(),
        cwd: session.cwd.clone(),
        title: String::new(),
        rollout: session
            .transcript
            .clone()
            .context("no recorded Codex transcript")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture {
        dir: PathBuf,
        db: Connection,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scratch")
                .join(format!(
                    "codex-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&dir).unwrap();
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch("CREATE TABLE threads(id TEXT,cwd TEXT,title TEXT,rollout_path TEXT,source TEXT,archived INTEGER)").unwrap();
            Self { dir, db }
        }
        fn add(&self, id: &str, source: &serde_json::Value, archived: bool) -> PathBuf {
            let path = self.dir.join(format!("rollout-{id}.jsonl"));
            let source_column = source
                .as_str()
                .map_or_else(|| source.to_string(), str::to_string);
            std::fs::write(&path, format!("{}\n", json!({"type":"session_meta","payload":{"id":id,"session_id":id,"cwd":"/work","source":source}}))).unwrap();
            self.db
                .execute(
                    "INSERT INTO threads VALUES (?1,'/work','title',?2,?3,?4)",
                    rusqlite::params![id, path.to_string_lossy(), source_column, archived],
                )
                .unwrap();
            path
        }
        fn process(files: &[PathBuf]) -> Process {
            Process {
                pid: 42,
                started_ms: 0,
                cwd: "/work".into(),
                args: vec!["codex".into()],
                files: files.iter().cloned().collect(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn cli_history_without_a_live_owner_is_not_captured() {
        let f = Fixture::new();
        let live = f.add("live", &json!("cli"), false);
        let history = f.add("history", &json!("cli"), false);
        std::fs::remove_file(history).unwrap(); // Not opened or validated.
        assert!(discover_from(&f.db, &[]).unwrap().is_empty());
        let sessions = discover_from(&f.db, &[Fixture::process(&[live])]).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "live");
        assert_eq!(sessions[0].pid, Some(42));
        assert_eq!(sessions[0].process_confidence, Confidence::Probable);
    }

    #[test]
    fn retained_rollouts_after_new_are_ambiguous_not_ordered_by_recency() {
        let f = Fixture::new();
        let one = f.add("one", &json!("cli"), false);
        let two = f.add("two", &json!("cli"), false);
        let sessions = discover_from(&f.db, &[Fixture::process(&[one, two])]).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(
            sessions
                .iter()
                .all(|s| s.process_confidence == Confidence::Ambiguous)
        );
    }

    #[test]
    fn source_and_archive_filters_exclude_gui_exec_and_subagents() {
        let f = Fixture::new();
        let cli = f.add("cli", &json!("cli"), false);
        let gui = f.add("gui", &json!("vscode"), false);
        let exec = f.add("exec", &json!("exec"), false);
        let sub = f.add(
            "sub",
            &json!({"subagent":{"parent_thread_id":"cli"}}),
            false,
        );
        let archived = f.add("archived", &json!("cli"), true);
        let sessions =
            discover_from(&f.db, &[Fixture::process(&[cli, gui, exec, sub, archived])]).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "cli");
    }

    #[test]
    fn mismatched_metadata_and_missing_live_transcripts_fail() {
        let f = Fixture::new();
        let path = f.add("live", &json!("cli"), false);
        let process = Fixture::process(std::slice::from_ref(&path));
        for payload in [
            json!({"id":"wrong","cwd":"/work","source":"cli"}),
            json!({"id":"live","session_id":"wrong","cwd":"/work","source":"cli"}),
            json!({"id":"live","cwd":"/other","source":"cli"}),
            json!({"id":"live","cwd":"/work","source":"vscode"}),
            json!({"cwd":"/work","source":"cli"}),
        ] {
            std::fs::write(
                &path,
                json!({"type":"session_meta","payload":payload}).to_string(),
            )
            .unwrap();
            assert!(discover_from(&f.db, std::slice::from_ref(&process)).is_err());
        }
        std::fs::remove_file(&path).unwrap();
        assert!(discover_from(&f.db, &[process]).is_err());
    }

    #[test]
    fn missing_index_rows_and_schema_drift_fail_instead_of_returning_empty() {
        let f = Fixture::new();
        let path = f.add("live", &json!("cli"), false);
        f.db.execute("DELETE FROM threads", []).unwrap();
        assert!(discover_from(&f.db, &[Fixture::process(&[path])]).is_err());
        f.db.execute_batch("ALTER TABLE threads RENAME COLUMN source TO old_source")
            .unwrap();
        assert!(discover_from(&f.db, &[]).is_err());
    }
}
