//! X11 environment checks and doctor diagnostics.

use anyhow::{Result, anyhow, bail};

use crate::platform::PermissionChecker;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;

#[derive(Debug, Clone)]
pub(crate) struct X11SupportProbe {
    pub(crate) display_reachable: bool,
    pub(crate) xtest_present: bool,
    pub(crate) xfixes_present: bool,
    pub(crate) xrandr_present: bool,
    pub(crate) xrecord_present: bool,
    pub(crate) error: Option<String>,
}

impl X11SupportProbe {
    pub(crate) fn can_capture(&self) -> bool {
        self.display_reachable
    }

    pub(crate) fn can_inject(&self) -> bool {
        self.display_reachable && self.xtest_present
    }

    pub(crate) fn can_discover_topology(&self) -> bool {
        self.display_reachable
    }

    pub(crate) fn permission_granted(&self) -> bool {
        self.display_reachable
    }
}

pub(crate) struct X11PermissionChecker;

impl X11PermissionChecker {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl PermissionChecker for X11PermissionChecker {
    fn check_accessibility_permission(&self) -> bool {
        probe_x11_support().permission_granted()
    }

    fn prompt_accessibility_permission(&self) -> Result<()> {
        ensure_accessibility_trusted("start")
    }
}

pub(crate) fn run_doctor_report() -> Result<()> {
    let probe = probe_x11_support();

    println!(
        "doctor: x11.display_access={}{}",
        if probe.display_reachable {
            "pass"
        } else {
            "fail"
        },
        probe
            .error
            .as_ref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default()
    );
    println!(
        "doctor: x11.extension.xtest={}",
        if probe.xtest_present { "pass" } else { "fail" }
    );
    println!(
        "doctor: x11.extension.xrandr={}",
        if probe.xrandr_present { "pass" } else { "fail" }
    );
    println!(
        "doctor: x11.extension.xfixes={}",
        if probe.xfixes_present { "pass" } else { "fail" }
    );
    println!(
        "doctor: x11.extension.xrecord={}",
        if probe.xrecord_present {
            "pass"
        } else {
            "warn"
        }
    );
    if !probe.xrecord_present {
        println!(
            "doctor: x11.capture_mode=polling-fallback (XRecord unavailable, capture remains functional with lower fidelity)"
        );
    }

    if !probe.display_reachable {
        bail!(
            "X11 display is not reachable. Ensure DISPLAY/XAUTHORITY are set and xhost/Xauthority grants allow this process."
        );
    }

    if !probe.xtest_present {
        bail!(
            "XTEST extension is required for event injection. Use an X server with XTEST enabled."
        );
    }

    Ok(())
}

pub(crate) fn ensure_accessibility_trusted(command_name: &str) -> Result<()> {
    let probe = probe_x11_support();
    if probe.permission_granted() {
        return Ok(());
    }

    let detail = probe.error.as_deref().unwrap_or("display access denied");
    bail!(
        "X11 display access is required before running `mouse-thing {command_name}`: {detail}. Ensure DISPLAY and XAUTHORITY are set and allow local client access."
    )
}

pub(crate) fn probe_x11_support() -> X11SupportProbe {
    let (conn, _) = match x11rb::connect(None) {
        Ok(pair) => pair,
        Err(err) => {
            return X11SupportProbe {
                display_reachable: false,
                xtest_present: false,
                xfixes_present: false,
                xrandr_present: false,
                xrecord_present: false,
                error: Some(format!("{err}")),
            };
        }
    };

    X11SupportProbe {
        display_reachable: true,
        xtest_present: extension_present(&conn, b"XTEST"),
        xfixes_present: extension_present(&conn, b"XFIXES"),
        xrandr_present: extension_present(&conn, b"RANDR"),
        xrecord_present: extension_present(&conn, b"RECORD"),
        error: None,
    }
}

fn extension_present<C: Connection>(conn: &C, ext_name: &'static [u8]) -> bool {
    let Ok(cookie) = conn.query_extension(ext_name) else {
        return false;
    };
    let Ok(reply) = cookie.reply() else {
        return false;
    };
    reply.present
}

pub(crate) fn required_extension_remediation() -> &'static str {
    "Verify X11 server extensions XTEST and RANDR are enabled and client auth allows this process."
}

pub(crate) fn display_access_remediation() -> &'static str {
    "Ensure DISPLAY points to your X server and Xauthority permits this process (e.g. xhost +SI:localuser:$USER)."
}

#[allow(dead_code)]
pub(crate) fn probe_or_err() -> Result<X11SupportProbe> {
    let probe = probe_x11_support();
    if !probe.display_reachable {
        return Err(anyhow!(
            "X11 display is not reachable: {}",
            probe.error.as_deref().unwrap_or("unknown error")
        ));
    }
    Ok(probe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    fn wait_for_display() -> bool {
        let has_display = env::var("DISPLAY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .is_some();
        if !has_display {
            return false;
        }
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(20) {
            if x11rb::connect(None).is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    #[test]
    #[serial]
    fn live_probe_reports_available_extensions() {
        if !wait_for_display() {
            eprintln!("SKIP live_probe_reports_available_extensions: no live X display");
            return;
        }
        let probe = probe_x11_support();
        assert!(probe.display_reachable);
        assert!(probe.xtest_present);
        assert!(probe.xrandr_present);
        assert!(probe.error.is_none());
        assert!(probe.can_capture());
        assert!(probe.can_inject());
        assert!(probe.can_discover_topology());
        assert!(probe.permission_granted());
        assert!(probe_or_err().is_ok());
    }

    #[test]
    #[serial]
    fn missing_display_reports_unavailable() {
        let saved = env::var("DISPLAY").ok();
        unsafe { env::remove_var("DISPLAY") };
        let probe = probe_x11_support();
        let err = probe_or_err();
        match saved {
            Some(previous) => unsafe { env::set_var("DISPLAY", previous) },
            None => unsafe { env::remove_var("DISPLAY") },
        }
        assert!(!probe.display_reachable);
        assert!(!probe.can_capture());
        assert!(!probe.can_inject());
        assert!(!probe.can_discover_topology());
        assert!(!probe.permission_granted());
        assert!(probe.error.is_some());
        assert!(err.is_err());
    }
}
