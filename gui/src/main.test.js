import { describe, expect, it } from "vitest";
import {
  getEdgeSlideRange,
  getNodeSize,
  getVirtualBounds,
  normalizeConnectedPlacement,
  projectToConnectedEdge,
} from "./layoutMath.js";

function makePeer(width, height) {
  return {
    peer_id: "test-peer",
    name: "Test",
    is_local: false,
    virtual_bounds: { width, height },
  };
}

function approximatelyEqual(a, b, tolerance = 0.0001) {
  return Math.abs(a - b) <= tolerance;
}

describe("edge projection primitives", () => {
  const EDGE_MARGIN = 8;
  const anchorRect = { x: 0, y: 0, width: 200, height: 120 };
  const peerRect = { width: 80, height: 60 };
  const contacts = {
    rightX: 148,
    leftX: -148,
    topY: -98,
    bottomY: 98,
  };

  it("projects arbitrary point to nearest connected edge", () => {
    const projected = projectToConnectedEdge(240, 30, anchorRect, peerRect, EDGE_MARGIN);
    expect(projected.edge).toBe("right");
    expect(projected.x).toBe(contacts.rightX);
    expect(projected.y).toBe(30);
  });

  it("breaks equal-distance corner ties with right-left-top-bottom priority", () => {
    const projected = projectToConnectedEdge(
      contacts.rightX,
      contacts.topY,
      anchorRect,
      peerRect,
      EDGE_MARGIN,
    );
    expect(projected.edge).toBe("right");
  });

  it("returns candidates that always touch exactly one contact axis for sampled points", () => {
    let sampled = 0;
    for (let x = -300; x <= 300; x += 100) {
      for (let y = -200; y <= 200; y += 50) {
        const projected = projectToConnectedEdge(x, y, anchorRect, peerRect, EDGE_MARGIN);
        const matchesContactAxis =
          approximatelyEqual(projected.x, contacts.leftX) ||
          approximatelyEqual(projected.x, contacts.rightX) ||
          approximatelyEqual(projected.y, contacts.topY) ||
          approximatelyEqual(projected.y, contacts.bottomY);
        expect(matchesContactAxis).toBe(true);
        sampled += 1;
      }
    }

    expect(sampled).toBe(63);
  });

  it("right-edge slide range keeps contact x fixed while y slides", () => {
    const rightRange = getEdgeSlideRange(anchorRect, peerRect, "right");
    expect(rightRange).toEqual({ min: -90, max: 90 });

    const a = projectToConnectedEdge(280, -20, anchorRect, peerRect, EDGE_MARGIN);
    const b = projectToConnectedEdge(280, 40, anchorRect, peerRect, EDGE_MARGIN);

    expect(a.edge).toBe("right");
    expect(b.edge).toBe("right");
    expect(a.x).toBe(contacts.rightX);
    expect(b.x).toBe(contacts.rightX);
    expect(a.y).not.toBe(b.y);
  });

  it("normalizeConnectedPlacement clamps projected coordinates to global layout bounds", () => {
    const outOfBoundsAnchor = { x: 480, y: 0, width: 200, height: 120 };
    const largerPeerRect = { width: 120, height: 80 };
    const normalized = normalizeConnectedPlacement(
      1000,
      0,
      largerPeerRect,
      outOfBoundsAnchor,
    );

    expect(normalized).toEqual({ x: 440, y: 0 });
  });
});

describe("getNodeSize", () => {
  describe("reference dimensions", () => {
    it("reference_16_9 yields 180x101", () => {
      const peer = makePeer(1920, 1080);
      const size = getNodeSize(peer);
      expect(size.width).toBe(180);
      expect(size.height).toBe(101);
    });

    it("ultra_wide_32_9 yields 260x73", () => {
      const peer = makePeer(5120, 1440);
      const size = getNodeSize(peer);
      expect(size.width).toBe(260);
      expect(size.height).toBe(73);
    });

    it("ultra_wide_34_9 yields 260x109", () => {
      const peer = makePeer(3440, 1440);
      const size = getNodeSize(peer);
      expect(size.width).toBe(260);
      expect(size.height).toBe(109);
    });

    it("portrait_9_16 yields 107x190", () => {
      const peer = makePeer(1080, 1920);
      const size = getNodeSize(peer);
      expect(size.width).toBe(107);
      expect(size.height).toBe(190);
    });

    it("portrait_9_18 yields 96x190", () => {
      const peer = makePeer(1080, 2160);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(190);
    });

    it("square yields 113x113", () => {
      const peer = makePeer(1200, 1200);
      const size = getNodeSize(peer);
      expect(size.width).toBe(113);
      expect(size.height).toBe(113);
    });
  });

  describe("scale factor", () => {
    it("half_reference yields 96x64", () => {
      const peer = makePeer(960, 540);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(64);
    });

    it("double_reference yields 260x146", () => {
      const peer = makePeer(3840, 2160);
      const size = getNodeSize(peer);
      expect(size.width).toBe(260);
      expect(size.height).toBe(146);
    });

    it("quarter_reference yields 96x64", () => {
      const peer = makePeer(480, 270);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(64);
    });
  });

  describe("boundary constraints", () => {
    it("width_at_minimum yields 96x72", () => {
      const peer = makePeer(800, 600);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(72);
    });

    it("height_at_minimum yields 96x71", () => {
      const peer = makePeer(400, 640);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(71);
    });

    it("both_at_minimum yields 96x96", () => {
      const peer = makePeer(320, 320);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(96);
    });

    it("width_at_maximum yields 240x84", () => {
      const peer = makePeer(2560, 900);
      const size = getNodeSize(peer);
      expect(size.width).toBe(240);
      expect(size.height).toBe(84);
    });

    it("height_at_maximum yields 100x160", () => {
      const peer = makePeer(900, 1440);
      const size = getNodeSize(peer);
      expect(size.width).toBe(100);
      expect(size.height).toBe(160);
    });

    it("both_at_maximum yields 260x139", () => {
      const peer = makePeer(3000, 1600);
      const size = getNodeSize(peer);
      expect(size.width).toBe(260);
      expect(size.height).toBe(139);
    });
  });

  describe("edge cases", () => {
    it("tiny_square yields 96x96", () => {
      const peer = makePeer(100, 100);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(96);
    });

    it("tiny_landscape yields 96x64", () => {
      const peer = makePeer(200, 100);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(64);
    });

    it("tiny_portrait yields 96x64", () => {
      const peer = makePeer(100, 200);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(64);
    });

    it("very_wide_10_1 yields 260x64", () => {
      const peer = makePeer(5000, 500);
      const size = getNodeSize(peer);
      expect(size.width).toBe(260);
      expect(size.height).toBe(64);
    });

    it("very_tall_1_10 yields 96x190", () => {
      const peer = makePeer(500, 5000);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(190);
    });
  });

  describe("default bounds", () => {
    it("null_peer uses DEFAULT_BOUNDS", () => {
      const size = getNodeSize(null);
      expect(size.width).toBe(135);
      expect(size.height).toBe(84);
    });

    it("undefined_virtual_bounds uses DEFAULT_BOUNDS", () => {
      const peer = { virtual_bounds: undefined };
      const size = getNodeSize(peer);
      expect(size.width).toBe(135);
      expect(size.height).toBe(84);
    });

    it("missing_virtual_bounds uses DEFAULT_BOUNDS", () => {
      const peer = {};
      const size = getNodeSize(peer);
      expect(size.width).toBe(135);
      expect(size.height).toBe(84);
    });
  });

  describe("special values", () => {
    it("zero_width_given treats width as 1 and yields 96x120", () => {
      const peer = makePeer(0, 1080);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(120);
    });

    it("zero_height_given treats height as 1 and yields 180x64", () => {
      const peer = makePeer(1920, 0);
      const size = getNodeSize(peer);
      expect(size.width).toBe(180);
      expect(size.height).toBe(64);
    });

    it("negative_width treats width as 1 and yields 96x120", () => {
      const peer = makePeer(-100, 1080);
      const size = getNodeSize(peer);
      expect(size.width).toBe(96);
      expect(size.height).toBe(120);
    });

    it("infinity_width falls back to DEFAULT_BOUNDS", () => {
      const peer = makePeer(Infinity, 1080);
      const size = getNodeSize(peer);
      expect(size.width).toBe(135);
      expect(size.height).toBe(84);
    });

    it("nan_height falls back to DEFAULT_BOUNDS", () => {
      const peer = makePeer(1920, NaN);
      const size = getNodeSize(peer);
      expect(size.width).toBe(135);
      expect(size.height).toBe(84);
    });
  });
});

describe("getVirtualBounds", () => {
  it("returns virtual_bounds when both dimensions are finite", () => {
    const peer = makePeer(1920, 1080);
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 1920, height: 1080 });
  });

  it("returns DEFAULT_BOUNDS when virtual_bounds is null", () => {
    const bounds = getVirtualBounds(null);
    expect(bounds).toEqual({ width: 1440, height: 900 });
  });

  it("returns DEFAULT_BOUNDS when virtual_bounds is undefined", () => {
    const bounds = getVirtualBounds({ virtual_bounds: undefined });
    expect(bounds).toEqual({ width: 1440, height: 900 });
  });

  it("returns DEFAULT_BOUNDS when virtual_bounds is missing", () => {
    const bounds = getVirtualBounds({});
    expect(bounds).toEqual({ width: 1440, height: 900 });
  });

  it("clamps zero width to 1", () => {
    const peer = makePeer(0, 1080);
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 1, height: 1080 });
  });

  it("clamps zero height to 1", () => {
    const peer = makePeer(1920, 0);
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 1920, height: 1 });
  });

  it("clamps negative width to 1", () => {
    const peer = makePeer(-100, 1080);
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 1, height: 1080 });
  });

  it("returns DEFAULT_BOUNDS when width is Infinity", () => {
    const peer = makePeer(Infinity, 1080);
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 1440, height: 900 });
  });

  it("returns DEFAULT_BOUNDS when height is NaN", () => {
    const peer = makePeer(1920, NaN);
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 1440, height: 900 });
  });
});

describe("getVirtualBounds with total_dimensions", () => {
  it("prefers total_dimensions over virtual_bounds", () => {
    const peer = {
      virtual_bounds: { width: 3840, height: 1080 },
      total_dimensions: { width: 4480, height: 1440 },
    };
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 4480, height: 1440 });
  });

  it("falls back to virtual_bounds when total_dimensions is missing", () => {
    const peer = {
      virtual_bounds: { width: 3840, height: 1080 },
    };
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 3840, height: 1080 });
  });

  it("falls back to virtual_bounds when total_dimensions has zero width", () => {
    const peer = {
      virtual_bounds: { width: 3840, height: 1080 },
      total_dimensions: { width: 0, height: 1440 },
    };
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 3840, height: 1080 });
  });

  it("falls back to DEFAULT_BOUNDS when only total_dimensions is present but invalid", () => {
    const peer = {
      total_dimensions: { width: Infinity, height: 1440 },
    };
    const bounds = getVirtualBounds(peer);
    expect(bounds).toEqual({ width: 1440, height: 900 });
  });
});
