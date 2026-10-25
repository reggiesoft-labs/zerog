# PRD: Auto-Connect Toggle for Peer Mesh

## Summary
Replace per-peer connect/disconnect buttons with a single toggle button that manages the entire peer mesh. When started, the system discovers and connects to all peers on the local network. When stopped, it disconnects from all peers and halts discovery/listening.

## Goals
- Simplify UX from per-peer management to single "Start/Stop" toggle
- Enable automatic mesh formation between all started peers on the same LAN
- Maintain existing peer discovery via local network broadcast (mDNS)

## Non-Goals
- Central signaling server or cloud-based discovery
- Manual peer selection or exclusion
- Persistent retry with backoff (connection attempts happen once per discovery cycle)

---

## User Stories

### US-1: Start Toggle Button
**As a** user  
**I want** a single "Start" button in the GUI  
**So that** I can begin discovering and connecting to all peers on my local network with one click

**Acceptance Criteria:**
- AC1: A toggle button labeled "Start" is visible when the engine is stopped
- AC2: Clicking "Start" initiates local network discovery (mDNS broadcast)
- AC3: Each discovered peer receives one connection attempt per discovery cycle
- AC4: Button label changes to "Stop" after successful start
- AC5: Status indicator reflects "Running" state after start
- AC6: Existing per-peer Connect/Disconnect buttons are removed or hidden

**Dependencies:** None

---

### US-2: Stop Toggle Button
**As a** user  
**I want** a single "Stop" button when the system is running  
**So that** I can disconnect from all peers and halt discovery with one click

**Acceptance Criteria:**
- AC1: When running, toggle button displays "Stop" label
- AC2: Clicking "Stop" disconnects all currently connected peers
- AC3: Discovery/listening services are stopped
- AC4: Button label changes to "Start" after successful stop
- AC5: Status indicator reflects "Stopped" state after stop
- AC6: All peer connection state is cleared

**Dependencies:** US-1

---

### US-3: Auto-Connect to Discovered Peers
**As a** started peer  
**I want** to automatically connect to other started peers on the same LAN  
**So that** the mesh forms without manual intervention

**Acceptance Criteria:**
- AC1: On each discovery cycle, newly discovered peers are connected to automatically
- AC2: Connection attempts are made once per discovery cycle (no persistent retry)
- AC3: Failed connections are retried on the next discovery cycle, not immediately
- AC4: Peer list UI shows connection status for all discovered peers
- AC5: Local peer appears in peer list with "(Local)" designation

**Dependencies:** US-1

---

### US-4: Peer Status Display
**As a** user  
**I want** to see which peers are discovered and their connection status  
**So that** I can verify the mesh is forming correctly

**Acceptance Criteria:**
- AC1: Peer list shows all discovered peers with hostname and IP address
- AC2: Each peer shows current connection state (connected/connecting/disconnected)
- AC3: Peer list updates as peers are discovered or disappear
- AC4: Last-seen timestamp is displayed for each peer
- AC5: Platform type (macOS/Windows/Linux) is shown when available

**Dependencies:** US-1

---

## Technical Notes
- Discovery: mDNS/broadcast on local network (existing mechanism)
- Backend: May require new Tauri command for "start mesh" / "stop mesh"
- State: Track `mesh_running` boolean alongside existing `engine_running`
- UI: Replace individual peer action buttons with read-only status display

## Quality Gates
For each user story implementation:
```bash
npm run lint && npm run typecheck && npm test
```

---

## Out of Scope
- Connection retry with exponential backoff
- Central signaling server
- Static peer list from config
- Soft stop (keep existing connections)
- Browser/E2E visual verification