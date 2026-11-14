#[cfg(any(target_os = "linux", target_os = "macos"))]
use zerog::core::platform::PeerEdge;

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires macOS accessibility permission"]
fn macos_capture_installation_lifecycle() {
    let (platform, _session) = zerog::platform::create_engine_pair(PeerEdge::Right)
        .expect("macOS factory should build MacosPlatformEngine");
    // Only production interfaces are exercised here; capture lifecycle is
    // verified via TestPlatformEngine in platform-specific integration tests.
    let _ = platform.get_display_manager();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Linux desktop session (X11 or Wayland)"]
fn linux_factory_builds_desktop_engine() {
    let (platform, _session) = zerog::platform::create_engine_pair(PeerEdge::Right)
        .expect("Linux factory should build an X11 or Wayland engine");
    // Starting would install real input capture on the desktop session, so
    // only production interfaces are exercised here; capture lifecycle is
    // verified via TestPlatformEngine in the runtime integration tests.
    let _ = platform.get_display_manager();
}
