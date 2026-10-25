import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import {
  clearCommandHandlers,
  initApp,
  loadHtml,
  registerDefaultCommands,
} from "./helpers.js";

describe.skip("License GUI integration (dormant until Phase 2)", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
    HTMLDialogElement.prototype.showModal = vi.fn(function () {
      this.open = true;
    });
    HTMLDialogElement.prototype.close = vi.fn(function () {
      this.open = false;
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  async function setupApp(licenseOverrides = {}, commandOverrides = {}) {
    registerDefaultCommands({
      license: licenseOverrides,
      commands: commandOverrides,
    });
    vi.resetModules();
    await import("../main.js");
    await initApp();
  }

  it("renders Free tier by default", async () => {
    await setupApp({ status: "free" });

    const badge = document.querySelector("#license-tier-badge");
    const text = document.querySelector("#license-status-text");
    const activateBtn = document.querySelector("#btn-activate-license");
    const removeBtn = document.querySelector("#btn-remove-license");

    expect(badge.textContent).toBe("Personal Free");
    expect(badge.className).toContain("license-badge--free");
    expect(text.textContent).toContain("Free tier");
    expect(activateBtn.textContent).toBe("Activate Pro");
    expect(removeBtn.hidden).toBe(true);
  });

  it("renders Pro Active tier when license is active", async () => {
    await setupApp({
      status: "active",
      licensee: "Test Licensee",
      tier: "pro",
      expires_at: null,
    });

    const badge = document.querySelector("#license-tier-badge");
    const text = document.querySelector("#license-status-text");
    const activateBtn = document.querySelector("#btn-activate-license");
    const removeBtn = document.querySelector("#btn-remove-license");

    expect(badge.textContent).toBe("Pro Active");
    expect(badge.className).toContain("license-badge--pro");
    expect(text.textContent).toContain("Test Licensee");
    expect(activateBtn.textContent).toBe("Change Key");
    expect(removeBtn.hidden).toBe(false);
  });

  it("opens dialog on Activate Pro click", async () => {
    await setupApp({ status: "free" });

    const activateBtn = document.querySelector("#btn-activate-license");
    const dialog = document.querySelector("#license-dialog");

    activateBtn.click();
    expect(dialog.showModal).toHaveBeenCalled();
  });

  it("submitting license key activates Pro", async () => {
    let setKeyCalledWith = null;
    await setupApp(
      { status: "free" },
      {
        set_license_key: (payload) => {
          setKeyCalledWith = payload.licenseKey;
          return {
            status: "active",
            licensee: "Test Licensee",
            tier: "pro",
            expires_at: null,
          };
        },
      }
    );

    const activateBtn = document.querySelector("#btn-activate-license");
    activateBtn.click();

    const input = document.querySelector("#license-key-input");
    const submitBtn = document.querySelector("#btn-license-submit");
    const dialog = document.querySelector("#license-dialog");

    input.value = "v1.test_payload.test_sig";
    submitBtn.click();

    await vi.advanceTimersByTimeAsync(0);

    expect(setKeyCalledWith).toBe("v1.test_payload.test_sig");
    expect(dialog.close).toHaveBeenCalled();

    const badge = document.querySelector("#license-tier-badge");
    expect(badge.textContent).toBe("Pro Active");
  });

  it("removing license returns tier to Free", async () => {
    let removeCalled = false;
    await setupApp(
      {
        status: "active",
        licensee: "Test Licensee",
        tier: "pro",
        expires_at: null,
      },
      {
        remove_license_key: () => {
          removeCalled = true;
          return { status: "free" };
        },
      }
    );

    const removeBtn = document.querySelector("#btn-remove-license");
    removeBtn.click();

    await vi.advanceTimersByTimeAsync(0);

    expect(removeCalled).toBe(true);
    const badge = document.querySelector("#license-tier-badge");
    expect(badge.textContent).toBe("Personal Free");
  });
});
