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
  dispatchPointer,
  getInvokeCallCount,
  fireEvent,
} from "./helpers.js";

// Closing the app at any moment must not silently drop an edit that is still
// sitting in the layout debounce window.
describe("App lifecycle flushes pending layout edits", () => {
  let savedConfig = null;

  beforeEach(() => {
    vi.useFakeTimers();
    loadHtml();
    savedConfig = null;
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    savedConfig = null;
    document.documentElement.innerHTML = "";
  });

  const SNAP_RIGHT = { x: 166, y: 0 };

  async function setupAndStartDrag() {
    const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });
    registerDefaultCommands({
      peers: [buildLocalPeer(), peer],
      status: defaultStatus({
        connected_peers: [{ peer_id: "peer-a", name: "Alpha" }],
      }),
      config: defaultConfig({
        placements: [
          {
            peer_id: "peer-a",
            peer_label: "Alpha",
            x: 400,
            y: 0,
          },
        ],
      }),
      commands: {
        save_config: ({ config }) => {
          savedConfig = config;
          return config;
        },
      },
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
    fireEvent("peers-updated", {
      peers: [buildLocalPeer(), peer],
    });

    const rect = document.querySelector('g[data-peer-id="peer-a"] rect');
    dispatchPointer(rect, "pointerdown", {
      clientX: 400,
      clientY: 0,
      pointerId: 1,
      button: 0,
    });
    dispatchPointer(document.querySelector("#layout-svg"), "pointermove", {
      clientX: SNAP_RIGHT.x + 5,
      clientY: 5,
      pointerId: 1,
    });
  }

  it("saves instantly on pagehide before the debounce timer fires", async () => {
    await setupAndStartDrag();
    const before = getInvokeCallCount("save_config");

    window.dispatchEvent(new Event("pagehide"));
    await vi.advanceTimersByTimeAsync(0);

    expect(getInvokeCallCount("save_config") - before).toBe(1);
    expect(savedConfig).not.toBeNull();
    expect(
      savedConfig.placements.find((p) => p.peer_id === "peer-a").x,
    ).toBe(SNAP_RIGHT.x);
  });

  it("saves instantly on beforeunload before the debounce timer fires", async () => {
    await setupAndStartDrag();
    const before = getInvokeCallCount("save_config");

    window.dispatchEvent(new Event("beforeunload"));
    await vi.advanceTimersByTimeAsync(0);

    expect(getInvokeCallCount("save_config") - before).toBe(1);
    expect(savedConfig).not.toBeNull();
  });

  it("saves instantly when the document becomes hidden", async () => {
    await setupAndStartDrag();
    const before = getInvokeCallCount("save_config");

    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "hidden",
    });
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(0);

    expect(getInvokeCallCount("save_config") - before).toBe(1);
    expect(savedConfig).not.toBeNull();
  });

  it("does not save on pagehide when there is no pending edit", async () => {
    const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });
    registerDefaultCommands({
      peers: [buildLocalPeer(), peer],
      status: defaultStatus({
        connected_peers: [{ peer_id: "peer-a", name: "Alpha" }],
      }),
      config: defaultConfig(),
      commands: {
        save_config: ({ config }) => {
          savedConfig = config;
          return config;
        },
      },
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();

    const before = getInvokeCallCount("save_config");
    window.dispatchEvent(new Event("pagehide"));
    await vi.advanceTimersByTimeAsync(0);

    expect(getInvokeCallCount("save_config") - before).toBe(0);
  });
});
