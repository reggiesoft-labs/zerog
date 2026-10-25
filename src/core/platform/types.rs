//! Platform-agnostic shared types used by engine and subsystem traits.
//!
//! These types provide a common language for describing coordinates,
//! display boundaries, and input states across different operating systems.

use serde::{Deserialize, Serialize};

/// Platform-agnostic coordinate in virtual desktop space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

/// Platform-agnostic display bounds in virtual desktop coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayBounds {
    /// Horizontal offset from virtual origin.
    pub x: i32,
    /// Vertical offset from virtual origin.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Current transfer direction for local input ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferMode {
    /// This machine owns the cursor and processes local edge crossings.
    LocalControl = 0,
    /// Another machine owns the cursor; local input is redirected remotely.
    RemoteControl = 1,
}

impl TransferMode {
    pub const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::RemoteControl,
            _ => Self::LocalControl,
        }
    }

    pub const fn owns_cursor(self) -> bool {
        matches!(self, Self::LocalControl)
    }

    pub const fn is_remote_control(self) -> bool {
        matches!(self, Self::RemoteControl)
    }
}

/// Policy published to native capture callbacks.
///
/// The cursor runtime owns [`TransferMode`]. Capture backends only need to
/// know whether they should let an event continue to local applications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePolicy {
    DeliverLocally = 0,
    SuppressLocalDelivery = 1,
}

impl CapturePolicy {
    pub const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::SuppressLocalDelivery,
            _ => Self::DeliverLocally,
        }
    }

    pub const fn from_transfer_mode(mode: TransferMode) -> Self {
        match mode {
            TransferMode::LocalControl => Self::DeliverLocally,
            TransferMode::RemoteControl => Self::SuppressLocalDelivery,
        }
    }

    pub const fn suppresses_local_delivery(self) -> bool {
        matches!(self, Self::SuppressLocalDelivery)
    }
}

/// Local captured input used by platform hooks and runtime ownership logic.
#[derive(Clone, Debug, PartialEq)]
pub enum CapturedInputEvent {
    /// Native pointer position in virtual-desktop coordinates and hardware
    /// relative movement. A zero delta is valid and must not be synthesized
    /// from an unrelated absolute warp.
    MouseMove {
        x: i32,
        y: i32,
        dx: f64,
        dy: f64,
        locally_delivered: bool,
    },
    MouseButton {
        button: u8,
        pressed: bool,
    },
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
    },
    KeyPress {
        keycode: u32,
    },
    KeyRelease {
        keycode: u32,
    },
}

/// Screen edge used for peer placement and transition logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerEdge {
    /// Left screen edge.
    Left = 0,
    /// Right screen edge.
    Right = 1,
    /// Top screen edge.
    Top = 2,
    /// Bottom screen edge.
    Bottom = 3,
}

/// Screen-relative coordinates (0,0 at top-left of display).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalCoords {
    pub x: i32,
    pub y: i32,
}

/// Edge-relative coordinates (0 at edge, positive away from sender).
///
/// When sender is on the Left edge: x increases rightward from 0
/// When sender is on the Right edge: x decreases leftward (negative values)
/// When sender is on the Top edge: y increases downward from 0
/// When sender is on the Bottom edge: y decreases upward (negative values)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualCoords {
    pub x: i32,
    pub y: i32,
}

impl LocalCoords {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl VirtualCoords {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl PeerEdge {
    /// Returns the canonical lowercase edge label used in config and serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    /// Encodes the edge as a `u8` for atomic storage or compact serialization.
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decodes a `u8` into a `PeerEdge`.
    ///
    /// Defaults to `Left` if the value is not a valid edge index (0-3).
    pub fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::Right,
            2 => Self::Top,
            3 => Self::Bottom,
            _ => Self::Left,
        }
    }
}

/// Operating system family for platform-specific input and modifier handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlatformFamily {
    Windows,
    Macos,
    Linux,
}

impl PlatformFamily {
    pub const LOCAL: Self = {
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::Macos
        }
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Self::Windows
        }
    };

    pub fn from_os_str(os: &str) -> Self {
        let lower = os.to_ascii_lowercase();
        if lower.contains("mac") || lower.contains("darwin") {
            Self::Macos
        } else if lower.contains("linux") {
            Self::Linux
        } else {
            Self::Windows
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }
}

impl Default for PlatformFamily {
    fn default() -> Self {
        Self::LOCAL
    }
}

/// Target key to map a modifier to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifierTarget {
    Ctrl,
    Alt,
    Meta,
}

impl ModifierTarget {
    #[must_use]
    pub const fn to_hid(self, is_right: bool) -> u32 {
        match (self, is_right) {
            (Self::Ctrl, false) => 0xE0,
            (Self::Ctrl, true) => 0xE4,
            (Self::Alt, false) => 0xE2,
            (Self::Alt, true) => 0xE6,
            (Self::Meta, false) => 0xE3,
            (Self::Meta, true) => 0xE7,
        }
    }
}

/// Custom modifier key mappings when transitioning across OS boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModifierMapping {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_ctrl: Option<ModifierTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_alt: Option<ModifierTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_meta: Option<ModifierTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_meta: Option<ModifierTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_alt: Option<ModifierTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_ctrl: Option<ModifierTarget>,
}

impl ModifierMapping {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.left_ctrl.is_none()
            && self.left_alt.is_none()
            && self.left_meta.is_none()
            && self.right_meta.is_none()
            && self.right_alt.is_none()
            && self.right_ctrl.is_none()
    }

    #[must_use]
    pub const fn translate(&self, keycode: u32) -> Option<u32> {
        match keycode {
            0xE0 => match self.left_ctrl {
                Some(target) => Some(target.to_hid(false)),
                None => None,
            },
            0xE2 => match self.left_alt {
                Some(target) => Some(target.to_hid(false)),
                None => None,
            },
            0xE3 => match self.left_meta {
                Some(target) => Some(target.to_hid(false)),
                None => None,
            },
            0xE4 => match self.right_ctrl {
                Some(target) => Some(target.to_hid(true)),
                None => match self.left_ctrl {
                    Some(target) => Some(target.to_hid(true)),
                    None => None,
                },
            },
            0xE6 => match self.right_alt {
                Some(target) => Some(target.to_hid(true)),
                None => match self.left_alt {
                    Some(target) => Some(target.to_hid(true)),
                    None => None,
                },
            },
            0xE7 => match self.right_meta {
                Some(target) => Some(target.to_hid(true)),
                None => match self.left_meta {
                    Some(target) => Some(target.to_hid(true)),
                    None => None,
                },
            },
            _ => None,
        }
    }
}

/// Translates a USB HID usage code when redirecting input across different OS families.
///
/// When custom `mapping` is provided and non-empty, it takes precedence for cross-OS routing.
/// Otherwise, when `swap_alt_cmd` is enabled and transitioning between macOS and PC (Windows / Linux):
/// - Alt/Option (0xE2 / 0xE6) <-> Command/Win (0xE3 / 0xE7).
/// - Control (0xE0 / 0xE4) remains untouched.
///
/// Otherwise, keys are passed through 1:1 without translation.
pub fn translate_hid_for_target_platform(
    keycode: u32,
    source: PlatformFamily,
    target: PlatformFamily,
    swap_alt_cmd: bool,
    mapping: Option<ModifierMapping>,
) -> u32 {
    if let Some(custom) = mapping {
        if !custom.is_empty() {
            return custom.translate(keycode).unwrap_or(keycode);
        }
    }

    let cross_mac_pc = (source == PlatformFamily::Macos && target != PlatformFamily::Macos)
        || (source != PlatformFamily::Macos && target == PlatformFamily::Macos);

    if cross_mac_pc && swap_alt_cmd {
        return match keycode {
            0xE2 => 0xE3,
            0xE3 => 0xE2,
            0xE6 => 0xE7,
            0xE7 => 0xE6,
            _ => keycode,
        };
    }

    keycode
}

/// Lines scrolled per wheel notch on each platform family.
///
/// Windows scrolls `WINDOWS_LINES_PER_NOTCH` lines per `WHEEL_DELTA` (120 units)
/// by default (`SPI_GETWHEELSCROLLLINES`), while macOS and Linux scroll a
/// single line/step per notch. The wire protocol carries raw rotation
/// (120 units/notch), so cross-platform pairs must compensate or the remote
/// side scrolls a different distance than the same wheel turn would locally.
pub const WINDOWS_LINES_PER_NOTCH: i32 = 3;

/// Scales a wire-format wheel delta so one physical wheel turn scrolls the
/// same distance on the target as it would have locally on the source.
///
/// Same-family pairs pass through unchanged. Windows sources scale up 3x
/// toward macOS/Linux targets; non-Windows sources scale down toward Windows
/// targets (snapping small nonzero deltas to ±1 so smooth gestures survive).
#[must_use]
pub const fn scale_wheel_for_target(delta: i32, source: PlatformFamily, target: PlatformFamily) -> i32 {
    if source as u8 == target as u8 {
        return delta;
    }
    if source as u8 == PlatformFamily::Windows as u8 {
        return delta.saturating_mul(WINDOWS_LINES_PER_NOTCH);
    }
    if target as u8 == PlatformFamily::Windows as u8 {
        let scaled = delta / WINDOWS_LINES_PER_NOTCH;
        if delta != 0 && scaled == 0 {
            return if delta > 0 { 1 } else { -1 };
        }
        return scaled;
    }
    delta
}

#[cfg(test)]
mod platform_family_tests {
    use super::*;

    #[test]
    fn parses_platform_strings() {
        assert_eq!(PlatformFamily::from_os_str("macos"), PlatformFamily::Macos);
        assert_eq!(PlatformFamily::from_os_str("darwin"), PlatformFamily::Macos);
        assert_eq!(PlatformFamily::from_os_str("linux"), PlatformFamily::Linux);
        assert_eq!(PlatformFamily::from_os_str("windows"), PlatformFamily::Windows);
        assert_eq!(PlatformFamily::from_os_str("unknown"), PlatformFamily::Windows);
    }

    #[test]
    fn passes_through_modifiers_when_swap_disabled() {
        assert_eq!(
            translate_hid_for_target_platform(0xE3, PlatformFamily::Macos, PlatformFamily::Windows, false, None),
            0xE3
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE0, PlatformFamily::Macos, PlatformFamily::Windows, false, None),
            0xE0
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE7, PlatformFamily::Macos, PlatformFamily::Windows, false, None),
            0xE7
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE4, PlatformFamily::Macos, PlatformFamily::Windows, false, None),
            0xE4
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE2, PlatformFamily::Macos, PlatformFamily::Windows, false, None),
            0xE2
        );
        assert_eq!(
            translate_hid_for_target_platform(0x06, PlatformFamily::Macos, PlatformFamily::Windows, false, None),
            0x06
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE0, PlatformFamily::Windows, PlatformFamily::Macos, false, None),
            0xE0
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE3, PlatformFamily::Windows, PlatformFamily::Macos, false, None),
            0xE3
        );
    }

    #[test]
    fn swaps_alt_and_cmd_when_enabled_across_mac_pc() {
        // macOS -> Windows: Option (0xE2/0xE6) <-> Command (0xE3/0xE7), Ctrl (0xE0/0xE4) untouched
        assert_eq!(
            translate_hid_for_target_platform(0xE2, PlatformFamily::Macos, PlatformFamily::Windows, true, None),
            0xE3
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE3, PlatformFamily::Macos, PlatformFamily::Windows, true, None),
            0xE2
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE6, PlatformFamily::Macos, PlatformFamily::Windows, true, None),
            0xE7
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE7, PlatformFamily::Macos, PlatformFamily::Windows, true, None),
            0xE6
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE0, PlatformFamily::Macos, PlatformFamily::Windows, true, None),
            0xE0
        );
        assert_eq!(
            translate_hid_for_target_platform(0x06, PlatformFamily::Macos, PlatformFamily::Windows, true, None),
            0x06
        );

        // Windows -> macOS: Alt (0xE2/0xE6) <-> Win (0xE3/0xE7), Ctrl (0xE0/0xE4) untouched
        assert_eq!(
            translate_hid_for_target_platform(0xE2, PlatformFamily::Windows, PlatformFamily::Macos, true, None),
            0xE3
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE3, PlatformFamily::Windows, PlatformFamily::Macos, true, None),
            0xE2
        );

        // Same platform: not swapped
        assert_eq!(
            translate_hid_for_target_platform(0xE2, PlatformFamily::Macos, PlatformFamily::Macos, true, None),
            0xE2
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE2, PlatformFamily::Windows, PlatformFamily::Windows, true, None),
            0xE2
        );
    }

    #[test]
    fn custom_modifier_mapping_overrides_swap() {
        let mapping = ModifierMapping {
            left_ctrl: Some(ModifierTarget::Alt),
            left_alt: Some(ModifierTarget::Meta),
            left_meta: Some(ModifierTarget::Ctrl),
            right_meta: Some(ModifierTarget::Ctrl),
            right_alt: Some(ModifierTarget::Meta),
            right_ctrl: Some(ModifierTarget::Alt),
        };

        // Mac -> Windows with custom mapping:
        // Left Cmd (0xE3) maps to Ctrl (0xE0)
        assert_eq!(
            translate_hid_for_target_platform(0xE3, PlatformFamily::Macos, PlatformFamily::Windows, true, Some(mapping)),
            0xE0
        );
        // Left Ctrl (0xE0) maps to Alt (0xE2)
        assert_eq!(
            translate_hid_for_target_platform(0xE0, PlatformFamily::Macos, PlatformFamily::Windows, true, Some(mapping)),
            0xE2
        );
        // Left Alt/Opt (0xE2) maps to Meta (0xE3)
        assert_eq!(
            translate_hid_for_target_platform(0xE2, PlatformFamily::Macos, PlatformFamily::Windows, true, Some(mapping)),
            0xE3
        );
        // Same platform with custom mapping applies custom mapping
        assert_eq!(
            translate_hid_for_target_platform(0xE3, PlatformFamily::Macos, PlatformFamily::Macos, true, Some(mapping)),
            0xE0
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE0, PlatformFamily::Windows, PlatformFamily::Windows, false, Some(mapping)),
            0xE2
        );

        // Right-side modifiers fall back to left mappings when right modifiers are None
        let left_only_mapping = ModifierMapping {
            left_ctrl: Some(ModifierTarget::Alt),
            left_alt: Some(ModifierTarget::Meta),
            left_meta: Some(ModifierTarget::Ctrl),
            right_meta: None,
            right_alt: None,
            right_ctrl: None,
        };
        // Right Ctrl (0xE4) falls back to Left Ctrl mapping (Alt) -> Right Alt (0xE6)
        assert_eq!(
            translate_hid_for_target_platform(0xE4, PlatformFamily::Macos, PlatformFamily::Windows, true, Some(left_only_mapping)),
            0xE6
        );
        // Right Alt (0xE6) falls back to Left Alt mapping (Meta) -> Right Meta (0xE7)
        assert_eq!(
            translate_hid_for_target_platform(0xE6, PlatformFamily::Macos, PlatformFamily::Windows, true, Some(left_only_mapping)),
            0xE7
        );
        // Right Meta (0xE7) falls back to Left Meta mapping (Ctrl) -> Right Ctrl (0xE4)
        assert_eq!(
            translate_hid_for_target_platform(0xE7, PlatformFamily::Macos, PlatformFamily::Windows, true, Some(left_only_mapping)),
            0xE4
        );
    }

    #[test]
    fn scales_wheel_up_from_windows_to_unix() {
        assert_eq!(
            scale_wheel_for_target(120, PlatformFamily::Windows, PlatformFamily::Macos),
            360
        );
        assert_eq!(
            scale_wheel_for_target(-120, PlatformFamily::Windows, PlatformFamily::Linux),
            -360
        );
    }

    #[test]
    fn scales_wheel_down_toward_windows() {
        assert_eq!(
            scale_wheel_for_target(360, PlatformFamily::Macos, PlatformFamily::Windows),
            120
        );
        assert_eq!(scale_wheel_for_target(1, PlatformFamily::Macos, PlatformFamily::Windows), 1);
        assert_eq!(
            scale_wheel_for_target(-1, PlatformFamily::Linux, PlatformFamily::Windows),
            -1
        );
    }

    #[test]
    fn leaves_same_family_wheel_unchanged() {
        assert_eq!(
            scale_wheel_for_target(120, PlatformFamily::Windows, PlatformFamily::Windows),
            120
        );
        assert_eq!(
            scale_wheel_for_target(120, PlatformFamily::Macos, PlatformFamily::Macos),
            120
        );
        assert_eq!(
            scale_wheel_for_target(120, PlatformFamily::Macos, PlatformFamily::Linux),
            120
        );
    }

    #[test]
    fn passes_through_same_platform_keys() {
        assert_eq!(
            translate_hid_for_target_platform(0xE0, PlatformFamily::Windows, PlatformFamily::Windows, false, None),
            0xE0
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE3, PlatformFamily::Macos, PlatformFamily::Macos, false, None),
            0xE3
        );
        assert_eq!(
            translate_hid_for_target_platform(0xE0, PlatformFamily::Windows, PlatformFamily::Linux, false, None),
            0xE0
        );
    }
}

