//! Small filesystem operations shared by snapshots and installation.
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("file has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .context("file has no name")?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), nonce()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

/// The kernel releases flock even if a capture times out or the machine dies.
pub fn try_lock(path: &Path) -> Result<Option<File>> {
    std::fs::create_dir_all(path.parent().context("lock has no parent")?)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    // SAFETY: file owns a live descriptor for the whole call; flock takes no pointers.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(file))
    } else {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Scratch, lock_released, lock_released_within};
    use std::time::Duration;

    #[test]
    fn capture_lock_is_exclusive_and_released_when_owner_exits() {
        let root = Scratch::new("lock");
        let path = root.0.join("capture.lock");
        let first = try_lock(&path).unwrap().unwrap();
        assert!(try_lock(&path).unwrap().is_none());
        drop(first);
        assert!(lock_released(&path));
    }

    #[test]
    fn waiting_for_release_still_reports_a_lock_that_is_never_dropped() {
        let root = Scratch::new("lock-held");
        let path = root.0.join("capture.lock");
        let held = try_lock(&path).unwrap().unwrap();
        assert!(!lock_released_within(&path, Duration::from_millis(50)));
        drop(held);
    }
}
