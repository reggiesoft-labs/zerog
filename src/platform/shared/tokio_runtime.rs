use std::sync::OnceLock;

/// Returns a reference to the global Tokio runtime handle.
/// This runtime is used for operations that require a Tokio context but
/// are not running within one.
pub fn global_runtime_handle() -> tokio::runtime::Handle {
    static HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

    HANDLE
        .get_or_init(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("tokio-runtime")
                .on_thread_start(|| {
                    crate::platform::shared::priority::set_realtime_thread_priority();
                })
                .build()
                .expect("failed to create global Tokio runtime");
            let handle = runtime.handle().clone();

            // Runtimes must stay on a live Tokio runtime.
            // This runtime is process-scoped and intentionally leaked.
            let leaked: &'static mut tokio::runtime::Runtime = Box::leak(Box::new(runtime));
            let _ = leaked;

            handle
        })
        .clone()
}
