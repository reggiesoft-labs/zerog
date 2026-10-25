//! Shared policy for tests that need a real Linux display server.
//!
//! Live tests skip themselves by default so `cargo test` stays usable on
//! machines without X11 or Wayland. The container harness sets
//! [`REQUIRE_LIVE_ENV`], which turns every skip into a hard failure so a broken
//! Xvfb/Weston environment can never report a vacuous pass.

/// When set to a truthy value, live tests fail instead of skipping.
pub(crate) const REQUIRE_LIVE_ENV: &str = "MOUSE_THING_REQUIRE_LIVE";

const X11_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const X11_CONNECT_POLL: std::time::Duration = std::time::Duration::from_millis(100);

fn require_live_enabled() -> bool {
    std::env::var(REQUIRE_LIVE_ENV)
        .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Returns `true` when a live test may run.
///
/// Callers probe their own environment and pass the result as `available`.
/// When `available` is false this either prints a skip notice (the default) or
/// panics when [`REQUIRE_LIVE_ENV`] is enabled.
pub(crate) fn require_live(test: &str, available: bool, unavailable_reason: &str) -> bool {
    if !require_live_enabled() {
        eprintln!(
            "SKIP {test}: live testing disabled by default (set {REQUIRE_LIVE_ENV}=1 in an isolated environment to run)"
        );
        return false;
    }

    if available {
        return true;
    }

    panic!(
        "{test} needs a live environment but it is unavailable: {unavailable_reason}. \
         {} is enabled, so this is treated as a failure.",
        REQUIRE_LIVE_ENV
    );
}

/// Probes for a usable X11 display, waiting briefly so a freshly started Xvfb
/// has time to accept connections.
pub(crate) fn x11_display_available() -> bool {
    let has_display = std::env::var("DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    if !has_display {
        return false;
    }

    let started = std::time::Instant::now();
    while started.elapsed() < X11_CONNECT_TIMEOUT {
        if x11rb::connect(None).is_ok() {
            return true;
        }
        std::thread::sleep(X11_CONNECT_POLL);
    }
    false
}

pub(crate) fn wayland_display_available() -> bool {
    std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref val) = self.original {
                unsafe { std::env::set_var(self.key, val) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    #[test]
    #[serial]
    fn require_live_skips_by_default() {
        let _guard = EnvGuard::remove(REQUIRE_LIVE_ENV);
        assert!(!require_live("test_skip", true, "display available"));
    }

    #[test]
    #[serial]
    fn require_live_allows_when_enabled_and_available() {
        let _guard = EnvGuard::set(REQUIRE_LIVE_ENV, "1");
        assert!(require_live("test_allow", true, "display available"));
    }

    #[test]
    #[serial]
    #[should_panic(expected = "needs a live environment but it is unavailable")]
    fn require_live_panics_when_enabled_and_unavailable() {
        let _guard = EnvGuard::set(REQUIRE_LIVE_ENV, "1");
        let _ = require_live("test_panic", false, "display missing");
    }
}

