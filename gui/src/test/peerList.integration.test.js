import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadHtml,
  registerDefaultCommands,
  registerCommand,
  stubSvgMethods,
  initApp,
  clearCommandHandlers,
  defaultStatus,
  defaultConfig,
  buildPeer,
  buildLocalPeer,
  fireEvent,
  hasEventListener,
  peerListSnapshot,
} from "./helpers.js";

describe("Peer list display", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    loadHtml();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearCommandHandlers();
    document.documentElement.innerHTML = "";
  });

  async function setupWithPeers(peers, statusOverrides = {}, configOverrides = {}) {
    const defaultConnected = peers
      .filter((p) => !p.is_local && p.discovery_status !== "stale")
      .map((p) => ({ peer_id: p.peer_id, name: p.name, platform: p.platform }));
    registerDefaultCommands({
      peers,
      status: defaultStatus({
        connected_peers: defaultConnected,
        ...statusOverrides,
      }),
      config: defaultConfig(configOverrides),
    });
    vi.resetModules();
    await import("../main.js");
    stubSvgMethods();
    await initApp();
    fireEvent("peers-updated", { peers });
  }

  describe("3.1 Peers appear in the list after discovery event", () => {
    it("renders peers from initial get_peers_cached response", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithPeers([localPeer, remotePeer]);

      const peerList = document.querySelector("#peer-list");
      const items = peerList.querySelectorAll("li");

      expect(items.length).toBe(1);
      expect(items[0].textContent).toContain("Alpha");
    });

    it("keeps a discovered peer when an older startup snapshot arrives later", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({ name: "MacBook", connected: false });
      let resolveSnapshot;
      registerDefaultCommands({
        status: defaultStatus({ engine_running: true }),
        commands: {
          get_peers_cached: () => new Promise((resolve) => {
            resolveSnapshot = resolve;
          }),
        },
      });
      vi.resetModules();
      await import("../main.js");
      stubSvgMethods();
      await initApp();

      fireEvent("peers-updated", { peers: [localPeer, remotePeer] });
      expect(document.querySelector("#peer-list").textContent).toContain("MacBook");

      resolveSnapshot([localPeer]);
      await vi.advanceTimersByTimeAsync(0);

      expect(document.querySelector("#peer-list").textContent).toContain("MacBook");
    });

    it("renders peers when peers-updated event fires", async () => {
      const localPeer = buildLocalPeer();
      await setupWithPeers([localPeer]);

      expect(hasEventListener("peers-updated")).toBe(true);

      const remotePeer = buildPeer({ peer_id: "peer-b", name: "Beta" });
      fireEvent("status-updated", {
        status: defaultStatus({
          connected_peers: [{ peer_id: "peer-b", name: "Beta" }],
        }),
      });
      fireEvent("peers-updated", { peers: [localPeer, remotePeer] });

      const peerList = document.querySelector("#peer-list");
      const items = peerList.querySelectorAll("li");

      expect(items.length).toBe(1);
      expect(items[0].textContent).toContain("Beta");
    });

    it("sets data-peer-id attribute on each list item", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({ peer_id: "peer-xyz-123", name: "TestPeer" });

      await setupWithPeers([localPeer, remotePeer]);

      const peerList = document.querySelector("#peer-list");
      const remoteItem = peerList.querySelector('[data-peer-id="peer-xyz-123"]');

      expect(remoteItem).not.toBeNull();
      expect(remoteItem.textContent).toContain("TestPeer");
    });

  });

  describe("3.2 Connected peer shows connected state", () => {
    it("displays connected indicator and disconnect action for peer in connected_peers array", async () => {
      const localPeer = buildLocalPeer();
      const connectedPeer = buildPeer({ peer_id: "peer-connected", name: "ConnectedPeer" });

      await setupWithPeers([localPeer, connectedPeer], {
        connected_peers: [{ peer_id: "peer-connected", name: "ConnectedPeer" }],
      });

      const peerList = document.querySelector("#peer-list");
      const connectedItem = peerList.querySelector('[data-peer-id="peer-connected"]');

      expect(connectedItem.querySelector(".peer-status-dot--connected")).not.toBeNull();
      expect(connectedItem.querySelector(".btn--peer-action").textContent).toBe("Disconnect");
      expect(connectedItem.classList.contains("peer-row--stale")).toBe(false);
    });

    it("displays pairing indicator and cancel button for peer in pairing state", async () => {
      const localPeer = buildLocalPeer();
      const pairingPeer = buildPeer({
        peer_id: "peer-pairing",
        name: "PairingPeer",
        connection_status: "pairing",
        connected: false,
      });

      await setupWithPeers([localPeer, pairingPeer], {
        connected_peers: [],
      });

      const peerList = document.querySelector("#peer-list");
      const pairingItem = peerList.querySelector('[data-peer-id="peer-pairing"]');

      expect(pairingItem.querySelector(".peer-status-dot--pairing")).not.toBeNull();
      const actionBtn = pairingItem.querySelector(".btn--peer-action");
      expect(actionBtn.textContent).toBe("Cancel pairing");
      expect(actionBtn.disabled).toBe(false);
      expect(actionBtn.dataset.peerAction).toBe("cancel");
      expect(actionBtn.classList.contains("btn--cancel")).toBe(true);
    });

    it.each([
      ["connecting", "Connecting..."],
      ["backoff", "Retrying..."],
    ])("shows startup %s state without a manual connect click", async (connectionStatus, label) => {
      const localPeer = buildLocalPeer();
      const connectingPeer = buildPeer({
        peer_id: "peer-connecting",
        name: "ConnectingPeer",
        connection_status: connectionStatus,
        connected: false,
      });

      await setupWithPeers([localPeer, connectingPeer], {
        connected_peers: [],
      });

      const peerList = document.querySelector("#peer-list");
      const item = peerList.querySelector('[data-peer-id="peer-connecting"]');
      const actionBtn = item.querySelector(".btn--peer-action");
      expect(actionBtn.textContent).toBe(label);
      expect(actionBtn.dataset.peerAction).toBe("cancel");
      expect(actionBtn.title).toBe("Cancel connection");
      expect(actionBtn.getAttribute("aria-label")).toBe("Cancel connection to ConnectingPeer");
      expect(item.querySelector('[data-peer-action="connect"]')).toBeNull();

      fireEvent("peers-updated", {
        peers: [localPeer, { ...connectingPeer, connection_status: "disconnected" }],
      });
      expect(peerList.querySelector('[data-peer-action="connect"]').textContent).toBe("Connect");

      fireEvent("peers-updated", {
        peers: [localPeer, { ...connectingPeer, connected: true, connection_status: "connected" }],
      });
      expect(peerList.querySelector('[data-peer-action="disconnect"]').textContent).toBe("Disconnect");
    });

    it("clicking cancel invokes cancel_connection", async () => {
      const localPeer = buildLocalPeer();
      const connectingPeer = buildPeer({
        peer_id: "peer-connecting",
        name: "ConnectingPeer",
        connection_status: "connecting",
        connected: false,
      });

      await setupWithPeers([localPeer, connectingPeer], {
        connected_peers: [],
      });

      let cancelPeerId = null;
      registerCommand("cancel_connection", ({ peerId }) => {
        cancelPeerId = peerId;
        return defaultStatus();
      });

      const peerList = document.querySelector("#peer-list");
      const item = peerList.querySelector('[data-peer-id="peer-connecting"]');
      const actionBtn = item.querySelector('[data-peer-action="cancel"]');
      actionBtn.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(cancelPeerId).toBe("peer-connecting");
    });

    it("shows disconnected discovered peer in peer list", async () => {
      const localPeer = buildLocalPeer();
      const disconnectedPeer = buildPeer({ peer_id: "peer-disconnected", name: "DisconnectedPeer" });

      await setupWithPeers([localPeer, disconnectedPeer], {
        connected_peers: [],
      });

      const peerList = document.querySelector("#peer-list");
      const disconnectedItem = peerList.querySelector('[data-peer-id="peer-disconnected"]');

      expect(disconnectedItem).not.toBeNull();
      expect(disconnectedItem.textContent).toContain("DisconnectedPeer");
    });

  });

  describe("3.3 Stale / disconnected peers are not rendered", () => {
    it("does not render stale peer in peer list", async () => {
      const localPeer = buildLocalPeer();
      const stalePeer = buildPeer({
        peer_id: "peer-stale",
        name: "StalePeer",
        discovery_status: "stale",
        last_seen_ms: 30000,
      });

      await setupWithPeers([localPeer, stalePeer]);

      const staleItem = document.querySelector('#peer-list [data-peer-id="peer-stale"]');

      expect(staleItem).toBeNull();
    });

    it("renders online connected peer", async () => {
      const onlinePeer = buildPeer({
        peer_id: "peer-online",
        discovery_status: "online",
      });

      await setupWithPeers([buildLocalPeer(), onlinePeer]);

      const onlineItem = document.querySelector('#peer-list [data-peer-id="peer-online"]');

      expect(onlineItem).not.toBeNull();
    });

  });

  describe("3.4 Empty peer list shows help message", () => {
    it("displays help message when no peers discovered", async () => {
      await setupWithPeers([]);

      const peerList = document.querySelector("#peer-list");
      const items = peerList.querySelectorAll("li");

      expect(items.length).toBe(1);
      expect(items[0].classList.contains("peer-meta")).toBe(true);
      expect(items[0].textContent).toBe(
        "No peers discovered. Peers appear when their engine is running."
      );
    });

    it("removes help message when peer appears", async () => {
      await setupWithPeers([]);

      let items = document.querySelectorAll("#peer-list li");
      expect(items.length).toBe(1);
      expect(items[0].classList.contains("peer-meta")).toBe(true);

      const peer = buildPeer();
      fireEvent("status-updated", {
        status: defaultStatus({
          connected_peers: [{ peer_id: peer.peer_id, name: peer.name }],
        }),
      });
      fireEvent("peers-updated", { peers: [buildLocalPeer(), peer] });

      items = document.querySelectorAll("#peer-list li");
      expect(items.length).toBe(1);
      expect(items[0].classList.contains("peer-meta")).toBe(false);
    });

  });

  describe("3.5 Local peer is hidden from discovered peers", () => {
    it("shows the empty-state message when only the local peer is present", async () => {
      const localPeer = buildLocalPeer({ name: "MyMachine" });

      await setupWithPeers([localPeer]);

      const items = document.querySelectorAll("#peer-list li");
      expect(items.length).toBe(1);
      expect(items[0].classList.contains("peer-meta")).toBe(true);
      expect(document.querySelector('#peer-list [data-peer-id="local-peer"]')).toBeNull();
    });

    it("does not render a local row when remote peers are present", async () => {
      const localPeer = buildLocalPeer();
      const remotePeer = buildPeer({ peer_id: "remote-1", name: "RemoteMachine" });

      await setupWithPeers([localPeer, remotePeer]);

      const remoteItem = document.querySelector('#peer-list [data-peer-id="remote-1"]');
      expect(remoteItem.textContent).toContain("RemoteMachine");
      expect(document.querySelector('#peer-list [data-peer-id="local-peer"]')).toBeNull();
    });
  });

  describe("3.6 Duplicate peer names show peer_id suffix", () => {
    it("appends truncated peer_id when names collide", async () => {
      const peer1 = buildPeer({ peer_id: "peer-alpha-123", name: "Shared" });
      const peer2 = buildPeer({ peer_id: "peer-beta-456", name: "Shared" });

      await setupWithPeers([buildLocalPeer(), peer1, peer2]);

      const item1 = document.querySelector('#peer-list [data-peer-id="peer-alpha-123"]');
      const item2 = document.querySelector('#peer-list [data-peer-id="peer-beta-456"]');

      expect(item1.textContent).toContain("Shared [peer-alp]");
      expect(item2.textContent).toContain("Shared [peer-bet]");
    });

    it("does not append suffix when names are unique", async () => {
      const peer1 = buildPeer({ peer_id: "peer-1", name: "Alpha" });
      const peer2 = buildPeer({ peer_id: "peer-2", name: "Beta" });

      await setupWithPeers([buildLocalPeer(), peer1, peer2]);

      const item1 = document.querySelector('#peer-list [data-peer-id="peer-1"]');
      const item2 = document.querySelector('#peer-list [data-peer-id="peer-2"]');

      expect(item1.textContent).not.toContain("[");
      expect(item2.textContent).not.toContain("[");
    });

  });

  describe("3.7 Status update re-renders peer list", () => {
    it("updates connection status when status-updated then peers-updated events fire", async () => {
      const peer = buildPeer({ peer_id: "peer-a", name: "Alpha" });

      await setupWithPeers([buildLocalPeer(), peer], {
        connected_peers: [],
      });

      let item = document.querySelector('#peer-list [data-peer-id="peer-a"]');
      expect(item).not.toBeNull();
      expect(item.querySelector(".peer-status-dot--disconnected")).not.toBeNull();
      expect(item.querySelector(".btn--peer-action").textContent).toBe("Connect");

      expect(hasEventListener("status-updated")).toBe(true);
      expect(hasEventListener("peers-updated")).toBe(true);

      fireEvent("status-updated", {
        status: defaultStatus({
          connected_peers: [{ peer_id: "peer-a", name: "Alpha" }],
        }),
      });

      fireEvent("peers-updated", { peers: [buildLocalPeer(), peer] });

      item = document.querySelector('#peer-list [data-peer-id="peer-a"]');
      expect(item).not.toBeNull();
      expect(item.querySelector(".peer-status-dot--connected")).not.toBeNull();
      expect(item.querySelector(".btn--peer-action").textContent).toBe("Disconnect");
    });

  });

  describe("Phase 1 repro: status freshness lag", () => {
    it("re-renders peer rows immediately on status-updated without waiting for peers-updated", async () => {
      const peer = buildPeer({ peer_id: "peer-lag", name: "LagPeer" });
      await setupWithPeers([buildLocalPeer(), peer], {
        connected_peers: [],
      });

      fireEvent("status-updated", {
        status: defaultStatus({
          connected_peers: [{ peer_id: "peer-lag", name: "LagPeer" }],
        }),
      });

      const item = document.querySelector('#peer-list [data-peer-id="peer-lag"]');
      expect(
        item?.querySelector(".peer-status-dot--connected"),
        `status-only refresh should mark peer connected; snapshot=${JSON.stringify(peerListSnapshot())}`,
      ).not.toBeNull();
      expect(item?.querySelector(".btn--peer-action").textContent).toBe("Disconnect");
    });

    it("re-renders peer rows immediately on disconnect status-updated", async () => {
      const peer = buildPeer({ peer_id: "peer-lag", name: "LagPeer" });
      await setupWithPeers([buildLocalPeer(), peer], {
        connected_peers: [{ peer_id: "peer-lag", name: "LagPeer" }],
      });

      let item = document.querySelector('#peer-list [data-peer-id="peer-lag"]');
      expect(item).not.toBeNull();

      fireEvent("status-updated", {
        status: defaultStatus({
          connected_peers: [],
        }),
      });

      item = document.querySelector('#peer-list [data-peer-id="peer-lag"]');
      expect(item).not.toBeNull();
      expect(
        item.querySelector(".peer-status-dot--disconnected"),
        `status-only refresh should update disconnected peer; snapshot=${JSON.stringify(peerListSnapshot())}`,
      ).not.toBeNull();
      expect(item.querySelector(".btn--peer-action").textContent).toBe("Connect");
    });
  });

  describe("Phase 1 repro: identity normalization", () => {
    it("filters self entries and deduplicates by canonical peer_id", async () => {
      const localPeer = buildLocalPeer({ peer_id: "anchor-peer", name: "Anchor" });
      const remote = buildPeer({ peer_id: "peer-dup", name: "Duplicate" });
      const duplicateRemote = buildPeer({ peer_id: " peer-dup ", name: "Duplicate Copy" });
      const leakedSelf = buildPeer({ peer_id: "anchor-peer", name: "Anchor Echo", is_local: false });

      await setupWithPeers([localPeer, remote, duplicateRemote, leakedSelf]);

      const rows = Array.from(document.querySelectorAll("#peer-list li"));
      const normalizedRemoteRows = rows.filter(
        (row) => (row.dataset.peerId ?? "").trim() === "peer-dup",
      );
      const leakedSelfRows = rows.filter(
        (row) => (row.dataset.peerId ?? "").trim() === "anchor-peer" && !row.textContent.includes("(Local)"),
      );

      expect(
        normalizedRemoteRows.length,
        `expected one canonical remote row; snapshot=${JSON.stringify(peerListSnapshot())}`,
      ).toBe(1);
      expect(
        leakedSelfRows.length,
        `expected zero leaked self rows; snapshot=${JSON.stringify(peerListSnapshot())}`,
      ).toBe(0);
    });

    it("keeps one canonical row after repeated updates with mixed peer_id casing", async () => {
      const localPeer = buildLocalPeer({ peer_id: "anchor-peer", name: "Anchor" });
      const first = buildPeer({ peer_id: "Peer-Dedup", name: "Dedup" });
      await setupWithPeers([localPeer, first]);

      const second = buildPeer({ peer_id: " peer-dedup ", name: "Dedup Copy" });
      fireEvent("peers-updated", { peers: [localPeer, second] });

      const rows = Array.from(document.querySelectorAll("#peer-list li"));
      const canonicalRows = rows.filter((row) => row.dataset.peerId === "peer-dedup");
      expect(
        canonicalRows.length,
        `expected one deduplicated row after repeated updates; snapshot=${JSON.stringify(peerListSnapshot())}`,
      ).toBe(1);
    });
  });

  describe("3.6 Manual connection and pairing controls", () => {
    it("shows connect button for disconnected discovered peer and connects when clicked", async () => {
      const localPeer = buildLocalPeer();
      const discoveredPeer = buildPeer({ peer_id: "peer-b", name: "Beta" });

      await setupWithPeers([localPeer, discoveredPeer], {
        connected_peers: [],
      });

      let acceptedPeerId = null;
      registerCommand("accept_connection", ({ peerId }) => {
        acceptedPeerId = peerId;
        return defaultStatus();
      });

      const item = document.querySelector('#peer-list [data-peer-id="peer-b"]');
      expect(item).not.toBeNull();

      const btn = item.querySelector(".btn--peer-action");
      expect(btn).not.toBeNull();
      expect(btn.textContent).toBe("Connect");
      expect(btn.dataset.peerAction).toBe("connect");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(acceptedPeerId).toBe("peer-b");
    });

    it("shows disconnect button for connected peer and disconnects when clicked", async () => {
      const localPeer = buildLocalPeer();
      const connectedPeer = buildPeer({ peer_id: "peer-c", name: "Gamma" });

      await setupWithPeers([localPeer, connectedPeer], {
        connected_peers: [{ peer_id: "peer-c", name: "Gamma" }],
      });

      let rejectedPeerId = null;
      registerCommand("reject_connection", ({ peerId }) => {
        rejectedPeerId = peerId;
        return defaultStatus();
      });

      const item = document.querySelector('#peer-list [data-peer-id="peer-c"]');
      expect(item).not.toBeNull();

      const btn = item.querySelector(".btn--peer-action");
      expect(btn).not.toBeNull();
      expect(btn.textContent).toBe("Disconnect");
      expect(btn.dataset.peerAction).toBe("disconnect");

      btn.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(rejectedPeerId).toBe("peer-c");
    });

    it("does not show disconnected devices in the display layout", async () => {
      const localPeer = buildLocalPeer();
      const disconnectedPeer = buildPeer({ peer_id: "peer-d", name: "Delta" });

      await setupWithPeers([localPeer, disconnectedPeer], {
        connected_peers: [],
      });

      const layoutSvg = document.querySelector("#layout-svg");
      const disconnectedNode = layoutSvg?.querySelector('[data-peer-id="peer-d"]');
      expect(disconnectedNode).toBeNull();
    });

    it("renders paired peer with Connect button when disconnected and no pairing badge", async () => {
      const localPeer = buildLocalPeer();
      const pairedPeer = buildPeer({ peer_id: "peer-paired-1", name: "PairedDevice" });

      await setupWithPeers([localPeer, pairedPeer], {
        connected_peers: [],
      }, {
        paired_peers: ["peer-paired-1"],
      });

      const item = document.querySelector('#peer-list [data-peer-id="peer-paired-1"]');
      expect(item).not.toBeNull();
      expect(item.querySelector(".peer-pairing-badge")).toBeNull();
      const actionBtn = item.querySelector(".btn--peer-action");
      expect(actionBtn.textContent).toBe("Connect");
    });

    it("clicking Disconnect disconnects peer without unpairing", async () => {
      const localPeer = buildLocalPeer();
      const connectedPeer = buildPeer({ peer_id: "peer-connected-1", name: "ActiveDevice" });

      await setupWithPeers([localPeer, connectedPeer], {
        connected_peers: [{ peer_id: "peer-connected-1", name: "ActiveDevice", platform: null }],
      }, {
        paired_peers: ["peer-connected-1"],
      });

      let rejectedPeerId = null;
      registerCommand("reject_connection", ({ peerId }) => {
        rejectedPeerId = peerId;
        return defaultStatus({ connected_peers: [] });
      });

      const item = document.querySelector('#peer-list [data-peer-id="peer-connected-1"]');
      expect(item).not.toBeNull();
      const disconnectBtn = item.querySelector(".btn--stop");
      expect(disconnectBtn).not.toBeNull();
      expect(disconnectBtn.textContent).toBe("Disconnect");

      disconnectBtn.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(rejectedPeerId).toBe("peer-connected-1");
    });

    it("renders unpaired peer without pairing badge", async () => {
      const localPeer = buildLocalPeer();
      const nearbyPeer = buildPeer({ peer_id: "peer-nearby-1", name: "NearbyDevice" });

      await setupWithPeers([localPeer, nearbyPeer], {
        connected_peers: [],
      }, {
        paired_peers: [],
      });

      const item = document.querySelector('#peer-list [data-peer-id="peer-nearby-1"]');
      expect(item).not.toBeNull();
      expect(item.querySelector(".peer-pairing-badge")).toBeNull();
    });

    it("renders paired peers without a My Devices header and Nearby Devices header when both are present", async () => {
      const localPeer = buildLocalPeer();
      const pairedPeer = buildPeer({ peer_id: "peer-paired-2", name: "PairedTwo" });
      const nearbyPeer = buildPeer({ peer_id: "peer-nearby-2", name: "NearbyTwo" });

      await setupWithPeers([localPeer, pairedPeer, nearbyPeer], {
        connected_peers: [],
      }, {
        paired_peers: ["peer-paired-2"],
      });

      const sectionHeaders = Array.from(
        document.querySelectorAll("#peer-list .peer-section-header"),
      ).map((el) => el.textContent);

      expect(sectionHeaders).toEqual(["Nearby Devices"]);
    });

    it("connected but unpaired peer renders without pairing badge", async () => {
      const localPeer = buildLocalPeer();
      const nearbyPeer = buildPeer({ peer_id: "peer-nearby-3", name: "ConnectedNearby" });

      await setupWithPeers([localPeer, nearbyPeer], {
        connected_peers: [{ peer_id: "peer-nearby-3", name: "ConnectedNearby", platform: null }],
      }, {
        paired_peers: [],
      });

      const item = document.querySelector('#peer-list [data-peer-id="peer-nearby-3"]');
      expect(item).not.toBeNull();
      expect(item.querySelector(".peer-pairing-badge")).toBeNull();
    });

    it("renders Forget button for paired peer but not for nearby peer", async () => {
      const localPeer = buildLocalPeer();
      const pairedPeer = buildPeer({ peer_id: "peer-paired-forget", name: "PairedForget" });
      const nearbyPeer = buildPeer({ peer_id: "peer-nearby-noforget", name: "NearbyNoForget" });

      await setupWithPeers([localPeer, pairedPeer, nearbyPeer], {
        connected_peers: [],
      }, {
        paired_peers: ["peer-paired-forget"],
      });

      const pairedItem = document.querySelector('#peer-list [data-peer-id="peer-paired-forget"]');
      expect(pairedItem).not.toBeNull();
      const forgetBtn = pairedItem.querySelector('.btn--peer-action[data-peer-action="forget"]');
      expect(forgetBtn).not.toBeNull();
      expect(forgetBtn.textContent).toBe("Forget");

      const nearbyItem = document.querySelector('#peer-list [data-peer-id="peer-nearby-noforget"]');
      expect(nearbyItem).not.toBeNull();
      expect(nearbyItem.querySelector('.btn--peer-action[data-peer-action="forget"]')).toBeNull();
    });

    it("clicking Forget invokes unpair_peer and unpairs device", async () => {
      const localPeer = buildLocalPeer();
      const pairedPeer = buildPeer({ peer_id: "peer-paired-unpair", name: "PairedUnpair" });

      await setupWithPeers([localPeer, pairedPeer], {
        connected_peers: [],
      }, {
        paired_peers: ["peer-paired-unpair"],
      });

      let unpairPeerId = null;
      registerCommand("unpair_peer", ({ peerId }) => {
        unpairPeerId = peerId;
        return defaultStatus();
      });

      const pairedItem = document.querySelector('#peer-list [data-peer-id="peer-paired-unpair"]');
      const forgetBtn = pairedItem.querySelector('.btn--peer-action[data-peer-action="forget"]');
      expect(forgetBtn).not.toBeNull();

      forgetBtn.click();
      await vi.advanceTimersByTimeAsync(0);

      expect(unpairPeerId).toBe("peer-paired-unpair");
    });
  });
});
