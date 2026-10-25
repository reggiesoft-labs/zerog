//! Linux platform errors with user-facing remediation text.

use std::fmt;

#[derive(Debug, Clone)]
pub(crate) enum LinuxPlatformError {
    InvalidBackendOverride {
        value: String,
        env_var: &'static str,
    },
    NoDisplayServerDetected,
}

impl fmt::Display for LinuxPlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBackendOverride { value, env_var } => write!(
                f,
                "Invalid Linux backend override `{value}` in {env_var}. Use `x11` or `wayland`."
            ),
            Self::NoDisplayServerDetected => write!(
                f,
                "No Linux display server detected. Start an X11/Wayland session, or set MOUSE_THING_LINUX_BACKEND=x11|wayland for debugging."
            ),
        }
    }
}

impl std::error::Error for LinuxPlatformError {}
