import { describe, expect, it } from "vitest";
import { normalizeConnectedPlacement } from "./layoutMath.js";
import {
  buildConfigPayload,
  createLayoutModel,
  getAnchorRect,
  placementWouldOverlap,
} from "./layoutModel.js";

const FULL_HD = { x: 0, y: 0, width: 1920, height: 1080 };

function makePeer(peerId, bounds = FULL_HD) {
  return {
    peer_id: peerId,
    name: peerId,
    is_local: false,
    connected: true,
    virtual_bounds: bounds,
  };
}

const localPeer = { ...makePeer("local"), is_local: true };

function buildModel(placements, remotePeers) {
  return createLayoutModel({ placements }, [localPeer, ...remotePeers], {});
}

function expectNoOverlaps(model) {
  const placements = new Map(model.placements.map((item) => [item.peerId, item]));
  for (const item of model.placements) {
    expect(
      placementWouldOverlap(item.peerId, item.x, item.y, item.size, {
        placements,
        localPeer: model.localPeer,
      }),
    ).toBe(false);
  }
}

function expectTouchesLocalEdge(model, item) {
  const anchorRect = getAnchorRect(model.localPeer);
  expect(normalizeConnectedPlacement(item.x, item.y, item.size, anchorRect)).toEqual({
    x: item.x,
    y: item.y,
  });
}

describe("createLayoutModel fallback placement", () => {
  it("keeps a new peer off a persisted peer at its default spot", () => {
    const mac = makePeer("mac");
    const persistedMac = buildModel([], [mac]).placements[0];
    const placements = [{ peer_id: "mac", x: persistedMac.x, y: persistedMac.y }];

    const model = buildModel(placements, [
      mac,
      makePeer("win", { x: 0, y: 0, width: 1280, height: 800 }),
    ]);

    const byId = new Map(model.placements.map((item) => [item.peerId, item]));
    expect(byId.get("mac")).toMatchObject({
      x: persistedMac.x,
      y: persistedMac.y,
      persisted: true,
    });
    expect(byId.get("win").persisted).toBe(false);
    expectNoOverlaps(model);
    expectTouchesLocalEdge(model, byId.get("win"));
  });

  it("spreads several new peers around persisted ones without overlap", () => {
    const persisted = makePeer("a-persisted");
    const persistedSpot = buildModel([], [persisted]).placements[0];
    const model = buildModel(
      [{ peer_id: "a-persisted", x: persistedSpot.x, y: persistedSpot.y }],
      [persisted, makePeer("b-new"), makePeer("c-new"), makePeer("d-new")],
    );

    expectNoOverlaps(model);
    for (const item of model.placements) {
      expectTouchesLocalEdge(model, item);
    }
  });

  it("moves a later persisted placement off an earlier one it overlaps", () => {
    // A remote layout merged ahead of a stale local placement for a peer the
    // sender does not know, both on the local right edge.
    const peers = [
      makePeer("mac", { x: 0, y: 0, width: 3024, height: 1964 }),
      makePeer("windows"),
    ];
    const rightSpot = buildModel([], [makePeer("windows")]).placements[0];
    const model = buildModel(
      [
        { peer_id: "windows", x: rightSpot.x, y: rightSpot.y },
        { peer_id: "mac", x: rightSpot.x + 24, y: rightSpot.y - 3 },
      ],
      peers,
    );

    const byId = new Map(model.placements.map((item) => [item.peerId, item]));
    expect(byId.get("windows")).toMatchObject({ x: rightSpot.x, y: rightSpot.y, repaired: false });
    expect(byId.get("mac").repaired).toBe(true);
    expectNoOverlaps(model);
    expectTouchesLocalEdge(model, byId.get("mac"));

    const resaved = buildModel(
      model.placements.map(({ peerId, x, y }) => ({ peer_id: peerId, x, y })),
      peers,
    );
    expect(resaved.placements.some((item) => item.repaired)).toBe(false);
  });
});

describe("createLayoutModel with three machines", () => {
  const anchorRect = getAnchorRect(localPeer);
  const size = { width: 180, height: 101 };
  const rightX = anchorRect.width / 2 + 8 + size.width / 2;

  it("keeps a peer that sits beyond another peer instead of snapping it onto local", () => {
    // Row of three seen from the left end, as rebased from the middle machine.
    const near = Math.round(rightX);
    const far = Math.round(rightX + size.width + 8);
    const model = buildModel(
      [
        { peer_id: "middle", x: near, y: 0 },
        { peer_id: "far", x: far, y: 0 },
      ],
      [makePeer("middle"), makePeer("far")],
    );

    const byId = new Map(model.placements.map((item) => [item.peerId, item]));
    expect(byId.get("middle")).toMatchObject({ x: near, y: 0 });
    expect(byId.get("far")).toMatchObject({ x: far, y: 0 });
    expectNoOverlaps(model);
  });

  it("leaves placements for peers this machine does not know untouched on save", () => {
    const config = {
      placements: [
        { peer_id: "middle", x: Math.round(rightX), y: 0 },
        { peer_id: "stranger", x: 900, y: 300 },
      ],
    };

    const payload = buildConfigPayload(config, [localPeer, makePeer("middle")]);

    expect(payload.placements.find((p) => p.peer_id === "stranger")).toMatchObject({
      x: 900,
      y: 300,
    });
  });

  it("snaps a floating placement back onto the local display", () => {
    const model = buildModel(
      [{ peer_id: "lost", x: 900, y: 0 }],
      [makePeer("lost")],
    );

    expectTouchesLocalEdge(model, model.placements[0]);
  });

  it("gives unplaced peers the edges the runtime falls back to", () => {
    // Runtime: placed peer keeps the right edge; unplaced peers take the
    // remaining edges in peer id order (left, then top).
    const model = buildModel(
      [{ peer_id: "placed", x: Math.round(rightX), y: 0 }],
      [makePeer("placed"), makePeer("b-zulu"), makePeer("a-yankee")],
    );
    const byId = new Map(model.placements.map((item) => [item.peerId, item]));

    expect(byId.get("a-yankee").x).toBeLessThan(0);
    expect(byId.get("a-yankee").y).toBe(0);
    expect(byId.get("b-zulu").x).toBe(0);
    expect(byId.get("b-zulu").y).toBeLessThan(0);
    expectNoOverlaps(model);
  });
});
