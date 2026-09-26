use std::process::{Command, Output};

fn roost(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(args)
        .output()
        .expect("roost binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_describes_the_public_commands() {
    let output = roost(&["--help"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    for command in ["browse", "capture", "restore", "install", "uninstall"] {
        assert!(help.contains(command), "help omitted {command}:\n{help}");
    }
    assert!(
        !help.contains("capture-worker"),
        "internal command was exposed"
    );
}

#[test]
fn version_matches_the_package_version() {
    let output = roost(&["--version"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output).trim(),
        concat!("roost ", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn invalid_arguments_fail_with_usage() {
    let output = roost(&["--not-a-roost-option"]);

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("unexpected argument"), "{error}");
    assert!(error.contains("Usage:"), "{error}");
}

#[test]
fn restore_of_a_missing_explicit_snapshot_fails_without_creating_it() {
    let missing = std::env::temp_dir().join(format!(
        "roost-cli-missing-{}-{}.toml",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    assert!(!missing.exists());

    let output = Command::new(env!("CARGO_BIN_EXE_roost"))
        .arg("restore")
        .arg("--snapshot")
        .arg(&missing)
        .output()
        .expect("roost binary should run");

    assert!(!output.status.success());
    assert!(stderr(&output).contains("reading"), "{}", stderr(&output));
    assert!(!missing.exists());
}
