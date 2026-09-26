//! Claude Code adapter.
//!
//! Claude Code keeps a registry of live sessions, one JSON file per process,
//! keyed by pid. Each file records the session id, working directory, an
//! optional name, how the process was started, and the process start time.
//! Transcripts live under a per-project directory whose name is the working
//! directory with every path separator replaced by a dash.
//!
//! Two things this adapter must not get wrong, both observed on a live
//! machine:
//!
//! - Registry entries are reaped by other live instances on pid liveness
//!   alone, so after a reboot an entry whose pid was reused looks alive. This
//!   adapter compares the recorded process start time with the process table
//!   and treats a mismatch as not live.
//! - Non-interactive runs register too, with a different entrypoint. They are
//!   excluded by that field rather than by guessing from the command line.

use super::AgentAdapter;
use crate::model::{Agent, Confidence, Session, Strategy};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Shape of one registry file. Unknown fields are tolerated because the
/// registry gains fields between releases; the fields used here are the ones
/// that were stable across every entry observed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryEntry {
    pid: u32,
    session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    name: Option<String>,
    /// "cli" for the interactive terminal client; other values are
    /// non-interactive or SDK entrypoints and are out of scope.
    #[serde(default)]
    entrypoint: Option<String>,
    /// Process start time as printed by `ps -o lstart`, e.g.
    /// "Sun Sep  6 19:30:10 2026". Compared as an exact string.
    #[serde(default)]
    proc_start: Option<String>,
}

pub struct ClaudeCode {
    home: PathBuf,
}

impl ClaudeCode {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    fn registry_dir(&self) -> PathBuf {
        self.home.join(".claude").join("sessions")
    }

    /// Transcript path re-derived from the working directory and session id,
    /// independently of anything the registry says about paths.
    pub fn transcript_path(&self, cwd: &Path, session_id: &str) -> PathBuf {
        self.home
            .join(".claude")
            .join("projects")
            .join(project_dir_name(cwd))
            .join(format!("{session_id}.jsonl"))
    }
}

/// Claude Code names a project directory by replacing every path separator
/// in the working directory with a dash, keeping the leading one.
pub fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

/// Process start times for every process, keyed by pid, in the same textual
/// form the registry records. The registry stores the time in UTC while `ps`
/// prints local time by default, so `ps` is run with UTC to make the strings
/// comparable. One listing for all pids costs about 20 ms; one per pid would
/// cost that many times over.
fn process_start_table() -> Option<HashMap<u32, String>> {
    let out = Command::new("ps")
        .env("TZ", "UTC")
        .args(["-axo", "pid=,lstart="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((pid, start)) = line.split_once(' ') else {
            continue;
        };
        if let Ok(pid) = pid.parse::<u32>() {
            map.insert(pid, start.trim().to_string());
        }
    }
    Some(map)
}

/// Whether a process with this pid exists and started at the recorded time.
/// Pid existence alone is not enough: pids are reused across reboots, and the
/// registry's own reaper makes exactly that mistake.
fn process_matches(
    pid: u32,
    proc_start: Option<&str>,
    table: Option<&HashMap<u32, String>>,
) -> Confidence {
    let Ok(native_pid) = libc::pid_t::try_from(pid) else {
        return Confidence::Unknown;
    };
    if native_pid <= 0 {
        return Confidence::Unknown;
    }
    // SAFETY: a positive pid addresses one process; signal 0 only checks
    // existence/permission, without delivering a signal or using pointers.
    let alive = unsafe { libc::kill(native_pid, 0) } == 0;
    if !alive {
        return Confidence::Unknown;
    }
    let (Some(expected), Some(table)) = (proc_start, table) else {
        return Confidence::Probable;
    };
    match table.get(&pid) {
        Some(actual) if actual == expected.trim() => Confidence::Exact,
        Some(_) => Confidence::Unknown,
        None => Confidence::Probable,
    }
}

/// The title Claude Code gave the conversation, which it also sets as the
/// terminal tab title. Stored in the transcript as an `ai-title` record; the
/// last one wins because the title is regenerated as the conversation moves.
pub fn transcript_title(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .filter(|l| l.contains("\"type\":\"ai-title\""))
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            v.get("aiTitle")?.as_str().map(str::to_string)
        })
}

impl AgentAdapter for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let dir = self.registry_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let table = process_start_table();
        let mut sessions = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let reg: RegistryEntry = serde_json::from_str(&text)
                .with_context(|| format!("unrecognised registry entry {}", path.display()))?;
            if reg.entrypoint.as_deref() != Some("cli") {
                continue;
            }
            let confidence = process_matches(reg.pid, reg.proc_start.as_deref(), table.as_ref());
            if confidence == Confidence::Unknown {
                continue;
            }
            let transcript = self.transcript_path(&reg.cwd, &reg.session_id);
            let transcript = transcript.is_file().then_some(transcript);
            let title = transcript.as_deref().and_then(transcript_title);
            sessions.push(Session {
                agent: Agent::ClaudeCode,
                session_id: reg.session_id,
                cwd: reg.cwd,
                label: reg.name,
                title,
                strategy: Strategy::ProcessRegistry,
                process_confidence: confidence,
                pid: Some(reg.pid),
                transcript,
                database: None,
                tab: None,
            });
        }
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_checks_only_positive_process_ids() {
        assert_eq!(process_matches(0, None, None), Confidence::Unknown);
        assert_eq!(process_matches(u32::MAX, None, None), Confidence::Unknown);
        assert_eq!(
            process_matches(std::process::id(), None, None),
            Confidence::Probable
        );
    }

    #[test]
    fn project_dir_replaces_every_separator() {
        assert_eq!(
            project_dir_name(Path::new("/Users/x/Developer")),
            "-Users-x-Developer"
        );
        assert_eq!(
            project_dir_name(Path::new("/private/tmp/a-b/c")),
            "-private-tmp-a-b-c"
        );
    }

    #[test]
    fn transcript_path_is_derived_from_cwd_and_id() {
        let a = ClaudeCode::new(PathBuf::from("/home/u"));
        assert_eq!(
            a.transcript_path(Path::new("/w/p"), "abc"),
            PathBuf::from("/home/u/.claude/projects/-w-p/abc.jsonl")
        );
    }

    #[test]
    fn registry_entry_tolerates_unknown_fields() {
        let e: RegistryEntry = serde_json::from_str(
            r#"{"pid":1,"sessionId":"s","cwd":"/x","name":null,"entrypoint":"cli","procStart":"t","status":"busy","peerFeatures":[]}"#,
        )
        .unwrap();
        assert_eq!(e.pid, 1);
        assert_eq!(e.entrypoint.as_deref(), Some("cli"));
    }
}
