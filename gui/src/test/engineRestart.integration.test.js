import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  registerCommand,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  defaultStatus,
  fireEvent,
  getButton,
  getInvokeCallCount,
  wasInvokeCalled,
  buildLocalPeer,
  buildPeer,
} from "./helpers.js";

// The engine runs whenever the window is open, so Restart is the only engine
// control. It must stay usable while the engine is stopped so a failed start
// at launch can be retried (restart_engine starts a stopped engine).
describe("Engine restart button", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
    registerDefaultCommands({
      status: defaultStatus({ engine_running: false }),
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  it("has no separate start/stop control", () => {
    expect(document.querySelector("#btn-engine-toggle")).toBeNull();
  });

  describe("Availability", () => {
    it("is enabled whether the engine is stopped or running", () => {
      const restartBtn = getButton("#btn-engine-restart");
      expect(restartBtn.disabled).toBe(false);
      expect(restartBtn.textContent).toBe("Restart");

      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: true }),
      });
      expect(restartBtn.disabled).toBe(false);
      expect(restartBtn.textContent).toBe("Restart");

      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: false }),
      });
      expect(restartBtn.disabled).toBe(false);
    });
  });

  describe("Retrying a stopped engine", () => {
    it("starts a stopped engine via restart_engine and updates status", async () => {
      registerCommand("restart_engine", () => defaultStatus({ engine_running: true }));

      const restartBtn = getButton("#btn-engine-restart");
      const statusEl = document.querySelector("#engine-running");
      const indicator = document.querySelector("#daemon-indicator");
      expect(statusEl.textContent).toBe("Stopped");
      expect(indicator.className).toContain("status-indicator-dot--stopped");

      restartBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(getInvokeCallCount("restart_engine")).toBe(1);
      expect(statusEl.textContent).toBe("Running");
      expect(indicator.className).toContain("status-indicator-dot--running");
      expect(restartBtn.disabled).toBe(false);
    });
  });

  describe("In-flight state", () => {
    it("shows Restarting... and is disabled while restart_engine is pending", async () => {
      let resolveRestart;
      let restartCalls = 0;
      registerCommand("restart_engine", () =>
        new Promise((resolve) => {
          restartCalls++;
          resolveRestart = () => resolve(defaultStatus({ engine_running: true }));
        }),
      );

      const restartBtn = getButton("#btn-engine-restart");

      restartBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(restartBtn.textContent).toBe("Restarting...");
      expect(restartBtn.className).toBe("btn btn-primary");
      expect(restartBtn.disabled).toBe(true);
      expect(restartCalls).toBe(1);

      resolveRestart();
      await vi.advanceTimersByTimeAsync(0);

      expect(restartBtn.textContent).toBe("Restart");
      expect(restartBtn.className).toBe("btn btn--restart");
      expect(restartBtn.disabled).toBe(false);
    });

    it("only sends one restart_engine call on rapid repeated clicks", async () => {
      let resolveRestart;
      let restartCalls = 0;
      registerCommand("restart_engine", () =>
        new Promise((resolve) => {
          restartCalls++;
          resolveRestart = () => resolve(defaultStatus({ engine_running: true }));
        }),
      );

      const restartBtn = getButton("#btn-engine-restart");

      restartBtn.click();
      restartBtn.click();
      restartBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(restartCalls).toBe(1);

      resolveRestart();
      await vi.advanceTimersByTimeAsync(0);

      expect(restartBtn.disabled).toBe(false);
      expect(document.querySelector("#engine-running").textContent).toBe("Running");
    });
  });

  describe("Restart failure shows error banner", () => {
    it("displays unwrapped error message for accessibility_permission_missing", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          restart_engine: () => Promise.reject({
            code: "accessibility_permission_missing",
            message: "Accessibility permission required",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-restart");
      const errorBanner = document.querySelector("#error-banner");
      const statusEl = document.querySelector("#engine-running");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toBe(
        "ZeroG needs Accessibility access. The helper is open; drag the ZeroG row into Privacy & Security > Accessibility."
      );
      expect(statusEl.textContent).toBe("Stopped");
      expect(btn.textContent).toBe("Restart");
      expect(btn.className).toContain("btn--restart");
      expect(btn.disabled).toBe(false);
    });

    it("displays unwrapped error message for socket_bind_failed", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          restart_engine: () => Promise.reject({
            code: "socket_bind_failed",
            message: "Socket bind failed",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-restart");
      const errorBanner = document.querySelector("#error-banner");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.textContent).toBe(
        "Socket bind failed. Another process may already be using the networking port."
      );
    });

    it("displays unwrapped error message for no_peers_discovered", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          restart_engine: () => Promise.reject({
            code: "no_peers_discovered",
            message: "No peers",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-restart");
      const errorBanner = document.querySelector("#error-banner");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.textContent).toBe(
        "No peers discovered. Ensure both machines are on the same LAN and multicast DNS is allowed."
      );
    });

    it("displays unwrapped error message for operation_in_progress", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          restart_engine: () => Promise.reject({
            code: "operation_in_progress",
            message: "Already trying",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-restart");
      const errorBanner = document.querySelector("#error-banner");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.textContent).toBe(
        "A connection attempt is already in progress."
      );
    });

    it("displays unwrapped error message for invalid_config", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          restart_engine: () => Promise.reject({
            code: "invalid_config",
            message: "Bad config",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-restart");
      const errorBanner = document.querySelector("#error-banner");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.textContent).toBe(
        "Layout config is invalid. Check peer assignments and retry."
      );
    });

    it("calls report_gui_error on failure", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          restart_engine: () => Promise.reject({
            code: "accessibility_permission_missing",
            message: "Accessibility permission required",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-restart");
      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(wasInvokeCalled("report_gui_error")).toBe(true);
    });

  });

  describe("Restart lifecycle hardening", () => {
    it("survives repeated restart cycles without surfacing errors", async () => {
      let restartCalls = 0;
      registerCommand("restart_engine", () => {
        restartCalls += 1;
        return defaultStatus({ engine_running: true });
      });

      const restartBtn = getButton("#btn-engine-restart");
      const errorBanner = document.querySelector("#error-banner");
      const statusEl = document.querySelector("#engine-running");

      const cycles = 8;
      for (let i = 0; i < cycles; i += 1) {
        restartBtn.click();
        await vi.advanceTimersByTimeAsync(0);
        expect(statusEl.textContent).toBe("Running");
        expect(restartBtn.disabled).toBe(false);
      }

      expect(restartCalls).toBe(cycles);
      expect(errorBanner.hidden).toBe(true);
      expect(errorBanner.textContent).toBe("");
    });
  });

  describe("Layout diagram synchronization", () => {
    it("restores connected peers in the layout after restarting a stopped engine", async () => {
      let running = true;
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({ peer_id: "peer-remote", name: "RemoteMac", connected: true });
      const connectedStatus = () =>
        defaultStatus({
          engine_running: true,
          connected_peers: [{ peer_id: "peer-remote", name: "RemoteMac" }],
        });
      clearCommandHandlers({ clearEvents: false });
      registerDefaultCommands({
        peers: [localPeer, remotePeer],
        status: connectedStatus(),
        commands: {
          restart_engine: () => {
            running = true;
            return connectedStatus();
          },
          get_peers_cached: () => [
            localPeer,
            buildPeer({ peer_id: "peer-remote", name: "RemoteMac", connected: running }),
          ],
        },
      });

      fireEvent("peers-updated", { peers: [localPeer, remotePeer] });
      fireEvent("status-updated", { status: connectedStatus() });
      expect(document.querySelector('g[data-peer-id="peer-remote"]')).not.toBeNull();

      running = false;
      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: false, connected_peers: [] }),
      });
      fireEvent("peers-updated", {
        peers: [localPeer, buildPeer({ peer_id: "peer-remote", name: "RemoteMac", connected: false })],
      });
      expect(document.querySelector('g[data-peer-id="peer-remote"]')).toBeNull();

      getButton("#btn-engine-restart").click();
      await vi.advanceTimersByTimeAsync(0);

      expect(document.querySelector('g[data-peer-id="peer-remote"]')).not.toBeNull();
    });
  });
});
