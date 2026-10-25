use std::sync::OnceLock;
use tokio::runtime::Handle;

/// Returns a handle to the process-scoped Wayland async runtime.
///
/// This runtime hosts all ashpd portal sessions and reis libei connections.
/// It is intentionally leaked (never dropped) because portal sessions must
/// remain alive for the entire process lifetime.
///
/// Pattern copied from quic/endpoint.rs:quic_runtime_handle().
pub(crate) fn wayland_runtime_handle() -> Handle {
    static HANDLE: OnceLock<Handle> = OnceLock::new();

    HANDLE
        .get_or_init(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("wayland-io")
                .build()
                .expect("failed to create Wayland I/O runtime");
            let handle = runtime.handle().clone();

            // Portal sessions and libei connections must stay on a live
            // Tokio runtime. This runtime is process-scoped and intentionally leaked.
            let leaked: &'static mut tokio::runtime::Runtime = Box::leak(Box::new(runtime));
            let _ = leaked;

            handle
        })
        .clone()
}

/// Runs an async future on the Wayland runtime, blocking the calling thread.
///
/// Use this at the sync/async boundary — e.g., inside EventCapture/EventInjection
/// trait methods that are called from synchronous engine worker threads.
pub(crate) fn wayland_block_on<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    let wayland_handle = wayland_runtime_handle();

    match tokio::runtime::Handle::try_current() {
        Err(_) => wayland_handle.block_on(future),
        Ok(current_handle)
            if matches!(
                current_handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) => tokio::task::block_in_place(|| wayland_handle.block_on(future)),
        Ok(_) => std::thread::scope(|scope| {
            scope
                .spawn(move || wayland_handle.block_on(future))
                .join()
                .expect("Wayland runtime thread panicked")
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::wayland_block_on;

    #[test]
    fn wayland_block_on_works_outside_tokio() {
        assert_eq!(wayland_block_on(async { 42 }), 42);
    }

    #[test]
    fn wayland_block_on_works_inside_multi_thread_tokio() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");

        let result = runtime.block_on(async { wayland_block_on(async { 42 }) });

        assert_eq!(result, 42);
    }

    #[test]
    fn wayland_block_on_works_inside_current_thread_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");

        let result = runtime.block_on(async { wayland_block_on(async { 42 }) });

        assert_eq!(result, 42);
    }
}
