use std::path::{Path, PathBuf};

/// Repeatable generated cases with shrinking. Override the seed to explore
/// another input sequence; keep every counterexample as a regression test.
pub fn check(property: impl quickcheck::Testable) {
    let seed = std::env::var("ROOST_TEST_SEED")
        .map_or(0, |s| s.parse().expect("ROOST_TEST_SEED must be a u64"));
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
