//! One capture run: discover sessions from every agent adapter, pair them with
//! terminal surfaces, and hand the result to the snapshot store.

use crate::adapters::ghostty::strip_status_glyph;
use crate::adapters::{
    AgentAdapter, Surface, TerminalAdapter, claude::ClaudeCode, codex::Codex, ghostty::Ghostty,
    opencode::Opencode,
};
use crate::model::{Confidence, Session, TabRef};
use crate::snapshot::{Snapshot, Store, home_dir};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

pub fn run(dry_run: bool) -> Result<()> {
    let store = Store::default_location()?;
    let _lock = if dry_run {
        None
    } else {
        let Some(lock) = crate::files::try_lock(&store.dir().join(".capture.lock"))? else {
            eprintln!("mysessions: another capture or restore is in progress; skipped");
            return Ok(());
        };
        Some(lock)
    };
    let home = home_dir()?;
    let agents: Vec<Box<dyn AgentAdapter>> = vec![
        Box::new(ClaudeCode::new(home.clone())),
        Box::new(Opencode::new(home.clone())),
        Box::new(Codex::new(home)),
    ];

    let (sessions, failed) = discover(
        &agents,
        &Ghostty,
        std::env::var_os("MYSESSIONS_TRACE_CAPTURE").is_some(),
    );

    let snap = Snapshot::new(sessions);
    if dry_run {
        print!("{}", snap.to_toml()?);
        anyhow::ensure!(!failed, "capture incomplete; no snapshot written");
        return Ok(());
    }
    // A failed adapter must not replace a good snapshot with a partial set.
    anyhow::ensure!(!failed, "capture incomplete; previous snapshot preserved");
    match store.write_if_changed(&snap)? {
        Some(p) => eprintln!(
            "mysessions: wrote {} ({} sessions)",
            p.display(),
            snap.sessions.len()
        ),
        None => eprintln!("mysessions: unchanged ({} sessions)", snap.sessions.len()),
    }
    Ok(())
}

fn discover(
    agents: &[Box<dyn AgentAdapter>],
    terminal: &dyn TerminalAdapter,
    trace: bool,
) -> (Vec<Session>, bool) {
    let mut sessions = Vec::new();
    let mut failed = false;
    for agent in agents {
        if trace {
            eprintln!("mysessions: starting {}", agent.name());
        }
        match agent.discover() {
            Ok(mut s) => sessions.append(&mut s),
            // One agent's storage drifting must not hide the others, but it
            // must be loud.
            Err(e) => {
                eprintln!("mysessions: {}: {e:#}", agent.name());
                failed = true;
            }
        }
        if trace {
            eprintln!("mysessions: finished {}", agent.name());
        }
    }

    if trace {
        eprintln!("mysessions: starting ghostty");
    }
    match terminal.list_surfaces() {
        Ok(surfaces) => pair_tabs(&mut sessions, &surfaces),
        Err(e) => {
            eprintln!("mysessions: {}: {e:#}", terminal.name());
            failed = true;
        }
    }
    if trace {
        eprintln!("mysessions: finished ghostty");
    }

    (sessions, failed)
}

/// Pair sessions with terminal surfaces. The terminal exposes no pid or tty,
/// so the key is working directory plus title.
///
/// Two passes. First, title matches, which are exact and claim their surface.
/// Second, elimination among what is left: a session gets the one unclaimed
/// surface in its directory only if it is also the one unpaired session
/// there. Anything else with candidates is recorded as ambiguous, pointing
/// at the first candidate so the user has something to look at, never
/// presented as a fact.
fn pair_tabs(sessions: &mut [Session], surfaces: &[Surface]) {
    let mut claimed: HashSet<&str> = HashSet::new();

    let mut titles_per_dir: HashMap<(String, String), usize> = HashMap::new();
    for s in sessions.iter() {
        if let Some(title) = &s.title {
            *titles_per_dir
                .entry((s.cwd.to_string_lossy().into_owned(), title.clone()))
                .or_default() += 1;
        }
    }
    for s in sessions.iter_mut() {
        let Some(title) = s.title.as_deref() else {
            continue;
        };
        let cwd = s.cwd.to_string_lossy();
        if titles_per_dir.get(&(cwd.to_string(), title.to_string())) != Some(&1) {
            continue;
        }
        let mut hits = surfaces.iter().filter(|f| {
            f.cwd == cwd
                && strip_status_glyph(&f.title) == title
                && !claimed.contains(f.surface_id.as_str())
        });
        if let (Some(f), None) = (hits.next(), hits.next()) {
            claimed.insert(&f.surface_id);
            s.tab = Some(tab_ref(f, Confidence::Exact));
        }
    }

    let mut unpaired_per_dir: HashMap<String, usize> = HashMap::new();
    for s in sessions.iter().filter(|s| s.tab.is_none()) {
        *unpaired_per_dir
            .entry(s.cwd.to_string_lossy().into_owned())
            .or_default() += 1;
    }

    for s in sessions.iter_mut().filter(|s| s.tab.is_none()) {
        let cwd = s.cwd.to_string_lossy().into_owned();
        let candidates: Vec<&Surface> = surfaces
            .iter()
            .filter(|f| f.cwd == cwd && !claimed.contains(f.surface_id.as_str()))
            .collect();
        let alone = unpaired_per_dir.get(&cwd).copied().unwrap_or(0) == 1;
        let (pick, confidence) = match candidates.as_slice() {
            [] => continue,
            [one] if alone => (*one, Confidence::Exact),
            [first, ..] => (*first, Confidence::Ambiguous),
        };
        s.tab = Some(tab_ref(pick, confidence));
    }
}

fn tab_ref(f: &Surface, confidence: Confidence) -> TabRef {
    TabRef {
        window_id: f.window_id.clone(),
        tab_id: f.tab_id.clone(),
        surface_id: f.surface_id.clone(),
        title: f.title.clone(),
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Strategy};
    use std::path::PathBuf;

    #[test]
    fn property_exact_tab_pairings_are_unique_and_independent_of_order() {
        // Colliding directories/titles must retain ambiguity when either
        // agent discovery or the terminal enumerates the same data differently.
        fn property(session_keys: Vec<(u8, u8)>, surface_keys: Vec<(u8, u8)>) -> bool {
            let mut sessions: Vec<_> = session_keys
                .into_iter()
                .enumerate()
                .map(|(i, (dir, title))| {
                    session(
                        &i.to_string(),
                        &format!("/dir/{}", dir % 4),
                        Some(&format!("title {}", title % 4)),
                    )
                })
                .collect();
            let mut surfaces: Vec<_> = surface_keys
                .into_iter()
                .enumerate()
                .map(|(i, (dir, title))| {
                    surface(
                        &i.to_string(),
                        &format!("/dir/{}", dir % 4),
                        &format!("title {}", title % 4),
                    )
                })
                .collect();
            let mut reversed = sessions.clone();
            pair_tabs(&mut sessions, &surfaces);
            reversed.reverse();
            surfaces.reverse();
            pair_tabs(&mut reversed, &surfaces);
            let exact = |sessions: &[Session]| {
                sessions
                    .iter()
                    .filter_map(|s| {
                        s.tab
                            .as_ref()
                            .filter(|tab| tab.confidence == Confidence::Exact)
                            .map(|tab| (s.session_id.clone(), tab.surface_id.clone()))
                    })
                    .collect::<std::collections::BTreeMap<_, _>>()
            };
            let forward = exact(&sessions);
            forward.values().collect::<HashSet<_>>().len() == forward.len()
                && forward == exact(&reversed)
        }
        crate::test_support::check(property as fn(Vec<(u8, u8)>, Vec<(u8, u8)>) -> bool);
    }

    fn session(id: &str, cwd: &str, title: Option<&str>) -> Session {
        Session {
            agent: Agent::ClaudeCode,
            session_id: id.into(),
            cwd: PathBuf::from(cwd),
            label: None,
            title: title.map(str::to_string),
            strategy: Strategy::ProcessRegistry,
            process_confidence: Confidence::Exact,
            pid: None,
            transcript: None,
            database: None,
            tab: None,
        }
    }

    fn surface(id: &str, cwd: &str, title: &str) -> Surface {
        Surface {
            window_id: "w".into(),
            tab_id: format!("tab-{id}"),
            surface_id: id.into(),
            cwd: cwd.into(),
            title: title.into(),
        }
    }

    #[test]
    fn title_match_claims_surface_and_blocks_elimination() {
        // Two sessions in one directory, one tab. The tab's title names the
        // second session; the first must not take it by elimination.
        let mut s = vec![
            session("a", "/d", Some("Fix corruption")),
            session("b", "/d", Some("Review issue 58")),
        ];
        let f = vec![surface("s1", "/d", "✳ Review issue 58")];
        pair_tabs(&mut s, &f);
        assert!(s[0].tab.is_none());
        assert_eq!(s[1].tab.as_ref().unwrap().surface_id, "s1");
        assert_eq!(s[1].tab.as_ref().unwrap().confidence, Confidence::Exact);
    }

    #[test]
    fn elimination_needs_one_session_and_one_surface() {
        let mut s = vec![session("a", "/d", Some("Old title"))];
        let f = vec![surface("s1", "/d", "✳ Newer title")];
        pair_tabs(&mut s, &f);
        assert_eq!(s[0].tab.as_ref().unwrap().confidence, Confidence::Exact);

        let mut s = vec![session("a", "/d", None), session("b", "/d", None)];
        let f = vec![surface("s1", "/d", "x"), surface("s2", "/d", "y")];
        pair_tabs(&mut s, &f);
        assert_eq!(s[0].tab.as_ref().unwrap().confidence, Confidence::Ambiguous);
        assert_eq!(s[1].tab.as_ref().unwrap().confidence, Confidence::Ambiguous);
    }

    #[test]
    fn duplicate_titles_cannot_claim_an_exact_surface() {
        let mut s = vec![
            session("a", "/d", Some("same")),
            session("b", "/d", Some("same")),
        ];
        pair_tabs(
            &mut s,
            &[surface("one", "/d", "same"), surface("two", "/d", "same")],
        );
        assert!(
            s.iter()
                .all(|s| s.tab.as_ref().unwrap().confidence == Confidence::Ambiguous)
        );
        let mut s = vec![session("a", "/d", Some("same"))];
        pair_tabs(
            &mut s,
            &[surface("one", "/d", "same"), surface("two", "/d", "same")],
        );
        assert_eq!(s[0].tab.as_ref().unwrap().confidence, Confidence::Ambiguous);
    }
}
