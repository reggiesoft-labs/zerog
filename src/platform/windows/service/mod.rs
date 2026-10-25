pub mod client;
pub mod protocol;
#[cfg(target_os = "windows")]
mod server;

#[cfg(target_os = "windows")]
pub use server::{run_service_main, run_service_standalone};

pub(crate) fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
