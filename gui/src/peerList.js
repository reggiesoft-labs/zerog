import {
  formatPeerPlatformLabel,
  formatRelativeTime,
  getPeerConnectionState,
  getPeerDisplayName,
  getPeerRowKey,
  isPeerConnectable,
  isPeerConnected,
  isPeerPaired,
} from "./peerViewModel.js";

export function normalizePeerId(peerId) {
  return String(peerId ?? "").trim().toLowerCase();
}

function peerPriority(peer) {
  if (peer?.is_local) {
    return 10;
  }
  let score = 0;
  if (!peer?.peer_id?.startsWith("direct-")) {
    score += 4;
  }
  if (peer?.connected) {
    score += 2;
  }
  if (peer?.discovery_status === "online") {
    score += 2;
  } else if (peer?.discovery_status === "stale") {
    score += 1;
  }
  return score;
}

export function normalizePeersSnapshot(peers) {
  const input = Array.isArray(peers) ? peers : [];
  const localPeerIds = new Set(
    input
      .filter((peer) => peer?.is_local)
      .map((peer) => normalizePeerId(peer?.peer_id))
      .filter((peerId) => peerId.length > 0),
  );

  const canonical = new Map();
  input.forEach((peer) => {
    const peerId = normalizePeerId(peer?.peer_id);
    if (!peerId) {
      return;
    }

    if (!peer?.is_local && localPeerIds.has(peerId)) {
      return;
    }

    const addr = String(peer?.addr ?? "").trim();
    if (!peer?.is_local && (addr.endsWith(":0") || addr === "0")) {
      return;
    }

    const next = {
      ...peer,
      peer_id: peerId,
    };

    const current = canonical.get(peerId);
    if (!current || peerPriority(next) > peerPriority(current)) {
      canonical.set(peerId, next);
    }
  });

  const realPeerAddrs = new Set();
  canonical.forEach((peer) => {
    if (!peer?.is_local && !peer?.peer_id?.startsWith("direct-")) {
      const addr = String(peer?.addr ?? "").trim();
      if (addr) {
        realPeerAddrs.add(addr);
      }
    }
  });

  return Array.from(canonical.values()).filter((peer) => {
    if (peer?.peer_id?.startsWith("direct-")) {
      const addr = String(peer?.addr ?? "").trim();
      if (realPeerAddrs.has(addr)) {
        return false;
      }
    }
    return true;
  });
}

export function renderPeerList({
  peers,
  state,
  ui,
  isStatusTabActive,
  appendEmptyPeerMessage,
  onConnectPeer,
  onDisconnectPeer,
  onUnpairPeer,
  onCancelPeer,
}) {
  const normalizedPeers = normalizePeersSnapshot(peers);
  state.peers = normalizedPeers;
  const visiblePeers = normalizedPeers.filter(
    (peer) => isPeerConnectable(peer, state.status),
  );

  if (typeof isStatusTabActive === "function" && !isStatusTabActive()) {
    return normalizedPeers;
  }

  ui.peerList.innerHTML = "";

  if (!visiblePeers.length) {
    appendEmptyPeerMessage(
      "No peers discovered. Peers appear when their engine is running.",
    );
    return normalizedPeers;
  }

  const pairedPeers = visiblePeers.filter((peer) =>
    isPeerPaired(peer, state.config),
  );
  const nearbyPeers = visiblePeers.filter(
    (peer) => !isPeerPaired(peer, state.config),
  );

  const renderItem = (peer) => {
    const item = document.createElement("li");
    item.dataset.peerId = getPeerRowKey(peer);

    const connected = isPeerConnected(peer, state.status);
    const connState = getPeerConnectionState(peer, state.status);
    const pairing = connState === "pairing";
    const cancelPending =
      !connected &&
      (connState === "connecting" ||
        connState === "pairing" ||
        connState === "backoff");

    const indicator = document.createElement("span");
    const statusClass = connected
      ? "peer-status-dot--connected"
      : pairing
        ? "peer-status-dot--pairing"
        : "peer-status-dot--disconnected";
    indicator.className = `peer-status-dot ${statusClass}`;

    const main = document.createElement("div");
    main.className = "peer-main";

    const header = document.createElement("div");
    header.className = "peer-header";

    const title = document.createElement("strong");
    title.textContent = getPeerDisplayName(peer, visiblePeers);
    header.append(title);

    const platformLabel = formatPeerPlatformLabel(peer.platform);
    if (platformLabel) {
      const platformBadge = document.createElement("span");
      platformBadge.className = "peer-platform-badge";
      platformBadge.textContent = platformLabel;
      header.append(platformBadge);
    }

    const meta = document.createElement("div");
    const timeLabel = formatRelativeTime(peer.last_seen_ms);

    meta.className = "peer-meta";
    meta.textContent = `${peer.addr || peer.hostname || "LAN"} • seen ${timeLabel}`;

    main.append(header, meta);
    item.append(indicator, main);

    const actions = document.createElement("div");
    actions.className = "peer-actions";

    const actionButton = document.createElement("button");
    actionButton.type = "button";
    if (cancelPending) {
      const label =
        connState === "pairing"
          ? "Cancel pairing"
          : connState === "backoff"
            ? "Retrying..."
            : "Connecting...";
      actionButton.className = "btn btn--peer-action btn--cancel";
      actionButton.textContent = label;
      actionButton.disabled = false;
      actionButton.dataset.peerAction = "cancel";
      actionButton.title = "Cancel connection";
      actionButton.setAttribute("aria-label", `Cancel connection to ${getPeerDisplayName(peer, visiblePeers)}`);
      actionButton.addEventListener("click", () => {
        if (typeof onCancelPeer === "function") {
          onCancelPeer(peer.peer_id);
        }
      });
    } else {
      actionButton.className = `btn btn--peer-action ${connected ? "btn--stop" : "btn-primary"}`;
      actionButton.textContent = connected ? "Disconnect" : "Connect";
      actionButton.dataset.peerAction = connected ? "disconnect" : "connect";
      actionButton.addEventListener("click", () => {
        if (connected) {
          if (typeof onDisconnectPeer === "function") {
            onDisconnectPeer(peer.peer_id);
          }
        } else {
          if (typeof onConnectPeer === "function") {
            onConnectPeer(peer.peer_id);
          }
        }
      });
    }
    actions.append(actionButton);

    if (isPeerPaired(peer, state.config)) {
      const forgetButton = document.createElement("button");
      forgetButton.type = "button";
      forgetButton.className = "btn btn--peer-action btn--forget";
      forgetButton.textContent = "Forget";
      forgetButton.dataset.peerAction = "forget";
      forgetButton.addEventListener("click", () => {
        if (typeof onUnpairPeer === "function") {
          onUnpairPeer(peer.peer_id);
        }
      });
      actions.append(forgetButton);
    }

    item.append(actions);
    return item;
  };

  if (pairedPeers.length > 0) {
    pairedPeers.forEach((peer) => {
      ui.peerList.appendChild(renderItem(peer));
    });
  }

  if (nearbyPeers.length > 0) {
    const sectionHeader = document.createElement("div");
    sectionHeader.className = "peer-section-header";
    sectionHeader.textContent = "Nearby Devices";
    ui.peerList.appendChild(sectionHeader);

    nearbyPeers.forEach((peer) => {
      ui.peerList.appendChild(renderItem(peer));
    });
  }

  return normalizedPeers;
}
