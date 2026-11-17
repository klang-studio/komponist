//! Shared policy for test-plugin bundles. Included only by test harnesses.

use std::path::Path;

/// Whether `path` exists. The gate requires fixtures; ad-hoc local tests may skip them.
pub fn available(path: &Path) -> bool {
    check(
        path,
        std::env::var_os("KOMPONIST_REQUIRE_FIXTURES").is_some_and(|v| v == "1"),
    )
}

/// Scan-based tests require the complete fixture set, not only the synth bundle.
pub fn bundle_dir(dir: &Path, format: &str) -> bool {
    [
        "kp-test-synth",
        "kp-test-fx",
        "kp-test-crash",
        "kp-test-slow",
        "kp-test-sc",
    ]
    .iter()
    .all(|name| available(&dir.join(format!("{name}.{format}"))))
}

fn check(path: &Path, required: bool) -> bool {
    if path.exists() {
        return true;
    }
    assert!(
        !required,
        "required test fixture is missing: {}; run scripts/build-test-plugins.sh",
        path.display()
    );
    eprintln!(
        "skipped: {} is missing; run scripts/build-test-plugins.sh",
        path.display()
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_bundle_skips_only_when_not_required() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/absent-test-plugin.clap");
        assert!(!path.exists());
        assert!(!check(&path, false));
        let failure = std::panic::catch_unwind(|| check(&path, true)).unwrap_err();
        let message = failure.downcast_ref::<String>().expect("panic message");
        assert!(message.contains(&path.display().to_string()));
        assert!(message.contains("scripts/build-test-plugins.sh"));
    }

    #[test]
    fn present_bundle_is_accepted_in_both_modes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(check(&path, false));
        assert!(check(&path, true));
    }
}
