use std::process::{Command, Output};

fn mysessions(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mysessions"))
        .args(args)
        .output()
        .expect("mysessions binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_describes_the_public_commands() {
    let output = mysessions(&["--help"]);

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
    let output = mysessions(&["--version"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output).trim(),
        concat!("mysessions ", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn invalid_arguments_fail_with_usage() {
    let output = mysessions(&["--not-a-mysessions-option"]);

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("unexpected argument"), "{error}");
    assert!(error.contains("Usage:"), "{error}");
}

#[test]
fn restore_of_a_missing_explicit_snapshot_fails_without_creating_it() {
    let missing = std::env::temp_dir().join(format!(
        "mysessions-cli-missing-{}-{}.toml",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    assert!(!missing.exists());

    let output = Command::new(env!("CARGO_BIN_EXE_mysessions"))
        .arg("restore")
        .arg("--snapshot")
        .arg(&missing)
        .output()
        .expect("mysessions binary should run");

    assert!(!output.status.success());
    assert!(stderr(&output).contains("reading"), "{}", stderr(&output));
    assert!(!missing.exists());
}
