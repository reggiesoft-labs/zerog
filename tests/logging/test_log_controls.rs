use mouse_thing::logging::{
    LOG_PRESET_ENV_VAR, LogPreset, RuntimeDiscoveryLoggingConfig, should_enable_ansi,
};

#[test]
fn parses_supported_log_presets() {
    assert_eq!(LogPreset::parse("normal"), Some(LogPreset::Normal));
    assert_eq!(LogPreset::parse("DEBUG"), Some(LogPreset::Debug));
    assert_eq!(LogPreset::parse(" trace "), Some(LogPreset::Trace));
    assert_eq!(LogPreset::parse("invalid"), None);
}

#[test]
fn falls_back_to_normal_preset_for_invalid_values() {
    let config =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("verbose"), None, true, false, false);

    assert_eq!(config.preset, LogPreset::Normal);
    assert_eq!(
        config.filter_directive,
        LogPreset::Normal.filter_directive().to_string()
    );
}

#[test]
fn uses_rust_log_override_when_present() {
    let config = RuntimeDiscoveryLoggingConfig::from_parts(
        Some("trace"),
        Some("warn,mouse_thing::platform::shared=debug"),
        true,
        false,
        false,
    );

    assert_eq!(config.preset, LogPreset::Trace);
    assert_eq!(
        config.filter_directive,
        "warn,mouse_thing::platform::shared=debug"
    );
}

#[test]
fn ansi_is_disabled_for_non_tty_ci_and_test_sinks() {
    assert!(should_enable_ansi(true, false, false));
    assert!(!should_enable_ansi(false, false, false));
    assert!(!should_enable_ansi(true, true, false));
    assert!(!should_enable_ansi(true, false, true));

    let non_tty =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("debug"), None, false, false, false);
    assert!(!non_tty.ansi_enabled);

    let ci = RuntimeDiscoveryLoggingConfig::from_parts(Some("debug"), None, true, true, false);
    assert!(!ci.ansi_enabled);
}

#[test]
fn exposes_log_preset_env_var_name() {
    assert_eq!(LOG_PRESET_ENV_VAR, "MOUSE_THING_LOG_PRESET");
}
