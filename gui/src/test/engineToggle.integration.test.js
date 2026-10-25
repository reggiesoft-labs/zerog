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

describe("Engine toggle button", () => {
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

  describe("2.1 Clicking Start when engine is stopped", () => {
    it("transitions button from Start to Stop and updates status text", async () => {
      registerCommand("start_engine", () => defaultStatus({ engine_running: true }));

      const btn = getButton("#btn-engine-toggle");
      const statusEl = document.querySelector("#engine-running");

      expect(btn.textContent).toBe("Start");
      expect(btn.className).toContain("btn--start");
      expect(btn.disabled).toBe(false);
      expect(statusEl.textContent).toBe("Stopped");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Stop");
      expect(btn.className).toContain("btn--stop");
      expect(btn.disabled).toBe(false);
      expect(statusEl.textContent).toBe("Running");
      expect(getInvokeCallCount("start_engine")).toBe(1);
    });

  });

  describe("2.2 Clicking Stop when engine is running", () => {
    it("transitions button from Stop to Start", async () => {
      let stopEngineCallCount = 0;
      clearCommandHandlers({ clearEvents: false });
      registerDefaultCommands({
        status: defaultStatus({ engine_running: true }),
        commands: {
          stop_engine: () => {
            stopEngineCallCount++;
            return defaultStatus({ engine_running: false });
          },
        },
      });

      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: true }),
      });

      const btn = getButton("#btn-engine-toggle");
      const statusEl = document.querySelector("#engine-running");

      expect(btn.textContent).toBe("Stop");
      expect(btn.className).toContain("btn--stop");
      expect(statusEl.textContent).toBe("Running");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Start");
      expect(btn.className).toContain("btn--start");
      expect(statusEl.textContent).toBe("Stopped");
      expect(stopEngineCallCount).toBe(1);
    });
  });

  describe("2.3 Button shows loading state during in-flight request", () => {
    it("shows Starting... and disabled while start_engine is pending", async () => {
      let resolveStart;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () => new Promise((resolve) => {
            resolveStart = () => resolve(defaultStatus({ engine_running: true }));
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Starting...");
      expect(btn.className).toBe("btn btn-primary");
      expect(btn.disabled).toBe(true);

      resolveStart();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Stop");
      expect(btn.className).toContain("btn--stop");
      expect(btn.disabled).toBe(false);
    });

    it("shows Stopping... when stopping", async () => {
      let resolveStop;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: true }),
        commands: {
          stop_engine: () => new Promise((resolve) => {
            resolveStop = () => resolve(defaultStatus({ engine_running: false }));
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Stopping...");
      expect(btn.className).toBe("btn btn-primary");
      expect(btn.disabled).toBe(true);

      resolveStop();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Start");
    });

  });

  describe("2.4 Engine start failure shows error banner", () => {
    it("displays unwrapped error message for accessibility_permission_missing", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () => Promise.reject({
            code: "accessibility_permission_missing",
            message: "Accessibility permission required",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      const errorBanner = document.querySelector("#error-banner");
      const statusEl = document.querySelector("#engine-running");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toBe(
        "ZeroG needs Accessibility access. The helper is open; drag the ZeroG row into Privacy & Security > Accessibility."
      );
      expect(statusEl.textContent).toBe("Stopped");
      expect(btn.textContent).toBe("Start");
      expect(btn.className).toContain("btn--start");
    });

    it("displays unwrapped error message for socket_bind_failed", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () => Promise.reject({
            code: "socket_bind_failed",
            message: "Socket bind failed",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
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
          start_engine: () => Promise.reject({
            code: "no_peers_discovered",
            message: "No peers",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
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
          start_engine: () => Promise.reject({
            code: "operation_in_progress",
            message: "Already trying",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
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
          start_engine: () => Promise.reject({
            code: "invalid_config",
            message: "Bad config",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
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
          start_engine: () => Promise.reject({
            code: "accessibility_permission_missing",
            message: "Accessibility permission required",
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(wasInvokeCalled("report_gui_error")).toBe(true);
    });

  });

  describe("2.5 Double-click is debounced by meshActionInProgress guard", () => {
    it("only sends one start_engine call on rapid double-click", async () => {
      let resolveStart;
      let startEngineCallCount = 0;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () => new Promise((resolve) => {
            startEngineCallCount++;
            resolveStart = () => resolve(defaultStatus({ engine_running: true }));
          }),
        },
      });

      const btn = getButton("#btn-engine-toggle");

      btn.click();
      btn.click();
      btn.click();

      await vi.advanceTimersByTimeAsync(0);

      expect(startEngineCallCount).toBe(1);

      resolveStart();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Stop");
    });

  });

  describe("Engine restart lifecycle hardening", () => {
    it("can start again immediately after stop without socket_bind_failed", async () => {
      let engineRunning = false;
      let startCalls = 0;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          get_status: () => defaultStatus({ engine_running: engineRunning }),
          start_engine: () => {
            startCalls += 1;
            engineRunning = true;
            return defaultStatus({ engine_running: true });
          },
          stop_engine: () => {
            engineRunning = false;
            return defaultStatus({ engine_running: false });
          },
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      const errorBanner = document.querySelector("#error-banner");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(btn.textContent).toBe("Stop");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(btn.textContent).toBe("Start");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(btn.textContent).toBe("Stop");
      expect(errorBanner.hidden).toBe(true);
      expect(startCalls).toBe(2);
    });

    it("survives rapid stop/start stress cycles without surfacing bind errors", async () => {
      let engineRunning = false;
      let startCalls = 0;
      let stopCalls = 0;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          get_status: () => defaultStatus({ engine_running: engineRunning }),
          start_engine: () => {
            startCalls += 1;
            engineRunning = true;
            return defaultStatus({ engine_running: true });
          },
          stop_engine: () => {
            stopCalls += 1;
            engineRunning = false;
            return defaultStatus({ engine_running: false });
          },
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      const errorBanner = document.querySelector("#error-banner");
      const statusEl = document.querySelector("#engine-running");

      const cycles = 8;
      for (let i = 0; i < cycles; i += 1) {
        btn.click();
        await vi.advanceTimersByTimeAsync(0);
        expect(btn.textContent).toBe("Stop");
        expect(statusEl.textContent).toBe("Running");

        btn.click();
        await vi.advanceTimersByTimeAsync(0);
        expect(btn.textContent).toBe("Start");
        expect(statusEl.textContent).toBe("Stopped");
      }

      expect(startCalls).toBe(cycles);
      expect(stopCalls).toBe(cycles);
      expect(errorBanner.hidden).toBe(true);
      expect(errorBanner.textContent).toBe("");
    });
  });

  describe("Restart button", () => {
    it("is disabled when engine is stopped and enabled when engine is running", async () => {
      const restartBtn = getButton("#btn-engine-restart");
      expect(restartBtn.disabled).toBe(true);
      expect(restartBtn.textContent).toBe("Restart");

      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: true }),
      });

      expect(restartBtn.disabled).toBe(false);
      expect(restartBtn.textContent).toBe("Restart");
    });

    it("invokes restart_engine and shows loading state while pending", async () => {
      let resolveRestart;
      let restartCalls = 0;
      clearCommandHandlers({ clearEvents: false });
      registerDefaultCommands({
        status: defaultStatus({ engine_running: true }),
        commands: {
          restart_engine: () =>
            new Promise((resolve) => {
              restartCalls++;
              resolveRestart = () => resolve(defaultStatus({ engine_running: true }));
            }),
        },
      });

      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: true }),
      });

      const restartBtn = getButton("#btn-engine-restart");
      const toggleBtn = getButton("#btn-engine-toggle");

      expect(restartBtn.disabled).toBe(false);
      expect(toggleBtn.textContent).toBe("Stop");

      restartBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(restartBtn.textContent).toBe("Restarting...");
      expect(restartBtn.disabled).toBe(true);
      expect(toggleBtn.disabled).toBe(true);
      expect(restartCalls).toBe(1);

      resolveRestart();
      await vi.advanceTimersByTimeAsync(0);

      expect(restartBtn.textContent).toBe("Restart");
      expect(restartBtn.disabled).toBe(false);
      expect(toggleBtn.disabled).toBe(false);
      expect(toggleBtn.textContent).toBe("Stop");
    });

    it("surfaces errors when restart_engine fails", async () => {
      clearCommandHandlers({ clearEvents: false });
      registerDefaultCommands({
        status: defaultStatus({ engine_running: true }),
        commands: {
          restart_engine: () =>
            Promise.reject({
              code: "socket_bind_failed",
              message: "Unable to bind socket",
            }),
        },
      });

      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: true }),
      });

      const restartBtn = getButton("#btn-engine-restart");
      const errorBanner = document.querySelector("#error-banner");

      restartBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toBe(
        "Socket bind failed. Another process may already be using the networking port.",
      );
      expect(restartBtn.disabled).toBe(false);
      expect(restartBtn.textContent).toBe("Restart");
    });
  });

  describe("2.6 Layout diagram synchronization", () => {
    it("updates layout diagram when engine is stopped then started with connected peer", async () => {
      let running = true;
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({ peer_id: "peer-remote", name: "RemoteMac", connected: true });
      clearCommandHandlers({ clearEvents: false });
      registerDefaultCommands({
        peers: [localPeer, remotePeer],
        status: defaultStatus({
          engine_running: true,
          connected_peers: [{ peer_id: "peer-remote", name: "RemoteMac" }],
        }),
        commands: {
          stop_engine: () => {
            running = false;
            return defaultStatus({ engine_running: false, connected_peers: [] });
          },
          start_engine: () => {
            running = true;
            return defaultStatus({
              engine_running: true,
              connected_peers: [{ peer_id: "peer-remote", name: "RemoteMac" }],
            });
          },
          get_peers_cached: () => [
            localPeer,
            buildPeer({ peer_id: "peer-remote", name: "RemoteMac", connected: running }),
          ],
        },
      });

      fireEvent("peers-updated", { peers: [localPeer, remotePeer] });
      fireEvent("status-updated", {
        status: defaultStatus({
          engine_running: true,
          connected_peers: [{ peer_id: "peer-remote", name: "RemoteMac" }],
        }),
      });

      expect(document.querySelector('g[data-peer-id="peer-remote"]')).not.toBeNull();

      const toggleBtn = getButton("#btn-engine-toggle");
      toggleBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(document.querySelector('g[data-peer-id="peer-remote"]')).toBeNull();

      toggleBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(document.querySelector('g[data-peer-id="peer-remote"]')).not.toBeNull();
    });
  });
});
