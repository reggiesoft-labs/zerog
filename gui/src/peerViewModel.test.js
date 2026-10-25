import { describe, expect, it } from "vitest";

import {
  formatPairingCode,
  formatPeerPlatformLabel,
  getPeerOsKind,
  getLegacyIssueMessages,
  getPeerConnectionState,
  getPeerDisplayName,
  getPeerRowKey,
  getPeerStatusLabel,
  isPeerCancelPending,
  isPeerConnectable,
  isPeerConnected,
  isPeerPaired,
} from "./peerViewModel.js";
import { normalizePeersSnapshot } from "./peerList.js";

describe("peer view model helpers", () => {
  it("identifies whether a peer is connected", () => {
    const peer = { peer_id: "peer-a", name: "Alpha", is_local: false };
    const statusConnected = { connected_peers: [{ peer_id: "peer-a" }] };
    const statusDisconnected = { connected_peers: [] };

    expect(isPeerConnected(peer, statusConnected)).toBe(true);
    expect(isPeerConnected(peer, statusDisconnected)).toBe(false);
  });
  it("uses peer_id as the row identity key", () => {
    expect(getPeerRowKey({ peer_id: "peer-123", name: "Shared" })).toBe("peer-123");
  });

  it("normalizes row identity key to canonical lowercase id", () => {
    expect(getPeerRowKey({ peer_id: "  PEER-123  " })).toBe("peer-123");
  });

  it("renders duplicate display names with a peer id suffix", () => {
    const peers = [
      { peer_id: "peer-alpha-1234", name: "Shared", is_local: false },
      { peer_id: "peer-beta-5678", name: "Shared", is_local: false },
    ];

    expect(getPeerDisplayName(peers[0], peers)).toBe("Shared [peer-alp]");
    expect(getPeerDisplayName(peers[1], peers)).toBe("Shared [peer-bet]");
  });

  it("marks stale peers as disconnected and non-connectable", () => {
    const peer = { peer_id: "peer-a", name: "Alpha", discovery_status: "stale", is_local: false };
    const status = {
      connected_peers: [],
      incoming_connections: [],
    };

    expect(getPeerStatusLabel(peer, status)).toBe("Stale");
    expect(isPeerConnectable(peer, status)).toBe(false);
  });

  it("matches status peers by canonical peer_id", () => {
    const peer = { peer_id: "Peer-A", name: "Alpha", discovery_status: "online", is_local: false };
    const status = {
      connected_peers: [{ peer_id: " peer-a " }],
      incoming_connections: [],
    };

    expect(getPeerStatusLabel(peer, status)).toBe("Connected");
    expect(isPeerConnectable(peer, status)).toBe(true);
  });

  it("formats legacy config ambiguity messages", () => {
    expect(
      getLegacyIssueMessages({
        legacy_issues: [
          {
            peer_name: "Shared",
            reason: "Multiple discovered peers share this display name",
            location: "left",
          },
        ],
      }),
    ).toEqual([
      "Shared: Multiple discovered peers share this display name (left)",
    ]);
  });

  it("treats peer as connected if peer.connected is true or present in status.connected_peers", () => {
    const connectedPeer = { peer_id: "peer-x", connected: true };
    const disconnectedPeer = { peer_id: "peer-y", connected: false };

    expect(isPeerConnected(connectedPeer, { connected_peers: [] })).toBe(true);
    expect(isPeerConnected(disconnectedPeer, { connected_peers: [{ peer_id: "peer-y" }] })).toBe(true);
    expect(isPeerConnected(disconnectedPeer, { connected_peers: [] })).toBe(false);
  });

  it("uses canonical paired boolean directly without config lookup", () => {
    const pairedPeer = { peer_id: "peer-x", paired: true };
    const unpairedPeer = { peer_id: "peer-y", paired: false };

    expect(isPeerPaired(pairedPeer, { paired_peers: [] })).toBe(true);
    expect(isPeerPaired(unpairedPeer, { paired_peers: ["peer-y"] })).toBe(false);
  });

  it("uses connection_status directly for status label", () => {
    expect(getPeerStatusLabel({ peer_id: "peer-1", connection_status: "incoming" }, {})).toBe("Incoming");
    expect(getPeerStatusLabel({ peer_id: "peer-2", connection_status: "connecting" }, {})).toBe("Connecting");
    expect(getPeerStatusLabel({ peer_id: "peer-3", connection_status: "backoff" }, {})).toBe("Backoff");
    expect(getPeerStatusLabel({ peer_id: "peer-4", connection_status: "pairing" }, {})).toBe("Pairing...");
  });

  it("classifies connecting/pairing/backoff as cancel-pending", () => {
    expect(getPeerConnectionState({ peer_id: "a", connection_status: "connecting" }, {})).toBe("connecting");
    expect(getPeerConnectionState({ peer_id: "a", connection_status: "pairing" }, {})).toBe("pairing");
    expect(getPeerConnectionState({ peer_id: "a", connection_status: "backoff" }, {})).toBe("backoff");
    expect(isPeerCancelPending({ peer_id: "a", connection_status: "connecting" }, {})).toBe(true);
    expect(isPeerCancelPending({ peer_id: "a", connection_status: "pairing" }, {})).toBe(true);
    expect(isPeerCancelPending({ peer_id: "a", connection_status: "backoff" }, {})).toBe(true);
    expect(isPeerCancelPending({ peer_id: "a", connection_status: "disconnected" }, {})).toBe(false);
    expect(isPeerCancelPending({ peer_id: "a", connected: true }, {})).toBe(false);
  });

  it("keeps connecting peers connectable so cancel stays reachable", () => {
    expect(isPeerConnectable({ peer_id: "a", connection_status: "connecting", discovery_status: "stale" }, {})).toBe(true);
    expect(isPeerConnectable({ peer_id: "a", connection_status: "backoff", discovery_status: "stale" }, {})).toBe(true);
  });

  it("formats peer platform labels correctly", () => {
    expect(formatPeerPlatformLabel("macos")).toBe("macOS");
    expect(formatPeerPlatformLabel("darwin")).toBe("macOS");
    expect(formatPeerPlatformLabel("windows")).toBe("Windows");
    expect(formatPeerPlatformLabel("win32")).toBe("Windows");
    expect(formatPeerPlatformLabel("linux")).toBe("Linux");
    expect(formatPeerPlatformLabel("freebsd")).toBe("Freebsd");
    expect(formatPeerPlatformLabel(null)).toBeNull();
    expect(formatPeerPlatformLabel("")).toBeNull();
  });

  it("maps peer platforms to layout icon kinds", () => {
    expect(getPeerOsKind("macos")).toBe("apple");
    expect(getPeerOsKind("darwin")).toBe("apple");
    expect(getPeerOsKind("windows")).toBe("windows");
    expect(getPeerOsKind("win32")).toBe("windows");
    expect(getPeerOsKind("linux")).toBe("linux");
    expect(getPeerOsKind("freebsd")).toBeNull();
    expect(getPeerOsKind(null)).toBeNull();
    expect(getPeerOsKind("")).toBeNull();
  });

  it("formats pairing codes as two digit groups", () => {
    expect(formatPairingCode("123456")).toBe("123 456");
    expect(formatPairingCode("000042")).toBe("000 042");
  });

  it("rejects malformed pairing codes", () => {
    expect(formatPairingCode("12345")).toBe("");
    expect(formatPairingCode("1234567")).toBe("");
    expect(formatPairingCode("12 3456")).toBe("");
    expect(formatPairingCode("abcdef")).toBe("");
    expect(formatPairingCode(null)).toBe("");
    expect(formatPairingCode(undefined)).toBe("");
  });
});

describe("normalizePeersSnapshot", () => {
  it("deduplicates direct peer when real peer with same address exists", () => {
    const peers = [
      { peer_id: "direct-192.168.1.50:51820", addr: "192.168.1.50:51820", is_local: false },
      { peer_id: "macbook-pro", addr: "192.168.1.50:51820", is_local: false, discovery_status: "online" },
    ];
    const normalized = normalizePeersSnapshot(peers);
    expect(normalized.length).toBe(1);
    expect(normalized[0].peer_id).toBe("macbook-pro");
  });

  it("retains direct peer when no real peer has that address", () => {
    const peers = [
      { peer_id: "direct-192.168.1.50:51820", addr: "192.168.1.50:51820", is_local: false },
      { peer_id: "macbook-pro", addr: "192.168.1.60:51820", is_local: false },
    ];
    const normalized = normalizePeersSnapshot(peers);
    expect(normalized.length).toBe(2);
  });
});
