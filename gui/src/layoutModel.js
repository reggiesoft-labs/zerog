import {
  LOCAL_ANCHOR,
  clamp,
  getNodeSize,
  normalizeConnectedPlacement,
} from "./layoutMath.js";
import { normalizePeerId } from "./peerList.js";
import { isPeerConnected } from "./peerViewModel.js";

const COLLISION_PADDING = 4;
const FALLBACK_RADIUS = 320;

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

function normalizePlacement(placement, peer, anchorRect) {
  const size = getNodeSize(peer);
  const position = getPlacementPosition(placement);
  const normalized = normalizeConnectedPlacement(
    position.x,
    position.y,
    size,
    anchorRect,
  );

  return {
    peer_id: normalizePeerId(placement?.peer_id),
    peer_label: String(placement?.peer_label ?? "").trim(),
    x: normalized.x,
    y: normalized.y,
  };
}

function getFallbackPlacement(peerId, index, totalCount) {
  const count = Math.max(1, totalCount);
  const angle = ((Math.PI * 2) / count) * index;
  const jitter =
    peerId.split("").reduce((sum, ch) => sum + ch.charCodeAt(0), 0) % 40;
  const radius = FALLBACK_RADIUS + jitter;

  return {
    x: Math.round(Math.cos(angle) * radius),
    y: Math.round(Math.sin(angle) * radius),
  };
}

export function createLayoutModel(config, peers, status = null) {
  const localPeer = getLocalPeer(peers);
  const remotePeers = getRemotePeers(peers, status);
  const anchorRect = getAnchorRect(localPeer);
  const placementMap = new Map();
  const placements = Array.isArray(config?.placements) ? config.placements : [];

  placements.forEach((placement) => {
    const peerId = normalizePeerId(placement?.peer_id);
    if (!peerId) {
      return;
    }

    const peer = getPeerById(peers, peerId);
    placementMap.set(peerId, normalizePlacement(placement, peer, anchorRect));
  });

  const unplacedPeers = remotePeers.filter(
    (peer) => !placementMap.has(normalizePeerId(peer.peer_id)),
  );
  let unplacedIndex = 0;

  return {
    localPeer,
    remotePeers,
    placements: remotePeers.map((peer) => {
      const peerId = normalizePeerId(peer.peer_id);
      const existing = placementMap.get(peerId);
      let fallback = null;
      if (!existing) {
        fallback = getFallbackPlacement(
          peerId,
          unplacedIndex,
          unplacedPeers.length,
        );
        unplacedIndex += 1;
      }
      const position =
        existing ?? normalizePlacement(fallback, peer, anchorRect);
      const size = getNodeSize(peer);

      return {
        peer,
        peerId,
        x: position.x,
        y: position.y,
        size,
        persisted: Boolean(existing),
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
    ? currentConfig.placements
        .map((placement) => {
          const peerId = normalizePeerId(placement?.peer_id);
          if (!peerId) {
            return null;
          }

          const peer = getPeerById(peers, peerId);
          const normalized = normalizePlacement(placement, peer, anchorRect);
          return {
            ...placement,
            ...normalized,
          };
        })
        .filter(Boolean)
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
