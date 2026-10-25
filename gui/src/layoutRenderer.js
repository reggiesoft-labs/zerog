import {
  formatPeerPlatformLabel,
  formatRelativeTime,
  getPeerDisplayName,
  getPeerOsKind,
  getPeerStatusLabel,
} from "./peerViewModel.js";
import { buildDisplayShapePath, getPeerDisplayShape } from "./layoutShape.js";
import { getNodeSize, getVirtualBounds, LOCAL_ANCHOR } from "./layoutMath.js";

const SVG_NS = "http://www.w3.org/2000/svg";
const PERF_WARN_MAIN_THREAD_MS = 16;
const OS_BADGE_SIZE = 26;
const OS_ICON_SIZE = 20;
const YOU_BADGE_WIDTH = 34;
const YOU_BADGE_HEIGHT = 18;
const OS_ICON_PATHS = {
  apple: "M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701",
  windows: "M2 3h9.5v9H2zM12.5 3H22v9h-9.5zM2 13h9.5v9H2zM12.5 13H22v9h-9.5z",
  linux: "M12.504 0c-.155 0-.315.008-.48.021-4.226.333-3.105 4.807-3.17 6.298-.076 1.092-.3 1.953-1.05 3.02-.885 1.051-2.127 2.75-2.716 4.521-.278.832-.41 1.684-.287 2.489a.424.424 0 00-.11.135c-.26.268-.45.6-.663.839-.199.199-.485.267-.797.4-.313.136-.658.269-.864.68-.09.189-.136.394-.132.602 0 .199.027.4.055.536.058.399.116.728.04.97-.249.68-.28 1.145-.106 1.484.174.334.535.47.94.601.81.2 1.91.135 2.774.6.926.466 1.866.67 2.616.47.526-.116.97-.464 1.208-.946.587-.003 1.23-.269 2.26-.334.699-.058 1.574.267 2.577.2.025.134.063.198.114.333l.003.003c.391.778 1.113 1.132 1.884 1.071.771-.06 1.592-.536 2.257-1.306.631-.765 1.683-1.084 2.378-1.503.348-.199.629-.469.649-.853.023-.4-.2-.811-.714-1.376v-.097l-.003-.003c-.17-.2-.25-.535-.338-.926-.085-.401-.182-.786-.492-1.046h-.003c-.059-.054-.123-.067-.188-.135a.357.357 0 00-.19-.064c.431-1.278.264-2.55-.173-3.694-.533-1.41-1.465-2.638-2.175-3.483-.796-1.005-1.576-1.957-1.56-3.368.026-2.152.236-6.133-3.544-6.139zm.529 3.405h.013c.213 0 .396.062.584.198.19.135.33.332.438.533.105.259.158.459.166.724 0-.02.006-.04.006-.06v.105a.086.086 0 01-.004-.021l-.004-.024a1.807 1.807 0 01-.15.706.953.953 0 01-.213.335.71.71 0 00-.088-.042c-.104-.045-.198-.064-.284-.133a1.312 1.312 0 00-.22-.066c.05-.06.146-.133.183-.198.053-.128.082-.264.088-.402v-.02a1.21 1.21 0 00-.061-.4c-.045-.134-.101-.2-.183-.333-.084-.066-.167-.132-.267-.132h-.016c-.093 0-.176.03-.262.132a.8.8 0 00-.205.334 1.18 1.18 0 00-.09.4v.019c.002.089.008.179.02.267-.193-.067-.438-.135-.607-.202a1.635 1.635 0 01-.018-.2v-.02a1.772 1.772 0 01.15-.768c.082-.22.232-.406.43-.533a.985.985 0 01.594-.2zm-2.962.059h.036c.142 0 .27.048.399.135.146.129.264.288.344.465.09.199.14.4.153.667v.004c.007.134.006.2-.002.266v.08c-.03.007-.056.018-.083.024-.152.055-.274.135-.393.2.012-.09.013-.18.003-.267v-.015c-.012-.133-.04-.2-.082-.333a.613.613 0 00-.166-.267.248.248 0 00-.183-.064h-.021c-.071.006-.13.04-.186.132a.552.552 0 00-.12.27.944.944 0 00-.023.33v.015c.012.135.037.2.08.334.046.134.098.2.166.268.01.009.02.018.034.024-.07.057-.117.07-.176.136a.304.304 0 01-.131.068 2.62 2.62 0 01-.275-.402 1.772 1.772 0 01-.155-.667 1.759 1.759 0 01.08-.668 1.43 1.43 0 01.283-.535c.128-.133.26-.2.418-.2zm1.37 1.706c.332 0 .733.065 1.216.399.293.2.523.269 1.052.468h.003c.255.136.405.266.478.399v-.131a.571.571 0 01.016.47c-.123.31-.516.643-1.063.842v.002c-.268.135-.501.333-.775.465-.276.135-.588.292-1.012.267a1.139 1.139 0 01-.448-.067 3.566 3.566 0 01-.322-.198c-.195-.135-.363-.332-.612-.465v-.005h-.005c-.4-.246-.616-.512-.686-.71-.07-.268-.005-.47.193-.6.224-.135.38-.271.483-.336.104-.074.143-.102.176-.131h.002v-.003c.169-.202.436-.47.839-.601.139-.036.294-.065.466-.065zm2.8 2.142c.358 1.417 1.196 3.475 1.735 4.473.286.534.855 1.659 1.102 3.024.156-.005.33.018.513.064.646-1.671-.546-3.467-1.089-3.966-.22-.2-.232-.335-.123-.335.59.534 1.365 1.572 1.646 2.757.13.535.16 1.104.021 1.67.067.028.135.06.205.067 1.032.534 1.413.938 1.23 1.537v-.043c-.06-.003-.12 0-.18 0h-.016c.151-.467-.182-.825-1.065-1.224-.915-.4-1.646-.336-1.77.465-.008.043-.013.066-.018.135-.068.023-.139.053-.209.064-.43.268-.662.669-.793 1.187-.13.533-.17 1.156-.205 1.869v.003c-.02.334-.17.838-.319 1.35-1.5 1.072-3.58 1.538-5.348.334a2.645 2.645 0 00-.402-.533 1.45 1.45 0 00-.275-.333c.182 0 .338-.03.465-.067a.615.615 0 00.314-.334c.108-.267 0-.697-.345-1.163-.345-.467-.931-.995-1.788-1.521-.63-.4-.986-.87-1.15-1.396-.165-.534-.143-1.085-.015-1.645.245-1.07.873-2.11 1.274-2.763.107-.065.037.135-.408.974-.396.751-1.14 2.497-.122 3.854a8.123 8.123 0 01.647-2.876c.564-1.278 1.743-3.504 1.836-5.268.048.036.217.135.289.202.218.133.38.333.59.465.21.201.477.335.876.335.039.003.075.006.11.006.412 0 .73-.134.997-.268.29-.134.52-.334.74-.4h.005c.467-.135.835-.402 1.044-.7zm2.185 8.958c.037.6.343 1.245.882 1.377.588.134 1.434-.333 1.791-.765l.211-.01c.315-.007.577.01.847.268l.003.003c.208.199.305.53.391.876.085.4.154.78.409 1.066.486.527.645.906.636 1.14l.003-.007v.018l-.003-.012c-.015.262-.185.396-.498.595-.63.401-1.746.712-2.457 1.57-.618.737-1.37 1.14-2.036 1.191-.664.053-1.237-.2-1.574-.898l-.005-.003c-.21-.4-.12-1.025.056-1.69.176-.668.428-1.344.463-1.897.037-.714.076-1.335.195-1.814.12-.465.308-.797.641-.984l.045-.022zm-10.814.049h.01c.053 0 .105.005.157.014.376.055.706.333 1.023.752l.91 1.664.003.003c.243.533.754 1.064 1.189 1.637.434.598.77 1.131.729 1.57v.006c-.057.744-.48 1.148-1.125 1.294-.645.135-1.52.002-2.395-.464-.968-.536-2.118-.469-2.857-.602-.369-.066-.61-.2-.723-.4-.11-.2-.113-.602.123-1.23v-.004l.002-.003c.117-.334.03-.752-.027-1.118-.055-.401-.083-.71.043-.94.16-.334.396-.4.69-.533.294-.135.64-.202.915-.47h.002v-.002c.256-.268.445-.601.668-.838.19-.201.38-.336.663-.336zm7.159-9.074c-.435.201-.945.535-1.488.535-.542 0-.97-.267-1.28-.466-.154-.134-.28-.268-.373-.335-.164-.134-.144-.333-.074-.333.109.016.129.134.199.2.096.066.215.2.36.333.292.2.68.467 1.167.467.485 0 1.053-.267 1.398-.466.195-.135.445-.334.648-.467.156-.136.149-.267.279-.267.128.016.034.134-.147.332a8.097 8.097 0 01-.69.468zm-1.082-1.583V5.64c-.006-.02.013-.042.029-.05.074-.043.18-.027.26.004.063 0 .16.067.15.135-.006.049-.085.066-.135.066-.055 0-.092-.043-.141-.068-.052-.018-.146-.008-.163-.065zm-.551 0c-.02.058-.113.049-.166.066-.047.025-.086.068-.14.068-.05 0-.13-.02-.136-.068-.01-.066.088-.133.15-.133.08-.031.184-.047.259-.005.019.009.036.03.03.05v.02h.003z",
};

function createSvgElement(tagName, attrs = {}) {
  const node = document.createElementNS(SVG_NS, tagName);
  Object.entries(attrs).forEach(([key, value]) => {
    node.setAttribute(key, String(value));
  });
  return node;
}

function getNodeRectForCenter(x, y, size) {
  return {
    x: x - size.width / 2,
    y: y - size.height / 2,
    width: size.width,
    height: size.height,
  };
}

function setViewBoxForNodes(svg, nodeRects) {
  if (!nodeRects.length) {
    if (svg.getAttribute("viewBox") !== "0 0 1000 700") {
      svg.setAttribute("viewBox", "0 0 1000 700");
    }
    return;
  }

  const minX = Math.min(...nodeRects.map((rect) => rect.x));
  const minY = Math.min(...nodeRects.map((rect) => rect.y));
  const maxX = Math.max(...nodeRects.map((rect) => rect.x + rect.width));
  const maxY = Math.max(...nodeRects.map((rect) => rect.y + rect.height));
  const padding = 38;
  const width = Math.max(340, maxX - minX + padding * 2);
  const height = Math.max(220, maxY - minY + padding * 2);

  const nextMinX = Math.round(minX - padding);
  const nextMinY = Math.round(minY - padding);
  const nextWidth = Math.round(width);
  const nextHeight = Math.round(height);
  const nextViewBox = `${nextMinX} ${nextMinY} ${nextWidth} ${nextHeight}`;
  if (svg.getAttribute("viewBox") !== nextViewBox) {
    svg.setAttribute("viewBox", nextViewBox);
  }
  return { minX: nextMinX, minY: nextMinY, width: nextWidth, height: nextHeight };
}

function truncateLabel(text, maxChars) {
  const str = String(text ?? "").trim();
  if (str.length <= maxChars) {
    return str;
  }
  return `${str.slice(0, Math.max(4, maxChars - 1))}…`;
}

function buildStandPath(frame) {
  const neckW = Math.max(8, Math.min(16, frame.width * 0.12));
  const baseW = Math.max(30, Math.min(56, frame.width * 0.35));
  const cx = frame.x + frame.width / 2;
  const bottom = frame.y + frame.height;

  const nL = Math.round(cx - neckW / 2);
  const nR = Math.round(cx + neckW / 2);
  const bL = Math.round(cx - baseW / 2);
  const bR = Math.round(cx + baseW / 2);

  return `M ${nL} ${bottom - 1} H ${nR} V ${bottom + 6} H ${bR} V ${bottom + 9.5} H ${bL} V ${bottom + 6} H ${nL} Z`;
}

function appendStand(group, frame) {
  group.appendChild(
    createSvgElement("path", {
      class: "layout-node-stand",
      d: buildStandPath(frame),
    }),
  );
}

function appendOsBadge(group, x, y, peer) {
  const kind = getPeerOsKind(peer?.platform);
  const path = OS_ICON_PATHS[kind];
  if (!path) {
    return;
  }
  const badge = createSvgElement("g", {
    class: `layout-node-badge layout-node-os layout-node-os--${kind}`,
    "aria-hidden": "true",
  });
  badge.appendChild(
    createSvgElement("rect", {
      x,
      y,
      width: OS_BADGE_SIZE,
      height: OS_BADGE_SIZE,
      rx: 6,
      class: "layout-node-badge-bg",
    }),
  );
  const icon = createSvgElement("g", {
    transform: `translate(${x + (OS_BADGE_SIZE - OS_ICON_SIZE) / 2} ${y + (OS_BADGE_SIZE - OS_ICON_SIZE) / 2}) scale(${OS_ICON_SIZE / 24})`,
  });
  icon.appendChild(createSvgElement("path", { d: path }));
  badge.appendChild(icon);
  group.appendChild(badge);
}

function appendYouBadge(group, x, y) {
  const badge = createSvgElement("g", {
    class: "layout-node-badge layout-node-anchor-badge",
    "aria-hidden": "true",
  });
  badge.appendChild(
    createSvgElement("rect", {
      x,
      y,
      width: YOU_BADGE_WIDTH,
      height: YOU_BADGE_HEIGHT,
      rx: 5,
      class: "layout-node-badge-bg layout-node-anchor-badge-bg",
    }),
  );
  const text = createSvgElement("text", {
    x: x + YOU_BADGE_WIDTH / 2,
    y: y + YOU_BADGE_HEIGHT / 2,
    class: "layout-node-anchor-badge-text",
  });
  text.textContent = "YOU";
  badge.appendChild(text);
  group.appendChild(badge);
}

function formatDisplayResolution(shape, bounds) {
  const count = Array.isArray(shape?.rects) ? shape.rects.length : 1;
  const res = `${bounds.width} × ${bounds.height}`;
  if (count > 1) {
    return `${count} displays (${res})`;
  }
  return res;
}

function buildLocalTooltip(localPeer, shape) {
  const fullName = localPeer?.name ?? "Local";
  const bounds = getVirtualBounds(localPeer);
  const platform = formatPeerPlatformLabel(localPeer?.platform);
  const addr = localPeer?.addr && localPeer.addr !== "local" ? localPeer.addr : localPeer?.hostname;
  const lines = [fullName, "Role: This machine (Local Anchor)"];
  if (platform) {
    lines.push(`OS: ${platform}`);
  }
  lines.push(`Resolution: ${formatDisplayResolution(shape, bounds)}`);
  if (addr) {
    lines.push(`Address: ${addr}`);
  }
  return lines.join("\n");
}

function buildPeerTooltip(item, peers, status, shape, activeTransfer) {
  const fullName = getPeerDisplayName(item.peer, peers);
  const bounds = getVirtualBounds(item.peer);
  const platform = formatPeerPlatformLabel(item.peer?.platform);
  const statusLabel = getPeerStatusLabel(item.peer, status);
  const addr = item.peer?.addr || item.peer?.hostname;
  const lines = [fullName];
  if (platform) {
    lines.push(`OS: ${platform}`);
  }
  if (statusLabel) {
    lines.push(`Status: ${statusLabel}`);
  }
  lines.push(`Resolution: ${formatDisplayResolution(shape, bounds)}`);
  if (addr) {
    lines.push(`Address: ${addr}`);
  }
  if (activeTransfer) {
    const pct = Math.round(Math.max(0, Math.min(1, activeTransfer.percent ?? 0)) * 100);
    const dir = activeTransfer.is_outgoing ? "Receiving" : "Sending";
    lines.push(`Transfer: ${dir} ${pct}%`);
  }
  if (Number.isFinite(item.peer?.last_seen_ms)) {
    lines.push(`Last seen: ${formatRelativeTime(item.peer.last_seen_ms)}`);
  }
  return lines.join("\n");
}

function appendLocalNode(svg, localPeer, localSize) {
  const group = createSvgElement("g", {
    class: "layout-node layout-node--local",
  });
  const frame = getNodeRectForCenter(LOCAL_ANCHOR.x, LOCAL_ANCHOR.y, localSize);
  const shape = getPeerDisplayShape(localPeer);
  const fullName = localPeer?.name ?? "Local";
  const maxChars = Math.max(8, Math.floor(frame.width / 9.5));

  const tooltip = createSvgElement("title");
  tooltip.textContent = buildLocalTooltip(localPeer, shape);
  group.appendChild(tooltip);

  group.appendChild(
    createSvgElement("rect", {
      class: "layout-node-hitbox",
      ...frame,
      fill: "transparent",
      stroke: "transparent",
      "pointer-events": "all",
    }),
  );
  appendStand(group, frame);
  group.appendChild(
    createSvgElement("path", {
      class: "layout-node-shape",
      d: buildDisplayShapePath(shape, frame),
      "fill-rule": "evenodd",
    }),
  );
  appendOsBadge(group, frame.x + 8, frame.y + 8, localPeer);
  appendYouBadge(
    group,
    frame.x + frame.width - YOU_BADGE_WIDTH - 8,
    frame.y + 8 + (OS_BADGE_SIZE - YOU_BADGE_HEIGHT) / 2,
  );

  const label = createSvgElement("text", {
    x: LOCAL_ANCHOR.x,
    y: LOCAL_ANCHOR.y,
    "text-anchor": "middle",
    class: "layout-node-label",
  });
  label.textContent = truncateLabel(fullName, maxChars);
  group.appendChild(label);
  svg.appendChild(group);
}

function appendPeerNode(svg, item, peers, status, activePeerId, dropTargetPeerId, activeTransfer) {
  const group = createSvgElement("g", {
    class: `layout-node layout-node--peer ${item.persisted ? "layout-node--persisted" : "layout-node--fallback"}`,
    "data-peer-id": item.peerId,
    "data-draggable": "true",
  });

  if (activePeerId === item.peerId) {
    group.classList.add("layout-node--active");
  }

  if (dropTargetPeerId === item.peerId) {
    group.classList.add("layout-node--drop-target");
  }

  if (activeTransfer) {
    group.classList.add("layout-node--transferring");
  }

  const frame = getNodeRectForCenter(item.x, item.y, item.size);
  const shape = getPeerDisplayShape(item.peer);
  const fullName = getPeerDisplayName(item.peer, peers);
  const maxChars = Math.max(8, Math.floor(frame.width / 9.5));
  const hasSubtag = !item.peer.virtual_bounds && !activeTransfer;
  const labelY = hasSubtag || activeTransfer ? item.y - 8 : item.y;

  const tooltip = createSvgElement("title");
  tooltip.textContent = buildPeerTooltip(item, peers, status, shape, activeTransfer);
  group.appendChild(tooltip);

  group.appendChild(
    createSvgElement("rect", {
      class: "layout-node-hitbox",
      ...frame,
      fill: "transparent",
      stroke: "transparent",
      "pointer-events": "all",
    }),
  );
  appendStand(group, frame);
  group.appendChild(
    createSvgElement("path", {
      class: "layout-node-shape",
      d: buildDisplayShapePath(shape, frame),
      "fill-rule": "evenodd",
    }),
  );
  appendOsBadge(group, frame.x + 8, frame.y + 8, item.peer);

  const label = createSvgElement("text", {
    x: item.x,
    y: labelY,
    "text-anchor": "middle",
    class: "layout-node-label",
  });
  label.textContent = truncateLabel(fullName, maxChars);
  group.appendChild(label);

  if (activeTransfer) {
    const barWidth = Math.min(frame.width - 24, 76);
    const barHeight = 5;
    const barX = item.x - barWidth / 2;
    const barY = item.y + frame.height / 2 - 12;
    group.appendChild(
      createSvgElement("rect", {
        x: barX,
        y: barY,
        width: barWidth,
        height: barHeight,
        rx: 2.5,
        class: "layout-transfer-track",
      }),
    );
    const pct = Math.max(0, Math.min(1, activeTransfer.percent ?? 0));
    group.appendChild(
      createSvgElement("rect", {
        x: barX,
        y: barY,
        width: Math.max(2, barWidth * pct),
        height: barHeight,
        rx: 2.5,
        class: "layout-transfer-fill",
      }),
    );
    const pctText = createSvgElement("text", {
      x: item.x,
      y: barY - 4,
      "text-anchor": "middle",
      class: "layout-transfer-label",
    });
    pctText.textContent = `${activeTransfer.is_outgoing ? "Receiving" : "Sending"} ${Math.round(pct * 100)}%`;
    group.appendChild(pctText);
  } else if (hasSubtag) {
    const tag = createSvgElement("text", {
      x: item.x,
      y: item.y + 13,
      "text-anchor": "middle",
      class: "layout-node-tag",
    });
    tag.textContent = "Default size";
    group.appendChild(tag);
  }

  svg.appendChild(group);
}

function updatePeerNodePosition(group, item, activeTransfer) {
  const frame = getNodeRectForCenter(item.x, item.y, item.size);
  const shape = getPeerDisplayShape(item.peer);
  const hasSubtag = !item.peer.virtual_bounds && !activeTransfer;
  const labelY = hasSubtag || activeTransfer ? item.y - 8 : item.y;

  const hitbox = group.querySelector(".layout-node-hitbox");
  if (hitbox) {
    hitbox.setAttribute("x", String(frame.x));
    hitbox.setAttribute("y", String(frame.y));
  }

  const stand = group.querySelector(".layout-node-stand");
  if (stand) {
    stand.setAttribute("d", buildStandPath(frame));
  }

  const shapePath = group.querySelector(".layout-node-shape");
  if (shapePath) {
    shapePath.setAttribute("d", buildDisplayShapePath(shape, frame));
  }

  const badgeRect = group.querySelector(".layout-node-badge-bg");
  if (badgeRect) {
    badgeRect.setAttribute("x", String(frame.x + 8));
    badgeRect.setAttribute("y", String(frame.y + 8));
  }

  const badgeIcon = group.querySelector(".layout-node-badge g");
  if (badgeIcon) {
    badgeIcon.setAttribute(
      "transform",
      `translate(${frame.x + 8 + (OS_BADGE_SIZE - OS_ICON_SIZE) / 2} ${frame.y + 8 + (OS_BADGE_SIZE - OS_ICON_SIZE) / 2}) scale(${OS_ICON_SIZE / 24})`,
    );
  }

  const label = group.querySelector(".layout-node-label");
  if (label) {
    label.setAttribute("x", String(item.x));
    label.setAttribute("y", String(labelY));
  }

  const tag = group.querySelector(".layout-node-tag");
  if (tag) {
    tag.setAttribute("x", String(item.x));
    tag.setAttribute("y", String(item.y + 13));
  }

  if (activeTransfer) {
    const barWidth = Math.min(frame.width - 24, 76);
    const barX = item.x - barWidth / 2;
    const barY = item.y + frame.height / 2 - 12;
    const track = group.querySelector(".layout-transfer-track");
    if (track) {
      track.setAttribute("x", String(barX));
      track.setAttribute("y", String(barY));
    }
    const fill = group.querySelector(".layout-transfer-fill");
    if (fill) {
      fill.setAttribute("x", String(barX));
      fill.setAttribute("y", String(barY));
    }
    const pctText = group.querySelector(".layout-transfer-label");
    if (pctText) {
      pctText.setAttribute("x", String(item.x));
      pctText.setAttribute("y", String(barY - 4));
    }
  }
}

export function createLayoutRenderer({
  state,
  ui,
  getLayoutModel,
  perfLog,
  isStatusTabActive,
}) {
  return function renderLayoutDiagram() {
    if (!ui.layoutSvg) {
      return;
    }

    const renderStartedAt = performance.now();
    const svg = ui.layoutSvg;
    const model = getLayoutModel();

    if (state.layoutDrag) {
      const draggedPeerId = state.layoutDrag.peerId;
      const draggedGroup = svg.querySelector(`g[data-peer-id="${draggedPeerId}"]`);
      if (draggedGroup && draggedGroup.classList.contains("layout-node--active")) {
        const draggedItem = model.placements.find((item) => item.peerId === draggedPeerId);
        if (draggedItem) {
          state.layoutRenderCache.remotePeers = model.remotePeers;
          state.layoutRenderCache.localPeer = model.localPeer;
          state.layoutRenderCache.placements = new Map(
            model.placements.map((item) => [item.peerId, item]),
          );
          const activeTransfer = state.activeTransfers?.get(draggedPeerId);
          updatePeerNodePosition(draggedGroup, draggedItem, activeTransfer);

          const nodeRects = model.placements.map((item) =>
            getNodeRectForCenter(item.x, item.y, item.size),
          );
          const localSizeForRender = getNodeSize(model.localPeer);
          nodeRects.unshift(
            getNodeRectForCenter(LOCAL_ANCHOR.x, LOCAL_ANCHOR.y, localSizeForRender),
          );
          setViewBoxForNodes(svg, nodeRects);

          const renderDurationMs = performance.now() - renderStartedAt;
          perfLog("debug", `renderLayoutDiagram took ${renderDurationMs.toFixed(1)}ms`, {
            renderDurationMs,
            placementCount: model.placements.length,
          });
          return;
        }
      }
    }

    svg.innerHTML = "";
    state.layoutRenderCache.remotePeers = model.remotePeers;
    state.layoutRenderCache.localPeer = model.localPeer;
    state.layoutRenderCache.placements = new Map(
      model.placements.map((item) => [item.peerId, item]),
    );

    const nodeRects = model.placements.map((item) =>
      getNodeRectForCenter(item.x, item.y, item.size),
    );
    const localSizeForRender = getNodeSize(model.localPeer);
    nodeRects.unshift(
      getNodeRectForCenter(LOCAL_ANCHOR.x, LOCAL_ANCHOR.y, localSizeForRender),
    );
    setViewBoxForNodes(svg, nodeRects);

    appendLocalNode(svg, model.localPeer, localSizeForRender);
    model.placements.forEach((item) => {
      const activeTransfer = state.activeTransfers?.get(item.peerId);
      appendPeerNode(
        svg,
        item,
        state.peers,
        state.status,
        state.layoutDrag?.peerId,
        state.fileDropTargetPeerId,
        activeTransfer,
      );
    });

    const renderDurationMs = performance.now() - renderStartedAt;
    const meta = {
      renderDurationMs,
      placementCount: model.placements.length,
    };
    if (renderDurationMs >= PERF_WARN_MAIN_THREAD_MS) {
      perfLog("warn", `renderLayoutDiagram took ${renderDurationMs.toFixed(1)}ms`, {
        ...meta,
        statusTabActive: typeof isStatusTabActive === "function" ? isStatusTabActive() : true,
        dragging: Boolean(state.layoutDrag),
      });
    } else {
      perfLog("debug", `renderLayoutDiagram took ${renderDurationMs.toFixed(1)}ms`, meta);
    }
  };
}
