#[cfg(target_os = "linux")]
#[path = "linux.rs"]
pub(crate) mod os;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
pub(crate) mod os;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
pub(crate) mod os;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("mouse-thing only supports Linux, macOS, and Windows targets");
