#[cfg(any(target_os = "linux", target_os = "macos"))]
use mouse_thing::core::platform::PeerEdge;

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires macOS accessibility permission"]
fn macos_capture_installation_lifecycle() {
    let (platform, _session) = mouse_thing::platform::create_engine_pair(PeerEdge::Right)
        .expect("macOS factory should build MacosPlatformEngine");
    // Only production interfaces are exercised here; capture lifecycle is
    // verified via TestPlatformEngine in platform-specific integration tests.
    let _ = platform.get_display_manager();
}

#[cfg(target_os = "linux")]
#[test]
fn placeholder_engines_report_not_implemented() {
    let (platform, _session) = mouse_thing::platform::create_engine_pair(PeerEdge::Right)
        .expect("placeholder factory should still return an engine object");
    let err = platform.start().expect_err("placeholder start should fail");
    assert!(
        err.to_string().contains("not implemented"),
        "unexpected placeholder error: {err}"
    );
}
