import { getVirtualBounds } from "./layoutMath.js";

function isFiniteDisplayRect(rect) {
  return (
    rect &&
    Number.isFinite(rect.x) &&
    Number.isFinite(rect.y) &&
    Number.isFinite(rect.width) &&
    Number.isFinite(rect.height) &&
    rect.width > 0 &&
    rect.height > 0
  );
}

function formatSvgNumber(value) {
  const rounded = Math.round(value * 1000) / 1000;
  return String(rounded);
}

function pointKey(x, y) {
  return `${x},${y}`;
}

function segmentKey(start, end) {
  if (start.x === end.x) {
    const minY = Math.min(start.y, end.y);
    const maxY = Math.max(start.y, end.y);
    return `v:${start.x}:${minY}:${maxY}`;
  }

  const minX = Math.min(start.x, end.x);
  const maxX = Math.max(start.x, end.x);
  return `h:${start.y}:${minX}:${maxX}`;
}

function toggleSegment(segmentMap, start, end) {
  const key = segmentKey(start, end);
  if (segmentMap.has(key)) {
    segmentMap.delete(key);
    return;
  }

  segmentMap.set(key, {
    start: { ...start },
    end: { ...end },
  });
}

function sortedUnique(values) {
  return [...new Set(values)].sort((a, b) => a - b);
}

function collectBoundarySegments(rects) {
  const xBreaks = sortedUnique(rects.flatMap((rect) => [rect.x, rect.x + rect.width]));
  const yBreaks = sortedUnique(rects.flatMap((rect) => [rect.y, rect.y + rect.height]));
  const segments = new Map();

  rects.forEach((rect) => {
    const left = rect.x;
    const right = rect.x + rect.width;
    const top = rect.y;
    const bottom = rect.y + rect.height;

    for (let index = 0; index < xBreaks.length - 1; index += 1) {
      const x1 = xBreaks[index];
      const x2 = xBreaks[index + 1];
      if (x1 < left || x2 > right) {
        continue;
      }

      toggleSegment(segments, { x: x1, y: top }, { x: x2, y: top });
      toggleSegment(segments, { x: x1, y: bottom }, { x: x2, y: bottom });
    }

    for (let index = 0; index < yBreaks.length - 1; index += 1) {
      const y1 = yBreaks[index];
      const y2 = yBreaks[index + 1];
      if (y1 < top || y2 > bottom) {
        continue;
      }

      toggleSegment(segments, { x: left, y: y1 }, { x: left, y: y2 });
      toggleSegment(segments, { x: right, y: y1 }, { x: right, y: y2 });
    }
  });

  return [...segments.values()];
}

function buildBoundaryLoops(segments) {
  const adjacency = new Map();

  segments.forEach((segment, index) => {
    [segment.start, segment.end].forEach((point) => {
      const key = pointKey(point.x, point.y);
      if (!adjacency.has(key)) {
        adjacency.set(key, []);
      }
      adjacency.get(key).push(index);
    });
  });

  const used = new Set();
  const loops = [];

  for (let index = 0; index < segments.length; index += 1) {
    if (used.has(index)) {
      continue;
    }

    const loop = [];
    let currentIndex = index;
    let currentPoint = segments[index].start;
    let nextPoint = segments[index].end;

    used.add(index);
    loop.push({ ...currentPoint });

    while (true) {
      loop.push({ ...nextPoint });
      if (pointKey(nextPoint.x, nextPoint.y) === pointKey(loop[0].x, loop[0].y)) {
        loop.pop();
        break;
      }

      const candidates = adjacency.get(pointKey(nextPoint.x, nextPoint.y)) ?? [];
      const nextIndex = candidates.find((candidate) => candidate !== currentIndex && !used.has(candidate));
      if (nextIndex === undefined) {
        return [];
      }

      used.add(nextIndex);
      const segment = segments[nextIndex];
      currentPoint = nextPoint;
      nextPoint =
        pointKey(segment.start.x, segment.start.y) === pointKey(currentPoint.x, currentPoint.y)
          ? segment.end
          : segment.start;
      currentIndex = nextIndex;
    }

    loops.push(loop);
  }

  return loops;
}

export function getPeerDisplayShape(peer) {
  const displayRects = Array.isArray(peer?.display_rects)
    ? peer.display_rects.filter(isFiniteDisplayRect)
    : [];

  if (displayRects.length) {
    const originX = Number.isFinite(peer?.virtual_bounds?.x)
      ? peer.virtual_bounds.x
      : Math.min(...displayRects.map((rect) => rect.x));
    const originY = Number.isFinite(peer?.virtual_bounds?.y)
      ? peer.virtual_bounds.y
      : Math.min(...displayRects.map((rect) => rect.y));
    const rects = displayRects.map((rect) => ({
      x: rect.x - originX,
      y: rect.y - originY,
      width: rect.width,
      height: rect.height,
    }));
    const width = Math.max(...rects.map((rect) => rect.x + rect.width));
    const height = Math.max(...rects.map((rect) => rect.y + rect.height));

    return {
      width: Math.max(1, width),
      height: Math.max(1, height),
      rects,
    };
  }

  const bounds = getVirtualBounds(peer);
  return {
    width: bounds.width,
    height: bounds.height,
    rects: [
      {
        x: 0,
        y: 0,
        width: bounds.width,
        height: bounds.height,
      },
    ],
  };
}

export function buildDisplayShapePath(shape, frame) {
  const rects = Array.isArray(shape?.rects) ? shape.rects.filter(isFiniteDisplayRect) : [];
  if (!rects.length) {
    return "";
  }

  const sourceWidth = Math.max(1, shape.width);
  const sourceHeight = Math.max(1, shape.height);
  if (!shape._cachedLoops) {
    const segments = collectBoundarySegments(rects);
    shape._cachedLoops = buildBoundaryLoops(segments);
  }
  const loops = shape._cachedLoops;
  if (!loops.length) {
    return "";
  }

  const scaleX = frame.width / sourceWidth;
  const scaleY = frame.height / sourceHeight;

  return loops
    .map((loop) => {
      const commands = loop.map((point, index) => {
        const x = frame.x + point.x * scaleX;
        const y = frame.y + point.y * scaleY;
        return `${index === 0 ? "M" : "L"} ${formatSvgNumber(x)} ${formatSvgNumber(y)}`;
      });
      return `${commands.join(" ")} Z`;
    })
    .join(" ");
}
