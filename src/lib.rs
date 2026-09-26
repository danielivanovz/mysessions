//! `roost` — capture which terminal coding-agent sessions are open, and reopen
//! them in terminal tabs after a restart.
//!
//! Capture is a short-lived run: read agent state and the terminal's tab list,
//! write a snapshot if anything changed, exit. Restore reads a snapshot and
//! opens one tab per session, running the agent's resume command in the
//! recorded directory. Restore never depends on an agent being alive, because
//! it runs from a cold shell after a reboot.

mod adapters;
mod bounded;
mod capture;
mod files;
mod install;
mod model;
mod restore;
mod snapshot;
#[cfg(test)]
mod test_support;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "roost", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Browse saved sessions, capture, and manage automatic capture.
    Browse {
        /// Open a particular snapshot instead of browsing saved history.
        #[arg(long)]
        snapshot: Option<std::path::PathBuf>,
    },
    /// Discover live sessions and write a snapshot if the set changed.
    Capture {
        /// Print what would be captured without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Bound the hook to 3 seconds and always exit successfully.
        #[arg(long, conflicts_with = "dry_run")]
        hook: bool,
    },
    /// Internal capture worker, supervised by the public capture command.
    #[command(hide = true)]
    CaptureWorker {
        #[arg(long)]
        dry_run: bool,
    },
    /// Validate and reopen saved conversations; dry run unless --apply is set.
    Restore(restore::Options),
    /// Install the executable, minute timer and Claude `SessionStart` hook.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the timer and hook, keeping the executable and saved snapshots.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
}

/// Parse the process arguments and run the selected command.
///
/// # Errors
///
/// Returns an error when the selected command cannot read, validate, capture,
/// restore, install, or uninstall the requested state.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Browse { snapshot: None }) {
        Command::Browse { snapshot } => tui::run(snapshot.as_deref()),
        Command::Capture { dry_run, hook } => bounded::capture(dry_run, hook),
        Command::CaptureWorker { dry_run } => capture::run(dry_run),
        Command::Restore(options) => restore::run(&options),
        Command::Install { dry_run } => install::run(false, dry_run),
        Command::Uninstall { dry_run } => install::run(true, dry_run),
    }
}
