import { describe, expect, it } from "vitest";

import { buildDisplayShapePath, getPeerDisplayShape } from "./layoutShape.js";

describe("getPeerDisplayShape", () => {
  it("normalizes display rects against the virtual origin", () => {
    const shape = getPeerDisplayShape({
      virtual_bounds: { x: -1920, y: -200, width: 3840, height: 1640 },
      display_rects: [
        { x: -1920, y: -200, width: 1920, height: 1080 },
        { x: 0, y: 160, width: 1920, height: 1280 },
      ],
    });

    expect(shape).toEqual({
      width: 3840,
      height: 1640,
      rects: [
        { x: 0, y: 0, width: 1920, height: 1080 },
        { x: 1920, y: 360, width: 1920, height: 1280 },
      ],
    });
  });
});

describe("buildDisplayShapePath", () => {
  it("renders a rectangular desktop as a single closed loop", () => {
    const path = buildDisplayShapePath(
      {
        width: 100,
        height: 50,
        rects: [{ x: 0, y: 0, width: 100, height: 50 }],
      },
      { x: 10, y: 20, width: 200, height: 100 },
    );

    expect(path).toBe("M 10 20 L 210 20 L 210 120 L 10 120 Z");
  });

  it("renders a concave monitor union without collapsing to the bounding box", () => {
    const path = buildDisplayShapePath(
      {
        width: 100,
        height: 100,
        rects: [
          { x: 0, y: 0, width: 100, height: 50 },
          { x: 50, y: 50, width: 50, height: 50 },
        ],
      },
      { x: 0, y: 0, width: 100, height: 100 },
    );

    expect(path).toContain("L 50 50");
    expect(path).toContain("L 50 100");
    expect(path).not.toBe("M 0 0 L 100 0 L 100 100 L 0 100 Z");
  });

  it("emits separate loops for enclosed holes", () => {
    const path = buildDisplayShapePath(
      {
        width: 100,
        height: 100,
        rects: [
          { x: 0, y: 0, width: 100, height: 20 },
          { x: 0, y: 20, width: 20, height: 60 },
          { x: 80, y: 20, width: 20, height: 60 },
          { x: 0, y: 80, width: 100, height: 20 },
        ],
      },
      { x: 0, y: 0, width: 100, height: 100 },
    );

    expect(path.match(/M /g)).toHaveLength(2);
  });
});
