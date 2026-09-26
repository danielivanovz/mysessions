//! Bound a capture, including its own subprocesses, without ever signalling an
//! agent. A hook invokes this supervisor and always returns success.
use anyhow::{Context, Result, bail};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn capture(dry_run: bool, hook: bool) -> Result<()> {
    let result = (|| {
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("capture-worker").stdin(Stdio::null());
        if dry_run {
            command.arg("--dry-run");
        }
        if hook {
            command.stdout(Stdio::null());
            let store = crate::snapshot::Store::default_location()?;
            std::fs::create_dir_all(store.dir())?;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(store.dir().join("capture.log"))?;
            command.stderr(Stdio::from(log));
        }
        let limit = Duration::from_secs(if hook { 3 } else { 10 });
        let status = run(&mut command, limit)?;
        if !status.success() {
            bail!("capture failed ({status})");
        }
        Ok(())
    })();
    if hook {
        if let Err(e) = result {
            eprintln!("roost: hook capture: {e:#}");
        }
        Ok(())
    } else {
        result
    }
}

pub fn run(command: &mut Command, limit: Duration) -> Result<std::process::ExitStatus> {
    let mut child = command
        .process_group(0)
        .spawn()
        .context("starting bounded command")?;
    // Convert explicitly before negating the pid into a process-group id. Do
    // not turn a platform assumption into a panic in this process supervisor.
    let group = i32::try_from(child.id()).context("child pid does not fit pid_t")?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if start.elapsed() >= limit {
            // SAFETY: kill takes no pointers. Only the process group we just created: capture and its ps,
            // lsof/osascript children. Agent processes are never in this group.
            let killed = unsafe { libc::kill(-group, libc::SIGKILL) };
            if killed != 0 {
                let error = std::io::Error::last_os_error();
                // Avoid blocking in wait if signalling the group failed. This
                // fallback only targets the child handle owned by this caller.
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("stopping timed-out capture process group");
            }
            child.wait().context("reaping timed-out capture process")?;
            bail!("capture exceeded {} seconds", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_command_returns_its_status() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 7"]);
        let status = run(&mut command, Duration::from_secs(1)).unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn stalled_capture_is_killed_with_its_own_child_process_group() {
        let root = crate::test_support::Scratch::new("deadline");
        let child_file = root.0.join("child-pid");
        let script = format!(
            "sleep 30 & echo $! > {}; wait",
            crate::snapshot::shell_quote(&child_file.to_string_lossy())
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        let start = Instant::now();
        assert!(run(&mut command, Duration::from_millis(150)).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(child_file).unwrap();
        // A killed child may briefly be a zombie; neither running nor sleeping
        // is allowed after the supervisor's deadline.
        let output = Command::new("/bin/ps")
            .args(["-p", pid.trim(), "-o", "stat="])
            .output()
            .unwrap();
        let state = String::from_utf8(output.stdout).unwrap();
        assert!(
            state.trim().is_empty() || state.trim().starts_with('Z'),
            "{state}"
        );
    }
}
