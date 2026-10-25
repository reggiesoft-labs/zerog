// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            // SAFETY: Executed early during single-threaded startup before GTK or WebKit initialization.
            unsafe {
                std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let install_mode = std::env::args().any(|arg| arg == "--install-service");
        let uninstall_mode = std::env::args().any(|arg| arg == "--uninstall-service");
        let enable_sd = std::env::args().any(|arg| arg == "--enable-secure-desktop");
        let disable_sd = std::env::args().any(|arg| arg == "--disable-secure-desktop");

        if install_mode {
            let result = mouse_thing::platform::service::client::install_service();
            std::process::exit(match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("[install-service] {error:#}");
                    1
                }
            });
        }
        if uninstall_mode {
            let result = mouse_thing::platform::service::client::uninstall_service();
            std::process::exit(match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("[uninstall-service] {error:#}");
                    1
                }
            });
        }
        if enable_sd || disable_sd {
            let result = mouse_thing::platform::service::client::set_secure_desktop_enabled(enable_sd);
            std::process::exit(match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("[secure-desktop] {error:#}");
                    1
                }
            });
        }

        let service_mode = std::env::args().any(|arg| arg == "--input-service");
        let standalone_mode = std::env::args().any(|arg| arg == "--input-service-standalone");
        if service_mode || standalone_mode {
            let result = if service_mode {
                mouse_thing::platform::service::run_service_main()
            } else {
                mouse_thing::platform::service::run_service_standalone()
            };
            std::process::exit(match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("[input-service] {error:#}");
                    1
                }
            });
        }
    }
    gui_lib::run()
}
