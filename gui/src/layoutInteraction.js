import {
  getNodeSize,
  LOCAL_ANCHOR,
  normalizeConnectedPlacement,
} from "./layoutMath.js";
import {
  getPeerById,
  placementWouldOverlap,
  updateConfigPlacement,
} from "./layoutModel.js";

function extractDraggablePeerId(eventTarget) {
  if (!(eventTarget instanceof Element)) {
    return null;
  }

  const node = eventTarget.closest("[data-draggable='true']");
  if (!node) {
    return null;
  }

  const peerId = node.getAttribute("data-peer-id");
  return peerId ? peerId.trim() : null;
}

export function clientToSvgPoint(svg, clientX, clientY) {
  const matrix = svg.getScreenCTM();
  if (!matrix) {
    return { x: 0, y: 0 };
  }

  const point = new DOMPoint(clientX, clientY);
  const transformed = point.matrixTransform(matrix.inverse());
  return {
    x: transformed.x,
    y: transformed.y,
  };
}

export function createLayoutInteraction({
  state,
  ui,
  renderLayoutDiagram,
  scheduleConfigSave,
  flushConfigSaveDebounce,
  markConfigEdited,
  setConfigStatus,
  perfLog,
  onDragEnd,
}) {
  function beginLayoutDrag(event) {
    if (!state.config || !ui.layoutSvg || event.button !== 0) {
      return;
    }

    const peerId = extractDraggablePeerId(event.target);
    const draggedPeer = getPeerById(state.peers, peerId);
    const renderPlacement = state.layoutRenderCache.placements.get(peerId);
    if (!peerId || !draggedPeer || !renderPlacement) {
      return;
    }

    state.perf.dragMoveCount = 0;
    state.perf.dragStartTs = performance.now();
    state.perf.dragLastMoveTs = state.perf.dragStartTs;
    state.layoutDrag = {
      peerId,
      pointerId: event.pointerId,
      offsetX: 0,
      offsetY: 0,
      nodeSize: renderPlacement.size,
      peer: draggedPeer,
    };

    ui.layoutSvg.classList.add("layout-svg--dragging");
    ui.layoutSvg.setPointerCapture(event.pointerId);
    event.preventDefault();
    renderLayoutDiagram();
  }

  let rafPending = false;
  let latestEvent = null;

  function processLayoutDrag(event) {
    if (
      !state.layoutDrag ||
      !ui.layoutSvg ||
      state.layoutDrag.pointerId !== event.pointerId
    ) {
      return;
    }

    const pointerPoint = clientToSvgPoint(
      ui.layoutSvg,
      event.clientX,
      event.clientY,
    );
    const now = performance.now();
    const sinceLastMoveMs = now - state.perf.dragLastMoveTs;
    state.perf.dragLastMoveTs = now;
    state.perf.dragMoveCount += 1;
    const nextX = Math.round(pointerPoint.x - state.layoutDrag.offsetX);
    const nextY = Math.round(pointerPoint.y - state.layoutDrag.offsetY);
    const draggedPeerSize = state.layoutDrag.nodeSize ?? getNodeSize(state.layoutDrag.peer);
    const localSize = getNodeSize(state.layoutRenderCache.localPeer);
    const projected = normalizeConnectedPlacement(
      nextX,
      nextY,
      draggedPeerSize,
      {
        x: LOCAL_ANCHOR.x,
        y: LOCAL_ANCHOR.y,
        width: localSize.width,
        height: localSize.height,
      },
    );
    const currentPlacement = state.layoutRenderCache.placements.get(
      state.layoutDrag.peerId,
    );

    if (
      placementWouldOverlap(
        state.layoutDrag.peerId,
        projected.x,
        projected.y,
        draggedPeerSize,
        state.layoutRenderCache,
      )
    ) {
      event.preventDefault();
      return;
    }

    if (
      currentPlacement &&
      currentPlacement.x === projected.x &&
      currentPlacement.y === projected.y
    ) {
      event.preventDefault();
      return;
    }

    state.config = updateConfigPlacement(
      state.config,
      state.layoutDrag.peer,
      projected.x,
      projected.y,
      state.peers,
    );
    markConfigEdited();
    setConfigStatus("Editing layout...");
    renderLayoutDiagram();
    scheduleConfigSave();

    if (sinceLastMoveMs > 80) {
      perfLog("warn", `drag pointer gap ${sinceLastMoveMs.toFixed(1)}ms`, {
        sinceLastMoveMs,
        dragMoveCount: state.perf.dragMoveCount,
        peerId: state.layoutDrag.peerId,
      });
    }

    event.preventDefault();
  }

  function continueLayoutDrag(event) {
    if (!state.layoutDrag || state.layoutDrag.pointerId !== event.pointerId) {
      return;
    }

    if (event.isTrusted && typeof requestAnimationFrame === "function") {
      latestEvent = event;
      if (!rafPending) {
        rafPending = true;
        requestAnimationFrame(() => {
          rafPending = false;
          if (latestEvent) {
            const ev = latestEvent;
            latestEvent = null;
            processLayoutDrag(ev);
          }
        });
      }
      event.preventDefault();
      return;
    }

    processLayoutDrag(event);
  }

  function endLayoutDrag(event) {
    if (!state.layoutDrag || state.layoutDrag.pointerId !== event.pointerId) {
      return;
    }

    if (latestEvent) {
      processLayoutDrag(latestEvent);
      latestEvent = null;
    }
    rafPending = false;

    const draggedPeerId = state.layoutDrag.peerId;
    state.layoutDrag = null;

    if (ui.layoutSvg?.hasPointerCapture(event.pointerId)) {
      ui.layoutSvg.releasePointerCapture(event.pointerId);
    }
    ui.layoutSvg?.classList.remove("layout-svg--dragging");

    const dragDurationMs = performance.now() - state.perf.dragStartTs;
    if (state.perf.dragMoveCount > 0) {
      const avgMoveIntervalMs = dragDurationMs / state.perf.dragMoveCount;
      perfLog("debug", "drag session completed", {
        peerId: draggedPeerId,
        dragDurationMs,
        dragMoveCount: state.perf.dragMoveCount,
        avgMoveIntervalMs,
      });
    }

    renderLayoutDiagram();
    flushConfigSaveDebounce();
    onDragEnd?.();
  }

  return {
    beginLayoutDrag,
    continueLayoutDrag,
    endLayoutDrag,
  };
}
