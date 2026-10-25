//! Shared keycode conversion between evdev, HID, and X11 keycode spaces.
//!
//! Keycode spaces:
//! - **evdev**: Linux kernel input subsystem codes (KEY_A = 30)
//! - **HID**: USB HID usage codes (network wire format) (KEY_A = 4)
//! - **X11**: evdev + 8 (KEY_A = 38)
//!
//! The conversion tables are used by both X11 and Wayland (libei) backends:
//! - X11 capture: x11_to_evdev -> evdev_to_hid
//! - X11 injection: hid_to_evdev -> evdev_to_x11
//! - libei capture: evdev_to_hid (libei uses evdev directly)
//! - libei injection: hid_to_evdev (libei uses evdev directly)

/// Convert an evdev keycode to HID usage code.
///
/// Used by: X11 capture (after x11_to_evdev), libei capture (directly).
/// Returns `None` for unmapped keycodes.
pub(crate) fn evdev_to_hid(evdev: u32) -> Option<u32> {
    match evdev {
        1 => Some(0x29),   // Esc
        2 => Some(0x1E),   // 1
        3 => Some(0x1F),   // 2
        4 => Some(0x20),   // 3
        5 => Some(0x21),   // 4
        6 => Some(0x22),   // 5
        7 => Some(0x23),   // 6
        8 => Some(0x24),   // 7
        9 => Some(0x25),   // 8
        10 => Some(0x26),  // 9
        11 => Some(0x27),  // 0
        12 => Some(0x2D),  // -
        13 => Some(0x2E),  // =
        14 => Some(0x2A),  // Backspace
        15 => Some(0x2B),  // Tab
        16 => Some(0x14),  // Q
        17 => Some(0x1A),  // W
        18 => Some(0x08),  // E
        19 => Some(0x15),  // R
        20 => Some(0x17),  // T
        21 => Some(0x1C),  // Y
        22 => Some(0x18),  // U
        23 => Some(0x0C),  // I
        24 => Some(0x12),  // O
        25 => Some(0x13),  // P
        26 => Some(0x2F),  // [
        27 => Some(0x30),  // ]
        28 => Some(0x28),  // Enter
        29 => Some(0xE0),  // Left Ctrl
        30 => Some(0x04),  // A
        31 => Some(0x16),  // S
        32 => Some(0x07),  // D
        33 => Some(0x09),  // F
        34 => Some(0x0A),  // G
        35 => Some(0x0B),  // H
        36 => Some(0x0D),  // J
        37 => Some(0x0E),  // K
        38 => Some(0x0F),  // L
        39 => Some(0x33),  // ;
        40 => Some(0x34),  // '
        41 => Some(0x35),  // `
        42 => Some(0xE1),  // Left Shift
        43 => Some(0x31),  // \
        44 => Some(0x1D),  // Z
        45 => Some(0x1B),  // X
        46 => Some(0x06),  // C
        47 => Some(0x19),  // V
        48 => Some(0x05),  // B
        49 => Some(0x11),  // N
        50 => Some(0x10),  // M
        51 => Some(0x36),  // ,
        52 => Some(0x37),  // .
        53 => Some(0x38),  // /
        54 => Some(0xE5),  // Right Shift
        55 => Some(0x55),  // Keypad *
        56 => Some(0xE2),  // Left Alt
        57 => Some(0x2C),  // Space
        58 => Some(0x39),  // Caps Lock
        59 => Some(0x3A),  // F1
        60 => Some(0x3B),  // F2
        61 => Some(0x3C),  // F3
        62 => Some(0x3D),  // F4
        63 => Some(0x3E),  // F5
        64 => Some(0x3F),  // F6
        65 => Some(0x40),  // F7
        66 => Some(0x41),  // F8
        67 => Some(0x42),  // F9
        68 => Some(0x43),  // F10
        69 => Some(0x53),  // Num Lock
        70 => Some(0x47),  // Scroll Lock
        71 => Some(0x5F),  // Keypad 7
        72 => Some(0x60),  // Keypad 8
        73 => Some(0x61),  // Keypad 9
        74 => Some(0x56),  // Keypad -
        75 => Some(0x5C),  // Keypad 4
        76 => Some(0x5D),  // Keypad 5
        77 => Some(0x5E),  // Keypad 6
        78 => Some(0x57),  // Keypad +
        79 => Some(0x59),  // Keypad 1
        80 => Some(0x5A),  // Keypad 2
        81 => Some(0x5B),  // Keypad 3
        82 => Some(0x62),  // Keypad 0
        83 => Some(0x63),  // Keypad .
        85 => Some(0x94),
        86 => Some(0x64),
        87 => Some(0x44),  // F11
        88 => Some(0x45),  // F12
        89 => Some(0x87),
        92 => Some(0x8A),
        93 => Some(0x88),
        94 => Some(0x8B),
        96 => Some(0x58),  // Keypad Enter
        97 => Some(0xE4),  // Right Ctrl
        98 => Some(0x54),  // Keypad /
        99 => Some(0x46),  // SysRq / Print Screen
        100 => Some(0xE6), // Right Alt
        102 => Some(0x4A), // Home
        103 => Some(0x52), // Up
        104 => Some(0x4B), // Page Up
        105 => Some(0x50), // Left
        106 => Some(0x4F), // Right
        107 => Some(0x4D), // End
        108 => Some(0x51), // Down
        109 => Some(0x4E), // Page Down
        110 => Some(0x49), // Insert
        111 => Some(0x4C), // Delete
        113 => Some(0x7F), // Mute
        114 => Some(0x81), // Volume Down
        115 => Some(0x80), // Volume Up
        117 => Some(0x67), // Keypad =
        119 => Some(0x48), // Pause
        121 => Some(0x85), // Keypad ,
        122 => Some(0x90),
        123 => Some(0x91),
        124 => Some(0x89),
        125 => Some(0xE3), // Left Meta
        126 => Some(0xE7), // Right Meta
        127 => Some(0x65), // Menu
        140 => Some(0x192),
        142 => Some(0x82),
        144 => Some(0x194),
        150 => Some(0x196),
        152 => Some(0x19E),
        155 => Some(0x18A),
        156 => Some(0x182),
        158 => Some(0x224),
        159 => Some(0x225),
        161 => Some(0xB8),
        163 => Some(0xB5), // Next Song
        164 => Some(0xCD), // Play/Pause
        165 => Some(0xB6), // Previous Song
        166 => Some(0xB7), // Stop
        172 => Some(0x223),
        173 => Some(0x227),
        183 => Some(0x68), // F13
        184 => Some(0x69), // F14
        185 => Some(0x6A), // F15
        186 => Some(0x6B), // F16
        187 => Some(0x6C), // F17
        188 => Some(0x6D), // F18
        189 => Some(0x6E), // F19
        190 => Some(0x6F), // F20
        191 => Some(0x70), // F21
        192 => Some(0x71), // F22
        193 => Some(0x72), // F23
        194 => Some(0x73), // F24
        217 => Some(0x221),
        224 => Some(0x000C_0070),
        225 => Some(0x000C_006F),
        226 => Some(0x193),
        228 => Some(0x000C_007C),
        229 => Some(0x000C_007A),
        230 => Some(0x000C_0079),
        _ => None,
    }
}

/// Convert a HID usage code to evdev keycode.
///
/// Used by: X11 injection (before evdev_to_x11), libei injection (directly).
/// Returns `None` for unmapped keycodes.
pub(crate) fn hid_to_evdev(hid: u32) -> Option<u32> {
    match hid {
        0x29 => Some(1),   // Esc
        0x1E => Some(2),   // 1
        0x1F => Some(3),   // 2
        0x20 => Some(4),   // 3
        0x21 => Some(5),   // 4
        0x22 => Some(6),   // 5
        0x23 => Some(7),   // 6
        0x24 => Some(8),   // 7
        0x25 => Some(9),   // 8
        0x26 => Some(10),  // 9
        0x27 => Some(11),  // 0
        0x2D => Some(12),  // -
        0x2E => Some(13),  // =
        0x2A => Some(14),  // Backspace
        0x2B => Some(15),  // Tab
        0x14 => Some(16),  // Q
        0x1A => Some(17),  // W
        0x08 => Some(18),  // E
        0x15 => Some(19),  // R
        0x17 => Some(20),  // T
        0x1C => Some(21),  // Y
        0x18 => Some(22),  // U
        0x0C => Some(23),  // I
        0x12 => Some(24),  // O
        0x13 => Some(25),  // P
        0x2F => Some(26),  // [
        0x30 => Some(27),  // ]
        0x28 => Some(28),  // Enter
        0xE0 => Some(29),  // Left Ctrl
        0x04 => Some(30),  // A
        0x16 => Some(31),  // S
        0x07 => Some(32),  // D
        0x09 => Some(33),  // F
        0x0A => Some(34),  // G
        0x0B => Some(35),  // H
        0x0D => Some(36),  // J
        0x0E => Some(37),  // K
        0x0F => Some(38),  // L
        0x33 => Some(39),  // ;
        0x34 => Some(40),  // '
        0x35 => Some(41),  // `
        0xE1 => Some(42),  // Left Shift
        0x31 => Some(43),  // \
        0x1D => Some(44),  // Z
        0x1B => Some(45),  // X
        0x06 => Some(46),  // C
        0x19 => Some(47),  // V
        0x05 => Some(48),  // B
        0x11 => Some(49),  // N
        0x10 => Some(50),  // M
        0x36 => Some(51),  // ,
        0x37 => Some(52),  // .
        0x38 => Some(53),  // /
        0xE5 => Some(54),  // Right Shift
        0x55 => Some(55),  // Keypad *
        0xE2 => Some(56),  // Left Alt
        0x2C => Some(57),  // Space
        0x39 => Some(58),  // Caps Lock
        0x3A => Some(59),  // F1
        0x3B => Some(60),  // F2
        0x3C => Some(61),  // F3
        0x3D => Some(62),  // F4
        0x3E => Some(63),  // F5
        0x3F => Some(64),  // F6
        0x40 => Some(65),  // F7
        0x41 => Some(66),  // F8
        0x42 => Some(67),  // F9
        0x43 => Some(68),  // F10
        0x53 => Some(69),  // Num Lock
        0x47 => Some(70),  // Scroll Lock
        0x5F => Some(71),  // Keypad 7
        0x60 => Some(72),  // Keypad 8
        0x61 => Some(73),  // Keypad 9
        0x56 => Some(74),  // Keypad -
        0x5C => Some(75),  // Keypad 4
        0x5D => Some(76),  // Keypad 5
        0x5E => Some(77),  // Keypad 6
        0x57 => Some(78),  // Keypad +
        0x59 => Some(79),  // Keypad 1
        0x5A => Some(80),  // Keypad 2
        0x5B => Some(81),  // Keypad 3
        0x62 => Some(82),  // Keypad 0
        0x63 => Some(83),  // Keypad .
        0x64 => Some(86),
        0x44 => Some(87),  // F11
        0x45 => Some(88),  // F12
        0x46 => Some(99),  // Print Screen
        0x48 => Some(119), // Pause
        0x58 => Some(96),  // Keypad Enter
        0xE4 => Some(97),  // Right Ctrl
        0x54 => Some(98),  // Keypad /
        0xE6 => Some(100), // Right Alt
        0x4A => Some(102), // Home
        0x52 => Some(103), // Up
        0x4B => Some(104), // Page Up
        0x50 => Some(105), // Left
        0x4F => Some(106), // Right
        0x4D => Some(107), // End
        0x51 => Some(108), // Down
        0x4E => Some(109), // Page Down
        0x49 => Some(110), // Insert
        0x4C => Some(111), // Delete
        0x7F => Some(113), // Mute
        0x80 => Some(115), // Volume Up
        0x81 => Some(114), // Volume Down
        0x82 => Some(142),
        0x67 => Some(117), // Keypad =
        0x85 => Some(121), // Keypad ,
        0x87 => Some(89),
        0x88 => Some(93),
        0x89 => Some(124),
        0x8A => Some(92),
        0x8B => Some(94),
        0x90 => Some(122),
        0x91 => Some(123),
        0x94 => Some(85),
        0xE3 => Some(125), // Left Meta
        0xE7 => Some(126), // Right Meta
        0x65 => Some(127), // Menu
        0xB5 => Some(163), // Next Song
        0xCD => Some(164), // Play/Pause
        0xB6 => Some(165), // Previous Song
        0xB7 => Some(166), // Stop
        0xB8 => Some(161),
        0x182 => Some(156),
        0x18A => Some(155),
        0x192 => Some(140),
        0x193 => Some(226),
        0x194 => Some(144),
        0x196 => Some(150),
        0x19E => Some(152),
        0x221 => Some(217),
        0x223 => Some(172),
        0x224 => Some(158),
        0x225 => Some(159),
        0x227 => Some(173),
        0x000C_006F => Some(225),
        0x000C_0070 => Some(224),
        0x000C_0079 => Some(230),
        0x000C_007A => Some(229),
        0x000C_007C => Some(228),
        0x68 => Some(183), // F13
        0x69 => Some(184), // F14
        0x6A => Some(185), // F15
        0x6B => Some(186), // F16
        0x6C => Some(187), // F17
        0x6D => Some(188), // F18
        0x6E => Some(189), // F19
        0x6F => Some(190), // F20
        0x70 => Some(191), // F21
        0x71 => Some(192), // F22
        0x72 => Some(193), // F23
        0x73 => Some(194), // F24
        _ => None,
    }
}

/// Convert X11 keycode to evdev keycode (subtract 8).
///
/// X11 keycodes are evdev keycodes offset by 8. This is a fixed mapping
/// defined by the X11 protocol's evdev keyboard driver.
pub(crate) fn x11_to_evdev(x11: u32) -> u32 {
    x11.saturating_sub(8)
}

/// Convert evdev keycode to X11 keycode (add 8).
///
/// X11 keycodes are evdev keycodes offset by 8. This is a fixed mapping
/// defined by the X11 protocol's evdev keyboard driver.
pub(crate) fn evdev_to_x11(evdev: u32) -> u32 {
    evdev + 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_to_hid_roundtrip() {
        // Test common keys roundtrip: evdev -> HID -> evdev
        let test_evdev_codes = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, // Esc, 1-0, -, =, BS, Tab
            16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, // Q-P, [, ], Enter
            29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, // LCtrl, A-L, ;, ', `
            42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, // LShift, \, Z-M, ,, ., /
            54, 55, 56, 57, 58, // RShift, KP*, LAlt, Space, Caps
            59, 60, 61, 62, 63, 64, 65, 66, 67, 68, // F1-F10
            69, 70, // NumLock, ScrollLock
            71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, // KP7-KP., KP-, KP+
            85, 86, 87, 88, 89, 92, 93, 94, // Zenkaku, 102nd, F11, F12, Ro, Henkan, Kana, Muhenkan
            96, 97, 98, 99, // KP Enter, RCtrl, KP/, SysRq
            100, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, // RAlt, Home-Delete
            113, 114, 115, 117, 119, 121, // Mute, VolDown, VolUp, KP=, Pause, KP,
            122, 123, 124, 125, 126, 127, // Hangul, Hanja, Yen, LMeta, RMeta, Menu
            140, 142, 144, 150, 152, 155, 156, 158, 159, // Calc, Sleep, File, WWW, ScreenLock, Mail, Bookmarks, Back, Forward
            161, 163, 164, 165, 166, // Eject, Next, Play/Pause, Prev, Stop
            172, 173, // Homepage, Refresh
            183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, // F13-F24
            217, 224, 225, 226, 228, 229, 230, // Search, BrightnessDown, BrightnessUp, Media, IllumToggle, IllumDown, IllumUp
        ];
        for evdev in test_evdev_codes {
            let hid = evdev_to_hid(evdev)
                .unwrap_or_else(|| panic!("evdev_to_hid({evdev}) returned None"));
            let back =
                hid_to_evdev(hid).unwrap_or_else(|| panic!("hid_to_evdev({hid:#x}) returned None"));
            assert_eq!(
                back, evdev,
                "roundtrip failed for evdev={evdev}: evdev -> hid={hid:#x} -> evdev={back}"
            );
        }
    }

    #[test]
    fn hid_to_evdev_roundtrip() {
        // Test from the HID side: hid -> evdev -> hid
        let test_hid_codes: Vec<u32> = vec![
            0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, // A-J
            0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, // K-T
            0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, // U-4
            0x22, 0x23, 0x24, 0x25, 0x26, 0x27, // 5-0
            0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, // Enter-backslash
            0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, // ;-CapsLock
            0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, // F1-F10
            0x44, 0x45, 0x46, 0x47, 0x48, // F11, F12, PrintScreen, ScrollLock, Pause
            0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, // Insert-Down
            0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, // Up-KPEnter
            0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61, // KP1-KP9
            0x62, 0x63, 0x64, 0x65, 0x67, // KP0, KP., 102nd, Menu, KP=
            0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x71, 0x72, 0x73, // F13-F24
            0x7F, 0x80, 0x81, 0x82, 0x85, 0x87, 0x88, 0x89, 0x8A, 0x8B, // Mute, VolUp, VolDown, Sleep, KP,, Ro, Kana, Yen, Henkan, Muhenkan
            0x90, 0x91, 0x94, // Hangul, Hanja, Zenkaku
            0xB5, 0xB6, 0xB7, 0xB8, 0xCD, // Next, Prev, Stop, Eject, Play/Pause
            0x182, 0x18A, 0x192, 0x193, 0x194, 0x196, 0x19E, // Bookmarks, Mail, Calc, Media, File, WWW, ScreenLock
            0x221, 0x223, 0x224, 0x225, 0x227, // Search, Home, Back, Forward, Refresh
            0x000C_006F, 0x000C_0070, 0x000C_0079, 0x000C_007A, 0x000C_007C, // BrightnessUp, BrightnessDown, IllumUp, IllumDown, IllumToggle
            0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, // Modifiers
        ];
        for hid in test_hid_codes {
            let evdev =
                hid_to_evdev(hid).unwrap_or_else(|| panic!("hid_to_evdev({hid:#x}) returned None"));
            let back = evdev_to_hid(evdev)
                .unwrap_or_else(|| panic!("evdev_to_hid({evdev}) returned None"));
            assert_eq!(
                back, hid,
                "roundtrip failed for hid={hid:#x}: hid -> evdev={evdev} -> hid={back:#x}"
            );
        }
    }

    #[test]
    fn x11_evdev_offset() {
        // X11 'a' = 38 (evdev 30 + 8), evdev 'a' = 30
        assert_eq!(x11_to_evdev(38), 30);
        assert_eq!(evdev_to_x11(30), 38);

        // X11 Escape = 9 (evdev 1 + 8), evdev Escape = 1
        assert_eq!(x11_to_evdev(9), 1);
        assert_eq!(evdev_to_x11(1), 9);

        // X11 Space = 65 (evdev 57 + 8)
        assert_eq!(x11_to_evdev(65), 57);
        assert_eq!(evdev_to_x11(57), 65);
    }

    #[test]
    fn x11_to_evdev_saturates_at_zero() {
        // X11 keycodes below 8 should saturate to 0
        assert_eq!(x11_to_evdev(0), 0);
        assert_eq!(x11_to_evdev(7), 0);
    }

    #[test]
    fn unknown_evdev_returns_none() {
        assert_eq!(evdev_to_hid(0), None);
        assert_eq!(evdev_to_hid(255), None);
        assert_eq!(evdev_to_hid(0xFFFF), None);
    }

    #[test]
    fn unknown_hid_returns_none() {
        assert_eq!(hid_to_evdev(0), None);
        assert_eq!(hid_to_evdev(0x01), None);
        assert_eq!(hid_to_evdev(0xFFFF), None);
    }

    #[test]
    fn specific_key_mappings() {
        // Verify a few well-known mappings explicitly
        assert_eq!(evdev_to_hid(30), Some(0x04)); // A
        assert_eq!(evdev_to_hid(28), Some(0x28)); // Enter
        assert_eq!(evdev_to_hid(57), Some(0x2C)); // Space
        assert_eq!(evdev_to_hid(29), Some(0xE0)); // Left Ctrl
        assert_eq!(evdev_to_hid(42), Some(0xE1)); // Left Shift
        assert_eq!(evdev_to_hid(125), Some(0xE3)); // Left Meta
        assert_eq!(evdev_to_hid(113), Some(0x7F));
        assert_eq!(evdev_to_hid(114), Some(0x81));
        assert_eq!(evdev_to_hid(115), Some(0x80));
        assert_eq!(evdev_to_hid(86), Some(0x64));
        assert_eq!(evdev_to_hid(140), Some(0x192));

        assert_eq!(hid_to_evdev(0x04), Some(30)); // A
        assert_eq!(hid_to_evdev(0x28), Some(28)); // Enter
        assert_eq!(hid_to_evdev(0x2C), Some(57)); // Space
        assert_eq!(hid_to_evdev(0xE0), Some(29)); // Left Ctrl
        assert_eq!(hid_to_evdev(0xE1), Some(42)); // Left Shift
        assert_eq!(hid_to_evdev(0xE3), Some(125)); // Left Meta
        assert_eq!(hid_to_evdev(0x7F), Some(113));
        assert_eq!(hid_to_evdev(0x81), Some(114));
        assert_eq!(hid_to_evdev(0x80), Some(115));
        assert_eq!(hid_to_evdev(0x64), Some(86));
        assert_eq!(hid_to_evdev(0x192), Some(140));
    }
}
