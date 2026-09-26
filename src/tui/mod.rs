//! Standalone session browser with explicit capture, restore and setup actions.
//! Merely browsing never captures, opens tabs or changes installation files.
mod controller;
mod state;
#[cfg(test)]
mod tests;
mod view;

use crate::{
    adapters::{TerminalAdapter, ghostty::Ghostty},
    files, restore,
    snapshot::{Snapshot, Store, home_dir},
};
use anyhow::{Context, Result, ensure};
use controller::Browser;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyEventKind},
};
use state::{Action, Screen, State};
use std::{
    io::{self, IsTerminal},
    path::Path,
    time::Duration,
};

pub fn run(snapshot: Option<&Path>) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "the session browser needs an interactive terminal; use roost restore for a text preview"
    );
    let store = Store::default_location()?;
    let mut browser = Browser::new(snapshot, &store)?;
    let home = home_dir()?;
    let mut state = State::default();
    if let Some(path) = browser.history.first() {
        load(&mut state, path, &home);
    }
    // Restore even if initialization partly succeeds and then fails. Ratatui
    // additionally installs a panic hook to restore before printing a panic.
    let result = (|| {
        let mut terminal = ratatui::try_init()?;
        event_loop(&mut terminal, &mut state, &mut browser, &home, &store)
    })();
    ratatui::restore();
    result
}

fn load(state: &mut State, path: &Path, home: &Path) {
    let result = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))
        .and_then(|text| Snapshot::from_toml(&text));
    match result {
        Ok(snapshot) => state.load(snapshot, home),
        Err(error) => state.load_error(format!("Cannot open this snapshot: {error:#}")),
    }
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    state: &mut State,
    browser: &mut Browser,
    home: &Path,
    store: &Store,
) -> Result<()> {
    loop {
        terminal.draw(|frame| view::draw(frame, state, browser.index, browser.history.len()))?;
        let action = next_action(terminal, state)?;
        if state.screen != Screen::Setup {
            browser.cancel_setup();
        }
        if action == Action::Quit {
            return Ok(());
        }
        let result = match action {
            Action::Submit => submit(state, home, store, &Ghostty, |state| {
                terminal
                    .draw(|frame| view::draw(frame, state, browser.index, browser.history.len()))
                    .map(|_| ())
                    .map_err(Into::into)
            }),
            Action::Capture => {
                state.message = "Capturing open sessions...".into();
                terminal
                    .draw(|frame| view::draw(frame, state, browser.index, browser.history.len()))?;
                browser.capture(state, store, home)
            }
            _ => dispatch(action, state, browser, store, home),
        };
        if let Err(error) = result {
            state.message = format!("Could not continue: {error:#}");
        }
    }
}

fn dispatch(
    action: Action,
    state: &mut State,
    browser: &mut Browser,
    store: &Store,
    home: &Path,
) -> Result<()> {
    match action {
        Action::Older => browser.navigate(state, home, 1),
        Action::Newer => browser.navigate(state, home, -1),
        Action::Refresh => return browser.refresh(state, store, home, false),
        Action::PrepareInstall => return browser.prepare_setup(state, false),
        Action::PrepareUninstall => return browser.prepare_setup(state, true),
        Action::ApplySetup => return browser.apply_setup(state),
        _ => {}
    }
    Ok(())
}

fn next_action(terminal: &DefaultTerminal, state: &mut State) -> Result<Action> {
    if !event::poll(Duration::from_millis(250))? {
        return Ok(Action::None);
    }
    let Event::Key(key) = event::read()? else {
        return Ok(Action::None);
    };
    if key.kind != KeyEventKind::Press {
        return Ok(Action::None);
    }
    // Small terminals retain a clear quit path, but cannot confirm an
    // action whose review is currently hidden by the resize message.
    let size = terminal.size()?;
    let action = if size.width < 60 || size.height < 20 {
        if key.code == event::KeyCode::Char('q')
            || (key.code == event::KeyCode::Char('c')
                && key.modifiers.contains(event::KeyModifiers::CONTROL))
        {
            Action::Quit
        } else {
            Action::None
        }
    } else {
        state.handle(key)
    };
    Ok(action)
}

fn submit(
    state: &mut State,
    home: &Path,
    store: &Store,
    terminal: &dyn TerminalAdapter,
    mut progress: impl FnMut(&State) -> Result<()>,
) -> Result<()> {
    ensure!(
        state.screen == Screen::Review && !state.batch.is_empty(),
        "review a selection before opening tabs"
    );
    let _lock = files::try_lock(&store.dir().join(".capture.lock"))?
        .context("a capture or restore is running; try again shortly")?;
    for index in state.batch.clone() {
        if state.entries[index].status != state::Status::Ready {
            continue;
        }
        state.message = format!("Opening a new tab for {}...", state.entries[index].title());
        progress(state)?;
        let result = restore::submit(&state.entries[index].session, home, terminal);
        state.record_attempt(index, result);
    }
    state.screen = Screen::Results;
    state.cursor = 0;
    state.message =
        "New tabs may need agent trust or sign-in. Check any failed attempt before retrying."
            .into();
    Ok(())
}
