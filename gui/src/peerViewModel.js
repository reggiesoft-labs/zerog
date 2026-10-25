export function getPeerRowKey(peer) {
  return String(peer?.peer_id ?? "").trim().toLowerCase();
}

export function getPeerDisplayName(peer, peers = []) {
  const name = String(peer?.name ?? "").trim();
  const peerId = getPeerRowKey(peer);
  if (!name) {
    return peerId || "Unknown";
  }

  const duplicateCount = peers.filter(
    (candidate) => !candidate?.is_local && String(candidate?.name ?? "").trim() === name,
  ).length;

  if (duplicateCount <= 1 || !peerId) {
    return name;
  }

  return `${name} [${peerId.slice(0, 8)}]`;
}

export function isPeerConnected(peer, status) {
  if (peer?.is_local) {
    return true;
  }
  if (peer?.connected) {
    return true;
  }

  const connectedPeers = status?.connected_peers ?? [];
  const peerId = getPeerRowKey(peer);
  return connectedPeers.some((candidate) => getPeerRowKey(candidate) === peerId);
}

export function isPeerPaired(peer, config) {
  if (peer?.is_local) {
    return false;
  }
  if (typeof peer?.paired === "boolean") {
    return peer.paired;
  }
  const peerId = getPeerRowKey(peer);
  if (!peerId) {
    return false;
  }
  const paired = Array.isArray(config?.paired_peers) ? config.paired_peers : [];
  return paired.some((id) => String(id ?? "").trim().toLowerCase() === peerId);
}

export function getPeerStatusLabel(peer, status) {
  if (peer?.is_local) {
    return "Anchor";
  }

  if (isPeerConnected(peer, status)) {
    return "Connected";
  }

  if (peer?.connection_status) {
    const norm = String(peer.connection_status).trim().toLowerCase();
    if (norm === "pairing") {
      return "Pairing...";
    }
    if (norm === "incoming") {
      return "Incoming";
    }
    if (norm === "connecting") {
      return "Connecting";
    }
    if (norm === "backoff") {
      return "Backoff";
    }
  }

  const incomingConnections = status?.incoming_connections ?? [];
  const peerId = getPeerRowKey(peer);
  const incoming = incomingConnections.some((candidate) => getPeerRowKey(candidate) === peerId);

  if (incoming) {
    return "Incoming";
  }
  if (peer?.discovery_status === "stale") {
    return "Stale";
  }

  return "Disconnected";
}

export function getPeerConnectionState(peer, status) {
  if (peer?.is_local) {
    return "local";
  }
  if (isPeerConnected(peer, status)) {
    return "connected";
  }
  const norm = String(peer?.connection_status ?? "").trim().toLowerCase();
  if (norm === "connecting" || norm === "pairing" || norm === "backoff") {
    return norm;
  }
  return "disconnected";
}

export function isPeerCancelPending(peer, status) {
  const state = getPeerConnectionState(peer, status);
  return state === "connecting" || state === "pairing" || state === "backoff";
}

export function isPeerConnectable(peer, status) {
  if (peer?.is_local) {
    return false;
  }

  if (isPeerConnected(peer, status)) {
    return true;
  }

  const normStatus = String(peer?.connection_status ?? "").trim().toLowerCase();
  if (
    normStatus === "incoming" ||
    normStatus === "pairing" ||
    normStatus === "connecting" ||
    normStatus === "backoff"
  ) {
    return true;
  }

  const peerId = getPeerRowKey(peer);
  const connectedPeers = status?.connected_peers ?? [];
  const incomingConnections = status?.incoming_connections ?? [];
  const activeSession =
    connectedPeers.some((candidate) => getPeerRowKey(candidate) === peerId) ||
    incomingConnections.some((candidate) => getPeerRowKey(candidate) === peerId);

  if (activeSession) {
    return true;
  }

  return peer?.discovery_status !== "stale";
}

export function getLegacyIssueMessages(config) {
  return (config?.legacy_issues ?? []).map((issue) => {
    const peerName = String(issue?.peer_name ?? "").trim() || "peer";
    const reason = String(issue?.reason ?? "").trim() || "needs reselection";
    const location = String(issue?.location ?? "").trim();
    return location ? `${peerName}: ${reason} (${location})` : `${peerName}: ${reason}`;
  });
}

export function formatPairingCode(rawCode) {
  const digits = String(rawCode ?? "").trim();
  if (!/^[0-9]{6}$/.test(digits)) {
    return "";
  }

  return `${digits.slice(0, 3)} ${digits.slice(3)}`;
}

export function formatPeerPlatformLabel(rawPlatform) {
  const normalized = String(rawPlatform ?? "").trim().toLowerCase();
  if (!normalized) {
    return null;
  }

  if (normalized === "macos" || normalized === "darwin") {
    return "macOS";
  }
  if (normalized === "windows" || normalized === "win32") {
    return "Windows";
  }
  if (normalized === "linux") {
    return "Linux";
  }

  return normalized.charAt(0).toUpperCase() + normalized.slice(1);
}

export function getPeerOsKind(rawPlatform) {
  const normalized = String(rawPlatform ?? "").trim().toLowerCase();
  if (normalized === "macos" || normalized === "darwin") {
    return "apple";
  }
  if (normalized === "windows" || normalized === "win32") {
    return "windows";
  }
  if (normalized === "linux") {
    return "linux";
  }
  return null;
}

export function formatRelativeTime(ms) {
  if (!Number.isFinite(ms) || ms < 0) {
    return "just now";
  }
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) {
    return "just now";
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m ago`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours}h ago`;
  }
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}
