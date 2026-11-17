//! Keeps ZeroG running in the background: closing the window hides it instead of
//! quitting, a menu bar / system tray icon reopens it, and "Launch at Login"
//! starts ZeroG hidden when the user signs in. On macOS, "Hide Dock Icon When
//! Closed" leaves only the menu bar icon while the window is closed.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
#[cfg(not(target_os = "macos"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, Runtime, Wry};

const MAIN_WINDOW: &str = "main";
const MENU_OPEN: &str = "tray-open";
const MENU_LAUNCH_AT_LOGIN: &str = "tray-launch-at-login";
const MENU_HIDE_DOCK: &str = "tray-hide-dock-on-close";
const MENU_QUIT: &str = "tray-quit";

/// Passed by the Windows/Linux login item so an automatic start stays in the background.
const HIDDEN_ARG: &str = "--hidden";

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn zerog_supports_sm_app_service() -> bool;
    fn zerog_launched_as_login_item() -> bool;
}

/// Emitted to the frontend whenever Launch at Login changes outside the settings toggle.
const LAUNCH_AT_LOGIN_EVENT: &str = "launch-at-login-changed";

/// Emitted to the frontend whenever Hide Dock Icon When Closed changes from the tray.
const HIDE_DOCK_EVENT: &str = "hide-dock-on-close-changed";

const PREFS_FILE: &str = "background.json";

struct TrayState {
    launch_at_login: CheckMenuItem<Wry>,
    hide_dock_on_close: Option<CheckMenuItem<Wry>>,
}

/// macOS only: whether closing the window also removes the Dock icon.
struct DockPreference(AtomicBool);

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct BackgroundPrefs {
    #[serde(default)]
    hide_dock_on_close: bool,
}

fn prefs_path<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(PREFS_FILE))
}

fn load_prefs<R: Runtime>(app: &AppHandle<R>) -> BackgroundPrefs {
    prefs_path(app)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_prefs<R: Runtime>(app: &AppHandle<R>, prefs: &BackgroundPrefs) -> Result<(), String> {
    let path = prefs_path(app).ok_or("no app config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(prefs).map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}

fn hide_dock_on_close<R: Runtime>(app: &AppHandle<R>) -> bool {
    cfg!(target_os = "macos")
        && app
            .try_state::<DockPreference>()
            .is_some_and(|pref| pref.0.load(Ordering::Relaxed))
}

/// Show or hide the Dock icon (macOS). The menu bar icon stays either way.
fn set_dock_icon_visible<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        if let Err(error) = app.set_activation_policy(policy) {
            eprintln!("[background] failed to change Dock visibility: {error}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

fn main_window_visible<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window(MAIN_WINDOW)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

/// Whether this launch came from the login item. Must be called during setup,
/// while macOS's launch Apple event is still current.
pub fn started_hidden() -> bool {
    #[cfg(target_os = "macos")]
    // SAFETY: reads the current Apple event on the main thread during launch.
    if unsafe { zerog_launched_as_login_item() } {
        return true;
    }
    std::env::args().any(|arg| arg == HIDDEN_ARG)
}

/// The OS login item for ZeroG. On macOS 13+ it's an SMAppService login item, which
/// appears under System Settings → General → Login Items → Open at Login; older
/// macOS falls back to a LaunchAgent. Windows uses the Run registry key, Linux an
/// XDG autostart entry.
fn login_item() -> Result<auto_launch::AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut builder = auto_launch::AutoLaunchBuilder::new();
    builder
        .set_app_name("ZeroG")
        .set_app_path(&exe.to_string_lossy())
        .set_args(&[HIDDEN_ARG]);
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain NSProcessInfo version query.
        let mode = if unsafe { zerog_supports_sm_app_service() } {
            auto_launch::MacOSLaunchMode::SMAppService
        } else {
            auto_launch::MacOSLaunchMode::LaunchAgent
        };
        builder.set_macos_launch_mode(mode);
    }
    builder.build().map_err(|error| error.to_string())
}

pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    if hide_dock_on_close(app) {
        set_dock_icon_visible(app, true);
    }
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

/// Hide a window instead of closing it; ZeroG keeps running in the background.
pub fn hide_window<R: Runtime>(app: &AppHandle<R>, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
    }
    if hide_dock_on_close(app) {
        set_dock_icon_visible(app, false);
    }
}

pub fn setup_tray(app: &App) -> tauri::Result<()> {
    let prefs = load_prefs(app.handle());
    app.manage(DockPreference(AtomicBool::new(prefs.hide_dock_on_close)));
    if started_hidden() && hide_dock_on_close(app.handle()) {
        set_dock_icon_visible(app.handle(), false);
    }

    let open = MenuItem::with_id(app, MENU_OPEN, "Open ZeroG", true, None::<&str>)?;
    let launch_at_login = CheckMenuItem::with_id(
        app,
        MENU_LAUNCH_AT_LOGIN,
        "Launch at Login",
        true,
        launch_at_login_enabled(app.handle()),
        None::<&str>,
    )?;
    let hide_dock = if cfg!(target_os = "macos") {
        Some(CheckMenuItem::with_id(
            app,
            MENU_HIDE_DOCK,
            "Hide Dock Icon When Closed",
            true,
            prefs.hide_dock_on_close,
            None::<&str>,
        )?)
    } else {
        None
    };
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit ZeroG", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &launch_at_login,
        ],
    )?;
    if let Some(hide_dock) = &hide_dock {
        menu.append(hide_dock)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&quit)?;

    let builder = TrayIconBuilder::with_id("main")
        .tooltip("ZeroG")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN => show_main_window(app),
            MENU_LAUNCH_AT_LOGIN => {
                let enable = !launch_at_login_enabled(app);
                if let Err(error) = set_launch_at_login_inner(app, enable) {
                    eprintln!("[tray] failed to change Launch at Login: {error}");
                }
                sync_launch_at_login(app);
            }
            MENU_HIDE_DOCK => {
                let enable = !hide_dock_on_close(app);
                if let Err(error) = set_hide_dock_on_close_inner(app, enable) {
                    eprintln!("[tray] failed to save Hide Dock Icon When Closed: {error}");
                }
            }
            MENU_QUIT => app.exit(0),
            _ => {}
        });

    // macOS: monochrome template icon that follows the menu bar's light/dark tint,
    // with the menu on left click like other menu bar apps. Elsewhere: the app icon,
    // where a left click opens the window and a right click shows the menu.
    #[cfg(target_os = "macos")]
    let builder = builder
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/tray-template.png"
        ))?)
        .icon_as_template(true)
        .show_menu_on_left_click(true);

    #[cfg(not(target_os = "macos"))]
    let builder = {
        let builder = builder
            .show_menu_on_left_click(false)
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    show_main_window(tray.app_handle());
                }
            });
        match app.default_window_icon() {
            Some(icon) => builder.icon(icon.clone()),
            None => builder,
        }
    };

    builder.build(app)?;
    app.manage(TrayState {
        launch_at_login,
        hide_dock_on_close: hide_dock,
    });
    Ok(())
}

fn launch_at_login_enabled<R: Runtime>(_app: &AppHandle<R>) -> bool {
    login_item()
        .and_then(|item| item.is_enabled().map_err(|error| error.to_string()))
        .unwrap_or(false)
}

fn set_launch_at_login_inner<R: Runtime>(_app: &AppHandle<R>, enabled: bool) -> Result<(), String> {
    let item = login_item()?;
    let result = if enabled {
        item.enable()
    } else {
        item.disable()
    };
    result.map_err(|error| error.to_string())
}

/// Push the actual login-item state to the tray checkmark and the settings toggle.
fn sync_launch_at_login(app: &AppHandle) {
    let enabled = launch_at_login_enabled(app);
    if let Some(tray) = app.try_state::<TrayState>() {
        let _ = tray.launch_at_login.set_checked(enabled);
    }
    let _ = app.emit(LAUNCH_AT_LOGIN_EVENT, enabled);
}

#[tauri::command]
pub fn get_launch_at_login(app: AppHandle) -> bool {
    launch_at_login_enabled(&app)
}

#[tauri::command]
pub fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let result = set_launch_at_login_inner(&app, enabled);
    sync_launch_at_login(&app);
    result.map(|()| launch_at_login_enabled(&app))
}

fn set_hide_dock_on_close_inner(app: &AppHandle, enabled: bool) -> Result<(), String> {
    if let Some(pref) = app.try_state::<DockPreference>() {
        pref.0.store(enabled, Ordering::Relaxed);
    }
    // Apply right away if the window is already closed.
    if !main_window_visible(app) {
        set_dock_icon_visible(app, !enabled);
    }
    if let Some(item) = app
        .try_state::<TrayState>()
        .and_then(|tray| tray.hide_dock_on_close.clone())
    {
        let _ = item.set_checked(enabled);
    }
    let _ = app.emit(HIDE_DOCK_EVENT, enabled);
    save_prefs(
        app,
        &BackgroundPrefs {
            hide_dock_on_close: enabled,
        },
    )
}

/// `None` where there is no Dock (Windows, Linux), so the settings row stays hidden.
#[tauri::command]
pub fn get_hide_dock_on_close(app: AppHandle) -> Option<bool> {
    cfg!(target_os = "macos").then(|| hide_dock_on_close(&app))
}

#[tauri::command]
pub fn set_hide_dock_on_close(app: AppHandle, enabled: bool) -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Err("Hiding the Dock icon is only available on macOS".into());
    }
    set_hide_dock_on_close_inner(&app, enabled)?;
    Ok(hide_dock_on_close(&app))
}
