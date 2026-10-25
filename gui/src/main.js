import { createLayoutController } from "./layoutDiagram.js";
import { createConfigSaveQueue } from "./configSaveQueue.js";
import { renderPeerList } from "./peerList.js";
import { formatPairingCode, formatPeerPlatformLabel, getLegacyIssueMessages } from "./peerViewModel.js";
import { clientToSvgPoint } from "./layoutInteraction.js";

function invoke(command, payload) {
  const fn = window.__TAURI__?.core?.invoke;
  if (!fn) {
    throw new Error(`Tauri invoke unavailable: ${command}`);
  }
  return fn(command, payload);
}

function listen(event, handler) {
  const fn = window.__TAURI__?.event?.listen;
  if (!fn) {
    return Promise.resolve(() => {});
  }
  return fn(event, handler);
}

function getCurrentWindow() {
  const fn = window.__TAURI__?.window?.getCurrentWindow;
  if (!fn) {
    return null;
  }
  return fn();
}

const PERF_DEBUG_FLAG = "mouseThingPerfDebug";
const AUTO_ACCEPT_FILES_KEY = "zerogAutoAcceptFiles";
const PERF_WARN_COMMAND_MS = 120;
const PERF_WARN_TAB_SWITCH_HANDLER_MS = 16;
const PERF_WARN_TAB_SWITCH_PAINT_MS = 16;
const PERF_WARN_LONG_TASK_MS = 50;
const MAX_RECENT_TRANSFERS = 6;

const state = {
  activeTab: "tab-main",
  status: null,
  peers: [],
  peerRevision: 0,
  config: null,
  configDirty: false,
  configRevision: 0,
  configSaveQueue: null,
  pendingRemoteConfig: null,
  layoutDrag: null,
  layoutRenderCache: {
    placements: new Map(),
    remotePeers: [],
    localPeer: null,
  },
  activeTransfers: new Map(),
  completedTransfers: [],
  fileDropTargetPeerId: null,
  configSaveDebounceId: null,
  perf: {
    dragMoveCount: 0,
    dragStartTs: 0,
    dragLastMoveTs: 0,
  },
  meshActionInProgress: false,
  engineRestartInProgress: false,
  peerActionsInFlight: new Set(),
  windowsInput: null,
  windowsInputLoading: false,
  license: null,
  licenseLoading: false,
  updater: {
    checking: false,
    downloading: false,
    updateAvailable: false,
    updateInfo: null,
    downloadedBytes: 0,
    bannerDismissed: false,
    updateReady: false,
    autoUpdateEnabled: true,
  },
};

const ui = {
  engineRunning: null,
  configForm: null,
  configStatus: null,
  peerList: null,
  errorBanner: null,
  tabButtons: null,
  tabPanes: null,
  layoutSvg: null,
  engineRestart: null,
  engineToggle: null,
  keyboardStripContainer: null,
  shareClipboard: null,
  keepMouseAlive: null,
  releaseTapKey: null,
  autoAcceptFiles: null,
  transferToastContainer: null,
  transfersButton: null,
  transfersCount: null,
  activeTransfersList: null,
  activeTransfersEmpty: null,
  recentTransfersList: null,
  recentTransfersEmpty: null,
  autoConnectPeers: null,
  pairingDialog: null,
  pairingDesc: null,
  pairingPeerName: null,
  pairingPeerPlatform: null,
  pairingCode: null,
  pairingCodeBlock: null,
  btnPairingAllow: null,
  btnPairingBlock: null,
  btnPairingCancel: null,
  windowsInputCard: null,
  windowsInputStatus: null,
  btnWinService: null,
  licenseTierBadge: null,
  licenseStatusText: null,
  btnActivateLicense: null,
  btnRemoveLicense: null,
  licenseDialog: null,
  licenseKeyInput: null,
  licenseModalError: null,
  btnLicenseCancel: null,
  btnLicenseSubmit: null,
  updaterStatusText: null,
  updaterProgressContainer: null,
  updaterProgressBar: null,
  btnCheckUpdates: null,
  btnInstallUpdate: null,
  updateBanner: null,
  updateBannerText: null,
  btnBannerUpdate: null,
  btnBannerDismiss: null,
  settingsUpdateBadge: null,
  autoUpdateToggle: null,
};

let configSyncListenerRegistered = false;
let peerEventListenerRegistered = false;
let statusEventListenerRegistered = false;
let pairingEventListenerRegistered = false;
let fileTransferListenerRegistered = false;
let fileDropListenerRegistered = false;
let currentPairingPeerId = null;
let currentPairingCanRespond = true;
let appInitialized = false;
const eventUnlisteners = new Map();
let layoutController = null;

const DOM_CONTENT_LOADED_HANDLER_KEY = "__mouseThingDomContentLoadedHandler";
const APP_CLEANUP_HANDLER_KEY = "__mouseThingAppCleanup";

function bindElements() {
  ui.engineRunning = document.querySelector("#engine-running");
  ui.configForm = document.querySelector("#config-form");
  ui.configStatus = document.querySelector("#config-status");
  ui.peerList = document.querySelector("#peer-list");
  ui.errorBanner = document.querySelector("#error-banner");
  ui.tabButtons = document.querySelectorAll(".tab-btn");
  ui.tabPanes = document.querySelectorAll(".tab-pane");
  ui.layoutSvg = document.querySelector("#layout-svg");
  ui.engineRestart = document.querySelector("#btn-engine-restart");
  ui.engineToggle = document.querySelector("#btn-engine-toggle");
  ui.keyboardStripContainer = document.querySelector("#keyboard-strip-container");
  ui.shareClipboard = document.querySelector("#share-clipboard");
  ui.keepMouseAlive = document.querySelector("#keep-mouse-alive");
  ui.releaseTapKey = document.querySelector("#release-tap-key");
  ui.autoAcceptFiles = document.querySelector("#auto-accept-files");
  ui.transferToastContainer = document.querySelector("#transfer-toast-container");
  ui.transferSnackbarContainer = document.querySelector("#transfer-snackbar-container");
  ui.transfersButton = document.querySelector("#btn-transfers");
  ui.transfersCount = document.querySelector("#transfers-active-count");
  ui.activeTransfersList = document.querySelector("#active-transfers-list");
  ui.activeTransfersEmpty = document.querySelector("#active-transfers-empty");
  ui.recentTransfersList = document.querySelector("#recent-transfers-list");
  ui.recentTransfersEmpty = document.querySelector("#recent-transfers-empty");
  ui.pairingDialog = document.querySelector("#pairing-dialog");
  ui.pairingDesc = document.querySelector("#pairing-desc");
  ui.pairingPeerName = document.querySelector("#pairing-peer-name");
  ui.pairingPeerPlatform = document.querySelector("#pairing-peer-platform");
  ui.pairingCode = document.querySelector("#pairing-peer-code");
  ui.pairingCodeBlock = document.querySelector("#pairing-peer-code-block");
  ui.btnPairingAllow = document.querySelector("#btn-pairing-allow");
  ui.btnPairingBlock = document.querySelector("#btn-pairing-block");
  ui.btnPairingCancel = document.querySelector("#btn-pairing-cancel");
  ui.windowsInputCard = document.querySelector("#windows-input-card");
  ui.windowsInputStatus = document.querySelector("#windows-input-status");
  ui.btnWinService = document.querySelector("#btn-win-service");
  ui.licenseTierBadge = document.querySelector("#license-tier-badge");
  ui.licenseStatusText = document.querySelector("#license-status-text");
  ui.btnActivateLicense = document.querySelector("#btn-activate-license");
  ui.btnRemoveLicense = document.querySelector("#btn-remove-license");
  ui.licenseDialog = document.querySelector("#license-dialog");
  ui.licenseKeyInput = document.querySelector("#license-key-input");
  ui.licenseModalError = document.querySelector("#license-modal-error");
  ui.btnLicenseCancel = document.querySelector("#btn-license-cancel");
  ui.btnLicenseSubmit = document.querySelector("#btn-license-submit");
  ui.updaterStatusText = document.querySelector("#updater-status-text");
  ui.updaterProgressContainer = document.querySelector("#updater-progress-container");
  ui.updaterProgressBar = document.querySelector("#updater-progress-bar");
  ui.btnCheckUpdates = document.querySelector("#btn-check-updates");
  ui.btnInstallUpdate = document.querySelector("#btn-install-update");
  ui.updateBanner = document.querySelector("#update-banner");
  ui.updateBannerText = document.querySelector("#update-banner-text");
  ui.btnBannerUpdate = document.querySelector("#btn-banner-update");
  ui.btnBannerDismiss = document.querySelector("#btn-banner-dismiss");
  ui.settingsUpdateBadge = document.querySelector("#settings-update-badge");
  ui.autoUpdateToggle = document.querySelector("#auto-update-toggle");
  ui.aboutAppVersion = document.querySelector("#about-app-version");

  try {
    const storedAuto = localStorage.getItem("zerog_auto_update");
    if (storedAuto !== null) {
      state.updater.autoUpdateEnabled = storedAuto === "true";
      if (ui.autoUpdateToggle) {
        ui.autoUpdateToggle.checked = state.updater.autoUpdateEnabled;
      }
    }
  } catch (_e) {}

  const activePane = Array.from(ui.tabPanes).find((pane) =>
    pane.classList.contains("tab-pane--active"),
  );
  state.activeTab = activePane?.id ?? "tab-main";
}

function showError(message) {
  console.error("[GUI ERROR]", message);
  invoke("report_gui_error", { message }).catch((error) => {
    console.error("[GUI ERROR LOGGING FAILED]", error);
  });
  ui.errorBanner.textContent = message;
  ui.errorBanner.hidden = false;
}

function clearError() {
  ui.errorBanner.hidden = true;
  ui.errorBanner.textContent = "";
}

function isPerfDebugEnabled() {
  try {
    return window.localStorage?.getItem(PERF_DEBUG_FLAG) === "1";
  } catch {
    return false;
  }
}

function perfLog(level, message, meta = undefined) {
  if (level === "debug" && !isPerfDebugEnabled()) {
    return;
  }

  const prefix = "[gui-perf]";
  if (meta === undefined) {
    if (level === "warn") {
      console.warn(prefix, message);
    } else {
      console.debug(prefix, message);
    }
    return;
  }

  if (level === "warn") {
    console.warn(prefix, message, meta);
  } else {
    console.debug(prefix, message, meta);
  }
}

async function invokeTimed(
  command,
  payload = undefined,
  warnThresholdMs = PERF_WARN_COMMAND_MS,
) {
  const start = performance.now();
  const result = await invoke(command, payload);
  const durationMs = performance.now() - start;

  if (durationMs >= warnThresholdMs) {
    perfLog("warn", `${command} took ${durationMs.toFixed(1)}ms`, {
      command,
      durationMs,
      statusTabActive: isStatusTabActive(),
      dragging: Boolean(state.layoutDrag),
    });
  } else {
    perfLog("debug", `${command} took ${durationMs.toFixed(1)}ms`);
  }

  return result;
}

function setupEventLoopLagProbe() {
  let expectedAt = performance.now() + 1000;

  window.setInterval(() => {
    const now = performance.now();
    const lagMs = now - expectedAt;
    expectedAt = now + 1000;

    if (lagMs > 120) {
      perfLog("warn", `event-loop lag ${lagMs.toFixed(1)}ms`, {
        lagMs,
        statusTabActive: isStatusTabActive(),
        dragging: Boolean(state.layoutDrag),
      });
    }
  }, 1000);
}

function setupLongTaskProbe() {
  if (typeof window.PerformanceObserver !== "function") {
    return;
  }

  try {
    const observer = new window.PerformanceObserver((list) => {
      list.getEntries().forEach((entry) => {
        if (entry.duration < PERF_WARN_LONG_TASK_MS) {
          return;
        }

        perfLog("warn", `long task detected (${entry.duration.toFixed(1)}ms)`, {
          durationMs: entry.duration,
          entryName: entry.name,
          statusTabActive: isStatusTabActive(),
          dragging: Boolean(state.layoutDrag),
        });
      });
    });
    observer.observe({ type: "longtask", buffered: true });
  } catch {
    perfLog("debug", "long task observer unsupported");
  }
}

function normalizePeerKey(peerId) {
  return String(peerId ?? "").trim().toLowerCase();
}

// Marks a peer action as in flight so rapid clicks (button mashing) cannot
// dispatch duplicate backend commands. Returns the normalized peer key, or
// null when the action should be ignored.
function beginPeerAction(peerId) {
  const normalized = normalizePeerKey(peerId);
  if (!normalized || state.peerActionsInFlight.has(normalized)) {
    return null;
  }
  state.peerActionsInFlight.add(normalized);
  return normalized;
}

async function handleConnectPeer(peerId) {
  const normalized = beginPeerAction(peerId);
  if (!normalized) {
    return;
  }
  try {
    const status = await invokeTimed("accept_connection", { peerId });
    if (Array.isArray(state.peers)) {
      const match = state.peers.find((p) => normalizePeerKey(p?.peer_id) === normalized);
      if (match) {
        match.connection_status = "connecting";
      }
    }
    if (status) {
      renderStatus(status);
    } else {
      renderPeerListView(state.peers);
      if (isStatusTabActive() && !state.layoutDrag) {
        layoutController?.renderLayoutDiagram();
      }
    }
  } catch (error) {
    showError(error?.message ?? String(error));
  } finally {
    state.peerActionsInFlight.delete(normalized);
  }
}

async function handleDisconnectPeer(peerId) {
  const normalized = beginPeerAction(peerId);
  if (!normalized) {
    return;
  }
  try {
    const status = await invokeTimed("reject_connection", { peerId });
    if (Array.isArray(state.peers)) {
      const match = state.peers.find((p) => normalizePeerKey(p?.peer_id) === normalized);
      if (match) {
        match.connected = false;
        match.connection_status = "disconnected";
      }
    }
    if (status) {
      renderStatus(status);
    } else {
      renderPeerListView(state.peers);
      if (isStatusTabActive() && !state.layoutDrag) {
        layoutController?.renderLayoutDiagram();
      }
    }
  } catch (error) {
    showError(error?.message ?? String(error));
  } finally {
    state.peerActionsInFlight.delete(normalized);
  }
}

async function handleCancelPeer(peerId) {
  const normalized = beginPeerAction(peerId);
  if (!normalized) {
    return;
  }
  try {
    const status = await invokeTimed("cancel_connection", { peerId });
    if (Array.isArray(state.peers)) {
      const match = state.peers.find((p) => normalizePeerKey(p?.peer_id) === normalized);
      if (match) {
        match.connected = false;
        match.connection_status = "disconnected";
      }
    }
    if (currentPairingPeerId && normalizePeerKey(currentPairingPeerId) === normalized) {
      hidePairingDialog();
    }
    if (status) {
      renderStatus(status);
    } else {
      renderPeerListView(state.peers);
      if (isStatusTabActive() && !state.layoutDrag) {
        layoutController?.renderLayoutDiagram();
      }
    }
  } catch (error) {
    showError(error?.message ?? String(error));
  } finally {
    state.peerActionsInFlight.delete(normalized);
  }
}

async function handleUnpairPeer(peerId) {
  const normalized = beginPeerAction(peerId);
  if (!normalized) {
    return;
  }
  try {
    const status = await invokeTimed("unpair_peer", { peerId });
    if (state.config && Array.isArray(state.config.paired_peers)) {
      state.config.paired_peers = state.config.paired_peers.filter(
        (id) => normalizePeerKey(id) !== normalized,
      );
    }
    if (Array.isArray(state.peers)) {
      const match = state.peers.find((p) => normalizePeerKey(p?.peer_id) === normalized);
      if (match) {
        match.paired = false;
        match.connected = false;
        match.connection_status = "disconnected";
      }
    }
    if (status) {
      renderStatus(status);
    } else {
      renderPeerListView(state.peers);
      if (isStatusTabActive() && !state.layoutDrag) {
        layoutController?.renderLayoutDiagram();
      }
    }
  } catch (error) {
    showError(error?.message ?? String(error));
  } finally {
    state.peerActionsInFlight.delete(normalized);
  }
}

function renderPeerListView(peers) {
  renderPeerList({
    peers,
    state,
    ui,
    isStatusTabActive,
    appendEmptyPeerMessage,
    onConnectPeer: handleConnectPeer,
    onDisconnectPeer: handleDisconnectPeer,
    onUnpairPeer: handleUnpairPeer,
    onCancelPeer: handleCancelPeer,
  });
}

function renderStatus(status) {
  state.status = status;
  ui.engineRunning.textContent = status.engine_running ? "Running" : "Stopped";
  const indicator = document.querySelector("#daemon-indicator");
  if (indicator) {
    indicator.className = `status-indicator-dot ${status.engine_running ? "status-indicator-dot--running" : "status-indicator-dot--stopped"}`;
  }
  renderPeerListView(state.peers);
  updateEngineToggleButton();
  if (isStatusTabActive() && !state.layoutDrag) {
    layoutController?.renderLayoutDiagram();
  }
}

function updateEngineToggleButton() {
  const isRunning = state.status?.engine_running ?? false;
  const isInProgress = state.meshActionInProgress;

  if (ui.engineToggle) {
    ui.engineToggle.disabled = isInProgress;

    if (isInProgress) {
      if (state.engineRestartInProgress) {
        ui.engineToggle.textContent = "Stop";
        ui.engineToggle.className = "btn btn--stop";
      } else {
        ui.engineToggle.textContent = isRunning ? "Stopping..." : "Starting...";
        ui.engineToggle.className = "btn btn-primary";
      }
    } else if (isRunning) {
      ui.engineToggle.textContent = "Stop";
      ui.engineToggle.className = "btn btn--stop";
    } else {
      ui.engineToggle.textContent = "Start";
      ui.engineToggle.className = "btn btn--start";
    }
  }

  if (ui.engineRestart) {
    ui.engineRestart.disabled = isInProgress || !isRunning;
    if (state.engineRestartInProgress) {
      ui.engineRestart.textContent = "Restarting...";
      ui.engineRestart.className = "btn btn-primary";
    } else {
      ui.engineRestart.textContent = "Restart";
      ui.engineRestart.className = "btn btn--restart";
    }
  }
}

function isHostMac() {
  return (
    typeof navigator !== "undefined" &&
    /Mac|iPhone|iPad|iPod/i.test(navigator.platform || navigator.userAgent || "")
  );
}

function isHostLinux() {
  return (
    typeof navigator !== "undefined" &&
    /Linux/i.test(navigator.platform || navigator.userAgent || "")
  );
}

function getActiveModifierValue(keyId, config, isMac) {
  if (config?.modifier_mapping) {
    if (config.modifier_mapping[keyId]) {
      return config.modifier_mapping[keyId];
    }
    if (keyId === "right_ctrl" && config.modifier_mapping.left_ctrl) {
      return config.modifier_mapping.left_ctrl;
    }
    if (keyId === "right_alt" && config.modifier_mapping.left_alt) {
      return config.modifier_mapping.left_alt;
    }
    if (keyId === "right_meta" && config.modifier_mapping.left_meta) {
      return config.modifier_mapping.left_meta;
    }
  }
  const isSwapped =
    config?.swap_alt_cmd !== undefined
      ? Boolean(config.swap_alt_cmd)
      : true;
  if (isMac) {
    if (keyId === "left_alt" || keyId === "right_alt") {
      return isSwapped ? "meta" : "alt";
    }
    if (keyId === "left_meta" || keyId === "right_meta") {
      return isSwapped ? "alt" : "meta";
    }
    return "ctrl";
  } else {
    if (keyId === "left_alt" || keyId === "right_alt") {
      return isSwapped ? "meta" : "alt";
    }
    if (keyId === "left_meta" || keyId === "right_meta") {
      return isSwapped ? "alt" : "meta";
    }
    return "ctrl";
  }
}

function getModifierTargetLabel(targetValue, isMac) {
  if (isMac) {
    switch (targetValue) {
      case "ctrl":
        return "Ctrl";
      case "alt":
        return "Alt";
      case "meta":
        return "Win ⊞";
      default:
        return targetValue;
    }
  } else {
    switch (targetValue) {
      case "ctrl":
        return "Control ⌃";
      case "alt":
        return "Option ⌥";
      case "meta":
        return "Cmd ⌘";
      default:
        return targetValue;
    }
  }
}

function renderKeyboardStrip(config) {
  if (!ui.keyboardStripContainer) {
    return;
  }

  const isMac = isHostMac();
  const keys = isMac
    ? [
        { id: "left_ctrl", local: "Control ⌃", options: ["ctrl", "alt", "meta"] },
        { id: "left_alt", local: "Option ⌥", options: ["alt", "meta", "ctrl"] },
        { id: "left_meta", local: "Cmd ⌘", options: ["meta", "alt", "ctrl"] },
      ]
    : [
        { id: "left_ctrl", local: "Ctrl", options: ["ctrl", "meta", "alt"] },
        { id: "left_meta", local: "Win / Super ⊞", options: ["meta", "alt", "ctrl"] },
        { id: "left_alt", local: "Alt", options: ["alt", "meta", "ctrl"] },
      ];

  ui.keyboardStripContainer.innerHTML = "";

  keys.forEach((key) => {
    const currentVal = getActiveModifierValue(key.id, config, isMac);
    const targetLabel = getModifierTargetLabel(currentVal, isMac);

    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "keycap";
    btn.dataset.keyId = key.id;
    btn.setAttribute(
      "aria-label",
      `${key.local} sends ${targetLabel}. Click to change.`,
    );

    const tagSpan = document.createElement("span");
    tagSpan.className = "keycap-tag";
    tagSpan.textContent = "Local";

    const localSpan = document.createElement("span");
    localSpan.className = "keycap-local";
    localSpan.textContent = key.local;

    const arrowSpan = document.createElement("span");
    arrowSpan.className = "keycap-arrow";
    arrowSpan.textContent = "sends";

    const targetSpan = document.createElement("span");
    targetSpan.className = "keycap-target";
    targetSpan.textContent = targetLabel;

    btn.appendChild(tagSpan);
    btn.appendChild(localSpan);
    btn.appendChild(arrowSpan);
    btn.appendChild(targetSpan);

    btn.addEventListener("click", () => {
      handleKeycapClick(key.id, key.options, isMac);
    });

    ui.keyboardStripContainer.appendChild(btn);
  });
}

function handleKeycapClick(keyId, options, isMac) {
  if (!state.config) {
    state.config = {};
  }
  const currentVal = getActiveModifierValue(keyId, state.config, isMac);
  const currentIndex = options.indexOf(currentVal);
  const nextVal = options[(currentIndex + 1) % options.length];

  if (!state.config.modifier_mapping) {
    const ctrlVal = getActiveModifierValue("left_ctrl", state.config, isMac);
    const altVal = getActiveModifierValue("left_alt", state.config, isMac);
    const metaVal = getActiveModifierValue("left_meta", state.config, isMac);
    state.config.modifier_mapping = {
      left_ctrl: ctrlVal,
      left_alt: altVal,
      left_meta: metaVal,
      right_meta: metaVal,
      right_alt: altVal,
      right_ctrl: ctrlVal,
    };
  }

  state.config.modifier_mapping[keyId] = nextVal;
  if (keyId === "left_ctrl") {
    state.config.modifier_mapping.right_ctrl = nextVal;
  } else if (keyId === "left_alt") {
    state.config.modifier_mapping.right_alt = nextVal;
  } else if (keyId === "left_meta") {
    state.config.modifier_mapping.right_meta = nextVal;
  }

  const leftAlt = state.config.modifier_mapping.left_alt;
  const leftMeta = state.config.modifier_mapping.left_meta;
  state.config.swap_alt_cmd = leftAlt === "meta" && leftMeta === "alt";

  renderKeyboardStrip(state.config);
  markConfigEdited();
  requestConfigSave();
}



function renderConfig(config, { renderLayout = true } = {}) {
  state.config = config;
  state.configDirty = false;
  state.pendingRemoteConfig = null;
  const legacyMessages = getLegacyIssueMessages(config);
  setConfigStatus(legacyMessages.join(" | "));

  renderKeyboardStrip(config);

  if (ui.shareClipboard) {
    ui.shareClipboard.checked =
      config?.share_clipboard !== undefined
        ? Boolean(config.share_clipboard)
        : true;
  }

  if (ui.keepMouseAlive) {
    ui.keepMouseAlive.checked = Boolean(config?.keep_mouse_alive);
  }

  if (ui.releaseTapKey) {
    ui.releaseTapKey.value = config?.release_tap_key ?? "escape";
  }

  if (ui.autoAcceptFiles) {
    ui.autoAcceptFiles.checked = isAutoAcceptFilesEnabled();
  }

  if (renderLayout && isStatusTabActive()) {
    layoutController?.renderLayoutDiagram();
  }

  renderPeerListView(state.peers);
}

function setConfigStatus(message) {
  if (ui.configStatus && ui.configStatus.textContent !== message) {
    ui.configStatus.textContent = message;
  }
}

function appendEmptyPeerMessage(message) {
  const empty = document.createElement("li");
  empty.textContent = message;
  empty.className = "peer-meta";
  ui.peerList.appendChild(empty);
}

function getConfigFromSyncEventPayload(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return null;
  }

  const { config } = payload;
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    return null;
  }

  return config;
}

function getPeersFromEventPayload(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return null;
  }

  return Array.isArray(payload.peers) ? payload.peers : null;
}

function getStatusFromEventPayload(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return null;
  }

  const { status } = payload;
  if (!status || typeof status !== "object" || Array.isArray(status)) {
    return null;
  }

  return status;
}

async function setupPeerEventListener() {
  if (peerEventListenerRegistered || typeof listen !== "function") {
    return;
  }

  try {
    const unlisten = await listen("peers-updated", (event) => {
      const peers = getPeersFromEventPayload(event?.payload);
      if (!peers) {
        return;
      }

      state.peerRevision += 1;
      renderPeerListView(peers);
      clearError();
      if (isStatusTabActive() && !state.layoutDrag) {
        layoutController?.renderLayoutDiagram();
      }
    });
    eventUnlisteners.set("peers-updated", unlisten);
    peerEventListenerRegistered = true;
  } catch (error) {
    console.warn("[GUI] Failed to register peers-updated listener", error);
  }
}

function markConfigEdited() {
  state.configRevision += 1;
  state.configDirty = true;
}

async function setupStatusEventListener() {
  if (statusEventListenerRegistered || typeof listen !== "function") {
    return;
  }

  try {
    const unlisten = await listen("status-updated", (event) => {
      const status = getStatusFromEventPayload(event?.payload);
      if (!status) {
        return;
      }

      renderStatus(status);
      clearError();
    });
    eventUnlisteners.set("status-updated", unlisten);
    statusEventListenerRegistered = true;
  } catch (error) {
    console.warn("[GUI] Failed to register status-updated listener", error);
  }
}

async function setupConfigSyncListener() {
  if (configSyncListenerRegistered) {
    return;
  }

  if (typeof listen !== "function") {
    console.warn(
      "[GUI] Event listener API unavailable; config sync will not work",
    );
    return;
  }

  try {
    const unlisten = await listen("config-sync-received", (event) => {
      const config = getConfigFromSyncEventPayload(event?.payload);
      if (!config) {
        return;
      }

      if (
        state.configDirty ||
        state.layoutDrag ||
        state.configSaveQueue?.isBusy()
      ) {
        state.pendingRemoteConfig = config;
        setConfigStatus("Remote layout update waiting for local changes.");
        return;
      }

      renderConfig(config);
    });
    eventUnlisteners.set("config-sync-received", unlisten);
    configSyncListenerRegistered = true;
  } catch (error) {
    console.warn("[GUI] Failed to register config-sync listener", error);
  }
}

async function setupPairingEventListener() {
  if (pairingEventListenerRegistered) {
    return;
  }

  if (typeof listen !== "function") {
    return;
  }

  try {
    const unlistenReq = await listen("pairing-requested", (event) => {
      const payload = event?.payload;
      if (!payload?.peer_id) {
        return;
      }
      showPairingDialog(
        payload.peer_id,
        payload.name,
        payload.platform,
        payload.code,
        payload.can_respond,
      );
    });
    eventUnlisteners.set("pairing-requested", unlistenReq);

    const unlistenRes = await listen("pairing-resolved", (event) => {
      const payload = event?.payload;
      if (payload?.peer_id && payload.peer_id === currentPairingPeerId) {
        hidePairingDialog();
      }
    });
    eventUnlisteners.set("pairing-resolved", unlistenRes);
    pairingEventListenerRegistered = true;
  } catch (error) {
    console.warn("[GUI] Failed to register pairing listeners", error);
  }
}

function showPairingDialog(peerId, name, platform, code, canRespond) {
  currentPairingPeerId = peerId;
  currentPairingCanRespond = canRespond !== false;
  const dialog = ui.pairingDialog ?? document.querySelector("#pairing-dialog");
  const descEl = ui.pairingDesc ?? document.querySelector("#pairing-desc");
  const nameEl = ui.pairingPeerName ?? document.querySelector("#pairing-peer-name");
  const platformEl = ui.pairingPeerPlatform ?? document.querySelector("#pairing-peer-platform");
  const codeEl = ui.pairingCode ?? document.querySelector("#pairing-peer-code");
  const codeBlockEl =
    ui.pairingCodeBlock ?? document.querySelector("#pairing-peer-code-block");
  const allowEl = ui.btnPairingAllow ?? document.querySelector("#btn-pairing-allow");
  const blockEl = ui.btnPairingBlock ?? document.querySelector("#btn-pairing-block");
  const cancelEl = ui.btnPairingCancel ?? document.querySelector("#btn-pairing-cancel");
  if (!dialog) {
    return;
  }
  const responding = currentPairingCanRespond;
  if (descEl) {
    descEl.textContent = responding
      ? "A device wants to share mouse and keyboard control with this computer. Compare the code below with the other device before allowing."
      : "Waiting for the other device to approve. Compare the code below with the other device. You can cancel the request below.";
  }
  if (allowEl) {
    allowEl.hidden = !responding;
  }
  if (blockEl) {
    blockEl.hidden = !responding;
  }
  if (cancelEl) {
    cancelEl.hidden = responding;
  }
  if (nameEl) {
    nameEl.textContent = name || peerId;
  }
  if (platformEl) {
    const formatted = formatPeerPlatformLabel(platform);
    platformEl.textContent = formatted || "";
    platformEl.style.display = formatted ? "inline-block" : "none";
  }
  if (codeEl && codeBlockEl) {
    const formatted = formatPairingCode(code);
    codeEl.textContent = formatted;
    codeBlockEl.hidden = !formatted;
  }
  // Both inbound and outbound waits are modal so the background blurs
  // and dims. Outbound still offers Cancel; inbound offers Allow/Block.
  if (typeof dialog.showModal === "function") {
    if (!dialog.open) {
      try {
        dialog.showModal();
      } catch (error) {
        // showModal throws when the document is no longer active (for example
        // while the app is closing); fall back to a plain open attribute.
        console.warn("[GUI] Failed to open pairing dialog", error);
        dialog.setAttribute("open", "");
      }
    }
  } else {
    dialog.setAttribute("open", "");
  }
}

function hidePairingDialog() {
  currentPairingPeerId = null;
  currentPairingCanRespond = true;
  const dialog = ui.pairingDialog ?? document.querySelector("#pairing-dialog");
  if (!dialog) {
    return;
  }
  if (typeof dialog.close === "function" && dialog.open) {
    try {
      dialog.close();
    } catch (error) {
      console.warn("[GUI] Failed to close pairing dialog", error);
      dialog.removeAttribute("open");
    }
  } else {
    dialog.removeAttribute("open");
  }
}

async function handleApprovePairing() {
  if (!currentPairingPeerId) {
    return;
  }
  const peerId = currentPairingPeerId;
  hidePairingDialog();
  try {
    const status = await invokeTimed("approve_pairing", { peerId });
    if (status) {
      renderStatus(status);
    }
  } catch (error) {
    showError(error?.message ?? String(error));
  }
}

async function handleRejectPairing() {
  if (!currentPairingPeerId) {
    return;
  }
  const peerId = currentPairingPeerId;
  hidePairingDialog();
  try {
    const status = await invokeTimed("reject_pairing", { peerId });
    if (status) {
      renderStatus(status);
    }
  } catch (error) {
    showError(error?.message ?? String(error));
  }
}

async function handleCancelPairing() {
  if (!currentPairingPeerId) {
    return;
  }
  const peerId = currentPairingPeerId;
  hidePairingDialog();
  await handleCancelPeer(peerId);
}

function handlePairingDialogCancel(event) {
  // Esc on an outbound wait cancels the request; Esc on an inbound request
  // is treated as Block so the pending connection does not linger.
  if (!currentPairingPeerId) {
    return;
  }
  event?.preventDefault?.();
  const peerId = currentPairingPeerId;
  const wasOutbound = !currentPairingCanRespond;
  hidePairingDialog();
  if (wasOutbound) {
    handleCancelPeer(peerId);
  } else {
    invokeTimed("reject_pairing", { peerId })
      .then((status) => {
        if (status) {
          renderStatus(status);
        }
      })
      .catch((error) => showError(error?.message ?? String(error)));
  }
}

function isAutoAcceptFilesEnabled() {
  try {
    return window.localStorage?.getItem(AUTO_ACCEPT_FILES_KEY) === "1";
  } catch {
    return false;
  }
}

function setAutoAcceptFilesEnabled(enabled) {
  try {
    window.localStorage?.setItem(AUTO_ACCEPT_FILES_KEY, enabled ? "1" : "0");
  } catch {
  }
}

function formatBytes(bytes) {
  if (typeof bytes !== "number" || bytes <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  return `${(bytes / Math.pow(1024, i)).toFixed(i > 0 ? 1 : 0)} ${units[i]}`;
}

function formatTransferRate(bytesPerSecond) {
  if (bytesPerSecond === null) {
    return "Calculating speed…";
  }

  const kilobytesPerSecond = bytesPerSecond / 1024;
  if (kilobytesPerSecond >= 1024) {
    return `${(kilobytesPerSecond / 1024).toFixed(1)} MB/s`;
  }
  if (kilobytesPerSecond >= 1) {
    return `${kilobytesPerSecond.toFixed(1)} KB/s`;
  }
  return "<1 KB/s";
}

async function openTransferLocation(savedPath, fileCount) {
  const opener = window.__TAURI__.opener;
  if (fileCount > 1) {
    await opener.openPath(savedPath);
    return;
  }
  await opener.revealItemInDir(savedPath);
}

function createTransferCenterItem(transfer) {
  const item = document.createElement("article");
  item.className = "transfers-item";

  const heading = document.createElement("div");
  heading.className = "transfers-item-heading";

  const name = document.createElement("span");
  name.className = "transfers-item-name";
  name.textContent = transfer.file_label ?? "Files";
  heading.appendChild(name);

  const status = document.createElement("span");
  status.className = "transfers-item-status";
  if (transfer.is_complete) {
    status.textContent = transfer.success ? "Complete" : "Failed";
  } else {
    status.textContent = `${Math.round((transfer.percent ?? 0) * 100)}%`;
  }
  heading.appendChild(status);
  item.appendChild(heading);

  const details = document.createElement("span");
  details.className = "transfers-item-details";
  if (transfer.is_complete) {
    if (!transfer.success) {
      details.textContent = transfer.error ?? "Transfer failed";
    } else if (transfer.is_outgoing) {
      details.textContent = "Sent";
    } else {
      details.textContent = "Received";
    }
  } else {
    const direction = transfer.is_outgoing ? "Sending" : "Receiving";
    const bytes = formatBytes(transfer.bytes_transferred ?? 0);
    const total = formatBytes(transfer.total_bytes ?? 0);
    const speed = formatTransferRate(transfer.bytes_per_second ?? null);
    details.textContent = `${direction} · ${bytes} of ${total} · ${speed}`;
  }
  item.appendChild(details);

  if (!transfer.is_complete) {
    const progress = document.createElement("div");
    progress.className = "transfers-item-progress";
    const fill = document.createElement("div");
    fill.className = "transfers-item-progress-fill";
    const percent = Math.max(0, Math.min(100, Math.round((transfer.percent ?? 0) * 100)));
    fill.style.width = `${percent}%`;
    progress.appendChild(fill);
    item.appendChild(progress);
  }

  if (transfer.is_complete && transfer.success && transfer.saved_path) {
    const openButton = document.createElement("button");
    openButton.type = "button";
    openButton.className = "btn btn-primary transfers-item-open";
    openButton.textContent = "Open folder";
    openButton.addEventListener("click", async () => {
      try {
        await openTransferLocation(transfer.saved_path, transfer.file_count ?? 1);
      } catch (error) {
        showError(error?.message ?? String(error));
      }
    });
    item.appendChild(openButton);
  }

  return item;
}

function renderTransfersPanel() {
  const uniqueTransfers = new Map();
  for (const transfer of state.activeTransfers.values()) {
    uniqueTransfers.set(transfer.transfer_id, transfer);
  }
  const activeTransfers = Array.from(uniqueTransfers.values());
  if (ui.transfersCount) {
    ui.transfersCount.hidden = activeTransfers.length === 0;
    ui.transfersCount.textContent = activeTransfers.length > 0 ? String(activeTransfers.length) : "";
  }
  if (ui.transfersButton) {
    ui.transfersButton.setAttribute(
      "aria-label",
      activeTransfers.length > 0 ? `Transfers, ${activeTransfers.length} active` : "Transfers",
    );
  }

  if (!isTransfersTabActive()) {
    return;
  }

  if (ui.activeTransfersList && ui.activeTransfersEmpty) {
    ui.activeTransfersList.replaceChildren();
    ui.activeTransfersEmpty.hidden = activeTransfers.length > 0;
    activeTransfers.forEach((transfer) => {
      ui.activeTransfersList.appendChild(createTransferCenterItem(transfer));
    });
  }

  if (ui.recentTransfersList && ui.recentTransfersEmpty) {
    ui.recentTransfersList.replaceChildren();
    ui.recentTransfersEmpty.hidden = state.completedTransfers.length > 0;
    state.completedTransfers.forEach((transfer) => {
      ui.recentTransfersList.appendChild(createTransferCenterItem(transfer));
    });
  }
}

function updateTaskbarProgress(percent) {
  try {
    const win = typeof getCurrentWindow === "function" ? getCurrentWindow() : null;
    if (win && typeof win.setProgressBar === "function") {
      if (typeof percent === "number" && percent >= 0 && percent < 1) {
        win.setProgressBar({ status: "Normal", progress: Math.round(percent * 100) });
      } else {
        win.setProgressBar({ status: "None" });
      }
    }
  } catch {
  }
}

function findPeerAtPoint(clientX, clientY) {
  if (typeof document.elementFromPoint === "function") {
    const el = document.elementFromPoint(clientX, clientY);
    const node = el?.closest?.("[data-peer-id]");
    if (node) {
      const peerId = node.getAttribute("data-peer-id");
      if (peerId) {
        return peerId.trim();
      }
    }
  }

  if (ui.layoutSvg && state.layoutRenderCache.placements) {
    const svgPoint = clientToSvgPoint(ui.layoutSvg, clientX, clientY);
    for (const [peerId, item] of state.layoutRenderCache.placements.entries()) {
      const hw = (item.size?.width ?? 120) / 2;
      const hh = (item.size?.height ?? 80) / 2;
      if (
        svgPoint.x >= item.x - hw &&
        svgPoint.x <= item.x + hw &&
        svgPoint.y >= item.y - hh &&
        svgPoint.y <= item.y + hh
      ) {
        return peerId;
      }
    }
  }
  return null;
}

function showTransferToast(options, toastContainer = ui.transferToastContainer) {
  const container = toastContainer ?? document.querySelector("#transfer-toast-container");
  if (!container) {
    return null;
  }

  let toast = container.querySelector(`[data-transfer-id="${options.id}"]`);
  if (!toast) {
    toast = document.createElement("div");
    toast.className = "transfer-toast";
    toast.setAttribute("data-transfer-id", options.id);
    container.appendChild(toast);
  }

  toast.classList.toggle("transfer-toast--success", options.success === true);
  toast.innerHTML = "";

  const content = document.createElement("div");
  content.className = "transfer-toast-content";

  const titleEl = document.createElement("span");
  titleEl.className = "transfer-toast-title";
  titleEl.textContent = options.title ?? "File Transfer";
  content.appendChild(titleEl);

  if (options.desc) {
    const descEl = document.createElement("span");
    descEl.className = "transfer-toast-desc";
    descEl.textContent = options.desc;
    content.appendChild(descEl);
  }

  if (options.showProgress) {
    const progressEl = document.createElement("div");
    progressEl.className = "transfer-toast-progress";
    const barEl = document.createElement("div");
    barEl.className = "transfer-toast-bar";
    const pct = Math.max(0, Math.min(100, Math.round((options.percent ?? 0) * 100)));
    barEl.style.width = `${pct}%`;
    progressEl.appendChild(barEl);
    content.appendChild(progressEl);
  }

  toast.appendChild(content);

  if (options.actions && options.actions.length > 0) {
    const actionsEl = document.createElement("div");
    actionsEl.className = "transfer-toast-actions";
    for (const act of options.actions) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `btn ${act.className ?? "btn-primary"}`;
      btn.textContent = act.label;
      btn.addEventListener("click", () => {
        act.onClick(toast);
      });
      actionsEl.appendChild(btn);
    }
    toast.appendChild(actionsEl);
  }

  if (options.autoDismissMs && options.autoDismissMs > 0) {
    window.setTimeout(() => {
      toast.remove();
    }, options.autoDismissMs);
  }

  return toast;
}

async function showTransferNotification(existing, payload) {
  const notification = window.__TAURI__?.notification;
  if (!notification) {
    return;
  }

  const isFocused = document.visibilityState === "visible" && document.hasFocus();

  try {
    let permissionGranted = await notification.isPermissionGranted();
    if (!permissionGranted) {
      permissionGranted = (await notification.requestPermission()) === "granted";
    }
    if (isFocused || !permissionGranted) {
      return;
    }

    const fileName = payload.saved_path?.split(/[\\/]/).pop();
    const body = existing?.is_outgoing
      ? "Files sent successfully."
      : existing?.file_count === 1 && fileName
        ? `Saved ${fileName} to Downloads.`
        : "Files saved to Downloads.";
    await notification.sendNotification({
      title: "Transfer Complete",
      body,
    });
  } catch (error) {
    console.warn("[GUI] Failed to show transfer notification", error);
  }
}

function dismissTransferToast(id) {
  const container = ui.transferToastContainer ?? document.querySelector("#transfer-toast-container");
  const toast = container?.querySelector?.(`[data-transfer-id="${id}"]`);
  if (toast) {
    toast.remove();
  }
}

let lastFileDropTime = 0;
let lastFileDropKey = "";

async function handleFileDrop(paths, position) {
  if (!paths || paths.length === 0) {
    return;
  }

  if (!state.license || state.license.status !== "active") {
    showTransferToast({
      id: "file-transfer-unavailable",
      title: "File Transfer",
      desc: "File transfer is not available in this release.",
      autoDismissMs: 4000,
    });
    return;
  }

  const now = Date.now();
  const dropKey = `${paths.join(":")}@${position?.x ?? 0},${position?.y ?? 0}`;
  if (now - lastFileDropTime < 800 && lastFileDropKey === dropKey) {
    return;
  }
  lastFileDropTime = now;
  lastFileDropKey = dropKey;

  let targetPeerId = null;
  if (position && typeof position.x === "number" && typeof position.y === "number") {
    const scaleFactor = window.devicePixelRatio || 1;
    targetPeerId = findPeerAtPoint(position.x, position.y);
    if (!targetPeerId && scaleFactor !== 1) {
      targetPeerId = findPeerAtPoint(position.x / scaleFactor, position.y / scaleFactor);
    }
  }

  if (!targetPeerId) {
    const remotePeers = state.peers.filter((p) => {
      return !p.is_local;
    });
    if (remotePeers.length === 1) {
      targetPeerId = remotePeers[0].peer_id;
    }
  }

  if (!targetPeerId) {
    showTransferToast({
      id: "drop-hint",
      title: "File Transfer",
      desc: "Drop files onto a remote display to transfer.",
      autoDismissMs: 3500,
    });
    return;
  }

  const targetPeer = state.peers.find((p) => {
    return p.peer_id === targetPeerId;
  });
  const peerName = targetPeer?.name ?? targetPeerId;
  const count = paths.length;
  const label = count === 1 ? "1 file" : `${count} files`;
  const fileLabel = count === 1 ? paths[0].split(/[\\/]/).pop() : label;

  try {
    const manifest = await invoke("start_file_transfer", {
      paths,
      targetPeerId,
    });
    const transferId = manifest?.transfer_id ?? `tx-${Date.now()}`;
    state.activeTransfers.set(transferId, {
      transfer_id: transferId,
      peer_id: targetPeerId,
      is_outgoing: true,
      file_count: count,
      file_label: fileLabel,
      percent: 0,
    });
    state.activeTransfers.set(targetPeerId, {
      transfer_id: transferId,
      peer_id: targetPeerId,
      is_outgoing: true,
      file_count: count,
      file_label: fileLabel,
      percent: 0,
    });
    layoutController?.renderLayoutDiagram();
    renderTransfersPanel();
    showTransferToast({
      id: transferId,
      title: `Sending ${label}`,
      desc: `Waiting for ${peerName} to accept...`,
      showProgress: true,
      percent: 0,
    });
  } catch (err) {
    showError(unwrapCommandError(err));
  }
}

async function setupFileDropListeners() {
  if (fileDropListenerRegistered) {
    return;
  }

  const onOver = (event) => {
    const pos = event?.payload?.position;
    if (pos && typeof pos.x === "number") {
      const scaleFactor = window.devicePixelRatio || 1;
      let peerId = findPeerAtPoint(pos.x, pos.y);
      if (!peerId && scaleFactor !== 1) {
        peerId = findPeerAtPoint(pos.x / scaleFactor, pos.y / scaleFactor);
      }
      if (peerId !== state.fileDropTargetPeerId) {
        state.fileDropTargetPeerId = peerId;
        layoutController?.renderLayoutDiagram();
      }
    }
  };

  const onLeave = () => {
    if (state.fileDropTargetPeerId !== null) {
      state.fileDropTargetPeerId = null;
      layoutController?.renderLayoutDiagram();
    }
  };

  const onDrop = async (event) => {
    state.fileDropTargetPeerId = null;
    layoutController?.renderLayoutDiagram();
    const payload = event?.payload;
    if (payload?.paths) {
      await handleFileDrop(payload.paths, payload.position);
    }
  };

  if (typeof listen === "function") {
    try {
      const unlistenOver = await listen("tauri://drag-over", onOver);
      eventUnlisteners.set("tauri://drag-over", unlistenOver);
      const unlistenEnter = await listen("tauri://drag-enter", onOver);
      eventUnlisteners.set("tauri://drag-enter", unlistenEnter);
      const unlistenLeave = await listen("tauri://drag-leave", onLeave);
      eventUnlisteners.set("tauri://drag-leave", unlistenLeave);
      const unlistenDrop = await listen("tauri://drag-drop", onDrop);
      eventUnlisteners.set("tauri://drag-drop", unlistenDrop);
      fileDropListenerRegistered = true;
      return;
    } catch (err) {
      console.warn("[GUI] Failed to register drag-drop event listeners", err);
    }
  }

  try {
    const win = typeof getCurrentWindow === "function" ? getCurrentWindow() : null;
    if (win && typeof win.onDragDropEvent === "function") {
      const unlistenWin = await win.onDragDropEvent(async (event) => {
        if (event.payload.type === "over" || event.payload.type === "enter") {
          onOver(event);
        } else if (event.payload.type === "leave") {
          onLeave();
        } else if (event.payload.type === "drop") {
          await onDrop(event);
        }
      });
      eventUnlisteners.set("window-drag-drop", unlistenWin);
      fileDropListenerRegistered = true;
    }
  } catch {
  }
}

async function setupFileTransferListeners() {
  if (fileTransferListenerRegistered || typeof listen !== "function") {
    return;
  }

  try {
    const unlistenOffer = await listen("file-transfer-offer", (event) => {
      const manifest = event?.payload;
      if (!manifest?.transfer_id) {
        return;
      }

      const fileCount = manifest.files?.length ?? 1;
      const fileLabel = fileCount === 1 ? manifest.files?.[0]?.name ?? "1 file" : `${fileCount} files`;
      const sizeLabel = formatBytes(manifest.total_size ?? 0);
      const sender = manifest.sender_name ?? "Remote peer";

      if (isAutoAcceptFilesEnabled()) {
        state.activeTransfers.set(manifest.transfer_id, {
          transfer_id: manifest.transfer_id,
          peer_id: manifest.sender_peer_id,
          is_outgoing: false,
          file_count: manifest.files?.length ?? 1,
          file_label: fileLabel,
          percent: 0,
        });
        if (manifest.sender_peer_id) {
          state.activeTransfers.set(manifest.sender_peer_id, {
            transfer_id: manifest.transfer_id,
            peer_id: manifest.sender_peer_id,
            is_outgoing: false,
            file_count: manifest.files?.length ?? 1,
            file_label: fileLabel,
            percent: 0,
          });
        }
        layoutController?.renderLayoutDiagram();
        renderTransfersPanel();

        invoke("accept_file_transfer", {
          transferId: manifest.transfer_id,
          savePath: null,
        }).catch((err) => {
          showError(unwrapCommandError(err));
        });

        showTransferToast({
          id: manifest.transfer_id,
          title: `Receiving ${fileLabel}`,
          desc: `${sender} (${sizeLabel}) -> Downloads`,
          showProgress: true,
          percent: 0,
        });
      } else {
        showTransferToast({
          id: manifest.transfer_id,
          title: "Incoming File Offer",
          desc: `${sender} wants to send ${fileLabel} (${sizeLabel})`,
          showProgress: false,
          actions: [
            {
              label: "Decline",
              className: "btn--stop",
              onClick: (toast) => {
                toast.remove();
                state.activeTransfers.delete(manifest.transfer_id);
                if (manifest.sender_peer_id) {
                  state.activeTransfers.delete(manifest.sender_peer_id);
                }
                layoutController?.renderLayoutDiagram();
                renderTransfersPanel();
                invoke("reject_file_transfer", {
                  transferId: manifest.transfer_id,
                  reason: "Declined by user",
                }).catch(() => {});
              },
            },
            {
              label: "Accept",
              className: "btn-primary",
              onClick: (toast) => {
                state.activeTransfers.set(manifest.transfer_id, {
                  transfer_id: manifest.transfer_id,
                  peer_id: manifest.sender_peer_id,
                  is_outgoing: false,
                  file_count: manifest.files?.length ?? 1,
                  file_label: fileLabel,
                  percent: 0,
                });
                if (manifest.sender_peer_id) {
                  state.activeTransfers.set(manifest.sender_peer_id, {
                    transfer_id: manifest.transfer_id,
                    peer_id: manifest.sender_peer_id,
                    is_outgoing: false,
                    file_count: manifest.files?.length ?? 1,
                    file_label: fileLabel,
                    percent: 0,
                  });
                }
                layoutController?.renderLayoutDiagram();
                renderTransfersPanel();

                invoke("accept_file_transfer", {
                  transferId: manifest.transfer_id,
                  savePath: null,
                }).catch((err) => {
                  showError(unwrapCommandError(err));
                });
                showTransferToast({
                  id: manifest.transfer_id,
                  title: `Receiving ${fileLabel}`,
                  desc: `${sender} (${sizeLabel}) -> Downloads`,
                  showProgress: true,
                  percent: 0,
                });
              },
            },
          ],
        });
      }
    });
    eventUnlisteners.set("file-transfer-offer", unlistenOffer);

    const unlistenProgress = await listen("file-transfer-progress", (event) => {
      const progress = event?.payload;
      if (!progress?.transfer_id) {
        return;
      }

      const existing = state.activeTransfers.get(progress.transfer_id);
      const peerId = existing?.peer_id;
      const bytesTransferred = progress.bytes_transferred ?? 0;
      const now = performance.now();
      const rateStartedAt = existing?.rate_started_at ?? now;
      const rateStartBytes = existing?.rate_start_bytes ?? bytesTransferred;
      const elapsedSeconds = (now - rateStartedAt) / 1000;
      const bytesPerSecond = elapsedSeconds > 0
        ? (bytesTransferred - rateStartBytes) / elapsedSeconds
        : null;
      const updated = {
        ...(existing ?? {}),
        transfer_id: progress.transfer_id,
        bytes_transferred: bytesTransferred,
        total_bytes: progress.total_bytes,
        percent: progress.percent,
        is_outgoing: progress.is_outgoing,
        rate_started_at: rateStartedAt,
        rate_start_bytes: rateStartBytes,
        bytes_per_second: bytesPerSecond,
      };
      state.activeTransfers.set(progress.transfer_id, updated);
      if (peerId) {
        state.activeTransfers.set(peerId, updated);
      }

      updateTaskbarProgress(progress.percent);
      layoutController?.renderLayoutDiagram();
      renderTransfersPanel();

      const pct = Math.round((progress.percent ?? 0) * 100);
      const title = progress.is_outgoing ? `Sending files (${pct}%)` : `Receiving files (${pct}%)`;
      const desc = `${formatBytes(bytesTransferred)} of ${formatBytes(progress.total_bytes ?? 0)} · ${formatTransferRate(bytesPerSecond)}`;

      showTransferToast({
        id: progress.transfer_id,
        title,
        desc,
        showProgress: true,
        percent: progress.percent,
      });
    });
    eventUnlisteners.set("file-transfer-progress", unlistenProgress);

    const unlistenComplete = await listen("file-transfer-complete", (event) => {
      const payload = event?.payload;
      if (!payload?.transfer_id) {
        return;
      }

      const existing = state.activeTransfers.get(payload.transfer_id);
      const peerId = existing?.peer_id;
      state.completedTransfers.unshift({
        ...(existing ?? {}),
        transfer_id: payload.transfer_id,
        is_complete: true,
        success: payload.success,
        saved_path: payload.saved_path,
        error: payload.error,
      });
      state.completedTransfers = state.completedTransfers.slice(0, MAX_RECENT_TRANSFERS);
      state.activeTransfers.delete(payload.transfer_id);
      if (peerId) {
        state.activeTransfers.delete(peerId);
      }

      updateTaskbarProgress(null);
      layoutController?.renderLayoutDiagram();
      renderTransfersPanel();

      if (payload.success) {
        dismissTransferToast(payload.transfer_id);
        showTransferToast({
          id: payload.transfer_id,
          title: "Transfer complete",
          success: true,
          showProgress: false,
          actions: payload.saved_path
            ? [
                {
                  label: "Open folder",
                  className: "btn--toast-action",
                  onClick: async (toast) => {
                    try {
                      await openTransferLocation(payload.saved_path, existing?.file_count ?? 1);
                      toast.remove();
                    } catch (error) {
                      showError(error?.message ?? String(error));
                    }
                  },
                },
              ]
            : [],
          autoDismissMs: 10000,
        }, ui.transferSnackbarContainer);
        void showTransferNotification(existing, payload);
      } else {
        dismissTransferToast(payload.transfer_id);
        showTransferToast({
          id: payload.transfer_id,
          title: "Transfer Failed",
          desc: payload.error ?? "File transfer was cancelled or failed.",
          showProgress: false,
          autoDismissMs: 6000,
        }, ui.transferSnackbarContainer);
      }
    });
    eventUnlisteners.set("file-transfer-complete", unlistenComplete);

    fileTransferListenerRegistered = true;
  } catch (error) {
    console.warn("[GUI] Failed to register file transfer listeners", error);
  }
}

function cleanupRuntimeEventListeners() {
  eventUnlisteners.forEach((unlisten) => {
    if (typeof unlisten !== "function") {
      return;
    }

    try {
      Promise.resolve(unlisten()).catch((error) => {
        console.warn("[GUI] Failed to remove event listener", error);
      });
    } catch (error) {
      console.warn("[GUI] Failed to remove event listener", error);
    }
  });

  eventUnlisteners.clear();
  hidePairingDialog();
  configSyncListenerRegistered = false;
  peerEventListenerRegistered = false;
  statusEventListenerRegistered = false;
  pairingEventListenerRegistered = false;
  fileTransferListenerRegistered = false;
  fileDropListenerRegistered = false;
  updaterListenerRegistered = false;
  state.activeTransfers.clear();
  state.fileDropTargetPeerId = null;
  updateTaskbarProgress(null);
}

function unwrapCommandError(error) {
  if (error && typeof error === "object" && "message" in error) {
    const code = String(error.code ?? "internal_error");

    if (code === "accessibility_permission_missing") {
      if (isHostMac()) {
        invoke("show_accessibility_helper").catch((helperError) => {
          console.warn("[GUI] Failed to open Accessibility helper", helperError);
        });
      }

      return "ZeroG needs Accessibility access. The helper is open; drag the ZeroG row into Privacy & Security > Accessibility.";
    }

    if (code === "socket_bind_failed") {
      return "Socket bind failed. Another process may already be using the networking port.";
    }

    if (code === "no_peers_discovered") {
      return "No peers discovered. Ensure both machines are on the same LAN and multicast DNS is allowed.";
    }

    if (code === "operation_in_progress") {
      return "A connection attempt is already in progress.";
    }

    if (code === "invalid_config") {
      return "Layout config is invalid. Check peer assignments and retry.";
    }

    if (
      code === "service_install_failed" ||
      code === "service_uninstall_failed" ||
      code === "secure_desktop_failed"
    ) {
      return error.message;
    }

    return `${code}: ${error.message}`;
  }

  return String(error);
}

function isWindowsPlatform() {
  if (typeof navigator === "undefined") {
    return false;
  }
  const platform = (
    navigator.userAgentData?.platform ?? navigator.platform ?? ""
  ).toLowerCase();
  if (platform.startsWith("win")) {
    return true;
  }
  return /windows/i.test(navigator.userAgent ?? "");
}

function renderWindowsInputStatus() {
  if (!ui.windowsInputCard) {
    return;
  }
  const isWin = isWindowsPlatform();
  const status = state.windowsInput;
  if (!isWin || (status && status.supported === false)) {
    ui.windowsInputCard.hidden = true;
    return;
  }
  ui.windowsInputCard.hidden = false;
  if (!status) {
    if (ui.windowsInputStatus) {
      ui.windowsInputStatus.className = "setting-help uac-status uac-status--disabled";
      ui.windowsInputStatus.textContent = state.windowsInputLoading
        ? "Checking status..."
        : "Status is unavailable.";
    }
    return;
  }

  if (ui.windowsInputStatus) {
    if (status.service_running) {
      ui.windowsInputStatus.className = "setting-help uac-status uac-status--active";
      ui.windowsInputStatus.textContent = "Active — remote control works during admin prompts.";
    } else if (status.secure_desktop_enabled === false) {
      ui.windowsInputStatus.className = "setting-help uac-status uac-status--warning";
      ui.windowsInputStatus.textContent = "Standard desktop active (less secure) — remote input works.";
    } else if (status.service_installed) {
      ui.windowsInputStatus.className = "setting-help uac-status uac-status--warning";
      ui.windowsInputStatus.textContent = "Service stopped — prompts will freeze remote input.";
    } else {
      ui.windowsInputStatus.className = "setting-help uac-status uac-status--disabled";
      ui.windowsInputStatus.textContent = "Disabled — admin prompts will freeze remote input.";
    }
  }

  if (ui.btnWinService) {
    ui.btnWinService.textContent = status.service_installed
      ? "Disable UAC Control"
      : "Enable UAC Control";
    ui.btnWinService.className = status.service_installed
      ? "btn btn--win-service"
      : "btn btn-primary btn--win-service";
    ui.btnWinService.disabled = state.windowsInputLoading;
  }
}

async function loadWindowsInputStatus() {
  if (!isWindowsPlatform()) {
    renderWindowsInputStatus();
    return;
  }
  if (state.windowsInputLoading) {
    return;
  }
  state.windowsInputLoading = true;
  renderWindowsInputStatus();
  try {
    state.windowsInput = await invokeTimed("get_windows_input_status");
    clearError();
  } catch (error) {
    showError(unwrapCommandError(error));
  } finally {
    state.windowsInputLoading = false;
    renderWindowsInputStatus();
  }
}

async function handleWinServiceToggle() {
  if (state.windowsInputLoading) {
    return;
  }
  const installed = state.windowsInput?.service_installed ?? false;
  state.windowsInputLoading = true;
  renderWindowsInputStatus();
  try {
    state.windowsInput = await invoke(
      installed ? "uninstall_input_service" : "install_input_service",
    );
    clearError();
  } catch (error) {
    showError(unwrapCommandError(error));
  } finally {
    state.windowsInputLoading = false;
    renderWindowsInputStatus();
    ui.btnWinService?.blur();
  }
}

function renderLicenseStatus() {
  const license = state.license;
  if (!ui.licenseTierBadge || !ui.licenseStatusText) {
    return;
  }

  if (!license || license.status === "free") {
    ui.licenseTierBadge.className = "license-badge license-badge--free";
    ui.licenseTierBadge.textContent = "Personal Free";
    ui.licenseStatusText.textContent = "Free tier: Unlimited devices & input sharing. File transfer & UAC control require Pro.";
    if (ui.btnActivateLicense) {
      ui.btnActivateLicense.textContent = "Activate Pro";
    }
    if (ui.btnRemoveLicense) {
      ui.btnRemoveLicense.hidden = true;
    }
    return;
  }

  if (license.status === "active") {
    ui.licenseTierBadge.className = "license-badge license-badge--pro";
    ui.licenseTierBadge.textContent = "Pro Active";
    const licensee = license.licensee ? `Licensed to ${license.licensee}` : "Pro License";
    const expiry = license.expires_at ? ` (Expires: ${new Date(license.expires_at * 1000).toLocaleDateString()})` : " (Perpetual)";
    ui.licenseStatusText.textContent = `${licensee}${expiry} — All features unlocked.`;
    if (ui.btnActivateLicense) {
      ui.btnActivateLicense.textContent = "Change Key";
    }
    if (ui.btnRemoveLicense) {
      ui.btnRemoveLicense.hidden = false;
    }
    return;
  }

  if (license.status === "expired") {
    ui.licenseTierBadge.className = "license-badge license-badge--expired";
    ui.licenseTierBadge.textContent = "Pro Expired";
    ui.licenseStatusText.textContent = `License for ${license.licensee || "user"} expired. Renew or activate a new key.`;
    if (ui.btnActivateLicense) {
      ui.btnActivateLicense.textContent = "Renew Pro";
    }
    if (ui.btnRemoveLicense) {
      ui.btnRemoveLicense.hidden = false;
    }
    return;
  }

  ui.licenseTierBadge.className = "license-badge license-badge--free";
  ui.licenseTierBadge.textContent = "Personal Free";
  ui.licenseStatusText.textContent = "Free tier: Unlimited devices & input sharing. File transfer & UAC control require Pro.";
}

async function loadLicenseStatus() {
  if (state.licenseLoading) {
    return;
  }
  state.licenseLoading = true;
  try {
    state.license = await invokeTimed("get_license_status");
  } catch (error) {
    showError(unwrapCommandError(error));
  } finally {
    state.licenseLoading = false;
    renderLicenseStatus();
  }
}

function openLicenseDialog() {
  if (!ui.licenseDialog) {
    return;
  }
  if (ui.licenseKeyInput) {
    ui.licenseKeyInput.value = "";
  }
  if (ui.licenseModalError) {
    ui.licenseModalError.hidden = true;
    ui.licenseModalError.textContent = "";
  }
  ui.licenseDialog.showModal();
}

function closeLicenseDialog() {
  if (ui.licenseDialog) {
    ui.licenseDialog.close();
  }
}

async function handleLicenseSubmit() {
  const key = ui.licenseKeyInput ? ui.licenseKeyInput.value.trim() : "";
  if (!key) {
    if (ui.licenseModalError) {
      ui.licenseModalError.textContent = "Please enter a license key.";
      ui.licenseModalError.hidden = false;
    }
    return;
  }

  if (ui.btnLicenseSubmit) {
    ui.btnLicenseSubmit.disabled = true;
  }
  try {
    const updated = await invokeTimed("set_license_key", { licenseKey: key });
    state.license = updated;
    renderLicenseStatus();
    closeLicenseDialog();
    clearError();
  } catch (error) {
    const unwrapped = unwrapCommandError(error);
    if (ui.licenseModalError) {
      ui.licenseModalError.textContent = unwrapped;
      ui.licenseModalError.hidden = false;
    }
  } finally {
    if (ui.btnLicenseSubmit) {
      ui.btnLicenseSubmit.disabled = false;
    }
  }
}

async function handleLicenseRemove() {
  if (state.licenseLoading) {
    return;
  }
  state.licenseLoading = true;
  try {
    state.license = await invokeTimed("remove_license_key");
    clearError();
  } catch (error) {
    showError(unwrapCommandError(error));
  } finally {
    state.licenseLoading = false;
    renderLicenseStatus();
  }
}

let updaterListenerRegistered = false;

async function setupUpdaterListeners() {
  if (updaterListenerRegistered) {
    return;
  }

  try {
    const unlistenProgress = await listen("updater-download-progress", (event) => {
      const payload = event?.payload;
      if (!payload) {
        return;
      }
      state.updater.downloadedBytes += Number(payload.chunk_length) || 0;
      const total = Number(payload.content_length) || 0;
      if (total > 0) {
        const pct = Math.min(100, Math.round((state.updater.downloadedBytes / total) * 100));
        if (ui.updaterProgressBar) {
          ui.updaterProgressBar.style.width = `${pct}%`;
        }
        if (ui.updaterStatusText) {
          ui.updaterStatusText.textContent = `Downloading update... ${pct}%`;
        }
      } else {
        const mb = (state.updater.downloadedBytes / (1024 * 1024)).toFixed(1);
        if (ui.updaterStatusText) {
          ui.updaterStatusText.textContent = `Downloading update... ${mb} MB`;
        }
      }
    });

    eventUnlisteners.set("updater-download-progress", unlistenProgress);
    updaterListenerRegistered = true;
  } catch (error) {
    console.warn("[GUI] Failed to register updater listeners", error);
  }
}

async function checkForUpdates({ silent = false } = {}) {
  if (state.updater.checking || state.updater.downloading) {
    return;
  }

  state.updater.checking = true;
  if (!silent && ui.updaterStatusText) {
    ui.updaterStatusText.textContent = "Checking for updates...";
  }
  if (ui.btnCheckUpdates) {
    ui.btnCheckUpdates.disabled = true;
    ui.btnCheckUpdates.textContent = "Checking...";
  }

  try {
    const res = await invokeTimed("check_for_updates");
    state.updater.updateInfo = res;
    state.updater.updateAvailable = Boolean(res?.available);

    if (res?.available) {
      const v = res.version ?? "new version";
      if (ui.updaterStatusText) {
        ui.updaterStatusText.textContent = `Update available: ${v}`;
      }
      if (ui.btnInstallUpdate) {
        ui.btnInstallUpdate.hidden = false;
        ui.btnInstallUpdate.disabled = false;
      }
      if (ui.settingsUpdateBadge) {
        ui.settingsUpdateBadge.hidden = false;
      }

      if (state.updater.autoUpdateEnabled) {
        handleInstallUpdate({ restart: false }).catch(() => {});
      } else {
        if (ui.updateBanner && !state.updater.bannerDismissed) {
          if (ui.updateBannerText) {
            ui.updateBannerText.textContent = `ZeroG ${v} is available.`;
          }
          if (ui.btnBannerUpdate) {
            ui.btnBannerUpdate.textContent = "Update & Restart";
          }
          ui.updateBanner.hidden = false;
        }
      }
    } else {
      const cur = res?.current_version ? ` v${res.current_version}` : "";
      if (ui.updaterStatusText) {
        ui.updaterStatusText.textContent = `ZeroG${cur} is up to date.`;
      }
      if (res?.current_version && ui.aboutAppVersion) {
        ui.aboutAppVersion.textContent = `v${res.current_version}`;
      }
      if (ui.btnInstallUpdate) {
        ui.btnInstallUpdate.hidden = true;
      }
      if (ui.settingsUpdateBadge) {
        ui.settingsUpdateBadge.hidden = true;
      }
      if (ui.updateBanner) {
        ui.updateBanner.hidden = true;
      }
    }
  } catch (error) {
    if (!silent) {
      const msg = error?.message || error || "Failed to check for updates.";
      if (ui.updaterStatusText) {
        ui.updaterStatusText.textContent = `Update check failed: ${msg}`;
      }
    }
  } finally {
    state.updater.checking = false;
    if (ui.btnCheckUpdates) {
      ui.btnCheckUpdates.disabled = false;
      ui.btnCheckUpdates.textContent = "Check for Updates";
    }
  }
}

async function handleInstallUpdate(options) {
  const restart = options?.restart ?? true;
  if (state.updater.updateReady) {
    if (ui.updaterStatusText) {
      ui.updaterStatusText.textContent = "Restarting ZeroG...";
    }
    await invokeTimed("restart_app");
    return;
  }

  if (state.updater.downloading) {
    return;
  }

  state.updater.downloading = true;
  state.updater.downloadedBytes = 0;

  if (ui.updateBanner) {
    ui.updateBanner.hidden = true;
  }

  if (ui.btnInstallUpdate) {
    ui.btnInstallUpdate.disabled = true;
    ui.btnInstallUpdate.textContent = restart ? "Installing..." : "Downloading...";
  }
  if (ui.btnCheckUpdates) {
    ui.btnCheckUpdates.disabled = true;
  }
  if (ui.updaterProgressContainer) {
    ui.updaterProgressContainer.hidden = false;
  }
  if (ui.updaterProgressBar) {
    ui.updaterProgressBar.style.width = "0%";
  }
  if (ui.updaterStatusText) {
    ui.updaterStatusText.textContent = "Downloading update...";
  }

  try {
    await invokeTimed("install_update", { restart });
    if (restart) {
      if (ui.updaterStatusText) {
        ui.updaterStatusText.textContent = "Update complete. Restarting ZeroG...";
      }
    } else {
      state.updater.updateReady = true;
      state.updater.downloading = false;
      const v = state.updater.updateInfo?.version ? ` v${state.updater.updateInfo.version}` : "";
      if (ui.updaterStatusText) {
        ui.updaterStatusText.textContent = `ZeroG${v} is ready. Restart to apply.`;
      }
      if (ui.btnInstallUpdate) {
        ui.btnInstallUpdate.hidden = false;
        ui.btnInstallUpdate.disabled = false;
        ui.btnInstallUpdate.textContent = "Restart Now";
      }
      if (ui.btnCheckUpdates) {
        ui.btnCheckUpdates.disabled = false;
      }
      if (ui.updaterProgressContainer) {
        ui.updaterProgressContainer.hidden = true;
      }
      if (ui.settingsUpdateBadge) {
        ui.settingsUpdateBadge.hidden = false;
      }
      if (ui.updateBanner && !state.updater.bannerDismissed) {
        if (ui.updateBannerText) {
          ui.updateBannerText.textContent = `ZeroG${v} is ready to apply.`;
        }
        if (ui.btnBannerUpdate) {
          ui.btnBannerUpdate.textContent = "Restart Now";
        }
        ui.updateBanner.hidden = false;
      }
    }
  } catch (error) {
    state.updater.downloading = false;
    if (ui.btnInstallUpdate) {
      ui.btnInstallUpdate.disabled = false;
      ui.btnInstallUpdate.textContent = "Install & Restart";
    }
    if (ui.btnCheckUpdates) {
      ui.btnCheckUpdates.disabled = false;
    }
    if (ui.updaterProgressContainer) {
      ui.updaterProgressContainer.hidden = true;
    }
    const msg = error?.message || error || "Failed to install update.";
    if (ui.updaterStatusText) {
      ui.updaterStatusText.textContent = `Installation failed: ${msg}`;
    }
  }
}

async function loadStatusSnapshot() {
  const status = await invokeTimed("get_status");
  renderStatus(status);
}

async function loadPeersSnapshot() {
  const revision = ++state.peerRevision;
  const peers = await invokeTimed("get_peers_cached");
  if (revision === state.peerRevision) {
    renderPeerListView(peers);
    if (isStatusTabActive() && !state.layoutDrag) {
      layoutController?.renderLayoutDiagram();
    }
  }
}

async function loadConfigSnapshot() {
  if (!state.configDirty) {
    const config = await invokeTimed("get_config");
    renderConfig(config, { renderLayout: false });
  }

  if (isStatusTabActive()) {
    await new Promise((resolve) => {
      setTimeout(resolve, 0);
    });
    if (isStatusTabActive()) {
      layoutController?.renderLayoutDiagram();
    }
  }
}

async function loadInitialState() {
  const results = await Promise.allSettled([
    loadStatusSnapshot(),
    loadPeersSnapshot(),
    loadConfigSnapshot(),
    loadWindowsInputStatus(),
    loadLicenseStatus(),
  ]);
  const failed = results.find((result) => result.status === "rejected");

  if (failed) {
    showError(unwrapCommandError(failed.reason));
  } else {
    clearError();
  }
}

async function handleEngineToggle() {
  if (state.meshActionInProgress) {
    return;
  }

  const isRunning = state.status?.engine_running ?? false;
  state.meshActionInProgress = true;
  updateEngineToggleButton();

  try {
    const nextStatus = await invoke(isRunning ? "stop_engine" : "start_engine");
    renderStatus(nextStatus);
    await loadPeersSnapshot();
    clearError();
  } catch (error) {
    showError(unwrapCommandError(error));
  } finally {
    state.meshActionInProgress = false;
    updateEngineToggleButton();
    ui.engineToggle?.blur();
    document.activeElement?.blur();
  }
}

async function handleEngineRestart() {
  if (state.meshActionInProgress) {
    return;
  }

  state.meshActionInProgress = true;
  state.engineRestartInProgress = true;
  updateEngineToggleButton();

  try {
    const nextStatus = await invoke("restart_engine");
    renderStatus(nextStatus);
    await loadPeersSnapshot();
    clearError();
  } catch (error) {
    showError(unwrapCommandError(error));
  } finally {
    state.engineRestartInProgress = false;
    state.meshActionInProgress = false;
    updateEngineToggleButton();
    ui.engineRestart?.blur();
    document.activeElement?.blur();
  }
}

function requestConfigSave() {
  if (!layoutController || !state.configSaveQueue) {
    return;
  }

  state.configSaveQueue.enqueue(
    state.configRevision,
    layoutController.buildConfigPayload(),
  );
}

// Commits a pending debounced layout edit immediately. Called when the window
// is hidden or closing so edits made just before quitting are not dropped by
// the debounce timer.
function flushPendingConfigSave() {
  layoutController?.flushConfigSaveDebounce();
}

function handleConfigSaved(nextConfig, request) {
  if (request.revision !== state.configRevision) {
    return;
  }

  renderConfig(nextConfig);
  state.pendingRemoteConfig = null;
  clearError();
}

function handleConfigSaveError(error) {
  setConfigStatus("");
  showError(unwrapCommandError(error));
}

function applyPendingRemoteConfigIfSafe() {
  if (
    state.pendingRemoteConfig === null ||
    state.configDirty ||
    state.layoutDrag ||
    state.configSaveQueue?.isBusy()
  ) {
    return;
  }

  const config = state.pendingRemoteConfig;
  state.pendingRemoteConfig = null;
  renderConfig(config);
  setConfigStatus("Remote layout updated.");
}

function switchTab(targetTabId) {
  state.activeTab = targetTabId;

  ui.tabButtons.forEach((button) => {
    const isActive = button.dataset.tab === targetTabId;
    button.classList.toggle("tab-btn--active", isActive);
  });

  ui.tabPanes.forEach((pane) => {
    const isActive = pane.id === targetTabId;
    pane.classList.toggle("tab-pane--active", isActive);
  });
}

function isStatusTabActive() {
  return state.activeTab === "tab-main";
}

function isConfigTabActive() {
  return state.activeTab === "tab-config";
}

function isTransfersTabActive() {
  return state.activeTab === "tab-transfers";
}

function replaceEventHandler(target, eventName, handlerKey, handler) {
  if (!target) {
    return;
  }

  const previousHandler = target[handlerKey];
  if (typeof previousHandler === "function") {
    target.removeEventListener(eventName, previousHandler);
  }

  target[handlerKey] = handler;
  target.addEventListener(eventName, handler);
}

function bindActions() {
  const windowHandle = getCurrentWindow?.();
  const openExternalUrl = (event) => {
    event.preventDefault();
    invoke("open_external_url", { url: event.currentTarget.href }).catch((error) => {
      console.error("Failed to open external URL:", error);
    });
  };

  replaceEventHandler(
    document.querySelector("#about-publisher-link"),
    "click",
    "__mouseThingAboutPublisherLinkHandler",
    openExternalUrl,
  );

  replaceEventHandler(
    document.querySelector("#about-support-link"),
    "click",
    "__mouseThingAboutSupportLinkHandler",
    openExternalUrl,
  );

  document.querySelectorAll("[data-window-action]").forEach((control) => {
    const action = control.dataset.windowAction;
    replaceEventHandler(
      control,
      "click",
      "__mouseThingWindowControlHandler",
      () => windowHandle?.[action]?.(),
    );
  });


  replaceEventHandler(
    ui.configForm,
    "submit",
    "__mouseThingConfigSubmitHandler",
    (event) => {
      event.preventDefault();
    },
  );

  replaceEventHandler(
    ui.layoutSvg,
    "pointerdown",
    "__mouseThingPointerDownHandler",
    layoutController.beginLayoutDrag,
  );
  replaceEventHandler(
    ui.layoutSvg,
    "pointermove",
    "__mouseThingPointerMoveHandler",
    layoutController.continueLayoutDrag,
  );
  replaceEventHandler(
    ui.layoutSvg,
    "pointerup",
    "__mouseThingPointerUpHandler",
    layoutController.endLayoutDrag,
  );
  replaceEventHandler(
    ui.layoutSvg,
    "pointercancel",
    "__mouseThingPointerCancelHandler",
    layoutController.endLayoutDrag,
  );

  if (ui.engineRestart) {
    replaceEventHandler(
      ui.engineRestart,
      "click",
      "__mouseThingEngineRestartHandler",
      handleEngineRestart,
    );
  }

  if (ui.engineToggle) {
    replaceEventHandler(
      ui.engineToggle,
      "click",
      "__mouseThingEngineToggleHandler",
      handleEngineToggle,
    );
  }

  if (ui.shareClipboard) {
    replaceEventHandler(
      ui.shareClipboard,
      "change",
      "__mouseThingShareClipboardChangeHandler",
      (event) => {
        if (!state.config) {
          state.config = {};
        }
        state.config.share_clipboard = event.target.checked;
        markConfigEdited();
        requestConfigSave();
      },
    );
  }

  if (ui.keepMouseAlive) {
    replaceEventHandler(
      ui.keepMouseAlive,
      "change",
      "__mouseThingKeepMouseAliveChangeHandler",
      (event) => {
        if (!state.config) {
          state.config = {};
        }
        state.config.keep_mouse_alive = event.target.checked;
        markConfigEdited();
        requestConfigSave();
      },
    );
  }

  if (ui.releaseTapKey) {
    replaceEventHandler(
      ui.releaseTapKey,
      "change",
      "__mouseThingReleaseTapKeyChangeHandler",
      (event) => {
        if (!state.config) {
          state.config = {};
        }
        state.config.release_tap_key = event.target.value;
        markConfigEdited();
        requestConfigSave();
        event.target.blur();
      },
    );
  }

  if (ui.autoAcceptFiles) {
    replaceEventHandler(
      ui.autoAcceptFiles,
      "change",
      "__mouseThingAutoAcceptFilesChangeHandler",
      (event) => {
        setAutoAcceptFilesEnabled(event.target.checked);
      },
    );
  }

  ui.tabButtons.forEach((button) => {
    replaceEventHandler(button, "click", "__mouseThingTabClickHandler", () => {
      const clickStartedAt = performance.now();
      const targetTab = button.dataset.tab;
      switchTab(targetTab);
      const handlerDurationMs = performance.now() - clickStartedAt;

      if (handlerDurationMs >= PERF_WARN_TAB_SWITCH_HANDLER_MS) {
        perfLog("warn", `tab-switch handler took ${handlerDurationMs.toFixed(1)}ms`, {
          handlerDurationMs,
          targetTab,
        });
      } else {
        perfLog("debug", `tab-switch handler took ${handlerDurationMs.toFixed(1)}ms`, {
          targetTab,
        });
      }

      window.requestAnimationFrame(() => {
        const clickToPaintMs = performance.now() - clickStartedAt;
        if (clickToPaintMs >= PERF_WARN_TAB_SWITCH_PAINT_MS) {
          perfLog("warn", `tab click->paint took ${clickToPaintMs.toFixed(1)}ms`, {
            clickToPaintMs,
            targetTab,
          });
        } else {
          perfLog("debug", `tab click->paint took ${clickToPaintMs.toFixed(1)}ms`, {
            targetTab,
          });
        }
      });

      if (targetTab === "tab-config") {
        if (state.config) {
          renderConfig(state.config, { renderLayout: false });
        }
      }

      if (targetTab === "tab-transfers") {
        renderTransfersPanel();
      }

      if (targetTab === "tab-main") {
        loadPeersSnapshot();
        renderPeerListView(state.peers);
        if (state.config) {
          layoutController?.renderLayoutDiagram();
        }
      }
    });
  });

  if (ui.btnPairingAllow) {
    replaceEventHandler(
      ui.btnPairingAllow,
      "click",
      "__mouseThingPairingAllowHandler",
      handleApprovePairing,
    );
  }

  if (ui.btnWinService) {
    replaceEventHandler(
      ui.btnWinService,
      "click",
      "__mouseThingWinServiceHandler",
      handleWinServiceToggle,
    );
  }

  if (ui.btnPairingBlock) {
    replaceEventHandler(
      ui.btnPairingBlock,
      "click",
      "__mouseThingPairingBlockHandler",
      handleRejectPairing,
    );
  }

  if (ui.btnPairingCancel) {
    replaceEventHandler(
      ui.btnPairingCancel,
      "click",
      "__mouseThingPairingCancelHandler",
      handleCancelPairing,
    );
  }

  if (ui.pairingDialog) {
    replaceEventHandler(
      ui.pairingDialog,
      "cancel",
      "__mouseThingPairingDialogCancelHandler",
      handlePairingDialogCancel,
    );
  }

  if (ui.btnActivateLicense) {
    replaceEventHandler(
      ui.btnActivateLicense,
      "click",
      "__mouseThingActivateLicenseHandler",
      openLicenseDialog,
    );
  }

  if (ui.btnRemoveLicense) {
    replaceEventHandler(
      ui.btnRemoveLicense,
      "click",
      "__mouseThingRemoveLicenseHandler",
      handleLicenseRemove,
    );
  }

  if (ui.btnLicenseCancel) {
    replaceEventHandler(
      ui.btnLicenseCancel,
      "click",
      "__mouseThingLicenseCancelHandler",
      closeLicenseDialog,
    );
  }

  if (ui.btnLicenseSubmit) {
    replaceEventHandler(
      ui.btnLicenseSubmit,
      "click",
      "__mouseThingLicenseSubmitHandler",
      handleLicenseSubmit,
    );
  }

  if (ui.btnCheckUpdates) {
    replaceEventHandler(
      ui.btnCheckUpdates,
      "click",
      "__mouseThingCheckUpdatesHandler",
      () => checkForUpdates({ silent: false }),
    );
  }

  if (ui.btnInstallUpdate) {
    replaceEventHandler(
      ui.btnInstallUpdate,
      "click",
      "__mouseThingInstallUpdateHandler",
      handleInstallUpdate,
    );
  }

  if (ui.btnBannerUpdate) {
    replaceEventHandler(
      ui.btnBannerUpdate,
      "click",
      "__mouseThingBannerUpdateHandler",
      handleInstallUpdate,
    );
  }

  if (ui.btnBannerDismiss) {
    replaceEventHandler(
      ui.btnBannerDismiss,
      "click",
      "__mouseThingBannerDismissHandler",
      () => {
        state.updater.bannerDismissed = true;
        if (ui.updateBanner) {
          ui.updateBanner.hidden = true;
        }
      },
    );
  }

  if (ui.autoUpdateToggle) {
    replaceEventHandler(
      ui.autoUpdateToggle,
      "change",
      "__mouseThingAutoUpdateToggleHandler",
      (e) => {
        state.updater.autoUpdateEnabled = Boolean(e.target.checked);
        try {
          localStorage.setItem("zerog_auto_update", String(state.updater.autoUpdateEnabled));
        } catch (_e) {}
      },
    );
  }
}

async function handleDomContentLoaded() {
  if (appInitialized) {
    return;
  }

  const previousCleanup = window[APP_CLEANUP_HANDLER_KEY];
  if (typeof previousCleanup === "function") {
    previousCleanup();
  }

  appInitialized = true;
  bindElements();
  if (isHostLinux()) {
    document.body.classList.add("platform-linux");
  }
  if (isHostMac()) {
    document.body.classList.add("platform-mac");
  }
  state.configSaveQueue = createConfigSaveQueue({
    save: (config) => invokeTimed("save_config", { config }, 80),
    onStart: () => setConfigStatus("Saving..."),
    onSaved: handleConfigSaved,
    onError: handleConfigSaveError,
    onIdle: applyPendingRemoteConfigIfSafe,
  });
  layoutController = createLayoutController({
    state,
    ui,
    perfLog,
    isStatusTabActive,
    setConfigStatus,
    saveConfig: requestConfigSave,
    markConfigEdited,
    onDragEnd: applyPendingRemoteConfigIfSafe,
  });
  bindActions();
  setupEventLoopLagProbe();
  setupLongTaskProbe();
  await setupPeerEventListener();
  await setupStatusEventListener();
  await setupConfigSyncListener();
  await setupPairingEventListener();
  await setupFileTransferListeners();
  await setupFileDropListeners();
  await loadInitialState();
  await setupUpdaterListeners();
  checkForUpdates({ silent: true }).catch(() => {});

  // Flush a pending debounced layout save before the webview goes away.
  // `pagehide`/`beforeunload` fire while the app closes, and the visibility
  // change covers minimize. The async save is best effort, but it prevents
  // the common case of losing an edit made moments before quitting.
  const onVisibilityChange = () => {
    if (document.visibilityState === "hidden") {
      flushPendingConfigSave();
    }
  };
  const onWindowFocus = () => {
    if (isStatusTabActive() && !state.layoutDrag) {
      loadPeersSnapshot();
    }
  };
  window.addEventListener("focus", onWindowFocus);
  window.addEventListener("pagehide", flushPendingConfigSave);
  window.addEventListener("beforeunload", flushPendingConfigSave);
  document.addEventListener("visibilitychange", onVisibilityChange);

  window[APP_CLEANUP_HANDLER_KEY] = () => {
    window.removeEventListener("focus", onWindowFocus);
    window.removeEventListener("pagehide", flushPendingConfigSave);
    window.removeEventListener("beforeunload", flushPendingConfigSave);
    document.removeEventListener("visibilitychange", onVisibilityChange);
    flushPendingConfigSave();
    cleanupRuntimeEventListeners();
    layoutController?.cleanup();
    state.configSaveQueue?.destroy();
    state.configSaveQueue = null;
    layoutController = null;
  };
}

const previousDomContentLoadedHandler = window[DOM_CONTENT_LOADED_HANDLER_KEY];
if (typeof previousDomContentLoadedHandler === "function") {
  window.removeEventListener(
    "DOMContentLoaded",
    previousDomContentLoadedHandler,
  );
}

window[DOM_CONTENT_LOADED_HANDLER_KEY] = handleDomContentLoaded;
if (document.readyState === "loading") {
  window.addEventListener("DOMContentLoaded", handleDomContentLoaded);
} else {
  handleDomContentLoaded();
}
