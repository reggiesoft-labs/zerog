import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  registerCommand,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  fireEvent,
  getButton,
} from "./helpers.js";

describe("Software updater integration", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
    registerDefaultCommands();
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    localStorage.clear();
    document.documentElement.innerHTML = "";
  });

  it("reports up-to-date when no update is available", async () => {
    registerCommand("check_for_updates", () => ({
      available: false,
      current_version: "1.0.0",
      version: null,
      date: null,
      body: null,
    }));

    const checkBtn = getButton("#btn-check-updates");
    const installBtn = getButton("#btn-install-update");
    const statusText = document.querySelector("#updater-status-text");

    expect(checkBtn).not.toBeNull();
    expect(installBtn.hidden).toBe(true);

    checkBtn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(statusText.textContent).toContain("ZeroG v1.0.0 is up to date");
    expect(installBtn.hidden).toBe(true);
    expect(checkBtn.disabled).toBe(false);
  });

  it("automatically downloads and stages update when auto-update is enabled", async () => {
    let installArgs = null;
    let resolveInstall;
    const installPromise = new Promise((resolve) => {
      resolveInstall = resolve;
    });
    registerCommand("check_for_updates", () => ({
      available: true,
      current_version: "1.0.0-beta.2",
      version: "1.0.0",
      date: "2026-10-19",
      body: "Version 1.0.0 release",
    }));
    registerCommand("install_update", (args) => {
      installArgs = args;
      return installPromise;
    });

    let restarted = false;
    registerCommand("restart_app", () => {
      restarted = true;
    });

    const checkBtn = getButton("#btn-check-updates");
    const installBtn = getButton("#btn-install-update");
    const statusText = document.querySelector("#updater-status-text");
    const banner = document.querySelector("#update-banner");
    const badge = document.querySelector("#settings-update-badge");
    const progressContainer = document.querySelector("#updater-progress-container");
    const progressBar = document.querySelector("#updater-progress-bar");

    checkBtn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(installArgs).toEqual({ restart: false });
    expect(installBtn.textContent).toBe("Downloading...");
    expect(installBtn.disabled).toBe(true);
    expect(progressContainer.hidden).toBe(false);

    fireEvent("updater-download-progress", {
      chunk_length: 500000,
      content_length: 1000000,
    });
    expect(progressBar.style.width).toBe("50%");

    resolveInstall();
    await vi.advanceTimersByTimeAsync(0);

    expect(statusText.textContent).toBe("ZeroG v1.0.0 is ready. Restart to apply.");
    expect(installBtn.hidden).toBe(false);
    expect(installBtn.disabled).toBe(false);
    expect(installBtn.textContent).toBe("Restart Now");
    expect(banner.hidden).toBe(false);
    expect(badge.hidden).toBe(false);
    expect(document.querySelector("#update-banner-text").textContent).toBe("ZeroG v1.0.0 is ready to apply.");

    installBtn.click();
    await vi.advanceTimersByTimeAsync(0);
    expect(restarted).toBe(true);
  });

  it("shows update available without auto-downloading when auto-update is disabled", async () => {
    let installCalled = false;
    let installArgs = null;
    let resolveInstall;
    const installPromise = new Promise((resolve) => {
      resolveInstall = resolve;
    });
    registerCommand("check_for_updates", () => ({
      available: true,
      current_version: "1.0.0-beta.2",
      version: "1.0.0",
      date: "2026-10-19",
      body: "Version 1.0.0 release",
    }));
    registerCommand("install_update", (args) => {
      installCalled = true;
      installArgs = args;
      return installPromise;
    });

    const autoToggle = document.querySelector("#auto-update-toggle");
    autoToggle.checked = false;
    autoToggle.dispatchEvent(new Event("change"));
    expect(localStorage.getItem("zerog_auto_update")).toBe("false");

    const checkBtn = getButton("#btn-check-updates");
    const installBtn = getButton("#btn-install-update");
    const statusText = document.querySelector("#updater-status-text");
    const banner = document.querySelector("#update-banner");
    const dismissBtn = getButton("#btn-banner-dismiss");

    checkBtn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(installCalled).toBe(false);
    expect(statusText.textContent).toBe("Update available: 1.0.0");
    expect(installBtn.hidden).toBe(false);
    expect(installBtn.disabled).toBe(false);
    expect(installBtn.textContent).toBe("Install & Restart");
    expect(banner.hidden).toBe(false);
    expect(document.querySelector("#update-banner-text").textContent).toBe("ZeroG 1.0.0 is available.");

    dismissBtn.click();
    expect(banner.hidden).toBe(true);

    installBtn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(installCalled).toBe(true);
    expect(installArgs).toEqual({ restart: true });
    expect(installBtn.disabled).toBe(true);

    resolveInstall();
    await vi.advanceTimersByTimeAsync(0);
    expect(statusText.textContent).toBe("Update complete. Restarting ZeroG...");
  });

  it("handles update check failure gracefully in manual check", async () => {
    registerCommand("check_for_updates", () => {
      throw new Error("Network unreachable");
    });

    const checkBtn = getButton("#btn-check-updates");
    const statusText = document.querySelector("#updater-status-text");

    checkBtn.click();
    await vi.advanceTimersByTimeAsync(0);

    expect(statusText.textContent).toContain("Update check failed: Network unreachable");
    expect(checkBtn.disabled).toBe(false);
  });
});
