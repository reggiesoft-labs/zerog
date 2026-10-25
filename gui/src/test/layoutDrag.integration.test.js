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
  buildPeer,
  buildLocalPeer,
  dispatchPointer,
  switchToConfigTab,
  switchToMainTab,
  getInvokeCallCount,
  fireEvent,
  getTextContent,
} from "./helpers.js";

const LAYOUT_COORDINATE_BOUNDS = {
  minX: -500,
  maxX: 500,
  minY: -400,
  maxY: 400,
};
// Local anchor (1440x900) -> 135x84
// Remote peer (1920x1080) -> 180x101
const NODE_SIZE_1920x1080 = { width: 180, height: 101 };
const NODE_SIZE_1440x900 = { width: 135, height: 84 };

const SNAP_TOP = { x: 0, y: -100 }; // - (84/2 + 101/2 + 8) = -(42 + 50.5 + 8) = -100.5 -> -100
const SNAP_BOTTOM = { x: 0, y: 101 }; // 100.5 -> 101
const SNAP_LEFT = { x: -165, y: 0 }; // - (135/2 + 180/2 + 8) = -(67.5 + 90 + 8) = -165.5 -> -165
const SNAP_RIGHT = { x: 166, y: 0 }; // 165.5 -> 166

function isConnectedPlacement(placement) {
  return (
    placement.x === SNAP_LEFT.x ||
    placement.x === SNAP_RIGHT.x ||
    placement.y === SNAP_TOP.y ||
    placement.y === SNAP_BOTTOM.y
  );
}

function getPlacementEdge(placement) {
  if (placement.x === SNAP_RIGHT.x) {
    return "right";
  }
  if (placement.x === SNAP_LEFT.x) {
    return "left";
  }
  if (placement.y === SNAP_TOP.y) {
    return "top";
  }
  if (placement.y === SNAP_BOTTOM.y) {
    return "bottom";
  }
  return "unknown";
}

describe("Layout drag and config updates", () => {
  let savedConfig = null;

  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
    savedConfig = null;
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    savedConfig = null;
    document.documentElement.innerHTML = "";
  });

  async function setupWithConfig(peers, config, commands = {}) {
    registerDefaultCommands({
      peers,
      config,
      commands: {
        save_config: ({ config }) => {
          savedConfig = config;
          return config;
        },
        ...commands,
      },
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

  function getLocalGroup() {
    return document.querySelector("g.layout-node--local");
  }

  function getRenderedPlacement(peerId) {
    const rect = getPeerGroup(peerId)?.querySelector("rect");
    if (!rect) {
      return null;
    }

    const left = Number(rect.getAttribute("x"));
    const top = Number(rect.getAttribute("y"));
    const width = Number(rect.getAttribute("width"));
    const height = Number(rect.getAttribute("height"));
    return {
      x: Math.round(left + width / 2),
      y: Math.round(top + height / 2),
    };
  }

  describe("4.1 Layout diagram rendering", () => {
    it("renders layout diagram on Status tab by default", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [localPeer, remotePeer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: 200,
              y: 100,
              persisted: true,
            },
          ],
        }),
      );

      const mainPane = document.querySelector("#tab-main");
      expect(mainPane.classList.contains("tab-pane--active")).toBe(true);
      const peerGroup = getPeerGroup("peer-a");
      expect(peerGroup).not.toBeNull();
    });

    it("renders local anchor node in SVG", async () => {
      await setupWithConfig([buildLocalPeer()], defaultConfig());

      const localGroup = getLocalGroup();
      expect(localGroup).not.toBeNull();
      expect(localGroup.classList.contains("layout-node--local")).toBe(true);
    });

    it("renders peer node with correct data-peer-id", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      expect(peerGroup).not.toBeNull();
      expect(peerGroup.classList.contains("layout-node--peer")).toBe(true);
      expect(peerGroup.classList.contains("layout-node--persisted")).toBe(true);
    });

    it("renders peer name as text in SVG group", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const textEl = peerGroup.querySelector("text");
      expect(textEl.textContent).toBe("Alpha");
    });

    it("adds layout-node--fallback class when peer is NOT in config placements", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      expect(peerGroup.classList.contains("layout-node--fallback")).toBe(true);
    });
  });

  describe("4.2 Edge-only snapping behavior", () => {
    it("snaps to right edge when dragged near it", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: 400,
              y: 0,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const rect = peerGroup.querySelector("rect");

      dispatchPointer(rect, "pointerdown", {
        clientX: 400,
        clientY: 0,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      // Drag near right edge snap position (158, 0)
      dispatchPointer(svg, "pointermove", {
        clientX: SNAP_RIGHT.x + 5,
        clientY: 5,
        pointerId: 1,
      });

      dispatchPointer(svg, "pointerup", {
        pointerId: 1,
      });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const placement = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(placement.x).toBe(SNAP_RIGHT.x);
      expect(placement.y).toBe(5);
    });

    it("snaps to left edge when dragged near it", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: -400,
              y: 0,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const rect = peerGroup.querySelector("rect");

      dispatchPointer(rect, "pointerdown", {
        clientX: -400,
        clientY: 0,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      // Drag near left edge snap position (-157, 0)
      dispatchPointer(svg, "pointermove", {
        clientX: SNAP_LEFT.x - 5,
        clientY: 5,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const placement = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(placement.x).toBe(SNAP_LEFT.x);
      expect(placement.y).toBe(5);
    });

    it("snaps to top edge when dragged near it", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: 0,
              y: -400,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const rect = peerGroup.querySelector("rect");

      dispatchPointer(rect, "pointerdown", {
        clientX: 0,
        clientY: -400,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      // Drag near top edge snap position (0, -92)
      dispatchPointer(svg, "pointermove", {
        clientX: 5,
        clientY: SNAP_TOP.y - 5,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const placement = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(placement.x).toBe(5);
      expect(placement.y).toBe(SNAP_TOP.y);
    });

    it("snaps to bottom edge when dragged near it", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: 0,
              y: 400,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const rect = peerGroup.querySelector("rect");

      dispatchPointer(rect, "pointerdown", {
        clientX: 0,
        clientY: 400,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      // Drag near bottom edge snap position (0, 93)
      dispatchPointer(svg, "pointermove", {
        clientX: 5,
        clientY: SNAP_BOTTOM.y + 5,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const placement = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(placement.x).toBe(5);
      expect(placement.y).toBe(SNAP_BOTTOM.y);
    });

    it("dragging to interior canvas points still saves connected-edge coordinates", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const rect = peerGroup.querySelector("rect");

      dispatchPointer(rect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      dispatchPointer(svg, "pointermove", {
        clientX: 300,
        clientY: 200,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointermove", {
        clientX: -220,
        clientY: 130,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointermove", {
        clientX: 40,
        clientY: -250,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const placement = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(isConnectedPlacement(placement)).toBe(true);
    });

    it("keeps x fixed while sliding along right edge", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: 400,
              y: 0,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const rect = peerGroup.querySelector("rect");

      dispatchPointer(rect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      dispatchPointer(svg, "pointermove", {
        clientX: 420,
        clientY: -40,
        pointerId: 1,
      });

      const movedRect = getPeerGroup("peer-a").querySelector("rect");
      const midX =
        Number(movedRect.getAttribute("x")) + NODE_SIZE_1920x1080.width / 2;
      const midY =
        Number(movedRect.getAttribute("y")) + NODE_SIZE_1920x1080.height / 2;
      expect(midX).toBe(SNAP_RIGHT.x);
      expect(midY).not.toBe(SNAP_RIGHT.y);

      dispatchPointer(svg, "pointermove", {
        clientX: 420,
        clientY: 80,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const placement = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(placement.x).toBe(SNAP_RIGHT.x);
      expect(placement.y).not.toBe(SNAP_RIGHT.y);
    });

    it("allows edge-adjacent placement (no overlap blocked)", async () => {
      const peerA = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      const peerB = buildPeer({ peer_id: "peer-b", name: "Beta" });

      await setupWithConfig(
        [buildLocalPeer(), peerA, peerB],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
            {
              peer_id: "peer-b",
              peer_label: "Beta",
              x: SNAP_BOTTOM.x,
              y: SNAP_BOTTOM.y,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroupA = getPeerGroup("peer-a");
      const rectA = peerGroupA.querySelector("rect");

      // Drag Peer A from Right to Top (should NOT overlap with Bottom)
      dispatchPointer(rectA, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      dispatchPointer(svg, "pointermove", {
        clientX: SNAP_TOP.x,
        clientY: SNAP_TOP.y,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const placementA = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(placementA.x).toBe(SNAP_TOP.x);
      expect(placementA.y).toBe(SNAP_TOP.y);
    });

    it("blocks snapping to an already occupied edge", async () => {
      const peerA = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      const peerB = buildPeer({ peer_id: "peer-b", name: "Beta" });

      await setupWithConfig(
        [buildLocalPeer(), peerA, peerB],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
            {
              peer_id: "peer-b",
              peer_label: "Beta",
              x: SNAP_TOP.x,
              y: SNAP_TOP.y,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroupA = getPeerGroup("peer-a");
      const rectA = peerGroupA.querySelector("rect");

      // Try to drag Peer A to Top (which is occupied by Peer B)
      dispatchPointer(rectA, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      dispatchPointer(svg, "pointermove", {
        clientX: SNAP_TOP.x,
        clientY: SNAP_TOP.y,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      // Should NOT have saved because Top was filtered out of valid positions
      expect(savedConfig).toBeNull();
    });
  });

  describe("4.3 Placement normalization", () => {
    it("normalizes disconnected persisted placement on render and next save", async () => {
      const peerA = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      const peerB = buildPeer({ peer_id: "peer-b", name: "Beta" });

      await setupWithConfig(
        [buildLocalPeer(), peerA, peerB],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: 120,
              y: 80,
              persisted: true,
            },
            {
              peer_id: "peer-b",
              peer_label: "Beta",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
          ],
        }),
      );

      const renderedA = getRenderedPlacement("peer-a");
      expect(renderedA).not.toBeNull();
      expect(isConnectedPlacement(renderedA)).toBe(true);

      const peerBRect = getPeerGroup("peer-b").querySelector("rect");
      dispatchPointer(peerBRect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      dispatchPointer(svg, "pointermove", {
        clientX: 0,
        clientY: -240,
        pointerId: 1,
      });
      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const savedA = savedConfig.placements.find((p) => p.peer_id === "peer-a");
      expect(savedA).toBeDefined();
      expect(isConnectedPlacement(savedA)).toBe(true);
    });

    it("renders fallback placement connected when persisted entry is missing", async () => {
      const peerA = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peerA],
        defaultConfig({
          placements: [],
        }),
      );

      const renderedA = getRenderedPlacement("peer-a");
      expect(renderedA).not.toBeNull();
      expect(isConnectedPlacement(renderedA)).toBe(true);
      expect(renderedA.x).toBe(SNAP_RIGHT.x);
      expect(renderedA.y).toBe(SNAP_RIGHT.y);
    });

    it("places newly connecting machine to the right of local if it has no position", async () => {
      const peerA = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      const peerB = buildPeer({ peer_id: "peer-b", name: "Beta" });

      await setupWithConfig(
        [buildLocalPeer(), peerA, peerB],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_TOP.x,
              y: SNAP_TOP.y,
              persisted: true,
            },
          ],
        }),
      );

      const renderedB = getRenderedPlacement("peer-b");
      expect(renderedB).not.toBeNull();
      expect(renderedB.x).toBe(SNAP_RIGHT.x);
      expect(renderedB.y).toBe(SNAP_RIGHT.y);
    });
  });

  describe("4.4 Edge transitions and occupancy", () => {
    it("keeps every intermediate drag position connected and transitions right to top", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
          ],
        }),
      );

      const peerRect = getPeerGroup("peer-a").querySelector("rect");
      dispatchPointer(peerRect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      const dragPath = [
        { x: 260, y: 10 },
        { x: 40, y: -260 },
        { x: -280, y: -40 },
        { x: 20, y: 260 },
      ];
      const intermediatePlacements = [];

      for (const point of dragPath) {
        dispatchPointer(svg, "pointermove", {
          clientX: point.x,
          clientY: point.y,
          pointerId: 1,
        });
        intermediatePlacements.push(getRenderedPlacement("peer-a"));
      }

      dispatchPointer(svg, "pointerup", { pointerId: 1 });
      await vi.advanceTimersByTimeAsync(0);

      intermediatePlacements.forEach((placement) => {
        expect(placement).not.toBeNull();
        expect(isConnectedPlacement(placement)).toBe(true);
      });

      const seenEdges = intermediatePlacements
        .filter((placement) => placement !== null)
        .map(getPlacementEdge);
      expect(seenEdges[0]).toBe("right");
      expect(seenEdges[1]).toBe("top");
      expect(seenEdges).toContain("left");
      expect(seenEdges).toContain("bottom");
    });

    it("stops at last non-overlapping connected position when an edge segment is blocked", async () => {
      const peerA = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      const peerB = buildPeer({ peer_id: "peer-b", name: "Beta" });

      await setupWithConfig(
        [buildLocalPeer(), peerA, peerB],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: -120,
              persisted: true,
            },
            {
              peer_id: "peer-b",
              peer_label: "Beta",
              x: SNAP_RIGHT.x,
              y: 40,
              persisted: true,
            },
          ],
        }),
      );

      const peerARect = getPeerGroup("peer-a").querySelector("rect");
      dispatchPointer(peerARect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: -120,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");
      dispatchPointer(svg, "pointermove", {
        clientX: 240,
        clientY: -60,
        pointerId: 1,
      });

      const lastNonOverlappingPlacement = getRenderedPlacement("peer-a");
      expect(lastNonOverlappingPlacement.x).toBe(SNAP_RIGHT.x);

      dispatchPointer(svg, "pointermove", {
        clientX: 240,
        clientY: 20,
        pointerId: 1,
      });

      const afterBlockedMove = getRenderedPlacement("peer-a");
      expect(afterBlockedMove).toEqual(lastNonOverlappingPlacement);

      dispatchPointer(svg, "pointerup", { pointerId: 1 });
      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig).not.toBeNull();
      const savedPlacement = savedConfig.placements.find(
        (placement) => placement.peer_id === "peer-a",
      );
      expect(savedPlacement.x).toBe(lastNonOverlappingPlacement.x);
      expect(savedPlacement.y).toBe(lastNonOverlappingPlacement.y);
      expect(isConnectedPlacement(savedPlacement)).toBe(true);
    });
  });

  describe("4.5 Debounce and UI state", () => {
    it("adds layout-node--active class during drag", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
          ],
        }),
      );

      const rect = document.querySelector('g[data-peer-id="peer-a"] rect');

      dispatchPointer(rect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const peerGroup = getPeerGroup("peer-a");
      expect(peerGroup.classList.contains("layout-node--active")).toBe(true);

      const svg = document.querySelector("#layout-svg");
      dispatchPointer(svg, "pointerup", { pointerId: 1 });
    });

    it("saves exactly once for rapid connected pointermove bursts", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
              persisted: true,
            },
          ],
        }),
      );

      const peerGroup = getPeerGroup("peer-a");
      const rect = peerGroup.querySelector("rect");

      const saveCountBefore = getInvokeCallCount("save_config");

      dispatchPointer(rect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });

      const svg = document.querySelector("#layout-svg");

      // Multiple moves inside one debounce window
      for (let i = 1; i <= 5; i++) {
        dispatchPointer(svg, "pointermove", {
          clientX: 300 + i * 10,
          clientY: 200 + i * 5,
          pointerId: 1,
        });
      }

      dispatchPointer(svg, "pointerup", { pointerId: 1 });

      await vi.advanceTimersByTimeAsync(250);

      expect(getInvokeCallCount("save_config") - saveCountBefore).toBe(1);

      const placement = savedConfig.placements.find(
        (p) => p.peer_id === "peer-a",
      );
      expect(isConnectedPlacement(placement)).toBe(true);
    });

    it("preserves unrelated config fields when saving a layout", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      const config = defaultConfig({
        movement_cadence_us: 1234,
        remote_layout_updated: true,
        legacy_issues: [
          { peer_name: "Old Alpha", reason: "reselect", location: "left" },
        ],
        zones: [
          {
            edge: "left",
            peer_id: "peer-a",
            peer_label: "Alpha",
            start_pct: 0,
            end_pct: 0.5,
          },
        ],
        placements: [
          {
            peer_id: "peer-a",
            peer_label: "Alpha",
            x: SNAP_RIGHT.x,
            y: SNAP_RIGHT.y,
          },
        ],
      });

      await setupWithConfig([buildLocalPeer(), peer], config);

      const rect = getPeerGroup("peer-a").querySelector("rect");
      dispatchPointer(rect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });
      dispatchPointer(document.querySelector("#layout-svg"), "pointermove", {
        clientX: 300,
        clientY: 120,
        pointerId: 1,
      });
      dispatchPointer(document.querySelector("#layout-svg"), "pointerup", {
        pointerId: 1,
      });
      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig.movement_cadence_us).toBe(1234);
      expect(savedConfig.remote_layout_updated).toBe(true);
      expect(savedConfig.legacy_issues).toEqual(config.legacy_issues);
      expect(savedConfig.zones).toEqual(config.zones);
    });

    it("keeps local edits when a remote config arrives during a drag save", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
            },
          ],
        }),
      );

      const rect = getPeerGroup("peer-a").querySelector("rect");
      const svg = document.querySelector("#layout-svg");
      dispatchPointer(rect, "pointerdown", {
        clientX: SNAP_RIGHT.x,
        clientY: SNAP_RIGHT.y,
        pointerId: 1,
        button: 0,
      });
      dispatchPointer(svg, "pointermove", {
        clientX: 300,
        clientY: 120,
        pointerId: 1,
      });

      fireEvent("config-sync-received", {
        config: defaultConfig({
          legacy_issues: [
            { peer_name: "Remote", reason: "changed", location: "right" },
          ],
        }),
      });
      expect(getTextContent("#config-status")).toBe(
        "Remote layout update waiting for local changes.",
      );

      dispatchPointer(svg, "pointerup", { pointerId: 1 });
      await vi.advanceTimersByTimeAsync(0);

      expect(savedConfig.placements).toHaveLength(1);
      expect(savedConfig.legacy_issues).toEqual([]);
      expect(getTextContent("#config-status")).toBe("");
    });

    it("sends the newest edit after an earlier save finishes", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });
      const saveCalls = [];
      let resolveFirstSave;
      const firstSave = new Promise((resolve) => {
        resolveFirstSave = resolve;
      });
      await setupWithConfig(
        [buildLocalPeer(), peer],
        defaultConfig({
          placements: [
            {
              peer_id: "peer-a",
              peer_label: "Alpha",
              x: SNAP_RIGHT.x,
              y: SNAP_RIGHT.y,
            },
          ],
        }),
        {
          save_config: ({ config }) => {
            saveCalls.push(config);
            return saveCalls.length === 1 ? firstSave : config;
          },
        },
      );

      const svg = document.querySelector("#layout-svg");
      const movePeer = (pointerId, y) => {
        const rect = getPeerGroup("peer-a").querySelector("rect");
        dispatchPointer(rect, "pointerdown", {
          clientX: SNAP_RIGHT.x,
          clientY: y,
          pointerId,
          button: 0,
        });
        dispatchPointer(svg, "pointermove", {
          clientX: 300,
          clientY: y,
          pointerId,
        });
        dispatchPointer(svg, "pointerup", { pointerId });
      };

      movePeer(1, 20);
      expect(saveCalls).toHaveLength(1);
      movePeer(2, 70);
      expect(saveCalls).toHaveLength(1);

      resolveFirstSave(saveCalls[0]);
      await vi.advanceTimersByTimeAsync(0);
      await Promise.resolve();

      expect(saveCalls).toHaveLength(2);
      expect(saveCalls[1].placements[0].y).not.toBe(
        saveCalls[0].placements[0].y,
      );
    });
  });

  describe("4.6 Disconnected peer visibility in layout diagram", () => {
    it("does not render disconnected peers in the layout diagram", async () => {
      const localPeer = buildLocalPeer();
      const connectedPeer = buildPeer({ peer_id: "peer-connected", name: "Connected" });
      const disconnectedPeer = buildPeer({ peer_id: "peer-disconnected", name: "Disconnected" });

      registerDefaultCommands({
        peers: [localPeer, connectedPeer, disconnectedPeer],
        status: defaultStatus({
          connected_peers: [{ peer_id: "peer-connected", name: "Connected" }],
        }),
        config: defaultConfig(),
      });
      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      expect(getPeerGroup("peer-connected")).not.toBeNull();
      expect(getPeerGroup("peer-disconnected")).toBeNull();
    });

    it("removes peer from layout diagram immediately on disconnect", async () => {
      const localPeer = buildLocalPeer();
      const peerA = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithConfig([localPeer, peerA], defaultConfig());

      expect(getPeerGroup("peer-a")).not.toBeNull();

      fireEvent("status-updated", {
        status: defaultStatus({
          connected_peers: [],
        }),
      });

      expect(getPeerGroup("peer-a")).toBeNull();
    });
  });
});
