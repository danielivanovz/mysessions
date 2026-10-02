use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Repeatable generated cases with shrinking. Override the seed to explore
/// another input sequence; keep every counterexample as a regression test.
pub fn check(property: impl quickcheck::Testable) {
    let seed = std::env::var("MYSESSIONS_TEST_SEED").map_or(0, |s| {
        s.parse().expect("MYSESSIONS_TEST_SEED must be a u64")
    });
    let cases = std::env::var("QUICKCHECK_TESTS")
        .map_or(256, |s| s.parse().expect("QUICKCHECK_TESTS must be a u64"));
    assert!(cases > 0, "QUICKCHECK_TESTS must be positive");
    eprintln!("QuickCheck seed={seed}, cases={cases}");
    quickcheck::QuickCheck::new()
        .rng(quickcheck::Gen::from_size_and_seed(32, seed))
        .tests(cases)
        .max_tests(cases)
        .min_tests_passed(cases)
        .quickcheck(property);
}

/// Whether the flock on `path` can be taken within two seconds.
///
/// Tests run on parallel threads and several spawn children. On macOS a child
/// started through `posix_spawn` keeps a reference to every descriptor of this
/// process, close-on-exec ones included, until it execs, so a lock dropped at
/// that moment stays held for a few milliseconds more. A lock whose owner still
/// holds it, or never released it, stays held past the bound and returns false.
/// The probe's own lock is released before returning.
pub fn lock_released(path: &Path) -> bool {
    lock_released_within(path, Duration::from_secs(2))
}

/// [`lock_released`] with an explicit bound, so a test can assert that a held
/// lock is still reported as held without waiting the full two seconds.
///
/// # Panics
///
/// If the lock file cannot be opened, or flock fails for any reason other than
/// contention.
pub fn lock_released_within(path: &Path, limit: Duration) -> bool {
    let start = Instant::now();
    loop {
        if crate::files::try_lock(path).unwrap().is_some() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(label: &str) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scratch")
            .join(format!(
                "{label}-{}-{}",
                std::process::id(),
                crate::files::nonce()
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
