import {
  LOCAL_ANCHOR,
  clamp,
  getNodeSize,
  getTouchingEdge,
  normalizeConnectedPlacement,
} from "./layoutMath.js";
import { normalizePeerId } from "./peerList.js";
import { isPeerConnected } from "./peerViewModel.js";

const COLLISION_PADDING = 4;
const FALLBACK_EDGE_ORDER = ["right", "left", "top", "bottom"];

export function normalizeConfigValue(value) {
  const trimmed = String(value ?? "").trim();
  if (!trimmed.length || trimmed.toLowerCase() === "none") {
    return null;
  }
  return trimmed;
}

export function getPeerById(peers, peerId) {
  const key = normalizePeerId(peerId);
  if (!key) {
    return null;
  }

  return (
    peers.find(
      (peer) => !peer.is_local && normalizePeerId(peer.peer_id) === key,
    ) ?? null
  );
}

export function getLocalPeer(peers) {
  return peers.find((peer) => peer.is_local) ?? null;
}

export function getRemotePeers(peers, status = null) {
  return peers
    .filter(
      (peer) =>
        !peer.is_local && Boolean(status) && isPeerConnected(peer, status),
    )
    .sort(
      (a, b) =>
        a.name.localeCompare(b.name) || a.peer_id.localeCompare(b.peer_id),
    );
}

export function getAnchorRect(localPeer) {
  const size = getNodeSize(localPeer);
  return {
    x: LOCAL_ANCHOR.x,
    y: LOCAL_ANCHOR.y,
    width: size.width,
    height: size.height,
  };
}

function getPlacementPosition(placement) {
  return {
    x: Number.isFinite(placement?.x) ? Math.round(placement.x) : 0,
    y: Number.isFinite(placement?.y) ? Math.round(placement.y) : 0,
  };
}

// Placements that touch the local display are re-snapped onto it. Placements
// that only touch another placed display (the far end of a row of three, as
// rebased from another machine's layout) keep their position, as do
// placements for peers this machine does not know. Anything else floats free
// and is snapped back onto the local display.
function normalizePlacements(placements, peers, anchorRect) {
  const entries = placements
    .map((placement) => {
      const peerId = normalizePeerId(placement?.peer_id);
      if (!peerId) {
        return null;
      }
      const peer = getPeerById(peers, peerId);
      const size = getNodeSize(peer);
      const position = getPlacementPosition(placement);
      return {
        placement,
        peerId,
        peer,
        size,
        position,
        rect: { ...position, ...size },
      };
    })
    .filter(Boolean);

  const touchesLocal = new Set(
    entries.filter((entry) => getTouchingEdge(anchorRect, entry.rect)),
  );
  const reached = new Set(touchesLocal);
  const queue = [...touchesLocal];
  while (queue.length) {
    const current = queue.shift();
    entries.forEach((entry) => {
      if (!reached.has(entry) && getTouchingEdge(current.rect, entry.rect)) {
        reached.add(entry);
        queue.push(entry);
      }
    });
  }

  return entries.map((entry) => {
    const keep = !entry.peer || (reached.has(entry) && !touchesLocal.has(entry));
    const position = keep
      ? entry.position
      : normalizeConnectedPlacement(
          entry.position.x,
          entry.position.y,
          entry.size,
          anchorRect,
        );
    return {
      source: entry.placement,
      normalized: {
        peer_id: entry.peerId,
        peer_label: String(entry.placement?.peer_label ?? "").trim(),
        x: position.x,
        y: position.y,
      },
    };
  });
}

function getLegacyEdge(config, peerId) {
  return (
    FALLBACK_EDGE_ORDER.find(
      (edge) => normalizePeerId(normalizeConfigValue(config?.[edge])) === peerId,
    ) ?? null
  );
}

function getEdgeCenterTarget(edge, size, anchorRect) {
  const reachX = anchorRect.width / 2 + size.width / 2;
  const reachY = anchorRect.height / 2 + size.height / 2;
  switch (edge) {
    case "left":
      return { x: anchorRect.x - reachX, y: anchorRect.y };
    case "top":
      return { x: anchorRect.x, y: anchorRect.y - reachY };
    case "bottom":
      return { x: anchorRect.x, y: anchorRect.y + reachY };
    default:
      return { x: anchorRect.x + reachX, y: anchorRect.y };
  }
}

// Gives each unplaced peer, in peer id order, its legacy edge or else the
// first edge no placed display touches. Mirrors the runtime fallback contacts
// in src/platform/shared/layout_state.rs, so the dashed spot shown for an
// unplaced peer is the edge the cursor actually crosses to reach it.
function assignFallbackEdges(config, unplacedPeerIds, usedEdges) {
  const used = new Set(usedEdges);
  const edges = new Map();
  [...unplacedPeerIds]
    .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0))
    .forEach((peerId) => {
      const preferred = getLegacyEdge(config, peerId) ?? FALLBACK_EDGE_ORDER[0];
      const edge =
        [preferred, ...FALLBACK_EDGE_ORDER].find((candidate) => !used.has(candidate)) ??
        preferred;
      used.add(edge);
      edges.set(peerId, edge);
    });
  return edges;
}

// Quarter-steps of the slide reach; 4 would only touch the local display at a corner.
const OPEN_SPOT_STEPS = [0, 1, -1, 2, -2, 3, -3];

// Keeps the preferred spot when it is free; otherwise tries each edge's
// center, then slides outward along the edges for the first spot that
// touches no other display.
function findOpenPlacement(preferred, peerId, size, anchorRect, occupied, localPeer) {
  const isOpen = (position) =>
    !placementWouldOverlap(peerId, position.x, position.y, size, {
      placements: occupied,
      localPeer,
    });
  if (isOpen(preferred)) {
    return preferred;
  }

  const reachX = anchorRect.width / 2 + size.width / 2;
  const reachY = anchorRect.height / 2 + size.height / 2;
  const directions = [
    (step) => ({ x: anchorRect.x + reachX, y: anchorRect.y + (reachY * step) / 4 }),
    (step) => ({ x: anchorRect.x - reachX, y: anchorRect.y + (reachY * step) / 4 }),
    (step) => ({ x: anchorRect.x + (reachX * step) / 4, y: anchorRect.y + reachY }),
    (step) => ({ x: anchorRect.x + (reachX * step) / 4, y: anchorRect.y - reachY }),
  ];
  for (const step of OPEN_SPOT_STEPS) {
    for (const direction of directions) {
      const target = direction(step);
      const candidate = normalizeConnectedPlacement(target.x, target.y, size, anchorRect);
      if (isOpen(candidate)) {
        return candidate;
      }
    }
  }

  return preferred;
}

export function createLayoutModel(config, peers, status = null) {
  const localPeer = getLocalPeer(peers);
  const remotePeers = getRemotePeers(peers, status);
  const anchorRect = getAnchorRect(localPeer);
  const placementMap = new Map();
  const placements = Array.isArray(config?.placements) ? config.placements : [];

  normalizePlacements(placements, peers, anchorRect).forEach(({ normalized }) => {
    placementMap.set(normalized.peer_id, normalized);
  });

  // Shown displays a fallback spot must avoid: persisted ones up front, then
  // each fallback as it is assigned. Persisted placements are taken in config
  // order, and one that collides with an earlier one is moved to an open spot
  // (flagged `repaired` so the editor saves it). A merged remote layout lists
  // the sender's placements first, so a stale local placement for a peer the
  // sender does not know is the one that yields.
  const shownPeers = new Map(
    remotePeers.map((peer) => [normalizePeerId(peer.peer_id), peer]),
  );
  const occupied = new Map();
  const usedEdges = new Set();
  const repairedPeerIds = new Set();
  placementMap.forEach((existing, peerId) => {
    const peer = shownPeers.get(peerId);
    if (!peer) {
      return;
    }
    const size = getNodeSize(peer);
    const position = findOpenPlacement(
      existing,
      peerId,
      size,
      anchorRect,
      occupied,
      localPeer,
    );
    if (position.x !== existing.x || position.y !== existing.y) {
      placementMap.set(peerId, { ...existing, x: position.x, y: position.y });
      repairedPeerIds.add(peerId);
    }
    occupied.set(peerId, { peerId, x: position.x, y: position.y, size });
    const edge = getTouchingEdge(anchorRect, { x: position.x, y: position.y, ...size });
    if (edge) {
      usedEdges.add(edge);
    }
  });
  const fallbackEdges = assignFallbackEdges(
    config,
    remotePeers
      .map((peer) => normalizePeerId(peer.peer_id))
      .filter((peerId) => !placementMap.has(peerId)),
    usedEdges,
  );

  return {
    localPeer,
    remotePeers,
    placements: remotePeers.map((peer) => {
      const peerId = normalizePeerId(peer.peer_id);
      const existing = placementMap.get(peerId);
      const size = getNodeSize(peer);
      let position = existing;
      if (!existing) {
        const target = getEdgeCenterTarget(fallbackEdges.get(peerId), size, anchorRect);
        position = findOpenPlacement(
          normalizeConnectedPlacement(target.x, target.y, size, anchorRect),
          peerId,
          size,
          anchorRect,
          occupied,
          localPeer,
        );
        occupied.set(peerId, { peerId, x: position.x, y: position.y, size });
      }

      return {
        peer,
        peerId,
        x: position.x,
        y: position.y,
        size,
        persisted: Boolean(existing),
        repaired: repairedPeerIds.has(peerId),
      };
    }),
  };
}

export function updateConfigPlacement(config, peer, x, y, peers) {
  if (!config) {
    return config;
  }

  const peerId = normalizePeerId(peer?.peer_id);
  if (!peerId) {
    return config;
  }

  const anchorRect = getAnchorRect(getLocalPeer(peers));
  const size = getNodeSize(peer);
  const position = normalizeConnectedPlacement(
    x,
    y,
    size,
    anchorRect,
  );
  const placements = Array.isArray(config.placements)
    ? config.placements.map((placement) => ({ ...placement }))
    : [];
  const nextPlacement = {
    peer_id: peerId,
    peer_label: String(peer?.name ?? "").trim(),
    x: position.x,
    y: position.y,
  };
  const existingIndex = placements.findIndex(
    (placement) => normalizePeerId(placement?.peer_id) === peerId,
  );

  if (existingIndex >= 0) {
    placements[existingIndex] = {
      ...placements[existingIndex],
      ...nextPlacement,
    };
  } else {
    placements.push(nextPlacement);
  }

  return {
    ...config,
    placements,
  };
}

export function buildConfigPayload(config, peers) {
  const currentConfig = config ?? {};
  const anchorRect = getAnchorRect(getLocalPeer(peers));
  const placements = Array.isArray(currentConfig.placements)
    ? normalizePlacements(currentConfig.placements, peers, anchorRect).map(
        ({ source, normalized }) => ({
          ...source,
          ...normalized,
        }),
      )
    : [];

  return {
    ...currentConfig,
    left: normalizeConfigValue(currentConfig.left),
    right: normalizeConfigValue(currentConfig.right),
    top: normalizeConfigValue(currentConfig.top),
    bottom: normalizeConfigValue(currentConfig.bottom),
    zones: Array.isArray(currentConfig.zones)
      ? currentConfig.zones.map((zone) => ({ ...zone }))
      : [],
    placements,
    legacy_issues: Array.isArray(currentConfig.legacy_issues)
      ? currentConfig.legacy_issues.map((issue) => ({ ...issue }))
      : [],
    batch_window_us: Number.isFinite(currentConfig.batch_window_us)
      ? currentConfig.batch_window_us
      : 0,
    max_events_per_packet: Number.isFinite(currentConfig.max_events_per_packet)
      ? currentConfig.max_events_per_packet
      : 32,
    movement_cadence_us: Number.isFinite(currentConfig.movement_cadence_us)
      ? currentConfig.movement_cadence_us
      : 8_333,
    metrics_log_path: normalizeConfigValue(currentConfig.metrics_log_path),
    remote_layout_updated: Boolean(currentConfig.remote_layout_updated),
    swap_alt_cmd:
      currentConfig.swap_alt_cmd !== undefined
        ? Boolean(currentConfig.swap_alt_cmd)
        : true,
    modifier_mapping: currentConfig.modifier_mapping ? { ...currentConfig.modifier_mapping } : null,
    keep_mouse_alive: Boolean(currentConfig.keep_mouse_alive),
    release_tap_key: currentConfig.release_tap_key ?? "escape",
    share_clipboard:
      currentConfig.share_clipboard !== undefined
        ? Boolean(currentConfig.share_clipboard)
        : true,
  };
}

function getNodeRectForCenter(x, y, size, padding = 0) {
  const maxInset = Math.max(
    0,
    Math.floor((Math.min(size.width, size.height) - 1) / 2),
  );
  const inset = clamp(padding, 0, maxInset);

  return {
    x: x - size.width / 2 + inset,
    y: y - size.height / 2 + inset,
    width: size.width - inset * 2,
    height: size.height - inset * 2,
  };
}

function rectsOverlap(a, b) {
  return (
    a.x < b.x + b.width &&
    a.x + a.width > b.x &&
    a.y < b.y + b.height &&
    a.y + a.height > b.y
  );
}

export function placementWouldOverlap(
  peerId,
  x,
  y,
  movingSize,
  { placements, localPeer },
) {
  const key = String(peerId ?? "").trim();
  if (!key) {
    return false;
  }

  const movingRect = getNodeRectForCenter(
    x,
    y,
    movingSize ?? getNodeSize(null),
    COLLISION_PADDING,
  );
  const localRect = getNodeRectForCenter(
    LOCAL_ANCHOR.x,
    LOCAL_ANCHOR.y,
    getNodeSize(localPeer),
    COLLISION_PADDING,
  );

  if (rectsOverlap(movingRect, localRect)) {
    return true;
  }

  for (const otherPlacement of placements.values()) {
    if (otherPlacement.peerId === key) {
      continue;
    }

    const otherRect = getNodeRectForCenter(
      otherPlacement.x,
      otherPlacement.y,
      otherPlacement.size,
      COLLISION_PADDING,
    );
    if (rectsOverlap(movingRect, otherRect)) {
      return true;
    }
  }

  return false;
}
