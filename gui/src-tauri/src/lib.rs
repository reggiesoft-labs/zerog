//! Tauri command layer for the mouse-thing GUI.
//!
//! This module provides the bridge between the React frontend and the
//! Rust core library. It implements Tauri commands for:
//! - Monitoring engine status and throughput
//! - Discovering network peers via mDNS
//! - Managing application configuration and screen layout
//! - Initiating peer connections and disconnections
//!
//! # Threading and State
//!
//! The [`AppState`] is shared across all Tauri commands using `RwLock` and
//! atomics to ensure safe concurrent access from both command threads
//! and background discovery loops.
#![allow(unexpected_cfgs)]

mod telemetry;

use mouse_thing::core::api::{ConfigSnapshot, PeerSnapshot, StatusSnapshot};
use mouse_thing::core::controller::events::{PeerUpdate, RecvError};
use mouse_thing::core::controller::Controller;
use mouse_thing::core::error::CoreError;
use mouse_thing::core::transfer::TransferEvent;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::time::Instant;
use tauri::{Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

const DEFAULT_LISTEN_PORT: u16 = mouse_thing::core::policies::DEFAULT_PORT;
const CONFIG_SYNC_RECEIVED_EVENT: &str = "config-sync-received";

/// Shared state for the Tauri application.
struct AppState {
    /// Controller for the core engine logic.
    controller: Arc<RwLock<Controller>>,
    /// Local UDP port used for peer communication.
    listen_port: u16,
}

/// Error type returned by Tauri commands to the frontend.
#[derive(Debug, Serialize)]
pub struct CommandError {
    /// Machine-readable error code (e.g., `accessibility_permission_missing`).
    code: &'static str,
    /// User-friendly error message.
    message: String,
    /// Additional technical details for debugging.
    details: Option<String>,
}

impl CommandError {
    fn from_core(error: CoreError) -> Self {
        let details = error.to_string();
        match error {
            CoreError::PermissionDenied(_) => Self {
                code: "accessibility_permission_missing",
                message: "ZeroG needs Accessibility access. The helper is open; drag the ZeroG row into Privacy & Security > Accessibility.".to_string(),
                details: Some(details),
            },
            CoreError::Network(_) => Self {
                code: "socket_bind_failed",
                message: "Unable to bind required network socket. Another process may already be using this port.".to_string(),
                details: Some(details),
            },
            CoreError::Discovery(_) => Self {
                code: "discovery_error",
                message: "Peer discovery failed. Ensure mDNS is available on your network.".to_string(),
                details: Some(details),
            },
            CoreError::Config(_) => Self {
                code: "config_error",
                message: "Configuration operation failed.".to_string(),
                details: Some(details),
            },
            CoreError::InvalidInput(_) => Self {
                code: "invalid_config",
                message: "Config validation failed. Check edge mappings and retry.".to_string(),
                details: Some(details),
            },
            CoreError::Platform(_) => Self {
                code: "platform_error",
                message: "Platform initialization failed.".to_string(),
                details: Some(details),
            },
            CoreError::Connection(_) => Self {
                code: "connection_error",
                message: "Connection operation failed.".to_string(),
                details: Some(details),
            },
            CoreError::Engine(_) => Self {
                code: "internal_error",
                message: "Request failed".to_string(),
                details: Some(details),
            },
        }
    }
}

impl From<CoreError> for CommandError {
    fn from(value: CoreError) -> Self {
        Self::from_core(value)
    }
}

fn internal_error(message: &'static str) -> CommandError {
    CommandError {
        code: "internal_error",
        message: message.to_string(),
        details: None,
    }
}

fn custom_command_error(message: String) -> CommandError {
    CommandError {
        code: "transfer_error",
        message,
        details: None,
    }
}

fn state_poisoned_error() -> CommandError {
    CommandError {
        code: "state_poisoned",
        message: "Application state lock failed (recovery attempt)".to_string(),
        details: None,
    }
}

fn with_read_controller<T>(
    state: &tauri::State<'_, AppState>,
    f: impl FnOnce(&Controller) -> Result<T, CoreError>,
) -> Result<T, CommandError> {
    let guard = state
        .controller
        .read()
        .map_err(|_| state_poisoned_error())?;
    f(&guard).map_err(CommandError::from)
}

fn with_write_controller<T>(
    state: &tauri::State<'_, AppState>,
    f: impl FnOnce(&mut Controller) -> Result<T, CoreError>,
) -> Result<T, CommandError> {
    let mut guard = state
        .controller
        .write()
        .map_err(|_| state_poisoned_error())?;
    f(&mut guard).map_err(CommandError::from)
}

fn with_write_controller_status(
    state: &tauri::State<'_, AppState>,
    f: impl FnOnce(&mut Controller) -> Result<(), CoreError>,
) -> Result<StatusSnapshot, CommandError> {
    with_write_controller(state, |controller| {
        f(controller)?;
        Ok(controller.get_status())
    })
}

#[derive(Clone, Serialize)]
struct ConfigSyncReceivedPayload {
    config: ConfigSnapshot,
}

#[derive(Clone, Serialize)]
struct PeersUpdatedPayload {
    peers: Vec<PeerSnapshot>,
}

#[derive(Clone, Serialize)]
struct StatusUpdatedPayload {
    status: StatusSnapshot,
}

#[derive(Clone, Serialize)]
struct PairingRequestedPayload {
    peer_id: String,
    name: String,
    platform: Option<String>,
    code: Option<String>,
    can_respond: bool,
}

#[derive(Clone, Serialize)]
struct PairingResolvedPayload {
    peer_id: String,
    approved: bool,
}

fn emit_peers_updated(app_handle: &tauri::AppHandle, fallback: Vec<PeerSnapshot>) {
    let state = app_handle.state::<AppState>();
    let peers = read_cached_peers(&state).unwrap_or(fallback);
    let payload = PeersUpdatedPayload { peers };
    if let Err(error) = app_handle.emit("peers-updated", payload.clone()) {
        let _ = writeln!(
            std::io::stderr(),
            "[gui-setup] failed to emit peers-updated: {}",
            error
        );
    }
    let _ = app_handle.emit_to("main", "peers-updated", payload);
}

fn emit_status_updated(app_handle: &tauri::AppHandle, status: StatusSnapshot) {
    let payload = StatusUpdatedPayload { status };
    if let Err(error) = app_handle.emit("status-updated", payload.clone()) {
        let _ = writeln!(
            std::io::stderr(),
            "[gui-setup] failed to emit status-updated: {}",
            error
        );
    }
    let _ = app_handle.emit_to("main", "status-updated", payload);
}

fn emit_config_sync_received(app_handle: &tauri::AppHandle, config: ConfigSnapshot) {
    let payload = ConfigSyncReceivedPayload { config };
    if let Err(error) = app_handle.emit(CONFIG_SYNC_RECEIVED_EVENT, payload.clone()) {
        let _ = writeln!(
            std::io::stderr(),
            "[gui-setup] failed to emit {}: {}",
            CONFIG_SYNC_RECEIVED_EVENT,
            error
        );
    }
    let _ = app_handle.emit_to("main", CONFIG_SYNC_RECEIVED_EVENT, payload);
}

fn emit_current_status(app_handle: &tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let status = match state.controller.read() {
        Ok(controller) => controller.get_status(),
        Err(_) => {
            let _ = writeln!(
                std::io::stderr(),
                "[gui-setup] failed to read controller status"
            );
            return;
        }
    };
    emit_status_updated(app_handle, status);
}

fn finalize_get_config_response(
    config: ConfigSnapshot,
    remote_updated: bool,
    emit_sync_event: impl FnOnce(&str, ConfigSyncReceivedPayload) -> Result<(), String>,
    log_emit_failure: impl FnOnce(&str, &str),
) -> ConfigSnapshot {
    if remote_updated {
        let payload = ConfigSyncReceivedPayload {
            config: config.clone(),
        };
        if let Err(error) = emit_sync_event(CONFIG_SYNC_RECEIVED_EVENT, payload) {
            log_emit_failure(CONFIG_SYNC_RECEIVED_EVENT, &error);
        }
    }

    config
}

pub(crate) fn discover_peers_with_wait(
    state: &tauri::State<'_, AppState>,
    wait: Duration,
) -> Result<Vec<PeerSnapshot>, CommandError> {
    with_write_controller(state, |controller| controller.discover_peers(wait))
}

pub(crate) fn read_cached_peers(
    state: &tauri::State<'_, AppState>,
) -> Result<Vec<PeerSnapshot>, CommandError> {
    with_read_controller(state, |controller| Ok(controller.get_peers_cached()))
}

async fn run_blocking_task<T, F>(
    app_handle: tauri::AppHandle,
    task_failed_message: &'static str,
    task: F,
) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(tauri::AppHandle) -> T + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || task(app_handle))
        .await
        .map_err(|_| internal_error(task_failed_message))
}

async fn run_logged_command<T, F>(
    app_handle: tauri::AppHandle,
    command: &'static str,
    task_failed_message: &'static str,
    log_timing: bool,
    task: F,
) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(tauri::AppHandle) -> Result<T, CommandError> + Send + 'static,
{
    let started_at = Instant::now();
    let result = run_blocking_task(app_handle, task_failed_message, move |app_handle| {
        telemetry::log_command_result(command, task(app_handle))
    })
    .await?;

    if log_timing {
        telemetry::log_command_timing(command, started_at);
    }

    result
}

async fn run_engine_toggle_command<T, F>(
    app_handle: tauri::AppHandle,
    command: &'static str,
    task_failed_message: &'static str,
    task: F,
) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(tauri::AppHandle) -> Result<T, CommandError> + Send + 'static,
{
    telemetry::log_engine_toggle_received(command);
    let result = run_blocking_task(app_handle, task_failed_message, task).await?;
    telemetry::log_engine_toggle_result(command, &result);
    mouse_thing::release_gui_focus();
    telemetry::log_command_result(command, result)
}

#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("Only HTTPS URLs can be opened".to_string());
    }

    #[cfg(target_os = "linux")]
    if std::env::var_os("APPIMAGE").is_some()
        && std::path::Path::new("/usr/bin/xdg-open").is_file()
    {
        return tauri_plugin_opener::open_url(&url, Some("/usr/bin/xdg-open"))
            .map_err(|error| error.to_string());
    }

    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_status(app_handle: tauri::AppHandle) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle,
        "get_status",
        "Status task failed",
        true,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            with_read_controller(&state, |controller| Ok(controller.get_status()))
        },
    )
    .await
}

#[tauri::command]
async fn get_peers(app_handle: tauri::AppHandle) -> Result<Vec<PeerSnapshot>, CommandError> {
    run_logged_command(
        app_handle,
        "get_peers",
        "Discovery task failed",
        true,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            discover_peers_with_wait(&state, Duration::from_millis(750))
        },
    )
    .await
}

#[tauri::command]
async fn get_peers_cached(app_handle: tauri::AppHandle) -> Result<Vec<PeerSnapshot>, CommandError> {
    run_blocking_task(app_handle, "Peer cache task failed", move |app_handle| {
        let state = app_handle.state::<AppState>();
        read_cached_peers(&state)
    })
    .await?
}

#[tauri::command]
async fn get_config(app_handle: tauri::AppHandle) -> Result<ConfigSnapshot, CommandError> {
    run_logged_command(
        app_handle,
        "get_config",
        "Config read task failed",
        true,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            with_read_controller(&state, |controller| controller.get_config()).map(
                |(config, remote_updated)| {
                    finalize_get_config_response(
                        config,
                        remote_updated,
                        |event, payload| {
                            app_handle
                                .emit(event, payload)
                                .map_err(|error| error.to_string())
                        },
                        |event, error| {
                            let _ = writeln!(
                                std::io::stderr(),
                                "[gui-command] get_config emit_failed event={} error={}",
                                event,
                                error
                            );
                        },
                    )
                },
            )
        },
    )
    .await
}

#[tauri::command]
async fn save_config(
    app_handle: tauri::AppHandle,
    config: ConfigSnapshot,
) -> Result<ConfigSnapshot, CommandError> {
    run_logged_command(
        app_handle,
        "save_config",
        "Config save task failed",
        true,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            with_write_controller(&state, |controller| {
                controller.save_config(config.clone())?;
                controller
                    .get_config()
                    .map(|(config, _remote_updated)| config)
            })
        },
    )
    .await
}

#[tauri::command]
async fn start_engine(app_handle: tauri::AppHandle) -> Result<StatusSnapshot, CommandError> {
    run_engine_toggle_command(
        app_handle.clone(),
        "start_engine",
        "Engine start task failed",
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            let status = with_write_controller_status(&state, |controller| controller.start(state.listen_port))?;
            if let Ok(peers) = read_cached_peers(&state) {
                emit_peers_updated(&app_handle, peers);
            }
            Ok(status)
        },
    )
    .await
}

#[tauri::command]
async fn stop_engine(app_handle: tauri::AppHandle) -> Result<StatusSnapshot, CommandError> {
    run_engine_toggle_command(
        app_handle.clone(),
        "stop_engine",
        "Engine stop task failed",
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            let status = with_write_controller_status(&state, |controller| {
                controller.stop();
                Ok(())
            })?;
            emit_peers_updated(&app_handle, Vec::new());
            Ok(status)
        },
    )
    .await
}

#[tauri::command]
async fn restart_engine(app_handle: tauri::AppHandle) -> Result<StatusSnapshot, CommandError> {
    run_engine_toggle_command(
        app_handle.clone(),
        "restart_engine",
        "Engine restart task failed",
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            let status = with_write_controller_status(&state, |controller| controller.restart(state.listen_port))?;
            if let Ok(peers) = read_cached_peers(&state) {
                emit_peers_updated(&app_handle, peers);
            }
            Ok(status)
        },
    )
    .await
}

#[tauri::command]
async fn accept_connection(
    app_handle: tauri::AppHandle,
    peer_id: String,
) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle.clone(),
        "accept_connection",
        "Accept connection task failed",
        false,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            let status = with_write_controller_status(&state, |controller| controller.connect(&peer_id))?;
            emit_peers_updated(&app_handle, Vec::new());
            Ok(status)
        },
    )
    .await
}

#[tauri::command]
async fn reject_connection(
    app_handle: tauri::AppHandle,
    peer_id: String,
) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle.clone(),
        "reject_connection",
        "Reject connection task failed",
        false,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            let status = with_write_controller_status(&state, |controller| controller.disconnect(&peer_id))?;
            emit_peers_updated(&app_handle, Vec::new());
            Ok(status)
        },
    )
    .await
}

#[tauri::command]
async fn cancel_connection(
    app_handle: tauri::AppHandle,
    peer_id: String,
) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle.clone(),
        "cancel_connection",
        "Cancel connection task failed",
        false,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            let status = with_write_controller_status(&state, |controller| {
                controller.cancel_connect(&peer_id)
            })?;
            emit_peers_updated(&app_handle, Vec::new());
            Ok(status)
        },
    )
    .await
}

#[tauri::command]
async fn unpair_peer(
    app_handle: tauri::AppHandle,
    peer_id: String,
) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle.clone(),
        "unpair_peer",
        "Unpair peer task failed",
        false,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            let status = with_write_controller_status(&state, |controller| controller.unpair_peer(&peer_id))?;
            emit_peers_updated(&app_handle, Vec::new());
            Ok(status)
        },
    )
    .await
}

fn respond_pairing_inner(
    app_handle: &tauri::AppHandle,
    peer_id: &str,
    approved: bool,
) -> Result<StatusSnapshot, CommandError> {
    let state = app_handle.state::<AppState>();
    let status = with_write_controller_status(&state, |controller| {
        controller.respond_pairing(peer_id, approved)
    })?;
    emit_peers_updated(app_handle, Vec::new());
    Ok(status)
}

#[tauri::command]
async fn approve_pairing(
    app_handle: tauri::AppHandle,
    peer_id: String,
) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle.clone(),
        "approve_pairing",
        "Approve pairing task failed",
        false,
        move |app_handle| respond_pairing_inner(&app_handle, &peer_id, true),
    )
    .await
}

#[tauri::command]
async fn reject_pairing(
    app_handle: tauri::AppHandle,
    peer_id: String,
) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle.clone(),
        "reject_pairing",
        "Reject pairing task failed",
        false,
        move |app_handle| respond_pairing_inner(&app_handle, &peer_id, false),
    )
    .await
}

#[tauri::command]
async fn connect_direct_address(
    app_handle: tauri::AppHandle,
    address: String,
) -> Result<StatusSnapshot, CommandError> {
    let addr: std::net::SocketAddr = address
        .parse()
        .map_err(|e| CommandError {
            code: "invalid_address",
            message: format!("Invalid socket address {}: {}", address, e),
            details: None,
        })?;

    run_logged_command(
        app_handle,
        "connect_direct_address",
        "Direct connect task failed",
        false,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            with_write_controller_status(&state, |controller| controller.connect_address(addr))
        },
    )
    .await
}

#[tauri::command]
async fn emergency_unlock(app_handle: tauri::AppHandle) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle,
        "emergency_unlock",
        "Emergency unlock task failed",
        false,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            with_write_controller_status(&state, |controller| controller.emergency_unlock())
        },
    )
    .await
}

#[tauri::command]
async fn force_disconnect(app_handle: tauri::AppHandle) -> Result<StatusSnapshot, CommandError> {
    run_logged_command(
        app_handle,
        "force_disconnect",
        "Force disconnect task failed",
        false,
        move |app_handle| {
            let state = app_handle.state::<AppState>();
            with_write_controller_status(&state, |controller| controller.force_disconnect_all())
        },
    )
    .await
}

#[derive(Debug, Clone, Serialize)]
struct WindowsInputStatusDto {
    supported: bool,
    elevated: bool,
    service_installed: bool,
    service_running: bool,
    secure_desktop_enabled: Option<bool>,
    service_reachable: bool,
}

fn query_windows_input_status() -> WindowsInputStatusDto {
    #[cfg(target_os = "windows")]
    {
        let status = mouse_thing::platform::service::client::query_status();
        return WindowsInputStatusDto {
            supported: status.supported,
            elevated: status.elevated,
            service_installed: status.service_installed,
            service_running: status.service_running,
            secure_desktop_enabled: status.secure_desktop_enabled,
            service_reachable: status.service_reachable,
        };
    }
    #[cfg(not(target_os = "windows"))]
    {
        WindowsInputStatusDto {
            supported: false,
            elevated: false,
            service_installed: false,
            service_running: false,
            secure_desktop_enabled: None,
            service_reachable: false,
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_action_error(code: &'static str, message: &str, error: anyhow::Error) -> CommandError {
    CommandError {
        code,
        message: message.to_string(),
        details: Some(error.to_string()),
    }
}

#[tauri::command]
async fn get_windows_input_status() -> Result<WindowsInputStatusDto, CommandError> {
    Ok(query_windows_input_status())
}

#[tauri::command]
async fn restart_as_admin(app_handle: tauri::AppHandle) -> Result<(), CommandError> {
    run_blocking_task(app_handle, "Administrator relaunch task failed", |_| {
        #[cfg(target_os = "windows")]
        {
            let result = mouse_thing::platform::service::client::restart_elevated();
            return result.map_err(|error| {
                windows_action_error(
                    "elevate_failed",
                    "Could not relaunch as administrator.",
                    error,
                )
            });
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(CommandError {
                code: "unsupported_platform",
                message: "Administrator relaunch is only available on Windows.".to_string(),
                details: None,
            })
        }
    })
    .await?
}

#[tauri::command]
async fn install_input_service(
    app_handle: tauri::AppHandle,
) -> Result<WindowsInputStatusDto, CommandError> {
    run_blocking_task(app_handle, "Input service install task failed", |_| {
        #[cfg(target_os = "windows")]
        {
            return mouse_thing::platform::service::client::install_service()
                .map(|_| query_windows_input_status())
                .map_err(|error| {
                    windows_action_error(
                        "service_install_failed",
                        &format!("Could not install the input service: {error}"),
                        error,
                    )
                });
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(CommandError {
                code: "unsupported_platform",
                message: "The input service is only available on Windows.".to_string(),
                details: None,
            })
        }
    })
    .await?
}

#[tauri::command]
async fn uninstall_input_service(
    app_handle: tauri::AppHandle,
) -> Result<WindowsInputStatusDto, CommandError> {
    run_blocking_task(app_handle, "Input service removal task failed", |_| {
        #[cfg(target_os = "windows")]
        {
            return mouse_thing::platform::service::client::uninstall_service()
                .map(|_| query_windows_input_status())
                .map_err(|error| {
                    windows_action_error(
                        "service_uninstall_failed",
                        &format!("Could not remove the input service: {error}"),
                        error,
                    )
                });
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(CommandError {
                code: "unsupported_platform",
                message: "The input service is only available on Windows.".to_string(),
                details: None,
            })
        }
    })
    .await?
}

#[tauri::command]
async fn set_secure_desktop_enabled(
    app_handle: tauri::AppHandle,
    enabled: bool,
) -> Result<WindowsInputStatusDto, CommandError> {
    run_blocking_task(
        app_handle,
        "Secure desktop setting task failed",
        move |_| {
            #[cfg(target_os = "windows")]
            {
                return mouse_thing::platform::service::client::set_secure_desktop_enabled(enabled)
                    .map(|_| query_windows_input_status())
                    .map_err(|error| {
                        windows_action_error(
                            "secure_desktop_failed",
                            &format!("Could not change the secure desktop setting: {error}"),
                            error,
                        )
                    });
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = enabled;
                Err(CommandError {
                    code: "unsupported_platform",
                    message: "The secure desktop setting is only available on Windows.".to_string(),
                    details: None,
                })
            }
        },
    )
    .await?
}

#[tauri::command]
fn report_gui_error(message: String) {
    let _ = writeln!(std::io::stderr(), "[gui-error-banner] {message}");
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn zerog_show_accessibility_helper(app_bundle_path: *const std::ffi::c_char);
}

#[tauri::command]
fn show_accessibility_helper() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let app_bundle_path = get_accessibility_app_bundle_path()
            .map(std::ffi::CString::new)
            .transpose()
            .map_err(|error| error.to_string())?;
        let app_bundle_path = app_bundle_path
            .as_ref()
            .map_or(std::ptr::null(), |path| path.as_ptr());

        unsafe {
            zerog_show_accessibility_helper(app_bundle_path);
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn get_accessibility_app_bundle_path() -> Option<String> {
    std::env::current_exe()
        .ok()?
        .ancestors()
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("app"))
        .map(|path| path.to_string_lossy().into_owned())
}

fn create_app_state() -> AppState {
    AppState {
        controller: Arc::new(RwLock::new(Controller::with_engine_pair(|edge| {
            mouse_thing::platform::create_engine_pair(edge)
                .map_err(|e| mouse_thing::core::error::CoreError::Platform(e.to_string()))
        }))),
        listen_port: DEFAULT_LISTEN_PORT,
    }
}

fn stop_controller(controller: &Arc<RwLock<Controller>>) {
    mouse_thing::platform::begin_shutdown();
    let mut controller = controller
        .write()
        .unwrap_or_else(|error| error.into_inner());
    controller.stop();
}

fn install_shutdown_handler(controller: Arc<RwLock<Controller>>) {
    if let Err(error) = ctrlc::set_handler(move || {
        stop_controller(&controller);
        std::process::exit(0);
    }) {
        let _ = writeln!(
            std::io::stderr(),
            "[gui-setup] failed to install process shutdown handler: {error}"
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LicenseStatusDto {
    Free,
    Active {
        licensee: String,
        tier: String,
        expires_at: Option<u64>,
    },
    Expired {
        licensee: String,
        expires_at: u64,
    },
    Invalid {
        reason: String,
    },
}

impl From<mouse_thing::core::license::LicenseStatus> for LicenseStatusDto {
    fn from(status: mouse_thing::core::license::LicenseStatus) -> Self {
        match status {
            mouse_thing::core::license::LicenseStatus::Free => LicenseStatusDto::Free,
            mouse_thing::core::license::LicenseStatus::Active { licensee, tier, expires_at } => {
                LicenseStatusDto::Active {
                    licensee,
                    tier: match tier {
                        mouse_thing::core::license::LicenseTier::Free => "free".to_string(),
                        mouse_thing::core::license::LicenseTier::Pro => "pro".to_string(),
                    },
                    expires_at,
                }
            }
            mouse_thing::core::license::LicenseStatus::Expired { licensee, expires_at } => {
                LicenseStatusDto::Expired { licensee, expires_at }
            }
            mouse_thing::core::license::LicenseStatus::Invalid { reason } => {
                LicenseStatusDto::Invalid { reason }
            }
        }
    }
}

#[tauri::command]
async fn get_license_status() -> Result<LicenseStatusDto, CommandError> {
    Ok(mouse_thing::core::config::load_license_status().into())
}

#[tauri::command]
async fn set_license_key(
    license_key: String,
) -> Result<LicenseStatusDto, CommandError> {
    let trimmed = license_key.trim().to_string();
    let status = mouse_thing::core::license::verify_license_key(
        &trimmed,
        &mouse_thing::core::license::DEFAULT_LICENSE_PUBKEY,
    );
    match status {
        mouse_thing::core::license::LicenseStatus::Active { .. } => {
            let saved = mouse_thing::core::config::save_license_key(Some(trimmed))
                .map_err(|e| custom_command_error(format!("Failed to save license key: {e}")))?;
            Ok(saved.into())
        }
        mouse_thing::core::license::LicenseStatus::Expired { expires_at, .. } => {
            Err(CommandError {
                code: "license_expired",
                message: format!("This license expired at timestamp {expires_at}."),
                details: None,
            })
        }
        mouse_thing::core::license::LicenseStatus::Invalid { reason } => {
            Err(CommandError {
                code: "invalid_license",
                message: format!("Invalid license key: {reason}"),
                details: None,
            })
        }
        mouse_thing::core::license::LicenseStatus::Free => {
            Err(CommandError {
                code: "invalid_license",
                message: "Empty license key.".to_string(),
                details: None,
            })
        }
    }
}

#[tauri::command]
async fn remove_license_key() -> Result<LicenseStatusDto, CommandError> {
    let saved = mouse_thing::core::config::save_license_key(None)
        .map_err(|e| custom_command_error(format!("Failed to remove license key: {e}")))?;
    Ok(saved.into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResponse {
    pub available: bool,
    pub current_version: String,
    pub version: Option<String>,
    pub date: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateProgressPayload {
    pub chunk_length: usize,
    pub content_length: Option<u64>,
}

#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateCheckResponse, CommandError> {
    let updater = app
        .updater()
        .map_err(|e| custom_command_error(format!("Updater initialization failed: {e}")))?;
    let update = updater
        .check()
        .await
        .map_err(|e| custom_command_error(format!("Failed to check for updates: {e}")))?;

    if let Some(update) = update {
        Ok(UpdateCheckResponse {
            available: true,
            current_version: update.current_version.clone(),
            version: Some(update.version.clone()),
            date: update.date.map(|d| d.to_string()),
            body: update.body.clone(),
        })
    } else {
        Ok(UpdateCheckResponse {
            available: false,
            current_version: app.package_info().version.to_string(),
            version: None,
            date: None,
            body: None,
        })
    }
}

#[tauri::command]
async fn install_update(
    app: tauri::AppHandle,
    restart: Option<bool>,
) -> Result<(), CommandError> {
    let updater = app
        .updater()
        .map_err(|e| custom_command_error(format!("Updater initialization failed: {e}")))?;
    let update = updater
        .check()
        .await
        .map_err(|e| custom_command_error(format!("Failed to check for updates: {e}")))?
        .ok_or_else(|| custom_command_error("No update available to install.".to_string()))?;

    let app_handle_progress = app.clone();
    update
        .download_and_install(
            move |chunk_length, content_length| {
                let _ = app_handle_progress.emit(
                    "updater-download-progress",
                    UpdateProgressPayload {
                        chunk_length,
                        content_length,
                    },
                );
            },
            || {},
        )
        .await
        .map_err(|e| custom_command_error(format!("Failed to install update: {e}")))?;

    if restart.unwrap_or(true) {
        let app_handle_restart = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            app_handle_restart.restart();
        });
    }
    Ok(())
}

#[tauri::command]
async fn restart_app(app: tauri::AppHandle) -> Result<(), CommandError> {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        app.restart();
    });
    Ok(())
}

#[tauri::command]
async fn start_file_transfer(
    app_handle: tauri::AppHandle,
    paths: Vec<String>,
    target_peer_id: Option<String>,
) -> Result<mouse_thing::core::transfer::FileManifest, CommandError> {
    if !mouse_thing::core::config::load_license_status().is_pro() {
        return Err(CommandError {
            code: "feature_unavailable",
            message: "File transfer is not available in this release.".to_string(),
            details: None,
        });
    }
    let state = app_handle.state::<AppState>();
    let controller = state
        .controller
        .read()
        .map_err(|_| state_poisoned_error())?;
    let sync = controller
        .transfer_sync()
        .ok_or_else(|| internal_error("transfer engine not running"))?;
    let path_bufs: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let (local_peer_id, local_name) = controller.local_identity();
    let local_name = if local_name.trim().is_empty() {
        "Local".to_string()
    } else {
        local_name
    };
    sync.start_transfer(path_bufs, &local_peer_id, &local_name, target_peer_id)
        .map_err(|e| custom_command_error(format!("failed to start transfer: {e}")))
}

#[tauri::command]
async fn accept_file_transfer(
    app_handle: tauri::AppHandle,
    transfer_id: String,
    save_path: Option<String>,
) -> Result<(), CommandError> {
    if !mouse_thing::core::config::load_license_status().is_pro() {
        return Err(CommandError {
            code: "feature_unavailable",
            message: "File transfer is not available in this release.".to_string(),
            details: None,
        });
    }
    let state = app_handle.state::<AppState>();
    let controller = state
        .controller
        .read()
        .map_err(|_| state_poisoned_error())?;
    let sync = controller
        .transfer_sync()
        .ok_or_else(|| internal_error("transfer engine not running"))?;
    let target_dir = match save_path {
        Some(p) => std::path::PathBuf::from(p),
        None => mouse_thing::core::transfer::default_download_dir(),
    };
    sync.accept_transfer(&transfer_id, target_dir)
        .map_err(|e| custom_command_error(format!("failed to accept transfer: {e}")))
}

#[tauri::command]
async fn reject_file_transfer(
    app_handle: tauri::AppHandle,
    transfer_id: String,
    reason: Option<String>,
) -> Result<(), CommandError> {
    let state = app_handle.state::<AppState>();
    let controller = state
        .controller
        .read()
        .map_err(|_| state_poisoned_error())?;
    let sync = controller
        .transfer_sync()
        .ok_or_else(|| internal_error("transfer engine not running"))?;
    sync.reject_transfer(&transfer_id, reason.as_deref().unwrap_or("rejected by user"))
        .map_err(|e| custom_command_error(format!("failed to reject transfer: {e}")))
}

#[cfg(target_os = "windows")]
const WINDOWS_CAPTION_COLOR_DARK: u32 = 0x0019_0F0B;
#[cfg(target_os = "windows")]
const WINDOWS_TEXT_COLOR_DARK: u32 = 0x00FC_FAF8;

#[cfg(target_os = "windows")]
fn configure_windows_title_bar(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute,
        DWMWA_CAPTION_COLOR,
        DWMWA_TEXT_COLOR,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
    };
    if let Ok(hwnd) = window.hwnd() {
        let raw_hwnd = hwnd.0 as windows_sys::Win32::Foundation::HWND;
        let dark_mode: windows_sys::Win32::Foundation::BOOL = 1;
        unsafe {
            let _ = DwmSetWindowAttribute(
                raw_hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                &dark_mode as *const _ as *const core::ffi::c_void,
                core::mem::size_of::<windows_sys::Win32::Foundation::BOOL>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                raw_hwnd,
                DWMWA_CAPTION_COLOR as u32,
                &WINDOWS_CAPTION_COLOR_DARK as *const _ as *const core::ffi::c_void,
                core::mem::size_of::<u32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                raw_hwnd,
                DWMWA_TEXT_COLOR as u32,
                &WINDOWS_TEXT_COLOR_DARK as *const _ as *const core::ffi::c_void,
                core::mem::size_of::<u32>() as u32,
            );
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        // WebKitGTK DMABUF renderer has known instabilities on certain Linux/Wayland setups.
        // Disable it preemptively if not already set.
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            // SAFETY: Executed early during single-threaded startup before GTK or WebKit initialization.
            unsafe {
                std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
        }
    }

    mouse_thing::install_crypto_provider();
    mouse_thing::logging::init_runtime_discovery_logging();
    let app_state = create_app_state();
    install_shutdown_handler(Arc::clone(&app_state.controller));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            open_external_url,
            get_status,
            get_peers,
            get_peers_cached,
            get_config,
            save_config,
            start_engine,
            stop_engine,
            restart_engine,
            accept_connection,
            reject_connection,
            cancel_connection,
            approve_pairing,
            reject_pairing,
            unpair_peer,
            connect_direct_address,
            emergency_unlock,
            force_disconnect,
            get_windows_input_status,
            restart_as_admin,
            install_input_service,
            uninstall_input_service,
            set_secure_desktop_enabled,
            report_gui_error,
            show_accessibility_helper,
            start_file_transfer,
            accept_file_transfer,
            reject_file_transfer,
            get_license_status,
            set_license_key,
            remove_license_key,
            check_for_updates,
            install_update,
            restart_app
        ])
        .setup(|app| {
            #[cfg(target_os = "windows")]
            for window in app.webview_windows().values() {
                configure_windows_title_bar(window);
            }

            #[cfg(target_os = "linux")]
            for window in app.webview_windows().values() {
                if let Err(error) = window.set_decorations(false) {
                    let _ = writeln!(
                        std::io::stderr(),
                        "[gui-setup] failed to hide Linux window decorations: {error}"
                    );
                }
            }

            for window in app.webview_windows().values() {
                if let Err(error) = window.show() {
                    let _ = writeln!(
                        std::io::stderr(),
                        "[gui-setup] failed to show window: {error}"
                    );
                }
            }

            let app_handle = app.handle().clone();
            let state = app.state::<AppState>();

            // Extract data needed for async block before spawning.
            // We must clone the inner AppState because tauri::State holds a reference
            // to app that can't be moved into an async block.
            let controller = state.controller.clone();
            let listen_port = state.listen_port;

            // Defer controller start and event subscription to after runtime is ready.
            // setup() runs before tokio runtime is active, so we must spawn this.
            tauri::async_runtime::spawn(run_controller(app_handle, controller, listen_port));

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| match event {
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
            let state = app_handle.state::<AppState>();
            stop_controller(&state.controller);
        }
        tauri::RunEvent::WindowEvent {
            event: tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed,
            ..
        } => {
            let state = app_handle.state::<AppState>();
            stop_controller(&state.controller);
        }
        _ => {}
    });
}

async fn start_controller(
    controller: Arc<RwLock<Controller>>,
    listen_port: u16,
) -> Result<(), String> {
    // Use spawn_blocking for controller operations that use std::sync::RwLock
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = controller.write().unwrap();
        guard.start(listen_port)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

async fn run_controller(
    app_handle: tauri::AppHandle,
    controller: Arc<RwLock<Controller>>,
    listen_port: u16,
) {
    // Subscribe to PeerUpdate events and forward to frontend
    let subscription_controller = Arc::clone(&controller);
    let receiver = tauri::async_runtime::spawn_blocking(move || {
        let guard = subscription_controller.read().unwrap();
        guard.subscribe_to_peer_updates()
    })
    .await;
    let mut receiver = match receiver {
        Ok(Some(receiver)) => receiver,
        Ok(None) => {
            let _ = writeln!(
                std::io::stderr(),
                "[gui-setup] discovery_tx not available, PeerUpdate events will not be forwarded"
            );
            return;
        }
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "[gui-setup] failed to subscribe to peer updates: {}",
                error
            );
            return;
        }
    };

    if let Err(error) = start_controller(Arc::clone(&controller), listen_port).await {
        let _ = writeln!(
            std::io::stderr(),
            "[gui-setup] failed to start controller: {}",
            error
        );
    } else {
        emit_current_status(&app_handle);
        let state = app_handle.state::<AppState>();
        if let Ok(peers) = read_cached_peers(&state) {
            emit_peers_updated(&app_handle, peers);
        }
    }

    let mut transfer_receiver: Option<tokio::sync::broadcast::Receiver<TransferEvent>> = None;
    let _ = writeln!(
        std::io::stderr(),
        "[gui-setup] starting PeerUpdate event forwarder"
    );
    loop {
        tokio::select! {
            update = receiver.recv() => {
                match update {
                    Ok(update) => {
                        if let PeerUpdate::StatusChanged(_) = &update {
                            update_transfer_receiver(
                                &mut transfer_receiver,
                                controller.read().unwrap().subscribe_to_transfer_updates(),
                            );
                        }
                        let _ = writeln!(
                            std::io::stderr(),
                            "[gui-setup] received PeerUpdate: {:?}",
                            update
                        );
                        forward_peer_update(&app_handle, update);
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        update_transfer_receiver(
                            &mut transfer_receiver,
                            controller.read().unwrap().subscribe_to_transfer_updates(),
                        );
                        let _ = writeln!(
                            std::io::stderr(),
                            "[gui-setup] PeerUpdate event forwarder lagged by {skipped} messages"
                        );
                        let state = app_handle.state::<AppState>();
                        if let Ok(peers) = read_cached_peers(&state) {
                            emit_peers_updated(&app_handle, peers);
                        }
                        emit_current_status(&app_handle);
                    }
                    Err(RecvError::Closed) => {
                        let _ = writeln!(
                            std::io::stderr(),
                            "[gui-setup] PeerUpdate event forwarder closed"
                        );
                        break;
                    }
                }
            }
            event = async {
                match transfer_receiver.as_mut() {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Ok(event) => forward_transfer_event(&app_handle, event),
                    Err(RecvError::Lagged(skipped)) => {
                        let _ = writeln!(
                            std::io::stderr(),
                            "[gui-setup] transfer event forwarder lagged by {skipped} messages"
                        );
                    }
                    Err(RecvError::Closed) => transfer_receiver = None,
                }
            }
        }
    }
}

fn update_transfer_receiver(
    receiver: &mut Option<tokio::sync::broadcast::Receiver<TransferEvent>>,
    current: Option<tokio::sync::broadcast::Receiver<TransferEvent>>,
) {
    match (receiver.as_ref(), current.as_ref()) {
        (Some(previous), Some(current)) if previous.same_channel(current) => {}
        _ => *receiver = current,
    }
}

fn forward_transfer_event(app_handle: &tauri::AppHandle, event: TransferEvent) {
    match event {
        TransferEvent::OfferReceived(manifest) => {
            let _ = app_handle.emit("file-transfer-offer", manifest);
        }
        TransferEvent::Progress(progress) => {
            let _ = app_handle.emit("file-transfer-progress", progress);
        }
        TransferEvent::Complete {
            transfer_id,
            success,
            error,
            saved_path,
        } => {
            let payload = serde_json::json!({
                "transfer_id": transfer_id,
                "success": success,
                "error": error,
                "saved_path": saved_path,
            });
            let _ = app_handle.emit("file-transfer-complete", payload);
        }
    }
}

fn forward_peer_update(app_handle: &tauri::AppHandle, update: PeerUpdate) {
    match update {
        PeerUpdate::Discovered(peers) => {
            emit_peers_updated(app_handle, peers);
            emit_current_status(app_handle);
        }
        PeerUpdate::Removed(_) | PeerUpdate::ConnectionLost { .. } => {
            let state = app_handle.state::<AppState>();
            if let Ok(peers) = read_cached_peers(&state) {
                emit_peers_updated(app_handle, peers);
            }
            emit_current_status(app_handle);
        }
        PeerUpdate::StatusChanged(status) => {
            emit_status_updated(app_handle, status);
        }
        PeerUpdate::ConfigChanged(config) => {
            emit_config_sync_received(app_handle, config);
        }
        PeerUpdate::PairingRequested {
            peer_id,
            name,
            platform,
            code,
            can_respond,
        } => {
            let payload = PairingRequestedPayload {
                peer_id,
                name,
                platform,
                code,
                can_respond,
            };
            if let Err(error) = app_handle.emit("pairing-requested", payload) {
                let _ = writeln!(
                    std::io::stderr(),
                    "[gui-setup] failed to emit pairing-requested: {}",
                    error
                );
            }
        }
        PeerUpdate::PairingResolved { peer_id, approved } => {
            let payload = PairingResolvedPayload {
                peer_id,
                approved,
            };
            if let Err(error) = app_handle.emit("pairing-resolved", payload) {
                let _ = writeln!(
                    std::io::stderr(),
                    "[gui-setup] failed to emit pairing-resolved: {}",
                    error
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn transfer_subscription_preserves_queued_events_on_repeated_updates() {
        let (sender, receiver) = tokio::sync::broadcast::channel(8);
        let mut receiver = Some(receiver);
        sender
            .send(TransferEvent::Complete {
                transfer_id: "completed-transfer".to_string(),
                success: true,
                error: None,
                saved_path: None,
            })
            .unwrap();

        update_transfer_receiver(&mut receiver, Some(sender.subscribe()));
        update_transfer_receiver(&mut receiver, Some(sender.subscribe()));

        assert!(matches!(
            receiver.as_mut().unwrap().try_recv().unwrap(),
            TransferEvent::Complete { transfer_id, .. } if transfer_id == "completed-transfer"
        ));
    }

    #[test]
    fn transfer_subscription_switches_channels_across_restart_and_stop() {
        let (old_sender, old_receiver) = tokio::sync::broadcast::channel(8);
        let mut receiver = Some(old_receiver);
        let old_event = TransferEvent::Complete {
            transfer_id: "old-transfer".to_string(),
            success: true,
            error: None,
            saved_path: None,
        };
        old_sender.send(old_event.clone()).unwrap();

        let (new_sender, new_receiver) = tokio::sync::broadcast::channel(8);
        update_transfer_receiver(&mut receiver, Some(new_receiver));
        assert_eq!(old_sender.receiver_count(), 0);
        assert!(old_sender.send(old_event).is_err());

        let new_event = TransferEvent::Complete {
            transfer_id: "new-transfer".to_string(),
            success: true,
            error: None,
            saved_path: None,
        };
        new_sender.send(new_event.clone()).unwrap();
        update_transfer_receiver(&mut receiver, Some(new_sender.subscribe()));
        assert!(matches!(
            receiver.as_mut().unwrap().try_recv().unwrap(),
            TransferEvent::Complete { transfer_id, .. } if transfer_id == "new-transfer"
        ));

        update_transfer_receiver(&mut receiver, None);
        assert!(receiver.is_none());
        assert_eq!(new_sender.receiver_count(), 0);
        assert!(new_sender.send(new_event.clone()).is_err());
        update_transfer_receiver(&mut receiver, Some(new_sender.subscribe()));
        new_sender.send(new_event).unwrap();
        assert!(matches!(
            receiver.as_mut().unwrap().try_recv().unwrap(),
            TransferEvent::Complete { transfer_id, .. } if transfer_id == "new-transfer"
        ));
    }

    #[allow(dead_code)]
    fn build_test_peer(peer_id: &str) -> PeerSnapshot {
        PeerSnapshot {
            peer_id: peer_id.to_string(),
            name: format!("peer-{peer_id}"),
            hostname: format!("{peer_id}.local"),
            addr: format!("127.0.0.1:{}", mouse_thing::core::policies::DEFAULT_PORT),
            platform: Some("test".to_string()),
            screen: None,
            virtual_bounds: None,
            display_rects: Vec::new(),
            last_seen_ms: 0,
            discovery_status: mouse_thing::core::api::DiscoveryStatusSnapshot::Online,
            is_local: false,
            connected: false,
            paired: false,
            connection_status: None,
        }
    }

    fn assert_config_json_eq(left: &ConfigSnapshot, right: &ConfigSnapshot) {
        assert_eq!(
            serde_json::to_value(left).expect("serialize left config"),
            serde_json::to_value(right).expect("serialize right config")
        );
    }

    #[test]
    fn get_config_no_remote_update_does_not_emit_event() {
        let emit_calls = Cell::new(0usize);
        let emit_failure_calls = Cell::new(0usize);
        let expected = ConfigSnapshot::default();

        let returned = finalize_get_config_response(
            expected.clone(),
            false,
            |_, _| {
                emit_calls.set(emit_calls.get() + 1);
                Ok(())
            },
            |_, _| emit_failure_calls.set(emit_failure_calls.get() + 1),
        );

        assert_eq!(emit_calls.get(), 0);
        assert_eq!(emit_failure_calls.get(), 0);
        assert_config_json_eq(&returned, &expected);
    }

    #[test]
    fn get_config_remote_update_emits_single_event_with_returned_config() {
        let emits: RefCell<Vec<(String, ConfigSyncReceivedPayload)>> = RefCell::new(Vec::new());
        let emit_failure_calls = Cell::new(0usize);
        let mut expected = ConfigSnapshot::default();
        expected.left = Some("peer-left".to_string());
        expected.batch_window_us = 1200;

        let returned = finalize_get_config_response(
            expected.clone(),
            true,
            |event, payload| {
                emits.borrow_mut().push((event.to_string(), payload));
                Ok(())
            },
            |_, _| emit_failure_calls.set(emit_failure_calls.get() + 1),
        );

        let emitted = emits.borrow();
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].0, CONFIG_SYNC_RECEIVED_EVENT);
        assert_config_json_eq(&emitted[0].1.config, &returned);
        assert_eq!(emit_failure_calls.get(), 0);
    }

    #[test]
    fn get_config_emit_failure_is_non_fatal_and_logs_telemetry() {
        let emit_calls = Cell::new(0usize);
        let emit_failure: RefCell<Option<(String, String)>> = RefCell::new(None);
        let mut expected = ConfigSnapshot::default();
        expected.right = Some("peer-right".to_string());

        let returned = finalize_get_config_response(
            expected.clone(),
            true,
            |_, _| {
                emit_calls.set(emit_calls.get() + 1);
                Err("window closed".to_string())
            },
            |event, error| {
                emit_failure
                    .borrow_mut()
                    .replace((event.to_string(), error.to_string()));
            },
        );

        assert_eq!(emit_calls.get(), 1);
        assert_config_json_eq(&returned, &expected);
        let failure = emit_failure
            .borrow()
            .clone()
            .expect("emit failure should be logged");
        assert_eq!(failure.0, CONFIG_SYNC_RECEIVED_EVENT);
        assert_eq!(failure.1, "window closed");
    }

    #[test]
    fn status_updated_payload_is_wrapped_for_the_frontend_contract() {
        let mut status = StatusSnapshot::default();
        status.engine_running = true;
        let payload = StatusUpdatedPayload { status };
        let json = serde_json::to_value(payload).expect("serialize status payload");

        assert_eq!(json["status"]["engine_running"], true);
    }

    #[test]
    fn invalid_input_maps_to_gui_friendly_invalid_config_error() {
        let mapped = CommandError::from_core(CoreError::InvalidInput(
            "placement peer name must be 1-128 characters".to_string(),
        ));

        assert_eq!(mapped.code, "invalid_config");
        assert_eq!(
            mapped.message,
            "Config validation failed. Check edge mappings and retry."
        );
        assert!(mapped.details.is_some());
    }

    #[test]
    fn discovery_errors_map_to_discovery_code() {
        let mapped = CommandError::from_core(CoreError::Discovery("mdns timeout".to_string()));

        assert_eq!(mapped.code, "discovery_error");
        assert_eq!(
            mapped.message,
            "Peer discovery failed. Ensure mDNS is available on your network."
        );
        assert!(mapped.details.is_some());
    }

    #[test]
    fn platform_error_maps_to_platform_error_code() {
        let mapped = CommandError::from_core(CoreError::Platform("init failed".to_string()));
        assert_eq!(mapped.code, "platform_error");
        assert!(mapped.details.is_some());
    }

    #[test]
    fn connection_error_maps_to_connection_error_code() {
        let mapped = CommandError::from_core(CoreError::Connection("timed out".to_string()));
        assert_eq!(mapped.code, "connection_error");
        assert!(mapped.details.is_some());
    }

    #[test]
    fn engine_error_maps_to_internal_error_code() {
        let mapped = CommandError::from_core(CoreError::Engine("crashed".to_string()));
        assert_eq!(mapped.code, "internal_error");
        assert!(mapped.details.is_some());
    }

    #[test]
    fn config_error_maps_to_config_error_code() {
        let mapped = CommandError::from_core(CoreError::Config("parse error".to_string()));
        assert_eq!(mapped.code, "config_error");
        assert!(mapped.details.is_some());
    }

    #[test]
    fn network_error_maps_to_socket_bind_failed_code() {
        let mapped = CommandError::from_core(CoreError::Network("bind error".to_string()));
        assert_eq!(mapped.code, "socket_bind_failed");
        assert!(mapped.details.is_some());
    }
}
