import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  registerCommand,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  defaultStatus,
  defaultConfig,
  buildPeer,
  buildLocalPeer,
  getInvokeCallCount,
} from "./helpers.js";

// Button mashing guard: a second click while the first backend command is
// still in flight must not dispatch a duplicate command.
describe("Peer action double-submit guard", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    loadHtml();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  async function setupWithPeers(peers, statusOverrides = {}, configOverrides = {}, commands = {}) {
    registerDefaultCommands({
      peers,
      status: defaultStatus(statusOverrides),
      config: defaultConfig(configOverrides),
      commands,
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
  }

  function getActionButton(peerId, action) {
    return document.querySelector(
      `#peer-list [data-peer-id="${peerId}"] [data-peer-action="${action}"]`,
    );
  }

  it("mashing Connect dispatches one accept_connection call", async () => {
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });
    let resolveConnect;
    let connectCalls = 0;

    await setupWithPeers([localPeer, peer], { connected_peers: [] }, {}, {
      accept_connection: () =>
        new Promise((resolve) => {
          connectCalls += 1;
          resolveConnect = () => resolve(defaultStatus());
        }),
    });

    const btn = getActionButton("peer-a", "connect");
    expect(btn).not.toBeNull();

    btn.click();
    btn.click();
    btn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(connectCalls).toBe(1);
    expect(getInvokeCallCount("accept_connection")).toBe(1);

    resolveConnect();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("releases the guard after completion so the peer can be cancelled", async () => {
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });
    let resolveConnect;
    let connectCalls = 0;

    await setupWithPeers([localPeer, peer], { connected_peers: [] }, {}, {
      accept_connection: () =>
        new Promise((resolve) => {
          connectCalls += 1;
          resolveConnect = () => resolve(defaultStatus());
        }),
    });

    getActionButton("peer-a", "connect").click();
    await vi.advanceTimersByTimeAsync(0);
    expect(connectCalls).toBe(1);

    resolveConnect();
    await vi.advanceTimersByTimeAsync(0);

    // The guard must have been released, so the follow-up Cancel works.
    const cancelBtn = getActionButton("peer-a", "cancel");
    expect(cancelBtn).not.toBeNull();
    cancelBtn.click();
    await vi.advanceTimersByTimeAsync(0);
    expect(getInvokeCallCount("cancel_connection")).toBe(1);
  });

  it("mashing Disconnect dispatches one reject_connection call", async () => {
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-b", name: "Beta" });
    let resolveDisconnect;
    let rejectCalls = 0;

    await setupWithPeers(
      [localPeer, peer],
      { connected_peers: [{ peer_id: "peer-b", name: "Beta" }] },
      {},
      {
        reject_connection: () =>
          new Promise((resolve) => {
            rejectCalls += 1;
            resolveDisconnect = () => resolve(defaultStatus());
          }),
      },
    );

    const btn = getActionButton("peer-b", "disconnect");
    expect(btn).not.toBeNull();

    btn.click();
    btn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(rejectCalls).toBe(1);

    resolveDisconnect();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("mashing Forget dispatches one unpair_peer call", async () => {
    const localPeer = buildLocalPeer();
    const peer = buildPeer({ peer_id: "peer-c", name: "Gamma" });
    let resolveUnpair;
    let unpairCalls = 0;

    await setupWithPeers(
      [localPeer, peer],
      { connected_peers: [] },
      { paired_peers: ["peer-c"] },
      {
        unpair_peer: () =>
          new Promise((resolve) => {
            unpairCalls += 1;
            resolveUnpair = () => resolve(defaultStatus());
          }),
      },
    );

    const btn = getActionButton("peer-c", "forget");
    expect(btn).not.toBeNull();

    btn.click();
    btn.click();
    btn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(unpairCalls).toBe(1);

    resolveUnpair();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("mashing Cancel dispatches one cancel_connection call", async () => {
    const localPeer = buildLocalPeer();
    const peer = buildPeer({
      peer_id: "peer-d",
      name: "Delta",
      connection_status: "connecting",
      connected: false,
    });
    let resolveCancel;
    let cancelCalls = 0;

    await setupWithPeers([localPeer, peer], { connected_peers: [] }, {}, {
      cancel_connection: () =>
        new Promise((resolve) => {
          cancelCalls += 1;
          resolveCancel = () => resolve(defaultStatus());
        }),
    });

    const btn = getActionButton("peer-d", "cancel");
    expect(btn).not.toBeNull();

    btn.click();
    btn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(cancelCalls).toBe(1);

    resolveCancel();
    await vi.advanceTimersByTimeAsync(0);
  });
});
