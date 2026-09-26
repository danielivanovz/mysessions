//! Shared data model: what a captured session is, and how sure we are of it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which agent a session belongs to. Also selects the resume command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Opencode,
    Codex,
}

/// How a session's identity was established. Carried through to the snapshot
/// because the three agents differ in kind and the user must be able to see
/// which entries are facts and which are inferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// The transcript itself records the working directory.
    SelfDescribing,
    /// A registry keyed by process id gave a direct lookup.
    ProcessRegistry,
    /// A log links sessions to a run; its start time only suggests a process.
    ProcessLog,
    /// Inferred from a live process's directory plus recency in the agent's store.
    DirectoryRecency,
}

/// Confidence in one pairing. Process-to-session and tab-to-session are
/// recorded separately because they fail independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    Exact,
    Probable,
    Ambiguous,
    Unknown,
}

/// Terminal placement of a session, when the terminal adapter could pair it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabRef {
    pub window_id: String,
    pub tab_id: String,
    pub surface_id: String,
    pub title: String,
    pub confidence: Confidence,
}

/// One live session as captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub agent: Agent,
    pub session_id: String,
    pub cwd: PathBuf,
    /// Human-readable label: the agent's session name, if any.
    pub label: Option<String>,
    /// The conversation title the agent shows in the terminal tab, if it
    /// records one. Used to pair the session with its tab.
    pub title: Option<String>,
    pub strategy: Strategy,
    pub process_confidence: Confidence,
    /// Process id observed at capture time. Informational only: pids are
    /// reused across reboots, so restore never relies on it.
    pub pid: Option<u32>,
    /// Path to the transcript, re-derived independently and checked to exist.
    pub transcript: Option<PathBuf>,
    /// Agents with database-backed conversations have no transcript file.
    /// Validate this database's session and message rows using `session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<PathBuf>,
    pub tab: Option<TabRef>,
}

impl Session {
    /// Shell command that resumes this session. Restore runs it after an
    /// explicit change of directory, never trusting the resume flag to do so.
    pub fn resume_command(&self) -> String {
        let id = crate::snapshot::shell_quote(&self.session_id);
        match self.agent {
            Agent::ClaudeCode => format!("claude -r {id}"),
            Agent::Opencode => format!("opencode -s {id}"),
            Agent::Codex => format!("codex resume {id}"),
        }
    }

    pub fn shell_command(&self) -> String {
        format!(
            "cd -- {} && {}",
            crate::snapshot::shell_quote(&self.cwd.to_string_lossy()),
            self.resume_command()
        )
    }
}
