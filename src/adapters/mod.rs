//! Adapter interfaces. Agent adapters answer "what is live and how do I resume
//! it"; terminal adapters answer "what tabs exist and open one". All the
//! version-dependent knowledge of an agent's private storage lives behind
//! these two traits and nowhere else.

pub mod claude;
pub mod codex;
pub mod ghostty;
pub mod opencode;
mod process;
mod sqlite;

use crate::model::Session;
use anyhow::Result;

/// Discovers live sessions for one agent.
///
/// Implementations must be strictly read-only with respect to agent state:
/// read process tables and files, never write into an agent's directory,
/// never signal or attach to a process.
pub trait AgentAdapter {
    /// Short name used in output and snapshots.
    fn name(&self) -> &'static str;

    /// Sessions that are live right now. An empty result is a valid answer,
    /// not an error; storage that looks stale or unrecognised is an error,
    /// so that format drift fails loudly instead of degrading quietly.
    fn discover(&self) -> Result<Vec<Session>>;
}

/// One terminal surface as reported by the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    pub window_id: String,
    pub tab_id: String,
    pub surface_id: String,
    pub cwd: String,
    pub title: String,
}

/// Lists and opens terminal tabs. Deliberately minimal so that most terminals
/// can be supported.
pub trait TerminalAdapter {
    fn name(&self) -> &'static str;

    /// Every surface in every tab of every window, including split panes.
    fn list_surfaces(&self) -> Result<Vec<Surface>>;

    /// Create a new tab and submit one command to its shell. Returns the new
    /// surface id; existing tabs are never used as input targets.
    fn open_tab(&self, cwd: &std::path::Path, command: &str) -> Result<String>;
}
