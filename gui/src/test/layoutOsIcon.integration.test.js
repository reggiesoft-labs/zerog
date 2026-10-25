import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  defaultConfig,
  buildPeer,
  buildLocalPeer,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  fireEvent,
} from "./helpers.js";

describe("Layout OS icons", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  async function setupWithPeers(peers) {
    registerDefaultCommands({ peers, config: defaultConfig() });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
    fireEvent("peers-updated", { peers });
  }

  it("renders platform icons on local and peer nodes, none for unknown platforms", async () => {
    await setupWithPeers([
      buildLocalPeer({ platform: "windows" }),
      buildPeer({ peer_id: "peer-mac", name: "Mac", platform: "macos" }),
      buildPeer({ peer_id: "peer-linux", name: "Tux", platform: "linux" }),
      buildPeer({ peer_id: "peer-unknown", name: "Mystery", platform: "freebsd" }),
    ]);

    expect(
      document.querySelector("g.layout-node--local .layout-node-os--windows"),
    ).not.toBeNull();
    expect(
      document.querySelector('g[data-peer-id="peer-mac"] .layout-node-os--apple'),
    ).not.toBeNull();
    expect(
      document.querySelector('g[data-peer-id="peer-linux"] .layout-node-os--linux'),
    ).not.toBeNull();
    expect(
      document.querySelector('g[data-peer-id="peer-unknown"] .layout-node-os'),
    ).toBeNull();
  });

  it("renders rich multi-line tooltips with OS, status, and resolution", async () => {
    await setupWithPeers([
      buildLocalPeer({ platform: "macos", name: "HostMac", addr: "192.168.1.10" }),
      buildPeer({
        peer_id: "peer-linux",
        name: "LinuxBox",
        platform: "linux",
        addr: "192.168.1.50:54321",
        virtual_bounds: { width: 1920, height: 1080 },
      }),
    ]);

    const localTitle = document.querySelector("g.layout-node--local title");
    expect(localTitle?.textContent).toContain("HostMac");
    expect(localTitle?.textContent).toContain("Role: This machine (Local Anchor)");
    expect(localTitle?.textContent).toContain("OS: macOS");
    expect(localTitle?.textContent).toContain("Address: 192.168.1.10");

    const peerTitle = document.querySelector('g[data-peer-id="peer-linux"] title');
    expect(peerTitle?.textContent).toContain("LinuxBox");
    expect(peerTitle?.textContent).toContain("OS: Linux");
    expect(peerTitle?.textContent).toContain("Resolution: 1920 × 1080");
    expect(peerTitle?.textContent).toContain("Address: 192.168.1.50:54321");
  });

  it("falls back to hostname for local tooltip when addr is local sentinel", async () => {
    await setupWithPeers([
      buildLocalPeer({
        platform: "macos",
        name: "HostMac",
        addr: "local",
        hostname: "macbook.local",
      }),
    ]);

    const localTitle = document.querySelector("g.layout-node--local title");
    expect(localTitle?.textContent).toContain("Address: macbook.local");
  });

  it("renders YOU anchor badge on local node and not on peer nodes", async () => {
    await setupWithPeers([
      buildLocalPeer({ platform: "macos", name: "HostMac" }),
      buildPeer({ peer_id: "peer-linux", name: "LinuxBox", platform: "linux" }),
    ]);

    const localYouBadge = document.querySelector("g.layout-node--local .layout-node-anchor-badge");
    expect(localYouBadge).not.toBeNull();
    expect(localYouBadge?.querySelector("text")?.textContent).toBe("YOU");

    const peerYouBadge = document.querySelector('g[data-peer-id="peer-linux"] .layout-node-anchor-badge');
    expect(peerYouBadge).toBeNull();
  });
});
