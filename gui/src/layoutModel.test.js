import { describe, expect, it } from "vitest";
import { normalizeConnectedPlacement } from "./layoutMath.js";
import {
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
});
