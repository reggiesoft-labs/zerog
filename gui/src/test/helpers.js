import { vi } from "vitest";
import { commandHandlers, eventListeners, mockInvoke } from "./setup.js";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export function defaultStatus(overrides = {}) {
  return {
    engine_running: false,
    out_events_per_sec: 0,
    in_events_per_sec: 0,
    connected_peers: [],
    incoming_connections: [],
    ...overrides,
  };
}

export function defaultConfig(overrides = {}) {
  return {
    left: null,
    right: null,
    top: null,
    bottom: null,
    zones: [],
    placements: [],
    batch_window_us: 0,
    max_events_per_packet: 32,
    movement_cadence_us: 8333,
    metrics_log_path: null,
    legacy_issues: [],
    swap_alt_cmd: true,
    modifier_mapping: null,
    share_clipboard: true,
    keep_mouse_alive: false,
    release_tap_key: "escape",
    auto_connect_peers: true,
    paired_peers: [],
    ...overrides,
  };
}

export function buildPeer(overrides = {}) {
  return {
    peer_id: "peer-a",
    name: "Alpha",
    hostname: "alpha.local",
    addr: "10.0.0.2:54321",
    platform: "macos",
    screen: null,
    virtual_bounds: { width: 1920, height: 1080 },
    last_seen_ms: 500,
    discovery_status: "online",
    is_local: false,
    ...overrides,
  };
}

export function buildLocalPeer(overrides = {}) {
  return buildPeer({
    peer_id: "local-peer",
    name: "Local",
    is_local: true,
    virtual_bounds: { width: 1440, height: 900 },
    ...overrides,
  });
}

export function registerCommand(command, handler) {
  commandHandlers.set(command, typeof handler === "function" ? handler : () => handler);
}

export function registerDefaultCommands(overrides = {}) {
  const peers = overrides.peers ?? [];
  const defaultConnectedPeers = peers
    .filter((p) => !p.is_local && p.discovery_status !== "stale" && p.discovery_status !== "disconnected")
    .map((p) => ({ peer_id: p.peer_id, name: p.name, platform: p.platform }));
  const status = overrides.status ?? defaultStatus({ connected_peers: defaultConnectedPeers });
  const config = overrides.config ?? defaultConfig();
  registerCommand("get_status", () => status);
  registerCommand("get_config", () => config);
  registerCommand("get_peers_cached", () => peers);
  registerCommand("report_gui_error", () => undefined);
  registerCommand("show_accessibility_helper", () => undefined);
  registerCommand("restart_engine", () => status);
  const windowsInputStatus = () => ({
    supported: false,
    elevated: false,
    service_installed: false,
    service_running: false,
    secure_desktop_enabled: null,
    service_reachable: false,
  });
  registerCommand("get_windows_input_status", windowsInputStatus);
  registerCommand("install_input_service", windowsInputStatus);
  registerCommand("uninstall_input_service", windowsInputStatus);
  registerCommand("set_secure_desktop_enabled", windowsInputStatus);
  registerCommand("restart_as_admin", () => undefined);
  registerCommand("accept_connection", () => status);
  registerCommand("reject_connection", () => status);
  registerCommand("cancel_connection", () => status);
  registerCommand("unpair_peer", () => status);
  const defaultLicense = overrides.license ?? {
    status: "active",
    licensee: "Test User",
    tier: "pro",
    expires_at: null,
  };
  registerCommand("get_license_status", () => defaultLicense);
  registerCommand("set_license_key", () => ({
    status: "active",
    licensee: "Activated User",
    tier: "pro",
    expires_at: null,
  }));
  registerCommand("remove_license_key", () => ({ status: "free" }));
  registerCommand("check_for_updates", () => ({
    available: false,
    current_version: "1.0.0",
    version: null,
    date: null,
    body: null,
  }));
  registerCommand("install_update", () => undefined);
  registerCommand("restart_app", () => undefined);

  if (overrides.commands) {
    for (const [cmd, handler] of Object.entries(overrides.commands)) {
      registerCommand(cmd, handler);
    }
  }
}

export function fireEvent(eventName, payload) {
  const callback = eventListeners.get(eventName);
  if (!callback) {
    throw new Error(`[test] No listener registered for event: ${eventName}`);
  }
  callback({ payload });
}

export function hasEventListener(eventName) {
  return eventListeners.has(eventName);
}

const htmlPath = resolve(import.meta.dirname, "..", "index.html");
const htmlContent = readFileSync(htmlPath, "utf-8");

export function loadHtml() {
  const bodyMatch = htmlContent.match(/<body[^>]*>([\s\S]*)<\/body>/i);
  if (bodyMatch) {
    document.body.innerHTML = bodyMatch[1];
  } else {
    document.documentElement.innerHTML = htmlContent;
  }
}

export function stubSvgMethods() {
  const svg = document.querySelector("#layout-svg");
  if (!svg) return;

  svg.getScreenCTM = () => new DOMMatrix([1, 0, 0, 1, 0, 0]);
  svg.setPointerCapture = vi.fn();
  svg.releasePointerCapture = vi.fn();
  svg.hasPointerCapture = vi.fn(() => true);
}

export async function initApp() {
  const event = new Event("DOMContentLoaded");
  window.dispatchEvent(event);
  await vi.advanceTimersByTimeAsync(0);
}

export function getTextContent(selector) {
  return document.querySelector(selector)?.textContent?.trim() ?? null;
}

export function getButton(selector) {
  return document.querySelector(selector);
}

export function clearCommandHandlers({ clearEvents = true } = {}) {
  commandHandlers.clear();
  if (clearEvents) {
    eventListeners.clear();
  }
  mockInvoke.mockClear();
}

export function createPointerEvent(type, options = {}) {
  // Use proper PointerEvent constructor to ensure correct interface properties
  const init = {
    bubbles: true,
    cancelable: true,
    clientX: options.clientX ?? 0,
    clientY: options.clientY ?? 0,
    pointerId: options.pointerId ?? 0,
    button: options.button ?? 0,
    isPrimary: options.isPrimary ?? true,
    ...options,
  };
  const event = new PointerEvent(type, init);
  event.preventDefault = vi.fn();
  return event;
}

export function dispatchPointer(target, type, options = {}) {
  const event = createPointerEvent(type, options);
  target.dispatchEvent(event);
  return event;
}

export async function switchToConfigTab() {
  const configTabBtn = document.querySelector('[data-tab="tab-config"]');
  if (!configTabBtn) throw new Error("Config tab button not found");
  configTabBtn.click();
  await vi.advanceTimersByTimeAsync(0);
}

export async function switchToTransfersTab() {
  const transfersTabBtn = document.querySelector('[data-tab="tab-transfers"]');
  if (!transfersTabBtn) throw new Error("Transfers tab button not found");
  transfersTabBtn.click();
  await vi.advanceTimersByTimeAsync(0);
}

export async function switchToMainTab() {
  const mainTabBtn = document.querySelector('[data-tab="tab-main"]');
  if (!mainTabBtn) throw new Error("Main tab button not found");
  mainTabBtn.click();
  await vi.advanceTimersByTimeAsync(0);
}

export function getInvokeCalls(command) {
  return mockInvoke.mock.calls.filter(call => call[0] === command);
}

export function wasInvokeCalled(command) {
  return mockInvoke.mock.calls.some(call => call[0] === command);
}

export function getInvokeCallCount(command) {
  return getInvokeCalls(command).length;
}

export function peerListSnapshot() {
  return Array.from(document.querySelectorAll("#peer-list li")).map((item) => ({
    peerId: item.dataset.peerId ?? null,
    text: item.textContent?.trim() ?? "",
    className: item.className,
  }));
}
