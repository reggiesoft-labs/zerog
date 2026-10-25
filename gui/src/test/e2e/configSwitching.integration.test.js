import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  defaultConfig,
  defaultStatus,
  buildPeer,
  buildLocalPeer,
  switchToConfigTab,
  getTextContent,
  fireEvent,
} from "../helpers.js";

describe("Config switching validation", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  describe("6.1 Legacy issues display correctly", () => {
    it("config with legacy_issues shows issue messages in status", async () => {
      loadHtml();
      const configWithIssues = defaultConfig({
        legacy_issues: [
          { peer_name: "Beta", reason: "needs reselection", location: "left" },
          { peer_name: "Alpha", reason: "unreachable", location: "right" },
        ],
      });
      registerDefaultCommands({
        peers: [buildLocalPeer()],
        status: defaultStatus(),
        config: configWithIssues,
      });

      vi.resetModules();
      await import("../../main.js");
      stubSvgMethods();
      await initApp();
      await switchToConfigTab();

      const statusText = getTextContent("#config-status");
      expect(statusText).toContain("Beta");
      expect(statusText).toContain("needs reselection");
      expect(statusText).toContain("left");
    });
  });

  describe("6.2 Rapid config switching maintains consistency", () => {
    it("rapidly switching between 3 configs leaves final config as last", async () => {
      loadHtml();
      const configs = [
        defaultConfig({ left: "peer-a", placements: [{ peer_id: "peer-a", x: 166, y: 0, peer_label: "Alpha" }] }),
        defaultConfig({ right: "peer-b", placements: [{ peer_id: "peer-b", x: -165, y: 0, peer_label: "Beta" }] }),
        defaultConfig({ 
          left: "peer-c", 
          placements: [{ peer_id: "peer-c", x: 0, y: -100, peer_label: "Gamma" }],
          legacy_issues: [{ peer_name: "Gamma", reason: "final-config", location: "left" }]
        }),
      ];

      let currentConfigIdx = 0;
      registerDefaultCommands({
        peers: [buildLocalPeer(), buildPeer({ peer_id: "peer-a" }), buildPeer({ peer_id: "peer-b" }), buildPeer({ peer_id: "peer-c" })],
        status: defaultStatus(),
        config: () => configs[currentConfigIdx],
      });

      vi.resetModules();
      await import("../../main.js");
      stubSvgMethods();
      await initApp();
      await switchToConfigTab();

      for (let i = 0; i < 5; i++) {
        for (let j = 0; j < configs.length; j++) {
          fireEvent("config-sync-received", { config: configs[j] });
          await vi.advanceTimersByTimeAsync(0);
        }
      }

      expect(getTextContent("#config-status")).toContain("Gamma");
      expect(getTextContent("#config-status")).toContain("final-config");
    });
  });
});
