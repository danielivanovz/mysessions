//! Interaction state, independent of terminal drawing and tab creation.
use crate::{
    model::{Agent, Confidence, Session},
    restore,
    snapshot::Snapshot,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub struct SetupReview {
    pub uninstall: bool,
    pub changes: Vec<(&'static str, PathBuf)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    Unavailable(String),
    Submitted,
    Failed(String),
}

pub struct Entry {
    pub session: Session,
    pub selected: bool,
    pub status: Status,
}

impl Entry {
    pub fn suggested(&self) -> bool {
        matches!(
            self.session.process_confidence,
            Confidence::Exact | Confidence::Probable
        )
    }

    pub fn title(&self) -> &str {
        let label = self.session.label.as_deref();
        let title = self.session.title.as_deref();
        // Claude's registry name can be a project plus a short suffix. Its
        // transcript title describes the conversation and is already captured.
        let candidates = if self.session.agent == Agent::ClaudeCode {
            [title, label]
        } else {
            [label, title]
        };
        candidates
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|s| !s.is_empty())
            .unwrap_or("Untitled conversation")
    }

    fn matches(&self, query: &str) -> bool {
        let text = format!(
            "{} {} {} {} {} {}",
            self.title(),
            self.session.label.as_deref().unwrap_or_default(),
            self.session.title.as_deref().unwrap_or_default(),
            self.session.cwd.display(),
            agent_name(self.session.agent),
            self.session.session_id
        )
        .to_lowercase();
        text.contains(query)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Browse,
    Review,
    Results,
    Setup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Older,
    Newer,
    Submit,
    Capture,
    Refresh,
    PrepareInstall,
    PrepareUninstall,
    ApplySetup,
}

pub struct State {
    pub snapshot: Option<Snapshot>,
    pub entries: Vec<Entry>,
    pub screen: Screen,
    pub cursor: usize,
    pub query: String,
    pub searching: bool,
    pub message: String,
    pub batch: Vec<usize>,
    pub setup: Option<SetupReview>,
    // Switching snapshots cannot offer a session already submitted in this run.
    attempted: HashSet<(Agent, String)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            snapshot: None,
            entries: Vec::new(),
            screen: Screen::Browse,
            cursor: 0,
            query: String::new(),
            searching: false,
            message: String::new(),
            batch: Vec::new(),
            setup: None,
            attempted: HashSet::new(),
        }
    }
}

impl State {
    pub fn load(&mut self, snapshot: Snapshot, home: &Path) {
        let mut seen = HashSet::new();
        self.entries = snapshot
            .sessions
            .iter()
            .filter(|s| seen.insert((s.agent, s.session_id.as_str())))
            .map(|s| {
                let status = if self.attempted.contains(&(s.agent, s.session_id.clone())) {
                    Status::Unavailable(
                        "Already attempted in this run; check its new tab before trying again."
                            .into(),
                    )
                } else {
                    restore::validate(s, home).map_or_else(
                        |e| Status::Unavailable(format!("{e:#}")),
                        |()| Status::Ready,
                    )
                };
                let mut entry = Entry {
                    session: s.clone(),
                    selected: false,
                    status,
                };
                entry.selected = entry.status == Status::Ready && entry.suggested();
                entry
            })
            .collect();
        self.snapshot = Some(snapshot);
        self.reset_view();
    }

    pub fn load_error(&mut self, error: String) {
        self.snapshot = None;
        self.entries.clear();
        self.reset_view();
        self.message = error;
    }

    fn reset_view(&mut self) {
        self.screen = Screen::Browse;
        self.cursor = 0;
        self.query.clear();
        self.searching = false;
        self.message.clear();
        self.batch.clear();
        self.setup = None;
    }

    pub fn visible(&self) -> Vec<usize> {
        if self.screen == Screen::Setup {
            return self
                .setup
                .as_ref()
                .map_or_else(Vec::new, |s| (0..s.changes.len()).collect());
        }
        if self.screen != Screen::Browse {
            return self.batch.clone();
        }
        let query = self.query.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, row)| row.matches(&query))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn selected(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, row)| row.selected)
            .map(|(index, _)| index)
            .collect()
    }

    pub fn current(&self) -> Option<&Entry> {
        if self.screen == Screen::Setup {
            return None;
        }
        self.visible()
            .get(self.cursor)
            .and_then(|&i| self.entries.get(i))
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.searching {
            return self.search_key(key.code);
        }
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc => {
                self.back();
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                Action::None
            }
            KeyCode::PageUp => {
                self.move_cursor(-10);
                Action::None
            }
            KeyCode::PageDown => {
                self.move_cursor(10);
                Action::None
            }
            _ => self.screen_key(key.code),
        }
    }

    fn screen_key(&mut self, key: KeyCode) -> Action {
        match self.screen {
            Screen::Browse => self.browse_key(key),
            Screen::Review if key == KeyCode::Enter => Action::Submit,
            Screen::Setup if key == KeyCode::Enter => Action::ApplySetup,
            Screen::Results if key == KeyCode::Enter => {
                self.back();
                Action::None
            }
            _ => Action::None,
        }
    }

    fn browse_key(&mut self, key: KeyCode) -> Action {
        match key {
            KeyCode::Char('[') => return Action::Older,
            KeyCode::Char(']') => return Action::Newer,
            KeyCode::Char('c') => return Action::Capture,
            KeyCode::Char('r') => return Action::Refresh,
            KeyCode::Char('i') => return Action::PrepareInstall,
            KeyCode::Char('u') => return Action::PrepareUninstall,
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('a') => self.select_suggested(),
            KeyCode::Char('n') => self.entries.iter_mut().for_each(|row| row.selected = false),
            KeyCode::Enter => self.review(),
            _ => {}
        }
        Action::None
    }

    fn search_key(&mut self, key: KeyCode) -> Action {
        match key {
            KeyCode::Esc => {
                self.query.clear();
                self.searching = false;
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char(c) if !c.is_control() => self.query.push(c),
            _ => {}
        }
        self.cursor = 0;
        Action::None
    }

    fn move_cursor(&mut self, offset: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(offset)
            .min(self.visible().len().saturating_sub(1));
    }

    fn toggle(&mut self) {
        if let Some(&index) = self.visible().get(self.cursor) {
            let row = &mut self.entries[index];
            if row.status == Status::Ready {
                row.selected = !row.selected;
            }
        }
    }

    fn select_suggested(&mut self) {
        for index in self.visible() {
            let row = &mut self.entries[index];
            if row.status == Status::Ready && row.suggested() {
                row.selected = true;
            }
        }
    }

    fn review(&mut self) {
        self.batch = self.selected();
        if self.batch.is_empty() {
            self.message = "Nothing selected. Use Space to choose a session.".into();
            return;
        }
        self.screen = Screen::Review;
        self.cursor = 0;
        self.message.clear();
    }

    pub fn back(&mut self) {
        self.screen = Screen::Browse;
        self.setup = None;
        self.query.clear();
        self.cursor = 0;
        self.message.clear();
    }

    pub fn review_setup(&mut self, uninstall: bool, changes: Vec<(&'static str, PathBuf)>) {
        self.back();
        self.screen = Screen::Setup;
        self.setup = Some(SetupReview { uninstall, changes });
    }

    pub fn record_attempt(&mut self, index: usize, result: anyhow::Result<String>) {
        let row = &mut self.entries[index];
        row.selected = false;
        row.status = result.map_or_else(
            |error| Status::Failed(format!("{error:#}")),
            |_| Status::Submitted,
        );
        self.attempted
            .insert((row.session.agent, row.session.session_id.clone()));
    }
}

pub fn agent_name(agent: Agent) -> &'static str {
    match agent {
        Agent::ClaudeCode => "Claude",
        Agent::Opencode => "opencode",
        Agent::Codex => "Codex",
    }
}

pub fn confidence(value: Confidence) -> &'static str {
    match value {
        Confidence::Exact => "Exact",
        Confidence::Probable => "Probable",
        Confidence::Ambiguous => "Ambiguous",
        Confidence::Unknown => "Unknown",
    }
}

/// Agent titles, paths and errors are data, never terminal control sequences.
pub fn safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
