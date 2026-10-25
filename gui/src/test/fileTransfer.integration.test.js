import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  defaultStatus,
  defaultConfig,
  buildPeer,
  buildLocalPeer,
  getInvokeCallCount,
} from "./helpers.js";
import { eventListeners } from "./setup.js";

describe("File transfer GUI integration", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    loadHtml();
    window.localStorage?.clear();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
    window.localStorage?.clear();
  });

  async function setupWithPeers(peers, overrides = {}) {
    const { status, config, ...commands } = overrides;
    registerDefaultCommands({
      peers,
      status: status ?? defaultStatus(),
      config: config ?? defaultConfig(),
      commands: {
        start_file_transfer: (payload) => ({
          transfer_id: "tx-test-1",
          sender_peer_id: "local-peer",
          sender_name: "Local",
          target_peer_id: payload.targetPeerId,
          files: [{ id: "f-1", name: "sample.png", size: 1024 }],
          total_size: 1024,
        }),
        accept_file_transfer: () => {},
        reject_file_transfer: () => {},
        ...commands,
      },
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
  }

  it("toggles auto-accept setting and persists to localStorage", async () => {
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-b", name: "Laptop" });
    await setupWithPeers([localPeer, peer]);

    const toggle = document.querySelector("#auto-accept-files");
    expect(toggle).not.toBeNull();
    expect(toggle.checked).toBe(false);

    toggle.checked = true;
    toggle.dispatchEvent(new Event("change"));

    expect(window.localStorage?.getItem("zerogAutoAcceptFiles")).toBe("1");

    toggle.checked = false;
    toggle.dispatchEvent(new Event("change"));

    expect(window.localStorage?.getItem("zerogAutoAcceptFiles")).toBe("0");
  });

  it("handles incoming offer with auto-accept enabled", async () => {
    window.localStorage?.setItem("zerogAutoAcceptFiles", "1");
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-b", name: "Laptop" });
    let acceptedTransferId = null;

    await setupWithPeers([localPeer, peer], {
      accept_file_transfer: (payload) => {
        acceptedTransferId = payload.transferId;
      },
    });

    const offerListener = eventListeners.get("file-transfer-offer");
    expect(typeof offerListener).toBe("function");

    await offerListener({
      payload: {
        transfer_id: "tx-auto-1",
        sender_peer_id: "peer-b",
        sender_name: "Laptop",
        files: [{ id: "f-1", name: "photo.jpg", size: 2048 }],
        total_size: 2048,
      },
    });

    expect(acceptedTransferId).toBe("tx-auto-1");
    const toast = document.querySelector('[data-transfer-id="tx-auto-1"]');
    expect(toast).not.toBeNull();
    expect(toast.textContent).toContain("Receiving photo.jpg");
  });

  it("handles incoming offer with manual accept and decline", async () => {
    window.localStorage?.setItem("zerogAutoAcceptFiles", "0");
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-b", name: "Laptop" });
    let rejectedTransferId = null;

    await setupWithPeers([localPeer, peer], {
      reject_file_transfer: (payload) => {
        rejectedTransferId = payload.transferId;
      },
    });

    const offerListener = eventListeners.get("file-transfer-offer");
    await offerListener({
      payload: {
        transfer_id: "tx-manual-1",
        sender_peer_id: "peer-b",
        sender_name: "Laptop",
        files: [{ id: "f-1", name: "notes.pdf", size: 4096 }],
        total_size: 4096,
      },
    });

    const toast = document.querySelector('[data-transfer-id="tx-manual-1"]');
    expect(toast).not.toBeNull();
    expect(toast.textContent).toContain("Incoming File Offer");
    expect(document.querySelector(".layout-transfer-track")).toBeNull();

    const declineBtn = toast.querySelector(".btn--stop");
    expect(declineBtn).not.toBeNull();
    declineBtn.click();

    expect(rejectedTransferId).toBe("tx-manual-1");
    expect(document.querySelector('[data-transfer-id="tx-manual-1"]')).toBeNull();
  });

  it("updates progress bar on progress events and finishes on complete", async () => {
    window.localStorage?.setItem("zerogAutoAcceptFiles", "1");
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-b", name: "Laptop" });

    await setupWithPeers([localPeer, peer]);

    const offerListener = eventListeners.get("file-transfer-offer");
    const progressListener = eventListeners.get("file-transfer-progress");
    const completeListener = eventListeners.get("file-transfer-complete");

    await offerListener({
      payload: {
        transfer_id: "tx-prog-1",
        sender_peer_id: "peer-b",
        sender_name: "Laptop",
        files: [{ id: "f-1", name: "archive.zip", size: 10000 }],
        total_size: 10000,
      },
    });

    await progressListener({
      payload: {
        transfer_id: "tx-prog-1",
        bytes_transferred: 5000,
        total_bytes: 10000,
        percent: 0.5,
        is_outgoing: false,
      },
    });

    const toast = document.querySelector('[data-transfer-id="tx-prog-1"]');
    expect(toast).not.toBeNull();
    const bar = toast.querySelector(".transfer-toast-bar");
    expect(bar.style.width).toBe("50%");

    await completeListener({
      payload: {
        transfer_id: "tx-prog-1",
        success: true,
        saved_path: "/home/user/Downloads/archive.zip",
      },
    });

    const completedToast = document.querySelector('[data-transfer-id="tx-prog-1"]');
    expect(completedToast).not.toBeNull();
    expect(completedToast.textContent).not.toContain("/home/user/Downloads/archive.zip");
    expect(completedToast.querySelector(".transfer-toast-desc")).toBeNull();
    expect(completedToast.querySelector(".transfer-toast-actions")?.textContent).toContain("Open folder");
  });

  it("handles drop event on canvas with single remote peer", async () => {
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-b", name: "Laptop" });
    let startedPayload = null;

    await setupWithPeers([localPeer, peer], {
      start_file_transfer: (payload) => {
        startedPayload = payload;
        return {
          transfer_id: "tx-drop-1",
          sender_peer_id: "local-peer",
          sender_name: "Local",
          target_peer_id: payload.targetPeerId,
          files: [{ id: "f-1", name: "report.pdf", size: 1024 }],
          total_size: 1024,
        };
      },
    });

    const dropListener = eventListeners.get("tauri://drag-drop");
    expect(typeof dropListener).toBe("function");

    await dropListener({
      payload: {
        paths: ["/home/user/report.pdf"],
        position: { x: 500, y: 300 },
      },
    });

    expect(startedPayload).not.toBeNull();
    expect(startedPayload.paths).toEqual(["/home/user/report.pdf"]);
    expect(startedPayload.targetPeerId).toBe("peer-b");
  });

  it("renders correct transfer direction on remote peer node", async () => {
    window.localStorage?.setItem("zerogAutoAcceptFiles", "1");
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-b", name: "Laptop" });

    await setupWithPeers([localPeer, peer], {
      status: defaultStatus({
        engine_running: true,
        connected_peers: [{ peer_id: "peer-b", name: "Laptop", platform: "linux" }],
      }),
      config: defaultConfig({
        placements: [{ peer_id: "peer-b", x: 600, y: 350, persisted: true }],
      }),
    });

    const offerListener = eventListeners.get("file-transfer-offer");
    await offerListener({
      payload: {
        transfer_id: "tx-label-1",
        sender_peer_id: "peer-b",
        sender_name: "Laptop",
        files: [{ id: "f-1", name: "file.zip", size: 5000 }],
        total_size: 5000,
      },
    });

    const label = document.querySelector(".layout-transfer-label");
    expect(label).not.toBeNull();
    expect(label.textContent).toBe("Sending 0%");

    const progressListener = eventListeners.get("file-transfer-progress");
    await progressListener({
      payload: {
        transfer_id: "tx-label-1",
        peer_id: "peer-b",
        bytes_transferred: 2500,
        total_bytes: 5000,
        percent: 0.5,
        is_outgoing: true,
      },
    });
    expect(document.querySelector(".layout-transfer-label")?.textContent).toBe("Receiving 50%");
  });
});
