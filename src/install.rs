//! Install only My Sessions' scheduler and `SessionStart` hook, preserving other
//! settings. Plans are read-only; applying a plan checks for concurrent edits.
use crate::{
    files,
    snapshot::{Store, home_dir, shell_quote},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "local.mysessions.capture";

struct Paths {
    binary: PathBuf,
    plist: PathBuf,
    settings: PathBuf,
    state: PathBuf,
}
impl Paths {
    fn new(home: &Path, state: PathBuf) -> Self {
        Self {
            binary: home.join(".local/bin/mysessions"),
            plist: home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            settings: home.join(".claude/settings.json"),
            state,
        }
    }
    fn record(&self) -> PathBuf {
        self.state.join("installation.toml")
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    active: bool,
    hook_command: String,
    plist: String,
}

struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    mode: u32,
}
impl Change {
    fn new(path: PathBuf, after: Option<Vec<u8>>, mode: u32) -> Result<Self> {
        let before = read_optional(&path)?;
        Ok(Self {
            path,
            before,
            after,
            mode,
        })
    }
    fn changed(&self) -> bool {
        self.before != self.after
    }
    fn write(&self) -> Result<()> {
        ensure!(
            read_optional(&self.path)? == self.before,
            "{} changed since planning; retry",
            self.path.display()
        );
        write_optional(&self.path, self.after.as_deref(), self.mode)
    }
    fn rollback(&self) -> Result<()> {
        ensure!(
            read_optional(&self.path)? == self.after,
            "{} changed concurrently; keeping that edit",
            self.path.display()
        );
        write_optional(&self.path, self.before.as_deref(), self.mode)
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        ensure!(
            !meta.file_type().is_symlink(),
            "refusing to replace symlink {}",
            path.display()
        );
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_optional(path: &Path, content: Option<&[u8]>, mode: u32) -> Result<()> {
    match content {
        Some(bytes) => files::atomic_write(path, bytes, mode),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
    }
}

fn record(paths: &Paths) -> Result<Option<Record>> {
    read_optional(&paths.record())?
        .map(|b| toml::from_str(std::str::from_utf8(&b)?).context("reading installation record"))
        .transpose()
}

fn environment(paths: &Paths) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        (
            "XDG_STATE_HOME".into(),
            paths.state.parent().unwrap().to_string_lossy().into_owned(),
        ),
    ]);
    for key in ["XDG_DATA_HOME", "CODEX_HOME"] {
        if let Some(value) = std::env::var_os(key) {
            env.insert(key.into(), value.to_string_lossy().into_owned());
        }
    }
    env
}

fn hook_command(paths: &Paths, env: &BTreeMap<String, String>) -> String {
    let assignments = env
        .iter()
        .map(|(k, v)| format!("{k}={}", shell_quote(v)))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{assignments} {} capture --hook >> {} 2>&1 || :",
        shell_quote(&paths.binary.to_string_lossy()),
        shell_quote(&paths.state.join("capture.log").to_string_lossy())
    )
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn plist(paths: &Paths, env: &BTreeMap<String, String>) -> String {
    let mut variables = String::new();
    for (key, value) in env {
        write!(
            variables,
            "<key>{}</key><string>{}</string>",
            xml(key),
            xml(value)
        )
        .expect("writing to a String cannot fail");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>capture</string></array>
  <key>StartInterval</key><integer>60</integer>
  <key>RunAtLoad</key><true/>
  <key>Umask</key><integer>63</integer>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>EnvironmentVariables</key><dict>{variables}</dict>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict></plist>
"#,
        xml(&paths.binary.to_string_lossy()),
        xml(&paths.state.join("capture.log").to_string_lossy()),
        xml(&paths.state.join("capture.log").to_string_lossy())
    )
}

/// Remove only exact managed commands, even if the user moved them into a
/// shared matcher group. Unrelated events and handlers retain their values.
fn settings(bytes: Option<&[u8]>, remove: &[&str], add: Option<&str>) -> Result<Vec<u8>> {
    let mut root: Value = match bytes {
        Some(b) => serde_json::from_slice(b).context("parsing Claude settings")?,
        None => json!({}),
    };
    let object = root
        .as_object_mut()
        .context("Claude settings must be an object")?;
    if add.is_some() && !object.contains_key("hooks") {
        object.insert("hooks".into(), json!({}));
    }
    if let Some(hooks) = object.get_mut("hooks") {
        let hooks = hooks
            .as_object_mut()
            .context("Claude hooks must be an object")?;
        update_start(hooks, remove, add)?;
        // Empty event containers are harmless and may predate installation.
        // Ownership of a command does not imply ownership of its container.
    }
    // Avoid formatting-only edits, notably on uninstall after a manual removal.
    if let Some(bytes) = bytes
        && serde_json::from_slice::<Value>(bytes)? == root
    {
        return Ok(bytes.to_vec());
    }
    let mut bytes = serde_json::to_vec_pretty(&root)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn update_start(
    hooks: &mut serde_json::Map<String, Value>,
    remove: &[&str],
    add: Option<&str>,
) -> Result<()> {
    if let Some(start) = hooks.get_mut("SessionStart") {
        remove_handlers(start, remove)?;
    }
    if let Some(command) = add {
        let groups = hooks
            .entry("SessionStart")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("SessionStart must be an array")?;
        groups.push(
            json!({"matcher":"", "hooks":[{"type":"command", "command":command, "timeout":5}]}),
        );
    }
    Ok(())
}

fn remove_handlers(start: &mut Value, remove: &[&str]) -> Result<()> {
    let groups = start
        .as_array_mut()
        .context("SessionStart must be an array")?;
    // Validate before editing, then remove groups only when our handler was
    // their last entry. Preserve pre-existing empty matcher groups verbatim.
    for group in groups.iter() {
        ensure!(
            group.get("hooks").is_some_and(Value::is_array),
            "SessionStart matcher must have a hooks array"
        );
    }
    groups.retain_mut(|group| {
        let handlers = group["hooks"]
            .as_array_mut()
            .expect("validated hooks array");
        let before = handlers.len();
        handlers.retain(|h| {
            !h.get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| remove.contains(&c))
        });
        before == handlers.len() || !handlers.is_empty()
    });
    Ok(())
}

struct Plan {
    changes: Vec<Change>,
    backup: Option<PathBuf>,
    hook: String,
    plist: String,
    uninstall: bool,
}

fn plan(paths: &Paths, source: &Path, uninstall: bool) -> Result<Plan> {
    let previous = record(paths)?;
    let env = environment(paths);
    let (command, document) = match (uninstall, previous.as_ref()) {
        (true, Some(previous)) => (previous.hook_command.clone(), previous.plist.clone()),
        (true, None) => bail!("no managed My Sessions installation found"),
        (false, _) => (hook_command(paths, &env), plist(paths, &env)),
    };
    check_ownership(paths, source, previous.as_ref(), uninstall)?;
    let mut changes = Vec::new();
    if !uninstall {
        changes.push(Change::new(
            paths.binary.clone(),
            Some(std::fs::read(source)?),
            0o755,
        )?);
    }
    if let Some(change) = settings_change(paths, &command, previous.as_ref(), uninstall)? {
        changes.push(change);
    }
    changes.push(Change::new(
        paths.plist.clone(),
        (!uninstall).then(|| document.as_bytes().to_vec()),
        0o600,
    )?);
    let record = Record {
        active: !uninstall,
        hook_command: command.clone(),
        plist: document.clone(),
    };
    changes.push(Change::new(
        paths.record(),
        Some(toml::to_string_pretty(&record)?.into_bytes()),
        0o600,
    )?);
    Ok(Plan {
        backup: changes
            .iter()
            .any(|c| c.path == paths.settings && c.changed() && c.before.is_some())
            .then(|| {
                paths
                    .state
                    .join(format!("claude-settings-{}.json", files::nonce()))
            }),
        changes,
        hook: command,
        plist: document,
        uninstall,
    })
}

fn settings_change(
    paths: &Paths,
    command: &str,
    previous: Option<&Record>,
    uninstall: bool,
) -> Result<Option<Change>> {
    let old_settings = read_optional(&paths.settings)?;
    let mut remove = vec![command];
    if let Some(previous) = previous {
        remove.push(&previous.hook_command);
    }
    let new_settings = settings(
        old_settings.as_deref(),
        &remove,
        (!uninstall).then_some(command),
    )?;
    if old_settings.is_some() || !uninstall {
        Ok(Some(Change {
            path: paths.settings.clone(),
            before: old_settings,
            after: Some(new_settings),
            mode: 0o600,
        }))
    } else {
        Ok(None)
    }
}

fn check_ownership(
    paths: &Paths,
    source: &Path,
    previous: Option<&Record>,
    uninstall: bool,
) -> Result<()> {
    if let Some(existing) = read_optional(&paths.plist)? {
        ensure!(
            previous.is_some_and(|p| p.plist.as_bytes() == existing),
            "existing launch agent is not the recorded My Sessions file; preserve or move it before retrying"
        );
    }
    if !uninstall {
        ensure!(
            previous.is_some() || !paths.binary.try_exists()? || source == paths.binary,
            "{} exists without a My Sessions installation record; refusing to overwrite it",
            paths.binary.display()
        );
    }
    Ok(())
}

trait Service {
    fn loaded(&self) -> Result<bool>;
    fn start(&self, plist: &Path) -> Result<()>;
    fn stop(&self) -> Result<()>;
}
struct Launchctl;
impl Launchctl {
    fn domain() -> String {
        // SAFETY: getuid takes no arguments or pointers and has no preconditions.
        format!("gui/{}", unsafe { libc::getuid() })
    }
    fn target() -> String {
        format!("{}/{LABEL}", Self::domain())
    }
    fn call(args: &[&str]) -> Result<()> {
        let out = Command::new("/bin/launchctl").args(args).output()?;
        ensure!(
            out.status.success(),
            "launchctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(())
    }
}
impl Service for Launchctl {
    fn loaded(&self) -> Result<bool> {
        Ok(Command::new("/bin/launchctl")
            .args(["print", &Self::target()])
            .output()?
            .status
            .success())
    }
    fn start(&self, plist: &Path) -> Result<()> {
        Self::call(&["bootstrap", &Self::domain(), &plist.to_string_lossy()])
    }
    fn stop(&self) -> Result<()> {
        Self::call(&["bootout", &Self::target()])
    }
}

fn apply(paths: &Paths, plan: &Plan, service: &dyn Service) -> Result<()> {
    // A TUI review can stay open while an editor or another installer runs.
    // Check even unchanged files before touching the service or writing backups.
    for change in &plan.changes {
        ensure!(
            read_optional(&change.path)? == change.before,
            "{} changed since planning; review a fresh plan",
            change.path.display()
        );
    }
    let loaded = service.loaded()?;
    ensure!(
        !loaded || record(paths)?.is_some(),
        "a service named {LABEL} is loaded without a My Sessions installation record; refusing to replace it"
    );
    if plan.changes.iter().all(|c| !c.changed()) && loaded != plan.uninstall {
        return Ok(());
    }
    if let Some(change) = plan
        .changes
        .iter()
        .find(|c| c.path == paths.settings && c.changed())
        && let Some(bytes) = &change.before
    {
        let backup = plan
            .backup
            .as_ref()
            .context("settings backup path missing")?;
        files::atomic_write(backup, bytes, 0o600)?;
    }
    if loaded {
        service.stop()?;
    }
    let mut applied: Vec<&Change> = Vec::new();
    let result: Result<()> = (|| {
        for change in plan.changes.iter().filter(|c| c.changed()) {
            change.write()?;
            applied.push(change);
        }
        if !plan.uninstall {
            service.start(&paths.plist)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let rollback_errors = rollback(paths, &applied, service, loaded);
        if !rollback_errors.is_empty() {
            bail!(
                "{error:#}; rollback needs attention: {}",
                rollback_errors.join("; ")
            );
        }
        return Err(error).context("installation change failed; restored previous files/service");
    }
    Ok(())
}

fn rollback(
    paths: &Paths,
    applied: &[&Change],
    service: &dyn Service,
    loaded: bool,
) -> Vec<String> {
    let mut rollback_errors = Vec::new();
    // A bootstrap failure normally means no service was registered, but
    // check before restoring files instead of relying on that assumption.
    match service.loaded() {
        Ok(true) => {
            if let Err(error) = service.stop() {
                rollback_errors.push(error.to_string());
            }
        }
        Ok(false) => {}
        Err(error) => rollback_errors.push(error.to_string()),
    }
    for change in applied.iter().rev() {
        if let Err(e) = change.rollback() {
            rollback_errors.push(e.to_string());
        }
    }
    if loaded && let Err(e) = service.start(&paths.plist) {
        rollback_errors.push(e.to_string());
    }
    rollback_errors
}

pub fn run(uninstall: bool, dry_run: bool) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "installation currently supports macOS only"
    );
    let paths = Paths::new(&home_dir()?, Store::default_location()?.dir().into());
    let _lock = if dry_run {
        None
    } else {
        Some(
            files::try_lock(&paths.state.join(".install.lock"))?
                .context("another install/uninstall is running")?,
        )
    };
    let plan = plan(&paths, &std::env::current_exe()?, uninstall)?;
    for change in plan.changes.iter().filter(|c| c.changed()) {
        println!(
            "{} {}",
            if change.after.is_some() {
                "write"
            } else {
                "remove"
            },
            change.path.display()
        );
    }
    if let Some(backup) = &plan.backup {
        println!("backup Claude settings to {}", backup.display());
    }
    if dry_run {
        println!(
            "\nSessionStart command:\n{}\n\nLaunch agent:\n{}",
            plan.hook, plan.plist
        );
        println!("Dry run: no files or services changed.");
        return Ok(());
    }
    apply(&paths, &plan, &Launchctl)?;
    eprintln!(
        "mysessions: {}",
        if uninstall {
            "scheduler and hook removed; binary, snapshots and backups retained"
        } else {
            "installed; capture scheduled every 60 seconds and on Claude SessionStart"
        }
    );
    Ok(())
}

/// Retains the exact bytes reviewed by the user until confirmation.
pub(crate) struct Prepared {
    paths: Paths,
    plan: Plan,
}

impl Prepared {
    pub(crate) fn new(uninstall: bool) -> Result<Self> {
        ensure!(
            cfg!(target_os = "macos"),
            "installation currently supports macOS only"
        );
        let paths = Paths::new(&home_dir()?, Store::default_location()?.dir().into());
        let plan = plan(&paths, &std::env::current_exe()?, uninstall)?;
        Ok(Self { paths, plan })
    }

    pub(crate) fn changes(&self) -> Vec<(&'static str, PathBuf)> {
        let mut changes: Vec<_> = self
            .plan
            .changes
            .iter()
            .map(|c| {
                let action = if !c.changed() {
                    "Unchanged"
                } else if c.after.is_none() {
                    "Remove"
                } else if c.before.is_some() {
                    "Update"
                } else {
                    "Create"
                };
                (action, c.path.clone())
            })
            .collect();
        if let Some(backup) = &self.plan.backup {
            changes.push(("Back up settings", backup.clone()));
        }
        changes
    }

    pub(crate) fn apply(self) -> Result<()> {
        let _lock = files::try_lock(&self.paths.state.join(".install.lock"))?
            .context("another install/uninstall is running")?;
        apply(&self.paths, &self.plan, &Launchctl)
    }
}

#[cfg(test)]
// QuickCheck requires owned arguments so it can generate and shrink them.
#[allow(clippy::needless_pass_by_value)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;
    use std::cell::Cell;

    #[test]
    fn property_uninstall_preserves_unrelated_settings_and_empty_groups() {
        fn property(commands: Vec<String>, setting: String, matcher: String) -> bool {
            let handlers: Vec<_> = commands
                .into_iter()
                .map(|c| json!({"type":"command", "command":format!("user:{c}")}))
                .collect();
            let original = json!({"custom":setting, "hooks": {
                "SessionStart":[{"matcher":matcher, "hooks":handlers}],
                "Stop":[]
            }});
            let bytes = serde_json::to_vec(&original).unwrap();
            let installed = settings(Some(&bytes), &["managed"], Some("managed")).unwrap();
            let repeated = settings(Some(&installed), &["managed"], Some("managed")).unwrap();
            let removed = settings(Some(&installed), &["managed"], None).unwrap();
            installed == repeated && serde_json::from_slice::<Value>(&removed).unwrap() == original
        }
        crate::test_support::check(property as fn(Vec<String>, String, String) -> bool);
    }

    #[derive(Default)]
    struct FakeService {
        loaded: Cell<bool>,
        starts: Cell<usize>,
        fail_next: Cell<bool>,
    }
    impl Service for FakeService {
        fn loaded(&self) -> Result<bool> {
            Ok(self.loaded.get())
        }
        fn start(&self, _: &Path) -> Result<()> {
            self.starts.set(self.starts.get() + 1);
            if self.fail_next.replace(false) {
                bail!("injected bootstrap failure");
            }
            self.loaded.set(true);
            Ok(())
        }
        fn stop(&self) -> Result<()> {
            self.loaded.set(false);
            Ok(())
        }
    }
    fn fixture(root: &Scratch) -> (Paths, PathBuf, Vec<u8>) {
        let paths = Paths::new(
            &root.0.join("home with ' quotes"),
            root.0.join("state/mysessions"),
        );
        let source = root.0.join("source");
        std::fs::write(&source, b"test executable").unwrap();
        let original = br#"{"env":{"EXISTING":"kept"},"hooks":{"SessionStart":[{"matcher":"resume","hooks":[{"type":"command","command":"echo user hook"}]}],"Stop":[{"hooks":[{"type":"command","command":"echo stop"}]}]}}"#.to_vec();
        files::atomic_write(&paths.settings, &original, 0o600).unwrap();
        (paths, source, original)
    }
    #[test]
    fn install_is_idempotent_and_uninstall_keeps_other_hooks_and_snapshots() {
        let root = Scratch::new("installation");
        let (paths, source, original) = fixture(&root);
        let service = FakeService::default();
        let first = plan(&paths, &source, false).unwrap();
        assert!(!paths.binary.exists()); // Planning is read-only.
        apply(&paths, &first, &service).unwrap();
        assert!(service.loaded.get());
        assert!(paths.binary.is_file());
        let doc: Value = serde_json::from_slice(&std::fs::read(&paths.settings).unwrap()).unwrap();
        assert_eq!(doc["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
        let second = plan(&paths, &source, false).unwrap();
        assert!(second.changes.iter().all(|c| !c.changed()));
        apply(&paths, &second, &service).unwrap();
        assert_eq!(service.starts.get(), 1);
        let snapshot = paths.state.join("saved.toml");
        std::fs::write(&snapshot, "preserve").unwrap();
        apply(&paths, &plan(&paths, &source, true).unwrap(), &service).unwrap();
        assert!(!service.loaded.get());
        assert!(!paths.plist.exists());
        assert!(paths.binary.exists());
        assert_eq!(std::fs::read_to_string(snapshot).unwrap(), "preserve");
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&paths.settings).unwrap()).unwrap(),
            serde_json::from_slice::<Value>(&original).unwrap()
        );
        assert!(!record(&paths).unwrap().unwrap().active);
        apply(&paths, &plan(&paths, &source, true).unwrap(), &service).unwrap();
    }
    #[test]
    fn failed_bootstrap_rolls_back_files_and_a_failed_upgrade_reloads_old_service() {
        let root = Scratch::new("install-rollback");
        let (paths, source, original) = fixture(&root);
        let service = FakeService::default();
        service.fail_next.set(true);
        assert!(apply(&paths, &plan(&paths, &source, false).unwrap(), &service).is_err());
        assert_eq!(std::fs::read(&paths.settings).unwrap(), original);
        assert!(!paths.binary.exists() && !paths.plist.exists() && !paths.record().exists());
        apply(&paths, &plan(&paths, &source, false).unwrap(), &service).unwrap();
        let before = std::fs::read(&paths.binary).unwrap();
        std::fs::write(&source, b"updated executable").unwrap();
        service.fail_next.set(true);
        assert!(apply(&paths, &plan(&paths, &source, false).unwrap(), &service).is_err());
        assert!(service.loaded.get());
        assert_eq!(std::fs::read(paths.binary).unwrap(), before);
    }
    #[test]
    fn concurrent_settings_edit_is_preserved_and_unmanaged_files_are_refused() {
        let root = Scratch::new("install-conflict");
        let (paths, source, _) = fixture(&root);
        let pending = plan(&paths, &source, false).unwrap();
        std::fs::write(&paths.settings, b"{\"new-user-setting\":true}").unwrap();
        assert!(apply(&paths, &pending, &FakeService::default()).is_err());
        assert_eq!(
            std::fs::read_to_string(&paths.settings).unwrap(),
            "{\"new-user-setting\":true}"
        );
        assert!(!paths.binary.exists());
        files::atomic_write(&paths.plist, b"unrelated service", 0o600).unwrap();
        assert!(plan(&paths, &source, false).is_err());
    }

    #[test]
    fn reviewed_backup_is_used_and_even_unchanged_files_are_rechecked() {
        let root = Scratch::new("install-review");
        let (paths, source, original) = fixture(&root);
        let prepared = Prepared {
            plan: plan(&paths, &source, false).unwrap(),
            paths,
        };
        let changes = prepared.changes();
        let backup = &changes
            .iter()
            .find(|(action, _)| *action == "Back up settings")
            .unwrap()
            .1;
        assert!(!backup.exists());
        let service = FakeService::default();
        apply(&prepared.paths, &prepared.plan, &service).unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), original);
        let repeated = plan(&prepared.paths, &source, false).unwrap();
        assert!(repeated.changes.iter().all(|c| !c.changed()));
        std::fs::write(&prepared.paths.settings, b"{\"edited\":true}").unwrap();
        let error = apply(&prepared.paths, &repeated, &service).unwrap_err();
        assert!(error.to_string().contains("changed since planning"));
        assert!(service.loaded.get());
        assert_eq!(service.starts.get(), 1);
        assert_eq!(
            std::fs::read(&prepared.paths.settings).unwrap(),
            b"{\"edited\":true}"
        );
    }

    #[test]
    fn failed_rollback_status_check_is_reported_even_when_files_are_restored() {
        struct UnreadableAfterStart(Cell<bool>);
        impl Service for UnreadableAfterStart {
            fn loaded(&self) -> Result<bool> {
                ensure!(self.0.replace(false), "cannot read service status");
                Ok(false)
            }
            fn start(&self, _: &Path) -> Result<()> {
                bail!("bootstrap failed")
            }
            fn stop(&self) -> Result<()> {
                bail!("service status is unknown")
            }
        }
        let root = Scratch::new("rollback-status");
        let (paths, source, original) = fixture(&root);
        let pending = plan(&paths, &source, false).unwrap();
        let error = apply(&paths, &pending, &UnreadableAfterStart(Cell::new(true))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("rollback needs attention: cannot read service status")
        );
        assert_eq!(std::fs::read(&paths.settings).unwrap(), original);
        assert!(!paths.binary.exists() && !paths.plist.exists());
    }
    #[test]
    fn shared_matcher_removes_only_managed_handler_and_rejects_invalid_settings() {
        let initial = br#"{"hooks":{"SessionStart":[{"matcher":"resume","hooks":[{"type":"command","command":"managed"},{"type":"command","command":"mine"}]}]}}"#;
        let doc: Value =
            serde_json::from_slice(&settings(Some(initial), &["managed"], None).unwrap()).unwrap();
        assert_eq!(
            doc["hooks"]["SessionStart"][0]["hooks"],
            json!([{"type":"command","command":"mine"}])
        );
        assert!(settings(Some(b"{broken"), &[], Some("managed")).is_err());
        assert!(settings(Some(b"{\"hooks\":[]}"), &[], Some("managed")).is_err());
    }

    #[test]
    fn removing_a_hook_keeps_preexisting_empty_groups_and_containers() {
        let original = br#"{ "hooks": {"SessionStart": [{"matcher":"", "hooks":[]}], "Stop":[]} }"#;
        let installed = settings(Some(original), &[], Some("managed")).unwrap();
        let removed = settings(Some(&installed), &["managed"], None).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&removed).unwrap(),
            serde_json::from_slice::<Value>(original).unwrap()
        );
        let empty = br#"{ "hooks": {"SessionStart": []} }"#;
        assert_eq!(settings(Some(empty), &["managed"], None).unwrap(), empty);
    }
    #[test]
    fn generated_plist_is_valid_xml_and_carries_literal_paths_and_minute_interval() {
        let root = Scratch::new("plist");
        let (paths, _, _) = fixture(&root);
        let doc = plist(&paths, &environment(&paths));
        let path = root.0.join("test.plist");
        std::fs::write(&path, doc).unwrap();
        let status = Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let out = Command::new("/usr/bin/plutil")
            .args(["-extract", "StartInterval", "raw", "-o", "-"])
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "60");
    }
}
