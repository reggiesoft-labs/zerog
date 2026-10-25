import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  initApp,
  clearCommandHandlers,
  fireEvent,
  getTextContent,
} from "./helpers.js";

describe("Pairing dialog authentication code", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    loadHtml();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  async function showPairingRequest(payload) {
    registerDefaultCommands();
    vi.resetModules();
    await import("../main.js");
    await initApp();
    fireEvent("pairing-requested", {
      peer_id: "peer-remote",
      name: "Remote Machine",
      platform: "macos",
      ...payload,
    });
  }

  it("shows the code and an approve button when this side must decide", async () => {
    await showPairingRequest({ code: "123456", can_respond: true });

    expect(getTextContent("#pairing-peer-code")).toBe("123 456");
    expect(document.querySelector("#pairing-peer-code-block").hidden).toBe(false);
    expect(document.querySelector("#btn-pairing-allow").hidden).toBe(false);
    expect(document.querySelector("#btn-pairing-block").hidden).toBe(false);
  });

  it("shows the code without approve buttons when waiting for the peer", async () => {
    await showPairingRequest({ code: "042042", can_respond: false });

    expect(getTextContent("#pairing-peer-code")).toBe("042 042");
    expect(document.querySelector("#pairing-peer-code-block").hidden).toBe(false);
    expect(document.querySelector("#btn-pairing-allow").hidden).toBe(true);
    expect(document.querySelector("#btn-pairing-block").hidden).toBe(true);
    expect(document.querySelector("#btn-pairing-cancel").hidden).toBe(false);
  });

  it("shows cancel button only for outbound waits", async () => {
    await showPairingRequest({ code: "123456", can_respond: true });
    expect(document.querySelector("#btn-pairing-cancel").hidden).toBe(true);
  });

  it("hides the code when the payload does not carry one", async () => {
    await showPairingRequest({ code: null, can_respond: true });

    expect(document.querySelector("#pairing-peer-code-block").hidden).toBe(true);
  });
});
