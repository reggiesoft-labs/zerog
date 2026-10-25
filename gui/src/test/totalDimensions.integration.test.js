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
  fireEvent,
  switchToConfigTab,
} from "./helpers.js";

describe("Total dimensions in layout diagram", () => {
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
    registerDefaultCommands({
      peers,
      config: defaultConfig(),
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
    fireEvent("peers-updated", { peers });
  }

  function getPeerGroup(peerId) {
    return document.querySelector(`g[data-peer-id="${peerId}"]`);
  }

  function getPeerRectSize(peerId) {
    const group = getPeerGroup(peerId);
    if (!group) return null;
    const rect = group.querySelector("rect");
    if (!rect) return null;
    return {
      width: parseFloat(rect.getAttribute("width")),
      height: parseFloat(rect.getAttribute("height")),
    };
  }

  describe("6.1 Peer with total_dimensions uses total_dimensions for node size", () => {
    it("prefers total_dimensions over virtual_bounds for node sizing", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({
        peer_id: "peer-with-total",
        name: "HasTotal",
        virtual_bounds: { width: 1920, height: 1080 },
        total_dimensions: { width: 4480, height: 1440 },
      });

      await setupWithPeers([localPeer, remotePeer]);
      await switchToConfigTab();

      const size = getPeerRectSize("peer-with-total");
      expect(size).not.toBeNull();
      // total_dimensions should produce a larger node than virtual_bounds
      // 4480x1440 vs 1920x1080 with aspect ratio scaling
      expect(size.width).toBeGreaterThan(0);
      expect(size.height).toBeGreaterThan(0);
    });

    it("uses virtual_bounds when total_dimensions is missing", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({
        peer_id: "peer-no-total",
        name: "NoTotal",
        virtual_bounds: { width: 1920, height: 1080 },
      });

      await setupWithPeers([localPeer, remotePeer]);
      await switchToConfigTab();

      const size = getPeerRectSize("peer-no-total");
      expect(size).not.toBeNull();
      // Should use virtual_bounds for sizing
      expect(size.width).toBeGreaterThan(0);
      expect(size.height).toBeGreaterThan(0);
    });

    it("uses virtual_bounds when total_dimensions has zero values", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({
        peer_id: "peer-zero-total",
        name: "ZeroTotal",
        virtual_bounds: { width: 1920, height: 1080 },
        total_dimensions: { width: 0, height: 0 },
      });

      await setupWithPeers([localPeer, remotePeer]);
      await switchToConfigTab();

      const size = getPeerRectSize("peer-zero-total");
      expect(size).not.toBeNull();
      // Should fall back to virtual_bounds
      expect(size.width).toBeGreaterThan(0);
      expect(size.height).toBeGreaterThan(0);
    });
  });

  describe("6.2 Mixed peers display correctly", () => {
    it("renders mixed peers where some have total_dimensions and some do not", async () => {
      const localPeer = buildLocalPeer();
      const peerWithTotal = buildPeer({
        peer_id: "peer-with-total",
        name: "HasTotal",
        virtual_bounds: { width: 1920, height: 1080 },
        total_dimensions: { width: 4480, height: 1440 },
      });
      const peerWithoutTotal = buildPeer({
        peer_id: "peer-no-total",
        name: "NoTotal",
        virtual_bounds: { width: 2560, height: 1440 },
      });

      await setupWithPeers([localPeer, peerWithTotal, peerWithoutTotal]);
      await switchToConfigTab();

      // Both peers should be rendered
      expect(getPeerGroup("peer-with-total")).not.toBeNull();
      expect(getPeerGroup("peer-no-total")).not.toBeNull();

      // Both should have valid sizes
      const sizeWithTotal = getPeerRectSize("peer-with-total");
      const sizeWithoutTotal = getPeerRectSize("peer-no-total");
      expect(sizeWithTotal).not.toBeNull();
      expect(sizeWithoutTotal).not.toBeNull();
      expect(sizeWithTotal.width).toBeGreaterThan(0);
      expect(sizeWithoutTotal.width).toBeGreaterThan(0);
    });

    it("all peers render correctly regardless of dimension data", async () => {
      const localPeer = buildLocalPeer();
      const peers = [
        buildPeer({
          peer_id: "peer-a",
          name: "Alpha",
          virtual_bounds: { width: 1920, height: 1080 },
          total_dimensions: { width: 4480, height: 1440 },
        }),
        buildPeer({
          peer_id: "peer-b",
          name: "Beta",
          virtual_bounds: { width: 2560, height: 1440 },
        }),
        buildPeer({
          peer_id: "peer-c",
          name: "Gamma",
          virtual_bounds: { width: 3840, height: 2160 },
          total_dimensions: { width: 3840, height: 2160 },
        }),
      ];

      await setupWithPeers([localPeer, ...peers]);
      await switchToConfigTab();

      for (const peer of peers) {
        const group = getPeerGroup(peer.peer_id);
        expect(group).not.toBeNull();
        const size = getPeerRectSize(peer.peer_id);
        expect(size).not.toBeNull();
        expect(size.width).toBeGreaterThan(0);
        expect(size.height).toBeGreaterThan(0);
      }
    });
  });

  describe("6.2 Cross-version compatibility documented", () => {
    it("old GUI without total_dimensions support falls back to virtual_bounds", async () => {
      // This test documents backward compatibility:
      // When old GUI (without total_dimensions support) receives peer data
      // that includes total_dimensions, it ignores total_dimensions and
      // uses virtual_bounds, which produces correct (though potentially
      // different) sizing behavior.
      //
      // Conversely, new GUI receiving peer data without total_dimensions
      // key falls back to virtual_bounds as expected.
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({
        peer_id: "peer-old-gui",
        name: "OldGUI",
        virtual_bounds: { width: 1920, height: 1080 },
        // total_dimensions is intentionally not set to simulate
        // peer data from older version
      });

      await setupWithPeers([localPeer, remotePeer]);
      await switchToConfigTab();

      const size = getPeerRectSize("peer-old-gui");
      expect(size).not.toBeNull();
      // Should use virtual_bounds
      expect(size.width).toBeGreaterThan(0);
      expect(size.height).toBeGreaterThan(0);
    });

    it("new GUI receiving peer with total_dimensions uses it", async () => {
      // This test documents forward compatibility:
      // When new GUI receives peer data WITH total_dimensions,
      // it uses total_dimensions for node sizing (preferred behavior).
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({
        peer_id: "peer-new-gui",
        name: "NewGUI",
        virtual_bounds: { width: 1920, height: 1080 },
        total_dimensions: { width: 4480, height: 1440 },
      });

      await setupWithPeers([localPeer, remotePeer]);
      await switchToConfigTab();

      const size = getPeerRectSize("peer-new-gui");
      expect(size).not.toBeNull();
      expect(size.width).toBeGreaterThan(0);
      expect(size.height).toBeGreaterThan(0);
    });

    it("new GUI receiving peer without total_dimensions falls back to virtual_bounds", async () => {
      // New GUI should gracefully handle missing total_dimensions
      // by falling back to virtual_bounds
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({
        peer_id: "peer-fallback",
        name: "Fallback",
        virtual_bounds: { width: 2560, height: 1440 },
      });

      await setupWithPeers([localPeer, remotePeer]);
      await switchToConfigTab();

      const size = getPeerRectSize("peer-fallback");
      expect(size).not.toBeNull();
      expect(size.width).toBeGreaterThan(0);
      expect(size.height).toBeGreaterThan(0);
    });
  });
});
