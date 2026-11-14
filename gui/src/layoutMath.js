const DEFAULT_BOUNDS = {
  width: 1440,
  height: 900,
};

export const LOCAL_ANCHOR = {
  x: 0,
  y: 0,
};

export const LAYOUT_COORDINATE_BOUNDS = {
  minX: -500,
  maxX: 500,
  minY: -400,
  maxY: 400,
};

export const EDGE_MARGIN = 8;
// Largest gap between facing edges that still counts as touching. Mirrors
// MAX_CONTACT_GAP in src/core/layout.rs, which drops runtime contacts to
// displays further away (such as the far end of a row of three).
export const MAX_CONTACT_GAP = EDGE_MARGIN + 48;
// Shared edge length a snapped display keeps with its neighbour, so a corner
// placement still leaves room to cross. Mirrors MIN_EDGE_OVERLAP in
// src/core/layout.rs.
export const MIN_EDGE_OVERLAP = 16;
// Displays may overlap by the editor's collision padding on both sides.
const MIN_CONTACT_GAP = -8;

const EDGE_TOP = "top";
const EDGE_RIGHT = "right";
const EDGE_BOTTOM = "bottom";
const EDGE_LEFT = "left";

export function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

function getEdgeContactCoordinates(anchorRect, peerRect, edge, edgeMargin) {
  void edge;
  const anchorHalfWidth = anchorRect.width / 2;
  const anchorHalfHeight = anchorRect.height / 2;
  const peerHalfWidth = peerRect.width / 2;
  const peerHalfHeight = peerRect.height / 2;

  return {
    topY: anchorRect.y - anchorHalfHeight - edgeMargin - peerHalfHeight,
    rightX: anchorRect.x + anchorHalfWidth + edgeMargin + peerHalfWidth,
    bottomY: anchorRect.y + anchorHalfHeight + edgeMargin + peerHalfHeight,
    leftX: anchorRect.x - anchorHalfWidth - edgeMargin - peerHalfWidth,
  };
}

export function getEdgeSlideRange(anchorRect, peerRect, edge) {
  const horizontal = edge === EDGE_RIGHT || edge === EDGE_LEFT;
  const center = horizontal ? anchorRect.y : anchorRect.x;
  const anchorLength = horizontal ? anchorRect.height : anchorRect.width;
  const peerLength = horizontal ? peerRect.height : peerRect.width;
  const minOverlap = Math.min(MIN_EDGE_OVERLAP, anchorLength, peerLength);
  const halfSpan = (anchorLength + peerLength) / 2 - minOverlap;
  return {
    min: center - halfSpan,
    max: center + halfSpan,
  };
}

// Edge of `anchorRect` that `peerRect` touches, or null when the two do not
// share a stretch of edge within MAX_CONTACT_GAP. Rects are centre-based.
export function getTouchingEdge(anchorRect, peerRect) {
  const overlapX =
    Math.min(anchorRect.x + anchorRect.width / 2, peerRect.x + peerRect.width / 2) -
    Math.max(anchorRect.x - anchorRect.width / 2, peerRect.x - peerRect.width / 2);
  const overlapY =
    Math.min(anchorRect.y + anchorRect.height / 2, peerRect.y + peerRect.height / 2) -
    Math.max(anchorRect.y - anchorRect.height / 2, peerRect.y - peerRect.height / 2);
  const gapX =
    Math.abs(peerRect.x - anchorRect.x) - (anchorRect.width + peerRect.width) / 2;
  const gapY =
    Math.abs(peerRect.y - anchorRect.y) - (anchorRect.height + peerRect.height) / 2;

  const touches = (gap) => gap >= MIN_CONTACT_GAP && gap <= MAX_CONTACT_GAP;
  if (overlapY > 0 && gapX >= gapY && touches(gapX)) {
    return peerRect.x > anchorRect.x ? EDGE_RIGHT : EDGE_LEFT;
  }
  if (overlapX > 0 && gapY > gapX && touches(gapY)) {
    return peerRect.y > anchorRect.y ? EDGE_BOTTOM : EDGE_TOP;
  }
  return null;
}

function nearestByDistanceThenPriority(
  targetX,
  targetY,
  candidates,
  priorityOrder,
) {
  const priorityRank = new Map(priorityOrder.map((edge, index) => [edge, index]));
  const tieTolerance = 1e-9;
  let winner = candidates[0];
  let bestDistance = Math.hypot(targetX - winner.x, targetY - winner.y);

  for (let index = 1; index < candidates.length; index += 1) {
    const candidate = candidates[index];
    const distance = Math.hypot(targetX - candidate.x, targetY - candidate.y);

    if (distance + tieTolerance < bestDistance) {
      winner = candidate;
      bestDistance = distance;
      continue;
    }

    if (Math.abs(distance - bestDistance) <= tieTolerance) {
      const candidateRank =
        priorityRank.get(candidate.edge) ?? Number.POSITIVE_INFINITY;
      const winnerRank = priorityRank.get(winner.edge) ?? Number.POSITIVE_INFINITY;
      if (candidateRank < winnerRank) {
        winner = candidate;
        bestDistance = distance;
      }
    }
  }

  return winner;
}

export function projectToConnectedEdge(
  targetX,
  targetY,
  anchorRect,
  peerRect,
  edgeMargin,
) {
  const contacts = getEdgeContactCoordinates(
    anchorRect,
    peerRect,
    EDGE_RIGHT,
    edgeMargin,
  );
  const rightRange = getEdgeSlideRange(anchorRect, peerRect, EDGE_RIGHT);
  const leftRange = getEdgeSlideRange(anchorRect, peerRect, EDGE_LEFT);
  const topRange = getEdgeSlideRange(anchorRect, peerRect, EDGE_TOP);
  const bottomRange = getEdgeSlideRange(anchorRect, peerRect, EDGE_BOTTOM);

  const candidates = [
    {
      edge: EDGE_RIGHT,
      x: contacts.rightX,
      y: clamp(targetY, rightRange.min, rightRange.max),
    },
    {
      edge: EDGE_LEFT,
      x: contacts.leftX,
      y: clamp(targetY, leftRange.min, leftRange.max),
    },
    {
      edge: EDGE_TOP,
      x: clamp(targetX, topRange.min, topRange.max),
      y: contacts.topY,
    },
    {
      edge: EDGE_BOTTOM,
      x: clamp(targetX, bottomRange.min, bottomRange.max),
      y: contacts.bottomY,
    },
  ];

  return nearestByDistanceThenPriority(targetX, targetY, candidates, [
    EDGE_RIGHT,
    EDGE_LEFT,
    EDGE_TOP,
    EDGE_BOTTOM,
  ]);
}

function getPlacementBoundsForSize(size) {
  const halfWidth = size.width / 2;
  const halfHeight = size.height / 2;

  return {
    minX: LAYOUT_COORDINATE_BOUNDS.minX + halfWidth,
    maxX: LAYOUT_COORDINATE_BOUNDS.maxX - halfWidth,
    minY: LAYOUT_COORDINATE_BOUNDS.minY + halfHeight,
    maxY: LAYOUT_COORDINATE_BOUNDS.maxY - halfHeight,
  };
}

export function clampPlacementToBounds(x, y, size) {
  const bounds = getPlacementBoundsForSize(size);
  const minX = Math.min(bounds.minX, bounds.maxX);
  const maxX = Math.max(bounds.minX, bounds.maxX);
  const minY = Math.min(bounds.minY, bounds.maxY);
  const maxY = Math.max(bounds.minY, bounds.maxY);

  return {
    x: Math.round(clamp(Math.round(x), minX, maxX)),
    y: Math.round(clamp(Math.round(y), minY, maxY)),
  };
}

export function normalizeConnectedPlacement(
  x,
  y,
  peerRect,
  anchorRect,
) {
  const projected = projectToConnectedEdge(
    x,
    y,
    anchorRect,
    peerRect,
    EDGE_MARGIN,
  );
  return clampPlacementToBounds(projected.x, projected.y, peerRect);
}

export function getVirtualBounds(peer) {
  if (
    peer?.total_dimensions &&
    Number.isFinite(peer.total_dimensions.width) &&
    Number.isFinite(peer.total_dimensions.height) &&
    peer.total_dimensions.width > 0 &&
    peer.total_dimensions.height > 0
  ) {
    return {
      width: Math.max(1, peer.total_dimensions.width),
      height: Math.max(1, peer.total_dimensions.height),
    };
  }

  if (
    peer?.virtual_bounds &&
    Number.isFinite(peer.virtual_bounds.width) &&
    Number.isFinite(peer.virtual_bounds.height)
  ) {
    return {
      width: Math.max(1, peer.virtual_bounds.width),
      height: Math.max(1, peer.virtual_bounds.height),
    };
  }

  return { ...DEFAULT_BOUNDS };
}

export function getNodeSize(peer) {
  const bounds = getVirtualBounds(peer);
  const aspectRatio = bounds.width / bounds.height;
  const maxWidth = 260;
  const maxHeight = 190;
  const minWidth = 96;
  const minHeight = 64;

  const referenceWidth = 1920;
  const referenceHeight = 1080;

  let width;
  let height;

  if (aspectRatio >= 1) {
    width = (bounds.width / referenceWidth) * 180;
    width = clamp(width, minWidth, maxWidth);
    height = width / aspectRatio;
    height = clamp(height, minHeight, maxHeight);

    if (height < minHeight) {
      height = minHeight;
      width = height * aspectRatio;
      width = clamp(width, minWidth, maxWidth);
    }
  } else {
    height = (bounds.height / referenceHeight) * 120;
    height = clamp(height, minHeight, maxHeight);
    width = height * aspectRatio;
    width = clamp(width, minWidth, maxWidth);

    if (width < minWidth) {
      width = minWidth;
      height = width / aspectRatio;
      height = clamp(height, minHeight, maxHeight);
    }
  }

  return {
    width: Math.round(width),
    height: Math.round(height),
  };
}
