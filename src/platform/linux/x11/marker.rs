//! Local self-injection suppression markers for X11.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SUPPRESS_WINDOW_US: u64 = 25_000;

static START_INSTANT: OnceLock<Instant> = OnceLock::new();
static LAST_MOVE_X: AtomicI32 = AtomicI32::new(i32::MIN);
static LAST_MOVE_Y: AtomicI32 = AtomicI32::new(i32::MIN);
static LAST_MOVE_AT_US: AtomicU64 = AtomicU64::new(0);
static LAST_BUTTON: AtomicU8 = AtomicU8::new(u8::MAX);
static LAST_BUTTON_PRESSED: AtomicBool = AtomicBool::new(false);
static LAST_BUTTON_AT_US: AtomicU64 = AtomicU64::new(0);
static LAST_KEY: AtomicU16 = AtomicU16::new(u16::MAX);
static LAST_KEY_PRESSED: AtomicBool = AtomicBool::new(false);
static LAST_KEY_AT_US: AtomicU64 = AtomicU64::new(0);
static SOURCE_MARKER: OnceLock<u64> = OnceLock::new();

pub(crate) fn source_marker() -> u64 {
    *SOURCE_MARKER.get_or_init(generate_source_marker)
}

pub(crate) fn note_injected_move(x: i32, y: i32) {
    LAST_MOVE_X.store(x, Ordering::Relaxed);
    LAST_MOVE_Y.store(y, Ordering::Relaxed);
    LAST_MOVE_AT_US.store(now_us(), Ordering::Relaxed);
}

pub(crate) fn should_suppress_move(x: i32, y: i32) -> bool {
    let at = LAST_MOVE_AT_US.load(Ordering::Relaxed);
    if at == 0 || now_us().saturating_sub(at) > SUPPRESS_WINDOW_US {
        return false;
    }
    LAST_MOVE_X.load(Ordering::Relaxed) == x && LAST_MOVE_Y.load(Ordering::Relaxed) == y
}

pub(crate) fn note_injected_button(button: u8, pressed: bool) {
    LAST_BUTTON.store(button, Ordering::Relaxed);
    LAST_BUTTON_PRESSED.store(pressed, Ordering::Relaxed);
    LAST_BUTTON_AT_US.store(now_us(), Ordering::Relaxed);
}

pub(crate) fn should_suppress_button(button: u8, pressed: bool) -> bool {
    let at = LAST_BUTTON_AT_US.load(Ordering::Relaxed);
    if at == 0 || now_us().saturating_sub(at) > SUPPRESS_WINDOW_US {
        return false;
    }

    LAST_BUTTON.load(Ordering::Relaxed) == button
        && LAST_BUTTON_PRESSED.load(Ordering::Relaxed) == pressed
}

pub(crate) fn note_injected_key(keycode: u32, pressed: bool) {
    LAST_KEY.store(keycode as u16, Ordering::Relaxed);
    LAST_KEY_PRESSED.store(pressed, Ordering::Relaxed);
    LAST_KEY_AT_US.store(now_us(), Ordering::Relaxed);
}

pub(crate) fn should_suppress_key(keycode: u32, pressed: bool) -> bool {
    let at = LAST_KEY_AT_US.load(Ordering::Relaxed);
    if at == 0 || now_us().saturating_sub(at) > SUPPRESS_WINDOW_US {
        return false;
    }

    LAST_KEY.load(Ordering::Relaxed) == keycode as u16
        && LAST_KEY_PRESSED.load(Ordering::Relaxed) == pressed
}

fn now_us() -> u64 {
    START_INSTANT
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros() as u64
}

fn generate_source_marker() -> u64 {
    let pid = std::process::id() as u64;
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = pid.rotate_left(17) ^ now_nanos.rotate_left(31) ^ 0xA5A5_5A5A_1F1F_0E0E;
    if mixed == 0 { 1 } else { mixed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// The marker module uses `0` as its "never injected" sentinel, so the
    /// shared clock has to be initialised and advanced past that value before
    /// the first `note_injected_*` call in the process can be suppressed.
    fn prime_clock() {
        let _ = now_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    #[test]
    #[serial]
    fn injected_button_is_suppressed_then_expires() {
        prime_clock();
        note_injected_button(0, true);

        assert!(
            should_suppress_button(0, true),
            "a just-injected button press must be suppressed to avoid echo loops"
        );
        assert!(
            !should_suppress_button(1, true),
            "a different button must not be suppressed"
        );
        assert!(
            !should_suppress_button(0, false),
            "the opposite button state must not be suppressed"
        );

        std::thread::sleep(std::time::Duration::from_millis(40));
        assert!(
            !should_suppress_button(0, true),
            "suppression must expire so genuine presses are still captured"
        );
    }

    #[test]
    #[serial]
    fn injected_key_and_move_suppression_matches_only_the_latest_event() {
        prime_clock();
        note_injected_key(0x04, true);
        assert!(should_suppress_key(0x04, true));
        assert!(!should_suppress_key(0x05, true));
        assert!(!should_suppress_key(0x04, false));

        note_injected_move(100, 100);
        assert!(should_suppress_move(100, 100));
        assert!(!should_suppress_move(101, 100));
        assert!(!should_suppress_move(100, 101));
    }
}
