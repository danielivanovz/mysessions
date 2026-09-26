//! opencode stores conversations in `SQLite`, without a process id. A log run
//! can be correlated with process start time, never more than probably. A run
//! can visit multiple sessions, and silent UI switches are not logged.

use super::{
    AgentAdapter,
    process::{Process, terminal_processes, utc_ms},
    sqlite,
};
use crate::model::{Agent, Confidence, Session, Strategy};
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub struct Opencode {
    home: PathBuf,
}

impl Opencode {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
}

pub fn validate_saved(session: &Session) -> Result<()> {
    let path = session
        .database
        .as_ref()
        .context("no recorded opencode database")?;
    let db = sqlite::open(path)?;
    let rows = read_rows(&db, &BTreeSet::from([session.cwd.clone()]))?;
    ensure!(
        rows.iter().any(|row| row.id == session.session_id),
        "opencode conversation is missing, archived, empty, or has a different directory"
    );
    Ok(())
}

#[derive(Debug)]
struct Row {
    id: String,
    directory: PathBuf,
    title: String,
    updated: i64,
}

#[derive(Debug, Default)]
struct Run {
    started: Option<i64>,
    directory: Option<PathBuf>,
    sessions: BTreeSet<String>,
}

impl AgentAdapter for Opencode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let mut processes = terminal_processes("opencode")?;
        processes.retain(|p| interactive(&p.args));
        let root = std::env::var_os("XDG_DATA_HOME")
            .map_or_else(|| self.home.join(".local/share"), PathBuf::from)
            .join("opencode");
        let path = root.join("opencode.db");
        if !path.try_exists()? {
            ensure!(
                processes.is_empty() && !root.join("storage").try_exists()?,
                "expected opencode.db is missing; legacy or changed storage is unsupported"
            );
            return Ok(Vec::new());
        }
        let db = sqlite::open(&path)?;
        let directories = processes.iter().map(|p| p.cwd.clone()).collect();
        let rows = read_rows(&db, &directories)?;
        for row in &rows {
            ensure!(
                row.id.starts_with("ses_")
                    && row
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "unrecognised opencode session id in a live directory"
            );
        }
        for p in &processes {
            ensure!(
                p.files.contains(&path),
                "opencode pid {} is not using {}; storage location or format changed",
                p.pid,
                path.display()
            );
        }
        if processes.is_empty() {
            return Ok(Vec::new());
        }
        let runs = read_runs(&root.join("log/opencode.log"))?;
        Ok(pair(&processes, &rows, &runs, &path))
    }
}

fn read_runs(path: &Path) -> Result<BTreeMap<String, Run>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_log(&text)
            .with_context(|| format!("unrecognised log format in {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).context("reading opencode log"),
    }
}

/// argv boundaries are preserved by the process adapter. Exclude subcommands
/// even if launched from a TTY. A positional directory is the TUI form.
fn interactive(args: &[String]) -> bool {
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session" | "-s" | "--model" | "-m" | "--agent" | "--prompt" | "--port"
            | "--hostname" | "--log-level" => {
                args.next();
            }
            "--help" | "-h" | "--version" | "-v" => return false,
            x if x.starts_with('-') => {}
            x => {
                return Path::new(x).is_absolute()
                    || x.starts_with('.')
                    || x.contains('/')
                    || ![
                        "run",
                        "serve",
                        "web",
                        "attach",
                        "session",
                        "auth",
                        "mcp",
                        "models",
                        "agent",
                        "debug",
                        "export",
                        "import",
                        "upgrade",
                        "uninstall",
                        "stats",
                        "github",
                        "pr",
                        "db",
                        "acp",
                        "completion",
                    ]
                    .contains(&x);
            }
        }
    }
    true
}

fn read_rows(db: &Connection, directories: &BTreeSet<PathBuf>) -> Result<Vec<Row>> {
    // Preparation checks the schema on every run, including empty databases.
    // Child sessions and rows without messages are not restorable TUI work.
    let mut query = db.prepare("SELECT s.id, s.directory, s.title, s.time_updated FROM session s WHERE s.directory = ?1 AND s.time_archived IS NULL AND s.parent_id IS NULL AND EXISTS(SELECT 1 FROM message m WHERE m.session_id = s.id) ORDER BY s.time_updated DESC, s.id")
        .context("unrecognised opencode session/message schema")?;
    let mut rows = Vec::new();
    for directory in directories {
        rows.extend(
            query
                .query_map([directory.to_string_lossy().as_ref()], |r| {
                    Ok(Row {
                        id: r.get(0)?,
                        directory: PathBuf::from(r.get::<_, String>(1)?),
                        title: r.get(2)?,
                        updated: r.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    Ok(rows)
}

/// Only complete log records participate; a writer may be appending the tail.
fn parse_log(text: &str) -> Result<BTreeMap<String, Run>> {
    let mut runs: BTreeMap<String, Run> = BTreeMap::new();
    for line in text.split_inclusive('\n').filter(|l| l.ends_with('\n')) {
        if line.trim().is_empty() {
            continue;
        }
        let fields = log_fields(line)?;
        let run_id = fields.get("run").context("log record has no run id")?;
        let timestamp = fields
            .get("timestamp")
            .context("log record has no timestamp")?;
        let message = fields.get("message").context("log record has no message")?;
        let is_first = !runs.contains_key(run_id);
        let run = runs.entry(run_id.clone()).or_default();
        if is_first && message == "creating instance" {
            run.started = Some(timestamp_ms(timestamp)?);
            run.directory = fields.get("directory").map(PathBuf::from);
        }
        if message == "created"
            && let Some(id) = fields.get("id").filter(|id| id.starts_with("ses_"))
        {
            run.sessions.insert(id.clone());
        }
        if let Some(id) = fields.get("session.id") {
            run.sessions.insert(id.clone());
        }
    }
    Ok(runs)
}

fn timestamp_ms(text: &str) -> Result<i64> {
    ensure!(
        text.is_ascii() && text.len() == 24 && &text[19..20] == "." && text.ends_with('Z'),
        "unsupported opencode timestamp {text}"
    );
    Ok(utc_ms(&text[..19], "%Y-%m-%dT%H:%M:%S")? + text[20..23].parse::<i64>()?)
}

fn log_fields(mut line: &str) -> Result<HashMap<String, String>> {
    let mut fields = HashMap::new();
    while !line.trim().is_empty() {
        line = line.trim_start();
        let (key, rest) = line.split_once('=').context("expected log key=value")?;
        ensure!(!key.chars().any(char::is_whitespace), "invalid log key");
        if rest.starts_with('"') {
            let mut escaped = false;
            let mut end = None;
            for (i, c) in rest.char_indices().skip(1) {
                if c == '"' && !escaped {
                    end = Some(i + 1);
                    break;
                }
                escaped = c == '\\' && !escaped;
            }
            let end = end.context("unterminated log string")?;
            fields.insert(key.into(), serde_json::from_str::<String>(&rest[..end])?);
            line = &rest[end..];
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            fields.insert(key.into(), rest[..end].into());
            line = &rest[end..];
        }
    }
    Ok(fields)
}

fn near(process: &Process, run: &Run) -> bool {
    // Includes the observed 2.8 s startup plus ps's second precision. This is
    // an inference window; overlapping starts remain ambiguous, never ranked.
    run.directory.as_ref() == Some(&process.cwd)
        && run
            .started
            .is_some_and(|t| (0..4000).contains(&(t - process.started_ms)))
}

fn candidate_ids(process: &Process, candidates: &[&Run]) -> BTreeSet<String> {
    let mut ids: BTreeSet<_> = candidates
        .iter()
        .flat_map(|r| r.sessions.iter().cloned())
        .collect();
    // A resume flag is evidence of launch intent, not the current UI: the
    // same process can switch later. It therefore also has a probable cap.
    for pair in process.args.windows(2) {
        if matches!(pair[0].as_str(), "-s" | "--session") {
            ids.insert(pair[1].clone());
        }
    }
    for arg in &process.args {
        if let Some(id) = arg.strip_prefix("--session=") {
            ids.insert(id.into());
        }
    }
    ids
}

fn pair(
    processes: &[Process],
    rows: &[Row],
    runs: &BTreeMap<String, Run>,
    database: &Path,
) -> Vec<Session> {
    let mut result: BTreeMap<String, Session> = BTreeMap::new();
    let mut fallback: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for process in processes {
        let candidates: Vec<_> = runs.values().filter(|r| near(process, r)).collect();
        let unique = candidates.len() == 1
            && processes.iter().filter(|p| near(p, candidates[0])).count() == 1;
        let ids = candidate_ids(process, &candidates);
        if ids.is_empty()
            && (candidates.is_empty()
                || process
                    .args
                    .iter()
                    .any(|a| matches!(a.as_str(), "-c" | "--continue")))
        {
            *fallback.entry(process.cwd.clone()).or_default() += 1;
            continue;
        }
        let eligible: Vec<_> = rows
            .iter()
            .filter(|r| r.directory == process.cwd && ids.contains(&r.id))
            .collect();
        let confidence = if unique && eligible.len() == 1 {
            Confidence::Probable
        } else {
            Confidence::Ambiguous
        };
        for row in eligible {
            let entry = make_session(
                row,
                database,
                confidence,
                Strategy::ProcessLog,
                Some(process.pid),
            );
            if let Some(old) = result.get_mut(&row.id) {
                old.pid = None;
                old.process_confidence = Confidence::Ambiguous;
            } else {
                result.insert(row.id.clone(), entry);
            }
        }
    }
    for (cwd, count) in fallback {
        eprintln!(
            "mysessions: opencode: log cannot identify {count} process(es) in {}; recency candidates are ambiguous",
            cwd.display()
        );
        let candidates: Vec<_> = rows
            .iter()
            .filter(|r| r.directory == cwd && !result.contains_key(&r.id))
            .collect();
        // Keep all ties at the cutoff. Equal timestamps must not become an
        // arbitrary subset just because SQL returned one id first.
        let cutoff = candidates
            .get(count.saturating_sub(1))
            .map_or(i64::MIN, |r| r.updated);
        for row in candidates.into_iter().filter(|r| r.updated >= cutoff) {
            result.insert(
                row.id.clone(),
                make_session(
                    row,
                    database,
                    Confidence::Ambiguous,
                    Strategy::DirectoryRecency,
                    None,
                ),
            );
        }
    }
    result.into_values().collect()
}

fn make_session(
    row: &Row,
    database: &Path,
    confidence: Confidence,
    strategy: Strategy,
    pid: Option<u32>,
) -> Session {
    Session {
        agent: Agent::Opencode,
        session_id: row.id.clone(),
        cwd: row.directory.clone(),
        label: Some(row.title.clone()),
        title: None,
        strategy,
        process_confidence: confidence,
        pid,
        transcript: None,
        database: Some(database.into()),
        tab: None,
    }
}

#[cfg(test)]
// QuickCheck requires owned arguments so it can generate and shrink them.
#[allow(clippy::needless_pass_by_value)]
mod tests {
    use super::*;

    #[test]
    fn property_log_values_preserve_quoted_text() {
        fn property(value: String) -> bool {
            let line = format!(
                "run=r message={} timestamp=t\n",
                serde_json::to_string(&value).unwrap()
            );
            let fields = log_fields(&line).unwrap();
            fields["message"] == value && fields["run"] == "r" && fields["timestamp"] == "t"
        }
        crate::test_support::check(property as fn(String) -> bool);
    }
    use std::collections::HashSet;

    fn process(pid: u32, start: i64) -> Process {
        Process {
            pid,
            started_ms: start,
            cwd: "/work".into(),
            args: vec!["opencode".into()],
            files: HashSet::new(),
        }
    }
    fn row(id: &str, updated: i64) -> Row {
        Row {
            id: id.into(),
            directory: "/work".into(),
            title: "Same title".into(),
            updated,
        }
    }
    fn run(start: i64, ids: &[&str]) -> Run {
        Run {
            started: Some(start),
            directory: Some("/work".into()),
            sessions: ids.iter().map(|s| (*s).to_string()).collect(),
        }
    }
    fn capture(p: &[Process], r: &[Row], runs: BTreeMap<String, Run>) -> Vec<Session> {
        pair(p, r, &runs, Path::new("/database"))
    }

    #[test]
    fn log_timing_is_probable_and_overlapping_starts_are_ambiguous() {
        let rows = [row("ses_a", 1), row("ses_b", 1)];
        let runs = BTreeMap::from([
            ("a".into(), run(2800, &["ses_a"])),
            ("b".into(), run(7800, &["ses_b"])),
        ]);
        let captured = capture(&[process(1, 0), process(2, 5000)], &rows, runs);
        assert_eq!(captured.len(), 2);
        assert!(
            captured
                .iter()
                .all(|s| s.process_confidence == Confidence::Probable)
        );
        assert_eq!(captured[0].pid, Some(1));
        assert_eq!(captured[1].pid, Some(2));
        let runs = BTreeMap::from([
            ("a".into(), run(1000, &["ses_a"])),
            ("b".into(), run(1100, &["ses_b"])),
        ]);
        let captured = capture(&[process(1, 0), process(2, 0)], &rows, runs);
        assert_eq!(captured.len(), 2);
        assert!(
            captured
                .iter()
                .all(|s| s.process_confidence == Confidence::Ambiguous && s.pid.is_none())
        );
    }

    #[test]
    fn multiple_sessions_in_a_run_are_not_reduced_to_last_created() {
        let captured = capture(
            &[process(1, 0)],
            &[row("ses_a", 2), row("ses_b", 1)],
            BTreeMap::from([("a".into(), run(1000, &["ses_a", "ses_b"]))]),
        );
        assert_eq!(captured.len(), 2);
        assert!(
            captured
                .iter()
                .all(|s| s.process_confidence == Confidence::Ambiguous)
        );
    }

    #[test]
    fn idle_run_does_not_capture_old_history_and_missing_log_keeps_all_recency_ties() {
        let rows = [row("ses_a", 2), row("ses_b", 2), row("ses_c", 1)];
        assert!(
            capture(
                &[process(1, 0)],
                &rows,
                BTreeMap::from([("a".into(), run(1000, &[]))])
            )
            .is_empty()
        );
        let captured = capture(&[process(1, 0)], &rows, BTreeMap::new());
        assert_eq!(captured.len(), 2);
        assert!(
            captured
                .iter()
                .all(|s| s.process_confidence == Confidence::Ambiguous
                    && s.strategy == Strategy::DirectoryRecency)
        );
    }

    #[test]
    fn parser_keeps_quoted_paths_and_detects_drift_but_ignores_incomplete_tail() {
        let log = "timestamp=2026-09-06T20:49:35.761Z level=INFO run=abc message=\"creating instance\" directory=\"/a path\"\ntimestamp=2026-09-06T20:50:14.016Z level=INFO run=abc message=created id=ses_one\ntimestamp=2026-09-06T20:50:14.017Z level=INFO run=abc message=process session.id=ses_two\npartial tail";
        let runs = parse_log(log).unwrap();
        assert_eq!(runs["abc"].started, Some(1_788_727_775_761));
        assert_eq!(runs["abc"].directory.as_deref(), Some(Path::new("/a path")));
        assert_eq!(runs["abc"].sessions.len(), 2);
        assert!(parse_log("INFO incompatible logger\n").is_err());
        let rotated =
            parse_log("timestamp=2026-09-06T20:50:14.016Z run=abc message=created id=ses_one\n")
                .unwrap();
        assert_eq!(rotated["abc"].started, None);
    }

    #[test]
    fn schema_filters_archived_children_and_empty_sessions_and_fails_on_drift() {
        let db = Connection::open_in_memory().unwrap();
        let directories = BTreeSet::from([PathBuf::from("/work")]);
        assert!(read_rows(&db, &directories).is_err());
        db.execute_batch("CREATE TABLE session(id TEXT,directory TEXT,title TEXT,time_updated INTEGER,time_archived INTEGER,parent_id TEXT); CREATE TABLE message(session_id TEXT); INSERT INTO session VALUES ('ses_live','/work','title',2,NULL,NULL),('ses_empty','/work','title',3,NULL,NULL),('ses_archived','/work','title',4,1,NULL),('ses_child','/work','title',5,NULL,'ses_live'); INSERT INTO message VALUES('ses_live'),('ses_archived'),('ses_child');").unwrap();
        let rows = read_rows(&db, &directories).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "ses_live");
        db.execute_batch("ALTER TABLE session RENAME COLUMN directory TO old_directory")
            .unwrap();
        assert!(read_rows(&db, &BTreeSet::new()).is_err());
    }

    #[test]
    fn noninteractive_subcommands_are_excluded_even_on_a_tty() {
        let args = |s: &[&str]| s.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        for command in ["run", "serve", "web", "attach", "session", "export"] {
            assert!(!interactive(&args(&["opencode", command])));
        }
        assert!(interactive(&args(&[
            "opencode",
            "--prompt",
            "run tests",
            "/a path"
        ])));
        assert!(interactive(&args(&["opencode", "-s", "ses_one"])));
    }
}
