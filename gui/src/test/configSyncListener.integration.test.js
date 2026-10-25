import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  defaultConfig,
  defaultStatus,
  buildLocalPeer,
  buildPeer,
  fireEvent,
  hasEventListener,
  getTextContent,
  getInvokeCallCount,
  registerCommand,
  switchToConfigTab,
} from "./helpers.js";
import { eventListeners, mockListen } from "./setup.js";

function installDefaultListenMock() {
  mockListen.mockImplementation(async (eventName, callback) => {
    eventListeners.set(eventName, callback);
    return () => {
      eventListeners.delete(eventName);
    };
  });
}

describe("Config sync listener integration", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    installDefaultListenMock();
    loadHtml();
    registerDefaultCommands({
      status: defaultStatus(),
      config: defaultConfig(),
      peers: [],
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    installDefaultListenMock();
    mockListen.mockClear();
    document.documentElement.innerHTML = "";
  });

  it("registers config-sync listener once across repeated startup", async () => {
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();

    await initApp();
    await initApp();

    const configSyncRegistrations = mockListen.mock.calls.filter(
      ([eventName]) => eventName === "config-sync-received",
    );
    expect(configSyncRegistrations).toHaveLength(1);
  });

  it("renders immediately from valid config-sync payload", async () => {
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();

    expect(getInvokeCallCount("get_config")).toBe(1);

    fireEvent("config-sync-received", {
      config: defaultConfig({
        legacy_issues: [{ peer_name: "Beta", reason: "needs reselection", location: "left" }],
      }),
    });

    expect(getTextContent("#config-status")).toBe("Beta: needs reselection (left)");
    expect(getInvokeCallCount("get_config")).toBe(1);
  });

  it("updates the layout diagram when a remote layout arrives", async () => {
    registerCommand("get_status", () =>
      defaultStatus({
        connected_peers: [{ peer_id: "peer-a", name: "Alpha" }],
      }),
    );
    registerCommand("get_peers_cached", () => [buildLocalPeer(), buildPeer()]);

    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();

    fireEvent("config-sync-received", {
      config: defaultConfig({
        placements: [{ peer_id: "peer-a", peer_label: "Alpha", x: 166, y: 0 }],
      }),
    });

    const peerGroup = document.querySelector('#layout-svg [data-peer-id="peer-a"]');
    expect(peerGroup.classList.contains("layout-node--persisted")).toBe(true);
    expect(peerGroup.querySelector(".layout-node-label").getAttribute("x")).toBe("166");
  });

  it("ignores malformed config-sync payload without throwing", async () => {
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();

    expect(() => fireEvent("config-sync-received", {})).not.toThrow();
    expect(() => fireEvent("config-sync-received", { config: null })).not.toThrow();
    expect(() => fireEvent("config-sync-received", { config: [] })).not.toThrow();
    expect(getTextContent("#config-status")).toBe("");
  });

  it("uses latest valid config when multiple sync events arrive rapidly", async () => {
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();

    fireEvent("config-sync-received", {
      config: defaultConfig({
        legacy_issues: [
          { peer_name: "Alpha", reason: "first-update", location: "left" },
        ],
      }),
    });
    fireEvent("config-sync-received", { config: null });
    fireEvent("config-sync-received", {
      config: defaultConfig({
        legacy_issues: [
          { peer_name: "Beta", reason: "winner-update", location: "right" },
        ],
      }),
    });

    expect(getTextContent("#config-status")).toBe("Beta: winner-update (right)");
    expect(getInvokeCallCount("get_config")).toBe(1);
  });

  it("does not crash when config-sync listener registration fails", async () => {
    mockListen.mockImplementation(async (eventName, callback) => {
      if (eventName === "config-sync-received") {
        throw new Error("listener unavailable");
      }

      eventListeners.set(eventName, callback);
      return () => {
        eventListeners.delete(eventName);
      };
    });

    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();

    await expect(initApp()).resolves.toBeUndefined();

    expect(hasEventListener("status-updated")).toBe(true);
    expect(hasEventListener("peers-updated")).toBe(true);
    expect(hasEventListener("config-sync-received")).toBe(false);

    await switchToConfigTab();
    expect(getInvokeCallCount("get_config")).toBeGreaterThan(0);
  });
});
