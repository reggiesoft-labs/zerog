import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  registerCommand,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  defaultStatus,
  getTextContent,
  getButton,
  getInvokeCallCount,
  wasInvokeCalled,
} from "./helpers.js";
import { mockInvoke } from "./setup.js";

describe("Error handling", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    mockInvoke.mockClear();
    document.documentElement.innerHTML = "";
  });

  describe("6.1 Various error codes are unwrapped correctly", () => {
    const errorCodes = [
      {
        code: "accessibility_permission_missing",
        expectedMessage:
          "ZeroG needs Accessibility access. The helper is open; drag the ZeroG row into Privacy & Security > Accessibility.",
      },
      {
        code: "socket_bind_failed",
        expectedMessage:
          "Socket bind failed. Another process may already be using the networking port.",
      },
      {
        code: "no_peers_discovered",
        expectedMessage:
          "No peers discovered. Ensure both machines are on the same LAN and multicast DNS is allowed.",
      },
      {
        code: "operation_in_progress",
        expectedMessage: "A connection attempt is already in progress.",
      },
      {
        code: "invalid_config",
        expectedMessage:
          "Layout config is invalid. Check peer assignments and retry.",
      },
    ];

    errorCodes.forEach(({ code, expectedMessage }) => {
      it(`unwraps ${code} to user-friendly message`, async () => {
        clearCommandHandlers();
        registerDefaultCommands({
          status: defaultStatus({ engine_running: false }),
          commands: {
            start_engine: () =>
              Promise.reject({ code, message: "Original error message" }),
          },
        });

        vi.resetModules();
        await import("../main.js");
        stubSvgMethods();
        await initApp();

        const btn = getButton("#btn-engine-toggle");
        btn.click();
        await vi.advanceTimersByTimeAsync(0);

        const errorBanner = document.querySelector("#error-banner");
        expect(errorBanner.hidden).toBe(false);
        expect(errorBanner.textContent).toBe(expectedMessage);
      });
    });

    it("uses internal_error when code is undefined", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () =>
            Promise.reject({ message: "Something went wrong" }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toBe("internal_error: Something went wrong");
    });
  });

  describe("6.2 Non-object errors are stringified", () => {
    const nonObjectErrors = [
      { error: "string error", description: "plain string" },
      { error: 42, description: "number" },
      { error: null, description: "null" },
      { error: undefined, description: "undefined" },
    ];

    nonObjectErrors.forEach(({ error, description }) => {
      it(`stringifies ${description} error`, async () => {
        clearCommandHandlers();
        registerDefaultCommands({
          status: defaultStatus({ engine_running: false }),
          commands: {
            start_engine: () => Promise.reject(error),
          },
        });

        vi.resetModules();
        await import("../main.js");
        stubSvgMethods();
        await initApp();

        const btn = getButton("#btn-engine-toggle");
        btn.click();
        await vi.advanceTimersByTimeAsync(0);

        const errorBanner = document.querySelector("#error-banner");
        expect(errorBanner.hidden).toBe(false);
        expect(errorBanner.textContent).toBe(String(error));
      });
    });

    it("handles error object without code property", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () =>
            Promise.reject({ message: "Error without code" }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toBe("internal_error: Error without code");
    });

    it("handles unknown error code with custom message", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () =>
            Promise.reject({ code: "unknown_code", message: "Unknown error details" }),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toBe("unknown_code: Unknown error details");
    });
  });

  describe("6.3 report_gui_error is called but failures are swallowed", () => {
    it("calls report_gui_error when an operation fails", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () =>
            Promise.reject({
              code: "accessibility_permission_missing",
              message: "Permission denied",
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

    it("swallows report_gui_error failures silently", async () => {
      clearCommandHandlers();
      let reportGuiErrorCalled = false;
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () =>
            Promise.reject({
              code: "accessibility_permission_missing",
              message: "Permission denied",
            }),
          report_gui_error: () => {
            reportGuiErrorCalled = true;
            return Promise.reject(new Error("Logging service unavailable"));
          },
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(reportGuiErrorCalled).toBe(true);

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toContain(
        "ZeroG needs Accessibility access"
      );
    });

    it("error banner still shows when report_gui_error fails", async () => {
      clearCommandHandlers();
      registerDefaultCommands({
        status: defaultStatus({ engine_running: false }),
        commands: {
          start_engine: () =>
            Promise.reject({
              code: "socket_bind_failed",
              message: "Port in use",
            }),
          report_gui_error: () => Promise.reject(new Error("Network error")),
        },
      });

      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      const btn = getButton("#btn-engine-toggle");
      btn.click();
      await vi.advanceTimersByTimeAsync(0);

      const errorBanner = document.querySelector("#error-banner");
      expect(errorBanner.hidden).toBe(false);
      expect(errorBanner.textContent).toBe(
        "Socket bind failed. Another process may already be using the networking port."
      );
    });
  });
});
