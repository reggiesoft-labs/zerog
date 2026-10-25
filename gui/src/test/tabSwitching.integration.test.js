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
  switchToConfigTab,
  switchToTransfersTab,
  switchToMainTab,
  fireEvent,
} from "./helpers.js";

describe("Tab switching and event-driven updates", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  async function setupApp(statusOverrides = {}, configOverrides = {}) {
    registerDefaultCommands({
      status: defaultStatus(statusOverrides),
      config: defaultConfig(configOverrides),
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
  }

  describe("5.1 Tab switching toggles visibility", () => {
    it("main tab is active by default", async () => {
      await setupApp();

      const mainPane = document.querySelector("#tab-main");
      const transfersPane = document.querySelector("#tab-transfers");
      const configPane = document.querySelector("#tab-config");
      const mainBtn = document.querySelector('[data-tab="tab-main"]');
      const transfersBtn = document.querySelector('[data-tab="tab-transfers"]');
      const configBtn = document.querySelector('[data-tab="tab-config"]');

      expect(mainPane.classList.contains("tab-pane--active")).toBe(true);
      expect(transfersPane.classList.contains("tab-pane--active")).toBe(false);
      expect(configPane.classList.contains("tab-pane--active")).toBe(false);
      expect(mainBtn.classList.contains("tab-btn--active")).toBe(true);
      expect(transfersBtn.classList.contains("tab-btn--active")).toBe(false);
      expect(configBtn.classList.contains("tab-btn--active")).toBe(false);
    });

    it("Transfers tab stays hidden and cannot be opened", async () => {
      await setupApp();

      await switchToTransfersTab();

      const mainPane = document.querySelector("#tab-main");
      const transfersPane = document.querySelector("#tab-transfers");
      const configPane = document.querySelector("#tab-config");
      const mainBtn = document.querySelector('[data-tab="tab-main"]');
      const transfersBtn = document.querySelector('[data-tab="tab-transfers"]');
      const configBtn = document.querySelector('[data-tab="tab-config"]');

      expect(transfersBtn.hidden).toBe(true);
      expect(transfersBtn.disabled).toBe(true);
      expect(transfersPane.hidden).toBe(true);
      expect(transfersPane.classList.contains("tab-pane--active")).toBe(false);
      expect(mainPane.classList.contains("tab-pane--active")).toBe(true);
      expect(configPane.classList.contains("tab-pane--active")).toBe(false);
      expect(transfersBtn.classList.contains("tab-btn--active")).toBe(false);
      expect(mainBtn.classList.contains("tab-btn--active")).toBe(true);
      expect(configBtn.classList.contains("tab-btn--active")).toBe(false);
    });

    it("clicking Config tab makes config pane active", async () => {
      await setupApp();

      await switchToConfigTab();

      const mainPane = document.querySelector("#tab-main");
      const transfersPane = document.querySelector("#tab-transfers");
      const configPane = document.querySelector("#tab-config");
      const mainBtn = document.querySelector('[data-tab="tab-main"]');
      const transfersBtn = document.querySelector('[data-tab="tab-transfers"]');
      const configBtn = document.querySelector('[data-tab="tab-config"]');

      expect(configPane.classList.contains("tab-pane--active")).toBe(true);
      expect(mainPane.classList.contains("tab-pane--active")).toBe(false);
      expect(transfersPane.classList.contains("tab-pane--active")).toBe(false);
      expect(configBtn.classList.contains("tab-btn--active")).toBe(true);
      expect(mainBtn.classList.contains("tab-btn--active")).toBe(false);
      expect(transfersBtn.classList.contains("tab-btn--active")).toBe(false);
    });

    it("clicking Main tab makes main pane active", async () => {
      await setupApp();

      await switchToConfigTab();
      await switchToMainTab();

      const mainPane = document.querySelector("#tab-main");
      const transfersPane = document.querySelector("#tab-transfers");
      const configPane = document.querySelector("#tab-config");
      const mainBtn = document.querySelector('[data-tab="tab-main"]');
      const transfersBtn = document.querySelector('[data-tab="tab-transfers"]');
      const configBtn = document.querySelector('[data-tab="tab-config"]');

      expect(mainPane.classList.contains("tab-pane--active")).toBe(true);
      expect(transfersPane.classList.contains("tab-pane--active")).toBe(false);
      expect(configPane.classList.contains("tab-pane--active")).toBe(false);
      expect(mainBtn.classList.contains("tab-btn--active")).toBe(true);
      expect(transfersBtn.classList.contains("tab-btn--active")).toBe(false);
      expect(configBtn.classList.contains("tab-btn--active")).toBe(false);
    });

  });

  describe("5.2 Error banner appears and clears", () => {
    it("error banner is hidden by default", async () => {
      await setupApp();

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(true);
      expect(errorBanner.textContent).toBe("");
    });

    it("error banner shows message on command failure", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        commands: {
          get_status: () => {
            throw new Error("Connection refused");
          },
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();
      await vi.advanceTimersByTimeAsync(0);

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toContain("Connection refused");
    });

    it("clears error after a successful initial snapshot load", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        commands: {
          get_status: () => defaultStatus(),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();
      await vi.advanceTimersByTimeAsync(0);

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(true);
    });

    it("clears error after an authoritative status update", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        commands: {
          start_engine: () => Promise.reject({ code: "socket_bind_failed", message: "Port in use" }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = document.querySelector("#btn-engine-toggle");
      const errorBanner = document.querySelector("#error-banner");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(errorBanner.hidden).toBe(false);

      fireEvent("status-updated", {
        status: defaultStatus({ engine_running: false }),
      });
      await vi.advanceTimersByTimeAsync(0);

      expect(errorBanner.hidden).toBe(true);
      expect(errorBanner.textContent).toBe("");
    });
  });

  describe("5.3 Event-driven updates", () => {
    it("does not poll after the initial snapshot load", async () => {
      let statusCalls = 0;
      let peerCalls = 0;
      let configCalls = 0;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig(),
        commands: {
          get_status: () => {
            statusCalls++;
            return defaultStatus();
          },
          get_peers_cached: () => {
            peerCalls++;
            return [];
          },
          get_config: () => {
            configCalls++;
            return defaultConfig();
          },
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();
      await vi.advanceTimersByTimeAsync(5000);

      expect(statusCalls).toBe(1);
      expect(peerCalls).toBe(1);
      expect(configCalls).toBe(1);
    });
  });

  describe("5.4 Config snapshot ownership", () => {
    it("does not reload config when switching to Config tab", async () => {
      let configRefreshCount = 0;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig(),
        commands: {
          get_config: () => {
            configRefreshCount++;
            return defaultConfig();
          },
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const initialConfigCalls = configRefreshCount;

      await switchToConfigTab();

      expect(configRefreshCount).toBe(initialConfigCalls);
    });
  });

  describe("5.5 Keyboard settings", () => {
    it("does not render the swap_alt_cmd checkbox", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig({ swap_alt_cmd: false }),
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      await switchToConfigTab();

      expect(document.querySelector("#swap-alt-cmd")).toBeNull();
    });

    it("renders keyboard strip keycaps and saves on keycap click", async () => {
      let savedConfig = null;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig({ swap_alt_cmd: false }),
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

      await switchToConfigTab();

      const keycaps = document.querySelectorAll(".keycap");
      expect(keycaps.length).toBe(3);

      const firstKeycap = keycaps[0];
      firstKeycap.click();

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      expect(savedConfig.modifier_mapping).not.toBeNull();
    });

    it("does not render modifier reset button", async () => {
      clearCommandHandlers();
      registerDefaultCommands();

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      await switchToConfigTab();

      const resetBtn = document.querySelector("#reset-modifiers-btn");
      expect(resetBtn).toBeNull();
    });

    it("renders macos default keycaps with Option to Win and Cmd to Alt", async () => {
      const originalPlatform = navigator.platform;
      Object.defineProperty(navigator, "platform", {
        value: "MacIntel",
        configurable: true,
      });

      try {
        clearCommandHandlers();
        registerDefaultCommands({
          status: defaultStatus(),
          config: defaultConfig({ swap_alt_cmd: true, modifier_mapping: null }),
        });

        vi.resetModules();
        await import("../main.js");
        stubSvgMethods();
        await initApp();

        await switchToConfigTab();

        const keycaps = document.querySelectorAll(".keycap");
        expect(keycaps.length).toBe(3);

        const targets = Array.from(keycaps).map(
          (k) => k.querySelector(".keycap-target")?.textContent,
        );
        expect(targets).toEqual(["Ctrl", "Win ⊞", "Alt"]);
      } finally {
        Object.defineProperty(navigator, "platform", {
          value: originalPlatform,
          configurable: true,
        });
      }
    });
  });

  describe("5.6 Mouse settings", () => {
    it("renders keep_mouse_alive checkbox state and saves on toggle", async () => {
      let savedConfig = null;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig({ keep_mouse_alive: false }),
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

      await switchToConfigTab();

      const checkbox = document.querySelector("#keep-mouse-alive");
      expect(checkbox).not.toBeNull();
      expect(checkbox.checked).toBe(false);

      checkbox.checked = true;
      checkbox.dispatchEvent(new Event("change"));

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      expect(savedConfig.keep_mouse_alive).toBe(true);
    });

    it("renders release_tap_key selection and saves on change", async () => {
      let savedConfig = null;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig({ release_tap_key: "f12" }),
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

      await switchToConfigTab();

      const select = document.querySelector("#release-tap-key");
      expect(select).not.toBeNull();
      expect(select.value).toBe("f12");

      select.value = "pause";
      select.dispatchEvent(new Event("change"));

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      expect(savedConfig.release_tap_key).toBe("pause");
    });
  });

  describe("5.7 Clipboard settings", () => {
    it("renders share_clipboard checkbox state and saves on toggle", async () => {
      let savedConfig = null;
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig({ share_clipboard: true }),
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

      await switchToConfigTab();

      const checkbox = document.querySelector("#share-clipboard");
      expect(checkbox).not.toBeNull();
      expect(checkbox.checked).toBe(true);

      checkbox.checked = false;
      checkbox.dispatchEvent(new Event("change"));

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      expect(savedConfig.share_clipboard).toBe(false);
    });
  });

  describe("5.8 Windows UAC settings", () => {
    const originalPlatform = navigator.platform;

    afterEach(() => {
      Object.defineProperty(navigator, "platform", {
        value: originalPlatform,
        configurable: true,
      });
    });

    it("renders disabled status and Enable button when service is not installed", async () => {
      Object.defineProperty(navigator, "platform", {
        value: "Win32",
        configurable: true,
      });

      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig(),
        commands: {
          get_windows_input_status: () => ({
            supported: true,
            elevated: false,
            service_installed: false,
            service_running: false,
            secure_desktop_enabled: true,
            service_reachable: false,
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      await switchToConfigTab();

      const statusEl = document.querySelector("#windows-input-status");
      const serviceBtn = document.querySelector("#btn-win-service");
      const card = document.querySelector("#windows-input-card");

      expect(card.hidden).toBe(false);
      expect(statusEl.textContent).toContain("Disabled");
      expect(statusEl.classList.contains("uac-status--disabled")).toBe(true);
      expect(serviceBtn.textContent).toBe("Enable UAC Control");
      expect(serviceBtn.classList.contains("btn-primary")).toBe(true);
    });

    it("renders active status and Disable button when service is running", async () => {
      Object.defineProperty(navigator, "platform", {
        value: "Win32",
        configurable: true,
      });

      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig(),
        commands: {
          get_windows_input_status: () => ({
            supported: true,
            elevated: false,
            service_installed: true,
            service_running: true,
            secure_desktop_enabled: true,
            service_reachable: true,
          }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      await switchToConfigTab();

      const statusEl = document.querySelector("#windows-input-status");
      const serviceBtn = document.querySelector("#btn-win-service");
      const card = document.querySelector("#windows-input-card");

      expect(card.hidden).toBe(false);
      expect(statusEl.textContent).toContain("Active");
      expect(statusEl.classList.contains("uac-status--active")).toBe(true);
      expect(serviceBtn.textContent).toBe("Disable UAC Control");
      expect(serviceBtn.classList.contains("btn-primary")).toBe(false);
    });

    it("hides Windows UAC settings on non-Windows platforms", async () => {
      Object.defineProperty(navigator, "platform", {
        value: "MacIntel",
        configurable: true,
      });

      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus(),
        config: defaultConfig(),
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      await switchToConfigTab();

      const card = document.querySelector("#windows-input-card");
      expect(card.hidden).toBe(true);
    });
  });
});

