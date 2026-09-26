use super::*;
use crate::{
    adapters::Surface,
    model::{Agent, Confidence, Session, Strategy},
    test_support::Scratch,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};
use state::{Status, safe};
use std::cell::RefCell;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn session(root: &Scratch, id: &str, confidence: Confidence) -> Session {
    let path = root.0.join(format!("{id}.jsonl"));
    std::fs::write(
        &path,
        serde_json::json!({"type":"session_meta", "payload": {
            "id":id, "cwd":root.0, "source":"cli"
        }})
        .to_string(),
    )
    .unwrap();
    Session {
        agent: Agent::Codex,
        session_id: id.into(),
        cwd: root.0.clone(),
        label: Some(format!("Conversation {id}")),
        title: None,
        strategy: Strategy::SelfDescribing,
        process_confidence: confidence,
        pid: None,
        transcript: Some(path),
        database: None,
        tab: None,
    }
}

fn snapshot(root: &Scratch) -> Snapshot {
    let exact = session(root, "exact", Confidence::Exact);
    let probable = session(root, "probable", Confidence::Probable);
    let uncertain = session(root, "uncertain", Confidence::Ambiguous);
    let missing = session(root, "missing", Confidence::Exact);
    std::fs::remove_file(missing.transcript.as_ref().unwrap()).unwrap();
    Snapshot {
        captured_at: 1,
        hostname: "test-host".into(),
        sessions: vec![exact.clone(), probable, uncertain, missing, exact],
    }
}

fn loaded(root: &Scratch) -> State {
    let mut state = State::default();
    state.load(snapshot(root), &root.0);
    state
}

#[derive(Default)]
struct FakeTerminal {
    calls: RefCell<Vec<String>>,
    fail: bool,
}

impl TerminalAdapter for FakeTerminal {
    fn name(&self) -> &'static str {
        "test"
    }
    fn list_surfaces(&self) -> Result<Vec<Surface>> {
        anyhow::bail!("restoration must not require live agents")
    }
    fn open_tab(&self, _: &Path, command: &str) -> Result<String> {
        self.calls.borrow_mut().push(command.into());
        ensure!(!self.fail, "terminal unavailable");
        Ok("surface".into())
    }
}

#[test]
fn defaults_select_valid_suggestions_and_explicit_choice_allows_uncertainty() {
    let root = Scratch::new("tui-selection");
    let mut state = loaded(&root);
    assert_eq!(state.entries.len(), 4); // Duplicate identities collapse.
    assert_eq!(state.selected(), [0, 1]);
    state.cursor = 2;
    state.handle(key(KeyCode::Char(' ')));
    assert_eq!(state.selected(), [0, 1, 2]);
    state.cursor = 3;
    state.handle(key(KeyCode::Char(' ')));
    assert_eq!(state.selected(), [0, 1, 2]); // Missing conversations cannot be selected.
}

#[test]
fn claude_prefers_conversation_title_with_nonblank_fallbacks() {
    let root = Scratch::new("tui-claude-title");
    let mut state = loaded(&root);
    let entry = &mut state.entries[0];
    entry.session.agent = Agent::ClaudeCode;
    entry.session.label = Some("project-82".into());
    entry.session.title = Some(" Fix workday data and rerun the backfill ".into());
    assert_eq!(entry.title(), "Fix workday data and rerun the backfill");
    for title in [None, Some(String::new()), Some(" \n ".into())] {
        entry.session.title = title;
        assert_eq!(entry.title(), "project-82");
    }
    entry.session.label = Some(" \t".into());
    assert_eq!(entry.title(), "Untitled conversation");
    entry.session.title = Some("Conversation title".into());
    entry.session.label = Some("Session label".into());
    for agent in [Agent::Codex, Agent::Opencode] {
        entry.session.agent = agent;
        assert_eq!(entry.title(), "Session label");
    }
}

#[test]
fn both_claude_title_and_registry_name_remain_searchable_without_rewriting_identity() {
    let root = Scratch::new("tui-claude-search");
    let mut state = loaded(&root);
    state.entries[0].session.agent = Agent::ClaudeCode;
    state.entries[0].session.label = Some("project-82".into());
    state.entries[0].session.title = Some("Fix workday data and rerun the backfill".into());
    let original = state.entries[0].session.clone();
    for query in ["PROJECT-82", "backfill"] {
        state.query = query.into();
        assert_eq!(state.visible(), [0]);
    }
    assert_eq!(state.entries[0].session, original);
}

#[test]
fn selected_conversation_title_wraps_beyond_the_table_column() {
    let root = Scratch::new("tui-title-detail");
    let mut state = loaded(&root);
    state.entries[0].session.agent = Agent::ClaudeCode;
    state.entries[0].session.label = Some("project-82".into());
    state.entries[0].session.title = Some(
        "Investigate duplicate records after migration and verify the complete backfill".into(),
    );
    let text = rendered(&state, 60, 20);
    assert!(text.contains("Investigate duplicate records"));
    assert!(text.contains("the complete backfill"));
    assert!(!text.contains("project-82"));
    assert!(text.contains("Enter review"));
    state.message = "Inspect the failed attempt.".into();
    state.entries[0].status =
        Status::Failed("Terminal unavailable: open Ghostty before retrying.".into());
    // Long titles/paths must not push an actionable failure below the panel.
    assert!(rendered(&state, 60, 20).contains("Terminal unavailable:"));
}

#[test]
fn review_includes_selections_hidden_by_search_and_requires_a_second_enter() {
    let root = Scratch::new("tui-review");
    let mut state = loaded(&root);
    state.handle(key(KeyCode::Char('/')));
    for c in "probable".chars() {
        state.handle(key(KeyCode::Char(c)));
    }
    assert_eq!(state.visible(), [1]);
    assert_eq!(state.handle(key(KeyCode::Enter)), Action::None); // Finish search.
    assert_eq!(state.handle(key(KeyCode::Enter)), Action::None); // Review, no submission.
    assert_eq!(state.visible(), [0, 1]);
    assert_eq!(state.handle(key(KeyCode::Enter)), Action::Submit);
    state.handle(key(KeyCode::Esc));
    assert!(state.screen == Screen::Browse);
}

#[test]
fn submit_revalidates_holds_capture_lock_and_prevents_duplicate_attempts() {
    let root = Scratch::new("tui-submit");
    let mut state = loaded(&root);
    let saved = state.snapshot.clone().unwrap();
    state.handle(key(KeyCode::Enter));
    // A transcript can disappear after the browser's initial validation.
    std::fs::remove_file(state.entries[1].session.transcript.as_ref().unwrap()).unwrap();
    let store = Store::new(root.0.join("state"), 20);
    let terminal = FakeTerminal::default();
    submit(&mut state, &root.0, &store, &terminal, |_| {
        assert!(files::try_lock(&store.dir().join(".capture.lock"))?.is_none());
        Ok(())
    })
    .unwrap();
    assert_eq!(terminal.calls.borrow().len(), 1);
    assert_eq!(state.entries[0].status, Status::Submitted);
    assert!(matches!(state.entries[1].status, Status::Failed(_)));
    assert!(state.selected().is_empty());
    state.load(saved, &root.0);
    assert!(state.selected().is_empty()); // Changing snapshots cannot reopen attempted ids.
    assert!(
        files::try_lock(&store.dir().join(".capture.lock"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn busy_capture_or_unreviewed_selection_never_opens_a_tab() {
    let root = Scratch::new("tui-lock");
    let mut state = loaded(&root);
    let terminal = FakeTerminal::default();
    let store = Store::new(root.0.join("state"), 20);
    assert!(submit(&mut state, &root.0, &store, &terminal, |_| Ok(())).is_err());
    state.handle(key(KeyCode::Enter));
    let _lock = files::try_lock(&store.dir().join(".capture.lock"))
        .unwrap()
        .unwrap();
    assert!(submit(&mut state, &root.0, &store, &terminal, |_| Ok(())).is_err());
    assert!(terminal.calls.borrow().is_empty());
}

#[test]
fn terminal_failures_are_reported_and_not_retried_by_accidental_enter() {
    let root = Scratch::new("tui-failed");
    let mut state = loaded(&root);
    state.handle(key(KeyCode::Enter));
    let terminal = FakeTerminal {
        fail: true,
        ..Default::default()
    };
    submit(
        &mut state,
        &root.0,
        &Store::new(root.0.join("state"), 20),
        &terminal,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(terminal.calls.borrow().len(), 2);
    assert!(
        state
            .batch
            .iter()
            .all(|&i| matches!(state.entries[i].status, Status::Failed(_)))
    );
    assert_eq!(state.handle(key(KeyCode::Enter)), Action::None);
    assert_eq!(state.handle(key(KeyCode::Enter)), Action::None);
    assert!(state.batch.is_empty());
}

fn rendered(state: &State, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| view::draw(frame, state, 0, 2))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn browser_explains_uncertainty_empty_states_and_small_terminal_requirements() {
    let root = Scratch::new("tui-render");
    let state = loaded(&root);
    let text = rendered(&state, 100, 28);
    assert!(text.contains("2 selected"));
    assert!(text.contains("Ambiguous") && text.contains("Unavailable"));
    assert!(text.contains("Enter review"));
    assert!(rendered(&state, 40, 10).contains("Resize to at least"));
    assert!(rendered(&State::default(), 100, 28).contains("mysessions capture"));
}

#[test]
fn corrupt_snapshot_is_visible_and_does_not_block_loading_an_older_one() {
    let root = Scratch::new("tui-history");
    let store = Store::new(root.0.join("state"), 20);
    let saved = snapshot(&root);
    let old = store.write_if_changed(&saved).unwrap().unwrap();
    let corrupt = store.dir().join("2.toml");
    std::fs::write(&corrupt, "broken = [").unwrap();
    std::fs::write(store.dir().join("installation.toml"), "active = true").unwrap();
    let history = store.history().unwrap();
    assert_eq!(history, [corrupt, old]);
    let mut state = State::default();
    load(&mut state, &history[0], &root.0);
    assert!(state.message.contains("Cannot open this snapshot"));
    load(&mut state, &history[1], &root.0);
    assert_eq!(state.selected(), [0, 1]);
}

#[test]
fn property_display_text_cannot_emit_terminal_controls() {
    // QuickCheck needs owned values to generate and shrink counterexamples.
    #[allow(clippy::needless_pass_by_value)]
    fn property(text: String) -> bool {
        let output = safe(&text);
        !output.chars().any(char::is_control) && safe(&output) == output
    }
    crate::test_support::check(property as fn(String) -> bool);
}

#[test]
fn navigation_stays_within_rows_and_empty_search_results() {
    let root = Scratch::new("tui-navigation");
    let mut state = loaded(&root);
    state.handle(key(KeyCode::PageDown));
    assert_eq!(state.cursor, 3);
    state.handle(key(KeyCode::Down));
    assert_eq!(state.cursor, 3);
    state.handle(key(KeyCode::PageUp));
    assert_eq!(state.cursor, 0);
    state.query = "no matching row".into();
    state.handle(key(KeyCode::PageDown));
    state.handle(key(KeyCode::Char(' ')));
    assert_eq!(state.cursor, 0);
    assert!(state.current().is_none());
    assert_eq!(state.selected(), [0, 1]);
}

#[test]
fn setup_requires_its_own_review_and_escape_cancels_confirmation() {
    let mut state = State::default();
    assert_eq!(
        state.handle(key(KeyCode::Char('i'))),
        Action::PrepareInstall
    );
    assert!(state.screen == Screen::Browse);
    state.review_setup(false, vec![("Create", "/example/bin/mysessions".into())]);
    let text = rendered(&state, 80, 24);
    assert!(text.contains("Enable automatic capture?"));
    assert!(text.contains("/example/bin/mysessions"));
    assert!(text.contains("Enter confirm setup"));
    assert_eq!(state.handle(key(KeyCode::Enter)), Action::ApplySetup);
    state.handle(key(KeyCode::Esc));
    assert!(state.setup.is_none());
    assert_eq!(state.handle(key(KeyCode::Enter)), Action::None);
    assert_eq!(
        state.handle(key(KeyCode::Char('u'))),
        Action::PrepareUninstall
    );
    state.review_setup(true, vec![("Remove", "/example/agent.plist".into())]);
    assert!(rendered(&state, 60, 20).contains("Disable automatic capture?"));
    assert!(rendered(&state, 60, 19).contains("Resize to at least"));
}

#[test]
fn typing_search_does_not_trigger_capture_or_installation() {
    let mut state = State::default();
    assert_eq!(state.handle(key(KeyCode::Char('c'))), Action::Capture);
    assert_eq!(state.handle(key(KeyCode::Char('r'))), Action::Refresh);
    state.handle(key(KeyCode::Char('/')));
    for c in "circuit".chars() {
        assert_eq!(state.handle(key(KeyCode::Char(c))), Action::None);
    }
    assert_eq!(state.query, "circuit");
    assert_eq!(
        state.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Action::Quit
    );
}

#[test]
fn capture_failure_preserves_choices_and_success_loads_newest_snapshot() {
    use std::os::unix::process::ExitStatusExt;
    let root = Scratch::new("tui-capture");
    let mut state = loaded(&root);
    let store = Store::new(root.0.join("state"), 20);
    let old = store
        .write_if_changed(state.snapshot.as_ref().unwrap())
        .unwrap()
        .unwrap();
    let mut browser = Browser::new(Some(&old), &store).unwrap();
    state.cursor = 2;
    state.handle(key(KeyCode::Char(' ')));
    let mut output = std::process::Output {
        status: std::process::ExitStatus::from_raw(256),
        stdout: Vec::new(),
        stderr: b"adapter failure".to_vec(),
    };
    let error = browser
        .capture_result(&mut state, &store, &root.0, &output)
        .unwrap_err();
    assert!(error.to_string().contains("adapter failure"));
    assert_eq!(state.selected(), [0, 1, 2]);
    assert_eq!(browser.history, std::slice::from_ref(&old));
    let mut newer = state.snapshot.clone().unwrap();
    newer.captured_at = 2;
    newer.sessions.truncate(1);
    let newest = store.write_if_changed(&newer).unwrap().unwrap();
    output.status = std::process::ExitStatus::from_raw(0);
    output.stderr = b"saved one session".to_vec();
    browser
        .capture_result(&mut state, &store, &root.0, &output)
        .unwrap();
    assert_eq!(browser.history, [newest, old]);
    assert_eq!(state.entries.len(), 1);
    assert_eq!(state.message, "saved one session");
    browser.navigate(&mut state, &root.0, 1);
    assert_eq!(state.entries.len(), 4);
    browser.refresh(&mut state, &store, &root.0, false).unwrap();
    assert_eq!(browser.index, 1); // Refresh keeps the currently inspected snapshot.
}

#[test]
fn setup_submission_without_a_prepared_plan_has_no_side_effects() {
    let root = Scratch::new("tui-setup-guard");
    let store = Store::new(root.0.join("state"), 20);
    let mut browser = Browser::new(None, &store).unwrap();
    let mut state = State::default();
    assert!(browser.apply_setup(&mut state).is_err());
    state.review_setup(false, Vec::new());
    assert!(browser.apply_setup(&mut state).is_err());
    assert!(!store.dir().exists());
}
