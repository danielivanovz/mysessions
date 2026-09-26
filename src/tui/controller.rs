//! Filesystem and process actions, kept outside the keyboard state machine.
use super::{
    load,
    state::{Screen, State},
};
use crate::{install::Prepared, snapshot::Store};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub struct Browser {
    pub history: Vec<PathBuf>,
    pub index: usize,
    explicit: Option<PathBuf>,
    pending: Option<Prepared>,
}

impl Browser {
    pub fn new(snapshot: Option<&Path>, store: &Store) -> Result<Self> {
        Ok(Self {
            history: snapshot
                .map_or_else(|| store.history(), |path| Ok(vec![path.to_path_buf()]))?,
            index: 0,
            explicit: snapshot.map(Path::to_path_buf),
            pending: None,
        })
    }

    pub fn navigate(&mut self, state: &mut State, home: &Path, offset: isize) {
        let index = self
            .index
            .saturating_add_signed(offset)
            .min(self.history.len().saturating_sub(1));
        if index != self.index {
            self.index = index;
            load(state, &self.history[index], home);
        }
    }

    pub fn refresh(
        &mut self,
        state: &mut State,
        store: &Store,
        home: &Path,
        latest: bool,
    ) -> Result<()> {
        let history = if latest {
            store.history()?
        } else {
            self.explicit
                .as_ref()
                .map_or_else(|| store.history(), |p| Ok(vec![p.clone()]))?
        };
        let current = self.history.get(self.index);
        let index = if latest {
            0
        } else {
            history.iter().position(|p| Some(p) == current).unwrap_or(0)
        };
        self.history = history;
        self.index = index;
        if latest {
            self.explicit = None;
        }
        if let Some(path) = self.history.get(index) {
            load(state, path, home);
        } else {
            state.load_error(String::new());
        }
        Ok(())
    }

    pub fn capture(&mut self, state: &mut State, store: &Store, home: &Path) -> Result<()> {
        // The public command supervises a worker with a ten-second deadline.
        // Captured output cannot write over the alternate-screen interface.
        let output = Command::new(std::env::current_exe()?)
            .arg("capture")
            .output()
            .context("starting capture")?;
        self.capture_result(state, store, home, &output)
    }

    pub(super) fn capture_result(
        &mut self,
        state: &mut State,
        store: &Store,
        home: &Path,
        output: &Output,
    ) -> Result<()> {
        let report = String::from_utf8_lossy(&output.stderr).trim().to_string();
        ensure!(
            output.status.success(),
            "Capture failed; previous snapshot kept. {report}"
        );
        self.refresh(state, store, home, true)?;
        // Keep parse errors visible if an external edit damaged the saved file.
        if state.message.is_empty() {
            state.message = if report.is_empty() {
                "Capture finished.".into()
            } else {
                report
            };
        }
        Ok(())
    }

    pub fn prepare_setup(&mut self, state: &mut State, uninstall: bool) -> Result<()> {
        self.pending = None;
        let prepared = Prepared::new(uninstall)?;
        state.review_setup(uninstall, prepared.changes());
        self.pending = Some(prepared);
        Ok(())
    }

    pub fn cancel_setup(&mut self) {
        self.pending = None;
    }

    pub fn apply_setup(&mut self, state: &mut State) -> Result<()> {
        ensure!(
            state.screen == Screen::Setup,
            "review setup before confirming"
        );
        let uninstall = state.setup.as_ref().context("no setup review")?.uninstall;
        let prepared = self.pending.take().context("review a fresh setup plan")?;
        // Consume the plan even on failure; retrying requires another review.
        let result = prepared.apply();
        state.back();
        result?;
        state.message = if uninstall {
            "Automatic capture disabled. Binary, snapshots and backups retained."
        } else {
            "Automatic capture enabled: every 60 seconds and on Claude SessionStart."
        }
        .into();
        Ok(())
    }
}
