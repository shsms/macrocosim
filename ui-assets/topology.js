// vis-network graph canvas, shared by the Topology and Formulas
// subviews. createGraphCanvas builds one canvas instance around a
// container element; the adapter argument supplies the mutation
// hooks, so the Topology canvas can wire Ctrl-drag connect and the
// edit context menu while the Formulas canvas stays read-only.
//
// The default `topology` export is the Topology subview's instance,
// with the same public API the rest of the SPA always drove:
//
// - topology.apply(snapshot)     — replace canvas state with /api/topology data
// - topology.fit()               — recenter on the current graph extent
// - topology.get(id)             — lookup the component object by id
// - topology.parentsOf / childrenOf / connections / allIds / selectedIds
// - topology.mainMeterId()       — the meter flagged :main t (if any)
// - topology.setSelectionHandler — wire showComponent / clearSide to the canvas
// - topology.highlight(ids, subtractedIds) — temporary highlight (explanation hover)
// - topology.resetLayout(name) / setSnap / alignSelection / scaleSelection
// - topology.setValues(on) / valuesOn() — toggle live metric values on nodes/edges

import { setStatus } from "./app.js";
import { showContextMenu } from "./editor.js";
import { evalQuoted } from "./inspect.js";
import { deadBandW, edgeFlow } from "./live.js";
import { invalidateMeasureCache, measurePill, pillFontsReady, pillModel, pillRenderer } from "./pill.js";
import { readSelectedMg, visibleSubview } from "./routing.js";

function getCss(name) {
  return getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
}

const CATEGORY_COLOR = {
  grid: getCss("--cat-grid"),
  meter: getCss("--cat-meter"),
  inverter: getCss("--cat-inverter"),
  battery: getCss("--cat-battery"),
  "ev-charger": getCss("--cat-ev-charger"),
  chp: getCss("--cat-chp"),
  "wind-turbine": getCss("--cat-wind-turbine"),
  "steam-boiler": getCss("--cat-steam-boiler"),
  "power-transformer": getCss("--cat-power-transformer"),
  breaker: getCss("--cat-breaker"),
};

// Inverters get a subtype-aware shade so battery-inverters and
// solar-inverters read as related-but-distinct on the canvas.
const INVERTER_SUBTYPE_COLOR = {
  battery: getCss("--cat-inverter-battery"),
  solar: getCss("--cat-inverter-solar"),
  hybrid: getCss("--cat-inverter-hybrid"),
};

function colorFor(c) {
  if (c.category === "inverter") {
    return INVERTER_SUBTYPE_COLOR[c.subtype] || CATEGORY_COLOR.inverter;
  }
  return CATEGORY_COLOR[c.category] || "#888";
}

const LIVE_KEY = "switchyard-topology-live";
const EDGE_LIVE_COLOR = "#79b8ff";

// The edge's look with no live flow on it — what buildVisData's
// `arrows: "to"` plus the visOptions defaults render. A function so
// every DataSet entry gets its own object.
function edgeRestStyle() {
  return {
    width: 1.5,
    arrows: { to: { enabled: true, scaleFactor: 0.6 }, middle: { enabled: false } },
    color: { color: "#6b7280", inherit: false },
  };
}

// The alignment actions for a multi-node selection, one entry each.
// The context menu's align section is built from this table. An
// entry carries either `mode` (an alignSelection mode) or `scale`
// (a scaleSelection axis + factor); `title` becomes the menu row's
// hover tooltip.
export const ALIGN_MODES = [
  {
    mode: "grid",
    group: "arrange as",
    menu: "Align as a grid",
    title:
      "Rank by flow depth: chains line up, a fan-out spreads its branches on their own rows",
  },
  {
    mode: "lanes",
    group: "arrange as",
    menu: "Align as lanes",
    title: "Compact subtree blocks, like the lanes layout, for the selection only",
  },
  {
    mode: "radial",
    group: "arrange as",
    menu: "Align as radial",
    title: "Root in the middle, one ring per depth, for the selection only",
  },
  {
    mode: "row",
    group: "nudge",
    menu: "Align in a row",
    title: "Same y for all selected nodes",
  },
  {
    mode: "column",
    group: "nudge",
    menu: "Align in a column",
    title: "Same x for all selected nodes",
  },
  {
    mode: "spread-h",
    group: "nudge",
    menu: "Spread horizontally",
    title: "Even horizontal spacing",
  },
  {
    mode: "spread-v",
    group: "nudge",
    menu: "Spread vertically",
    title: "Even vertical spacing",
  },
  {
    mode: "reverse-h",
    group: "nudge",
    menu: "Reverse left–right",
    title: "Mirror the selection left to right",
  },
  {
    mode: "reverse-v",
    group: "nudge",
    menu: "Reverse top–bottom",
    title: "Mirror the selection top to bottom",
  },
  {
    scale: { axis: "x", factor: 1.25 },
    group: "spacing",
    menu: "Wider",
    title: "Scale the horizontal gaps up",
  },
  {
    scale: { axis: "x", factor: 0.8 },
    group: "spacing",
    menu: "Narrower",
    title: "Scale the horizontal gaps down",
  },
  {
    scale: { axis: "y", factor: 1.25 },
    group: "spacing",
    menu: "Taller",
    title: "Scale the vertical gaps up",
  },
  {
    scale: { axis: "y", factor: 0.8 },
    group: "spacing",
    menu: "Shorter",
    title: "Scale the vertical gaps down",
  },
];

// The flow depth of every node in `ids`: the number of steps on the
// longest path from a start (a node with no parent in `parents`). An
// edge back into an unfinished node (a cycle in an invalid graph,
// which the canvas still renders) reads 0 instead of recursing
// forever.
function flowDepths(ids, parents) {
  const depth = new Map();
  const depthOf = (id) => {
    if (!depth.has(id)) {
      depth.set(id, 0);
      depth.set(id, Math.max(-1, ...parents.get(id).map(depthOf)) + 1);
    }
    return depth.get(id);
  };
  for (const id of ids) depthOf(id);
  return depth;
}

// One graph canvas around the container element `containerId`. The
// adapter supplies what varies between the subviews:
//
// - onConnect(from, to)  — enables Ctrl-drag edge creation; absent,
//                          the canvas never enters addEdge mode.
// - onContextMenu(x, y)  — right-click menu; absent, right-click is
//                          inert (beyond the selection reset).
// - onApply(data)        — called on every apply() with the raw
//                          snapshot (status pill, empty-hint flags).
export function createGraphCanvas(containerId, adapter = {}) {
  let network = null;
  let nodesDS = null;
  let edgesDS = null;
  const componentById = new Map();
  // Id of the meter currently flagged `:main t` (per the snapshot's
  // top-level `main_meter_id`). Captured here so copy/paste can
  // mark the pasted copy as `:main t` when the source meter was.
  let mainMeterId = null;
  let onSelect = null;
  let onDeselect = null;
  let selectionAtMousedown = [];
  // Selection saved while an explanation hover borrows the highlight.
  let highlightStash = null;
  // Nodes whose highlight colour was flipped to red (subtracted terms),
  // to restore on unhighlight.
  let redHighlighted = [];
  // True once the user drags a node: refreshes then keep the manual
  // arrangement instead of re-running the layout.
  let manualArrangement = false;
  // Set when a layout ran against node sizes vis-network hadn't
  // measured yet (before the first draw, or right after new nodes
  // landed). The afterDrawing handler then re-runs the layout once
  // with real bounding boxes — without this, the first paint stacks
  // siblings on top of each other.
  let pendingMeasuredRelayout = false;
  let initialFitDone = false;
  // A jump-to-node target waiting to be centered. The fit paths
  // (ResizeObserver, fit(), the initial afterDrawing fit) consume it
  // instead of fitting, so the reveal wins over the subview-switch
  // refits by construction rather than by frame ordering. Expires
  // after a moment so a much-later resize doesn't re-center a stale
  // target.
  let pendingReveal = null; // { id, rightInset, until }
  // ── live overlay state ─────────────────────────────────────────
  // Per-component last-known live values, fed by applySample from
  // the WS sample stream; ids marked dirty are flushed to the
  // canvas in ONE nodesDS/edgesDS update per second.
  const liveValues = new Map(); // id -> { p, q, soc }
  const liveDirty = new Set();
  let liveEnabled = localStorage.getItem(LIVE_KEY) !== "0";
  let liveFlushTimer = null;
  // Largest |active power bound| seen — the magnitude reference for
  // edge flow scaling. Reset on topology refresh.
  let maxAbsBoundW = 0;
  // The microgrid the live values belong to. Component ids collide
  // across microgrids (and the list view forwards every microgrid's
  // samples), so a switch drops everything accumulated so far.
  let liveMg = null;
  // Smoke-test hook: how many times apply() ran.
  let applyCount = 0;
  // Pill sizes vis has not applied yet. vis-network takes a custom
  // shape's `nodeDimensions` into account only on the draw *after*
  // the one that reported them, and only when the node is flagged
  // for refresh — so the renderer reports every size, and
  // afterDrawing re-stamps the nodes whose size moved (any update
  // flags the refresh), and schedules a measured relayout when the
  // axis that moved is one the current layout spends.
  const knownSize = new Map(); // id -> { w, h } last reported
  const sizeDirty = new Set();
  // The pill model each node's renderer draws, by component id.
  const pillModels = new Map();
  // Scratch 2-D context for measuring pills outside a draw. vis's own
  // getBoundingBox() is useless for custom shapes (ShapeBase falls
  // back to the generic `size` option), so the layout measures the
  // models itself — the same cached measurement the renderer draws
  // with.
  let measureCtx = null;
  function noteSize(id, w, h) {
    const prev = knownSize.get(id);
    if (prev && prev.w === w && prev.h === h) return;
    knownSize.set(id, { w, h });
    sizeDirty.add(id);
  }
  const valuesDefault = adapter.valuesOn !== false;

  function syncLiveMg() {
    const mg = readSelectedMg();
    if (mg === liveMg) return;
    liveValues.clear();
    liveDirty.clear();
    maxAbsBoundW = 0;
    liveMg = mg;
  }

  // The vis node for a component: a custom-drawn pill carrying the
  // component's last live sample (when values are on and a sample
  // exists). No `label`: the pill draws its own text, and a label vis
  // thinks it has to place widens the node's bounding box (see the
  // `size` re-stamp in afterDrawing). `title` (the vis tooltip) only
  // on canvases without a hover card.
  function nodeFor(c) {
    const model = pillModel(c, liveEnabled ? liveValues.get(c.id) : null, {
      valuesOn: liveEnabled && valuesDefault,
      dotColor: colorFor(c),
      deadBand: deadBandW(maxAbsBoundW),
    });
    if (redHighlighted.includes(c.id)) model.highlight = "subtracted";
    // vis-network binds a custom shape's ctxRenderer when the node is
    // FIRST created and ignores every later one (Node.updateShape
    // re-creates the shape only when the shape *name* changes), so a
    // new sample has to reach the canvas through the very model
    // object that renderer closed over: assign into it in place.
    const kept = pillModels.get(c.id);
    if (kept) Object.assign(kept, model);
    else pillModels.set(c.id, model);
    const drawn = kept ?? model;
    const node = {
      id: c.id,
      shape: "custom",
      ctxRenderer: pillRenderer(drawn, noteSize),
      pillModel: drawn,
    };
    if (adapter.tooltip !== false) node.title = `#${c.id} — ${c.name}`;
    return node;
  }

  function liveEntry(id) {
    let e = liveValues.get(id);
    if (!e) {
      e = { p: null, q: null, soc: null, dc: null, energy: null, pLo: null, pHi: null, qLo: null, qHi: null, ts: null, hist: [] };
      liveValues.set(id, e);
    }
    return e;
  }

  function armLiveFlush() {
    if (liveFlushTimer !== null) return;
    liveFlushTimer = setTimeout(() => {
      liveFlushTimer = null;
      flushLive();
    }, 1000);
  }

  // One batched canvas update for every dirty component. Parked
  // while the subview is hidden (subview enter calls flushLive()
  // directly to catch up).
  function flushLive() {
    if (!liveEnabled || !nodesDS || liveDirty.size === 0) return;
    if (visibleSubview() !== "topology") return;
    const nodeUpdates = [];
    for (const id of liveDirty) {
      const c = componentById.get(id);
      if (c) nodeUpdates.push(nodeFor(c));
    }
    // Edge flow: recompute every edge that touches a dirty child.
    // Parent counts come from the live edge set (parallel paths
    // split the child's flow, matching the meter aggregation rule).
    const edgeUpdates = [];
    if (edgesDS) {
      const parentCount = new Map();
      for (const e of edgesDS.get()) {
        parentCount.set(e.to, (parentCount.get(e.to) || 0) + 1);
      }
      for (const e of edgesDS.get()) {
        if (!liveDirty.has(e.to)) continue;
        const child = liveValues.get(e.to);
        const flow = edgeFlow(child ? child.p : null, parentCount.get(e.to) || 1, maxAbsBoundW);
        if (!flow.chevron) {
          edgeUpdates.push({ id: e.id, ...edgeRestStyle() });
          continue;
        }
        edgeUpdates.push({
          id: e.id,
          width: flow.width,
          arrows: {
            to: { enabled: true, scaleFactor: 0.6 },
            middle: {
              enabled: true,
              type: "arrow",
              // Negative flips the chevron toward the parent —
              // physical flow for export/generation.
              scaleFactor: (flow.towardParent ? -1 : 1) * flow.scale,
            },
          },
          color: { color: EDGE_LIVE_COLOR, inherit: false },
        });
      }
    }
    liveDirty.clear();
    if (nodeUpdates.length) nodesDS.update(nodeUpdates);
    if (edgeUpdates.length) edgesDS.update(edgeUpdates);
  }
  function applyPendingReveal() {
    if (!pendingReveal || !network) return false;
    if (Date.now() > pendingReveal.until) {
      pendingReveal = null;
      return false;
    }
    const pos = network.getPositions([pendingReveal.id])[pendingReveal.id];
    if (!pos) {
      pendingReveal = null;
      return false;
    }
    network.moveTo({
      position: pos,
      scale: network.getScale(),
      offset: { x: -pendingReveal.rightInset / 2, y: 0 },
      animation: false,
    });
    return true;
  }
  // Magnetic drag grid: dragged nodes move in PITCH steps from where
  // the drag started (the grid is relative, so nodes keep their
  // alignment with rows the auto-layouts made). Toggled from the
  // canvas header via setSnap.
  const PITCH = 20;
  let snapEnabled = true;
  // The active node drag: the pointer's start point, every dragged
  // node's start position, the grabbed node (the grid anchor for the
  // drawn grid lines), and the snapped target positions of the last
  // pointer move. null while no node drag is running.
  let dragState = null;

  const container = () => document.getElementById(containerId);

  // Computes where the dragged nodes belong: their drag-start
  // positions plus the pointer's movement — locked to one axis while
  // Alt (or Shift) is held, and rounded to PITCH steps when snapping
  // is on. The whole selection moves rigidly: one shared delta, so
  // relative offsets are kept. The targets are stored on dragState;
  // the beforeDrawing hook applies them (vis overwrites node
  // positions with the raw pointer delta right after this event, so
  // writing them here would be futile).
  function applyDrag(params) {
    if (!dragState || !params.nodes.length) return;
    let dx = params.pointer.canvas.x - dragState.pointer.x;
    let dy = params.pointer.canvas.y - dragState.pointer.y;
    // Axis lock: keep the dominant direction, zero the other. Alt is
    // the binding — at drag start Shift belongs to vis's selection
    // box — but Shift pressed after the drag has started works too.
    if (params.event?.srcEvent?.altKey || params.event?.srcEvent?.shiftKey) {
      if (Math.abs(dx) >= Math.abs(dy)) dy = 0;
      else dx = 0;
    }
    if (snapEnabled) {
      dx = Math.round(dx / PITCH) * PITCH;
      dy = Math.round(dy / PITCH) * PITCH;
    }
    dragState.targets = {};
    for (const [id, p] of Object.entries(dragState.start)) {
      dragState.targets[id] = { x: p.x + dx, y: p.y + dy };
    }
  }

  // Re-places the dragged nodes just before each frame is drawn.
  // This runs after vis set the raw drag positions, so the frame
  // renders the snapped ones. Writing `.x`/`.y` on the body nodes is
  // how vis's own drag code moves nodes; unlike `moveNode` it does
  // not schedule another redraw, so this cannot loop.
  function applyDragTargets() {
    if (!dragState?.targets) return;
    for (const [id, p] of Object.entries(dragState.targets)) {
      const node = network.body.nodes[id];
      if (node) {
        node.x = p.x;
        node.y = p.y;
      }
    }
  }

  // Faint grid lines while a node drag is snapping, anchored at the
  // grabbed node's start position (the grid is relative to the drag).
  function drawSnapGrid(ctx) {
    if (!dragState || !snapEnabled) return;
    const anchor = dragState.start[dragState.anchorId];
    const el = container();
    const tl = network.DOMtoCanvas({ x: 0, y: 0 });
    const br = network.DOMtoCanvas({ x: el.clientWidth, y: el.clientHeight });
    // Zoomed far out the lines would only add noise; skip them.
    if ((br.x - tl.x) / PITCH > 400 || (br.y - tl.y) / PITCH > 400) return;
    ctx.save();
    ctx.strokeStyle = "rgba(110, 118, 129, 0.25)";
    ctx.lineWidth = 1 / network.getScale();
    ctx.beginPath();
    const first = (from, origin) => origin + Math.ceil((from - origin) / PITCH) * PITCH;
    for (let x = first(tl.x, anchor.x); x <= br.x; x += PITCH) {
      ctx.moveTo(x, tl.y);
      ctx.lineTo(x, br.y);
    }
    for (let y = first(tl.y, anchor.y); y <= br.y; y += PITCH) {
      ctx.moveTo(tl.x, y);
      ctx.lineTo(br.x, y);
    }
    ctx.stroke();
    ctx.restore();
  }

  // Every selection change funnels through notifySelection, whatever
  // gesture caused it (click, shift-click, rubber band, programmatic).
  // `focusId` is the node the gesture was aimed at (clicked,
  // double-clicked, right-clicked): the inspector follows it, not
  // whichever node happens to sit first in the selection set.
  let lastNotified = "";
  function notifySelection(focusId) {
    if (!network) return;
    // While an explanation hover borrows the selection, it must not
    // reach the panels as if the user selected those nodes.
    if (highlightStash !== null) return;
    const sel = network.getSelectedNodes();
    const key = sel.join(",");
    if (key === lastNotified) return;
    lastNotified = key;
    if (sel.length && onSelect) {
      const focus = focusId != null && sel.includes(focusId) ? focusId : sel[0];
      onSelect(componentById.get(focus));
    } else if (!sel.length && onDeselect) onDeselect();
  }

  function restoreRedHighlights() {
    if (!redHighlighted.length) return;
    const ids = redHighlighted.filter((id) => componentById.has(id));
    redHighlighted = [];
    nodesDS.update(ids.map((id) => nodeFor(componentById.get(id))));
  }

  function buildVisData(data) {
    componentById.clear();
    const nodes = data.components.map((c) => {
      componentById.set(c.id, c);
      return nodeFor(c);
    });
    // Forget the models and sizes of components that left.
    for (const id of [...pillModels.keys()]) {
      if (componentById.has(id)) continue;
      pillModels.delete(id);
      knownSize.delete(id);
    }
    const visibleEdges = data.connections.map(([p, c]) => ({
      id: `${p}-${c}`,
      from: p,
      to: c,
      arrows: "to",
    }));
    // Hidden edges (parent → hidden child) render dashed so the
    // user can see the link without confusing them with the public
    // gRPC topology — same visual cue the hidden node itself uses.
    const hiddenEdges = (data.hidden_connections || []).map(([p, c]) => ({
      id: `${p}-${c}`,
      from: p,
      to: c,
      arrows: "to",
      dashes: true,
    }));
    return { nodes, edges: [...visibleEdges, ...hiddenEdges] };
  }

  const visOptions = {
    // No vis-network layout module: layoutHierarchy() places every
    // node itself. (The hierarchical module would also pin nodes to
    // their level while dragging; without it nodes drag freely on
    // both axes.)
    layout: { improvedLayout: false },
    physics: { enabled: false },
    interaction: {
      hover: true,
      dragNodes: true,
      // Vis-network handles Shift+drag rubber-band on empty canvas
      // when this is on.
      multiselect: true,
      selectConnectedEdges: false,
      navigationButtons: false,
      keyboard: { enabled: false },
    },
    edges: {
      color: { color: "#6b7280", highlight: "#79b8ff", hover: "#b0b8c1" },
      width: 1.5,
      smooth: { enabled: true, type: "cubicBezier", forceDirection: "horizontal", roundness: 0.4 },
      arrows: { to: { enabled: true, scaleFactor: 0.6 } },
    },
    // The manipulation API powers Ctrl+drag connect; the toolbar
    // stays hidden because we drive edit modes via key state.
    manipulation: {
      enabled: false,
      addEdge: (data, callback) => {
        if (data.from !== data.to && adapter.onConnect) {
          adapter.onConnect(data.from, data.to);
        }
        // Don't apply locally — the topology refresh will redraw
        // with the new edge once the mutation lands on the server.
        callback(null);
      },
    },
  };

  function apply(data) {
    applyCount++;
    mainMeterId = typeof data.main_meter_id === "number" ? data.main_meter_id : null;
    if (adapter.onApply) adapter.onApply(data);
    const prevIds = new Set(componentById.keys());
    syncLiveMg();
    const { nodes, edges } = buildVisData(data);
    // Live overlay: forget components that left the topology, mark
    // every survivor dirty so the next flush rebuilds its label
    // (names/categories may have changed), and reset the flow
    // scale reference (rated bounds may have changed too).
    for (const id of [...liveValues.keys()]) {
      if (!componentById.has(id)) liveValues.delete(id);
      else liveDirty.add(id);
    }
    maxAbsBoundW = 0;
    if (liveDirty.size) armLiveFlush();
    if (!network) {
      nodesDS = new vis.DataSet(nodes);
      edgesDS = new vis.DataSet(edges);
      const el = container();
      network = new vis.Network(el, { nodes: nodesDS, edges: edgesDS }, visOptions);
      // Re-frame whenever the container resizes — switching subviews
      // (display:none → display:block) and dragging the drawer
      // splitter all fall through here. Without this, vis-
      // network's camera sticks to whatever extent was captured on
      // first paint and a graph that was wider than the canvas at
      // construction shows only half of itself afterwards.
      if (typeof ResizeObserver !== "undefined") {
        const ro = new ResizeObserver(() => {
          if (network && el.offsetWidth > 0 && el.offsetHeight > 0 && !applyPendingReveal()) {
            network.fit({ animation: false });
          }
        });
        ro.observe(el);
      }
      // vis-network's first auto-fit happens on stabilization, but
      // we ship with `physics.enabled = false` so stabilization
      // doesn't actually fire. After each draw: re-run a layout that
      // was computed against unmeasured node boxes (the relayout
      // clears the flag first, so the draw it triggers doesn't
      // loop), and fit the camera once on the initial reveal —
      // later relayouts keep the user's pan/zoom, matching how
      // topology refreshes behave.
      network.on("afterDrawing", () => {
        if (sizeDirty.size) {
          // A refresh may have removed a node since it last drew; an
          // update() for an unknown id would add a blank one back.
          const ids = [...sizeDirty].filter((id) => nodesDS.get(id) && knownSize.has(id));
          sizeDirty.clear();
          if (ids.length) {
            // The stamped value does little: ShapeBase.resize takes
            // `2 * size` only as the fallback for a custom shape that
            // has not reported nodeDimensions yet, so half the pill's
            // larger side is what the very first draw and any
            // bounding box read before it get to work with.
            // The *update* is the point. resize() re-reads
            // customSizeWidth/Height only when the node is flagged
            // for refresh, so without an update() vis would keep
            // measuring, fitting and hit-testing this node against
            // the box it had one size ago.
            nodesDS.update(
              ids.map((id) => {
                const { w, h } = knownSize.get(id);
                return { id, size: Math.max(w, h) / 2 };
              }),
            );
            if (!manualArrangement) pendingMeasuredRelayout = true;
            return;
          }
        }
        if (pendingMeasuredRelayout) {
          pendingMeasuredRelayout = false;
          if (!manualArrangement) layoutHierarchy();
        }
        if (!initialFitDone) {
          initialFitDone = true;
          if (!applyPendingReveal()) network.fit({ animation: false });
        }
      });
      // Fonts may land after the first paint: re-measure everything
      // and let the size-dirty path relayout.
      pillFontsReady.then(() => {
        invalidateMeasureCache();
        knownSize.clear();
        nodesDS.update(nodesDS.getIds().map((id) => ({ id })));
      });
      network.on("click", (params) => {
        const shiftKey = params.event?.srcEvent?.shiftKey;
        if (params.nodes.length) {
          const id = params.nodes[0];
          if (shiftKey) {
            // Shift-click toggles this node in / out of the selection
            // that existed when the mousedown landed. Reading
            // getSelectedNodes() here would see vis-network's
            // single-click auto-select that already ran for this
            // event, so we use the mousedown snapshot instead.
            const sel = new Set(selectionAtMousedown);
            if (sel.has(id)) sel.delete(id);
            else sel.add(id);
            network.selectNodes([...sel]);
          }
        } else if (shiftKey) {
          // A shift-click on empty canvas ends a rubber-band drag;
          // keep whatever it selected.
        } else {
          network.unselectAll();
        }
        notifySelection(params.nodes.length ? params.nodes[0] : undefined);
      });
      // Double-click selects the node together with everything it
      // feeds (its whole subtree), e.g. a meter with its inverters
      // and batteries. With Shift held, the subtree is added to the
      // selection instead of replacing it.
      network.on("doubleClick", (params) => {
        // The event's `nodes` hold the selection, not the node under
        // the cursor — and a shift-double-click's own shift-clicks
        // leave the selection elsewhere. Resolve the node by position.
        const root = network.getNodeAt(params.pointer.DOM);
        if (root == null) return;
        const succs = new Map();
        for (const e of edgesDS.get()) {
          if (!succs.has(e.from)) succs.set(e.from, []);
          succs.get(e.from).push(e.to);
        }
        const selected = new Set(
          params.event?.srcEvent?.shiftKey ? network.getSelectedNodes() : [],
        );
        // The walk keeps its own visited set: a subtree node that is
        // already in the kept selection must not cut the walk short.
        const visited = new Set();
        const queue = [root];
        while (queue.length) {
          const id = queue.pop();
          if (visited.has(id)) continue;
          visited.add(id);
          selected.add(id);
          queue.push(...(succs.get(id) || []));
        }
        network.selectNodes([...selected].filter((id) => componentById.has(id)));
        notifySelection(root);
      });
      // The rubber-band selection (shift-drag on empty canvas) changes
      // the selection without firing `click` with nodes; catch every
      // interaction end.
      network.on("release", () => setTimeout(notifySelection, 0));
      // Right-click → context menu. Right-clicking a node *not* in
      // the current selection resets the selection to that one node
      // first, matching the standard editor convention.
      network.on("oncontext", (params) => {
        params.event.preventDefault();
        const nodeAt = network.getNodeAt(params.pointer.DOM);
        if (nodeAt != null) {
          const sel = network.getSelectedNodes();
          if (!sel.includes(nodeAt)) {
            network.selectNodes([nodeAt]);
            notifySelection(nodeAt);
          }
        }
        if (adapter.onContextMenu) {
          adapter.onContextMenu(params.event.clientX, params.event.clientY);
        }
      });
      if (adapter.onConnect) {
        // Ctrl/Cmd toggles vis-network's addEdge mode. Hold Ctrl
        // (Cmd on Mac), drag from one node to another to wire them.
        // Gated on this canvas actually being visible (offsetParent
        // is null while its subview is hidden) so Ctrl-chords typed
        // in the REPL or on other views don't arm edit mode.
        document.addEventListener("keydown", (e) => {
          if (
            (e.key === "Control" || e.key === "Meta") &&
            network &&
            el.offsetParent !== null
          ) {
            network.addEdgeMode();
          }
        });
        document.addEventListener("keyup", (e) => {
          if ((e.key === "Control" || e.key === "Meta") && network) {
            network.disableEditMode();
          }
        });
        // Cmd+Tab / window switches swallow the keyup, leaving
        // addEdge mode permanently armed — the next node drag would
        // silently eval a persisted (connect a b). Disarm on blur.
        window.addEventListener("blur", () => {
          if (network) network.disableEditMode();
        });
      }
      // Capture the selection state at mousedown — vis-network's
      // single-click selection runs before our `click` handler, so
      // by the time we read getSelectedNodes() it's already been
      // overwritten. Snap it here and the shift-click toggle in the
      // click handler can compute against the pre-click set.
      el.addEventListener(
        "mousedown",
        () => {
          selectionAtMousedown = network ? network.getSelectedNodes() : [];
        },
        true,
      );
      // Node drags: applyDrag computes the snapped (and
      // axis-locked) targets on each move, and the beforeDrawing
      // hook places the nodes there so every frame renders the
      // snapped positions.
      network.on("dragStart", (params) => {
        if (!params.nodes.length) return;
        dragState = {
          pointer: { ...params.pointer.canvas },
          start: network.getPositions(params.nodes),
          anchorId: params.nodes[0],
        };
      });
      network.on("dragging", applyDrag);
      network.on("beforeDrawing", (ctx) => {
        applyDragTargets();
        drawSnapGrid(ctx);
      });
      // Once the user arranges nodes by hand, refreshes stop
      // re-running the layout (Re-layout brings it back).
      network.on("dragEnd", (params) => {
        // Any node drag counts as a manual arrangement, even if the
        // drag state went missing (then the raw positions stand).
        if (params.nodes.length) {
          manualArrangement = true;
          applyDrag(params);
          // Commit through the public API: it also schedules the
          // redraw that clears the drawn grid lines.
          for (const [id, p] of Object.entries(dragState?.targets ?? {})) {
            network.moveNode(id, p.x, p.y);
          }
        }
        dragState = null;
      });
    } else {
      // Diff the DataSets — preserves selection, layout positions,
      // and any in-flight drag interactions, instead of tearing
      // down the canvas on every WS topology event.
      const prevSelected = network.getSelectedNodes();
      const newIds = new Set(nodes.map((n) => n.id));
      const stale = nodesDS.getIds().filter((id) => !newIds.has(id));
      if (stale.length) nodesDS.remove(stale);
      nodesDS.update(nodes);

      const newEdgeIds = new Set(edges.map((e) => e.id));
      const staleEdges = edgesDS.getIds().filter((id) => !newEdgeIds.has(id));
      if (staleEdges.length) edgesDS.remove(staleEdges);
      // Existing edges whose child has live values keep their
      // arrows/width/color (DataSet .update merges per field); ones
      // whose child has none — a microgrid switch just cleared the
      // map, or the child lost telemetry — drop back to the rest
      // style so a colliding edge id can't carry a stale chevron
      // over. New edges get the structural `arrows: "to"`.
      const edgeUpdates = edges.map((e) => {
        if (!edgesDS.get(e.id)) return e;
        const base = { id: e.id, from: e.from, to: e.to, ...(e.dashes ? { dashes: true } : {}) };
        return liveValues.has(e.to) ? base : { ...base, ...edgeRestStyle() };
      });
      edgesDS.update(edgeUpdates);

      if (prevSelected.length) {
        // Re-select what survived. The notify must fire even when
        // the selected ids are unchanged: the component data behind
        // them is fresh (a rename, a health flip), and the inspector
        // re-renders from this callback — so drop the dedup key
        // first.
        network.selectNodes(prevSelected.filter((id) => componentById.has(id)));
        lastNotified = "";
        notifySelection();
      }
    }
    if (manualArrangement) {
      // Keep the user's arrangement; just give the components added
      // since the last refresh a sensible spot.
      placeNewNodes([...componentById.keys()].filter((id) => !prevIds.has(id)));
    } else {
      layoutHierarchy();
      // Only nodes this apply introduced lack measured boxes — a
      // refresh that added none (the common WS topology_changed)
      // needs no second layout pass.
      pendingMeasuredRelayout = [...componentById.keys()].some((id) => !prevIds.has(id));
    }
  }

  // Places newly added nodes without disturbing a manual arrangement:
  // next to their first parent when connected, in a row under the
  // graph otherwise.
  function placeNewNodes(ids) {
    if (!network || !ids.length) return;
    const added = new Set(ids);
    const positions = network.getPositions();
    const parentOf = new Map();
    for (const e of edgesDS.get()) {
      if (added.has(e.to) && !parentOf.has(e.to) && !added.has(e.from)) {
        parentOf.set(e.to, e.from);
      }
    }
    // The bottom of the user's arrangement — the new nodes' own spawn
    // positions must not count, or the free row drifts down on every
    // refresh.
    let maxY = 0;
    for (const [id, p] of Object.entries(positions)) {
      if (!added.has(Number(id))) maxY = Math.max(maxY, p.y);
    }
    let free = 0;
    const perParent = new Map();
    for (const id of ids) {
      const parent = parentOf.get(id);
      if (parent != null && positions[parent]) {
        // Several new children of one parent step downward instead of
        // stacking on one point.
        const nth = perParent.get(parent) || 0;
        perParent.set(parent, nth + 1);
        network.moveNode(
          id,
          positions[parent].x + 260,
          positions[parent].y + 60 * (nth + 1),
        );
      } else {
        network.moveNode(id, free * 220, maxY + 100);
        free += 1;
      }
    }
  }

  // ---------------------------------------------------------- layouts
  //
  // Named layout algorithms. Each computes a full position map from
  // the layout tree: every node hangs under the predecessor on its
  // longest path, so its column matches its flow depth; extra
  // (diamond) edges still render, they just don't drive placement.
  //
  // - lanes (default): x from the depth in the node's own subtree;
  //   children stack as compact blocks; a node with many child
  //   subtrees spreads them over two lanes; a large all-leaf group
  //   packs into two staggered mini columns.
  // - columns: classic strict columns — x from the global depth, one
  //   stack of subtree blocks, no lanes, no leaf packing.
  // - topdown: like columns, top to bottom (each depth is a row).
  // - radial: the root in the middle, one ring per depth, each
  //   subtree in a sector sized by its leaf count.

  let currentLayout = "lanes";

  const SEP = 260; // column separation, must exceed the widest node
  const GAP = 22; // gap between node boxes along the stacking axis
  const LEAF_OFFSET = 130; // stagger of the second leaf mini column
  const LANE_THRESHOLD = 8; // children before a node spreads lanes

  // The layout tree over `ids` (the whole graph by default; a
  // selection for the "lanes" / "radial" alignments). Edges to nodes
  // outside `ids` are ignored. A node with several parents (a
  // diamond) hangs under the parent on its longest path, so its tree
  // depth equals its flow depth — the same rank the "grid" alignment
  // uses.
  function buildTree(ids = [...componentById.keys()]) {
    const preds = new Map(ids.map((id) => [id, []]));
    for (const e of edgesDS.get()) {
      if (!preds.has(e.from) || !preds.has(e.to)) continue;
      preds.get(e.to).push(e.from);
    }
    const depth = flowDepths(ids, preds);
    const children = new Map(ids.map((id) => [id, []]));
    for (const id of ids) {
      const parents = preds.get(id);
      if (!parents.length) continue;
      const deepest = parents.reduce((a, b) =>
        depth.get(b) > depth.get(a) || (depth.get(b) === depth.get(a) && b < a) ? b : a,
      );
      children.get(deepest).push(id);
    }
    for (const kids of children.values()) kids.sort((a, b) => a - b);
    const roots = ids
      .filter((id) => preds.get(id).length === 0)
      .sort((a, b) => a - b);
    return { ids, children, roots };
  }

  function nodeSizes(ids) {
    if (!measureCtx) measureCtx = document.createElement("canvas").getContext("2d");
    const width = new Map();
    const height = new Map();
    for (const id of ids) {
      const model = pillModels.get(id);
      if (!model) {
        // Every laid-out node is built by nodeFor, so a missing model
        // means the two went out of sync — lay out on a guess, but say so.
        console.error(`topology: no pill model for node ${id}; laying out on a guess`);
      }
      const d = model ? measurePill(measureCtx, model) : null;
      width.set(id, d ? d.width : 120);
      height.set(id, d ? d.height : 34);
    }
    return { width, height };
  }

  // Memoized subtree depth over `children`: a leaf counts 1.
  function subtreeDepthFn(children) {
    const depthOf = new Map();
    function subtreeDepth(id) {
      if (depthOf.has(id)) return depthOf.get(id);
      const kids = children.get(id);
      const depth = kids.length ? 1 + Math.max(...kids.map(subtreeDepth)) : 1;
      depthOf.set(id, depth);
      return depth;
    }
    return subtreeDepth;
  }

  // The tidy block layout behind lanes / columns / topdown. Stacks
  // each parent's children as a compact block and centers the parent
  // on it, so chains render as straight lines.
  function tidyLayout({ laneWrap, leafPack, vertical }, tree, pos) {
    const { ids, children, roots } = tree;
    const { width, height } = nodeSizes(ids);
    // Stacking-axis extent of a node: height in the left-to-right
    // layouts, width in the top-down one.
    const extent = (id) => (vertical ? width.get(id) : height.get(id)) + GAP;
    const depthSep = vertical ? 150 : SEP;
    const leafOffset = vertical ? 65 : LEAF_OFFSET;

    const subtreeDepth = subtreeDepthFn(children);

    function put(id, depth, main) {
      const d = depth * depthSep;
      pos.set(id, vertical ? { x: main, y: d } : { x: d, y: main });
    }

    // Lays out `id` at `depth`, its block starting at `cursor` on the
    // stacking axis. Returns the block's end and the node's center.
    function layoutSubtree(id, depth, cursor) {
      const kids = children.get(id);
      const own = extent(id);
      if (!kids.length) {
        const main = cursor + own / 2;
        put(id, depth, main);
        return { end: cursor + own, main };
      }

      let end = cursor;
      const centers = [];
      if (leafPack && kids.length >= 4 && kids.every((k) => !children.get(k).length)) {
        // A large all-leaf group: two staggered mini columns, each
        // with its own cursor so varying node sizes cannot overlap
        // within a column.
        const columnCursor = [end, end + extent(kids[0]) / 2];
        for (let i = 0; i < kids.length; i++) {
          const column = i % 2;
          const h = extent(kids[i]);
          const main = columnCursor[column] + h / 2;
          columnCursor[column] += h;
          const d = (depth + 1) * depthSep + column * leafOffset;
          pos.set(kids[i], vertical ? { x: main, y: d } : { x: d, y: main });
          centers.push(main);
        }
        end = Math.max(...columnCursor);
      } else if (laneWrap && kids.length >= LANE_THRESHOLD) {
        // Spread the child subtrees over two lanes. Greedy balance:
        // each subtree goes to the shorter lane.
        const laneDepth = Math.max(...kids.map(subtreeDepth)) + 1;
        const laneCursor = [cursor, cursor];
        for (const kid of kids) {
          const lane = laneCursor[0] <= laneCursor[1] ? 0 : 1;
          const laid = layoutSubtree(kid, depth + 1 + lane * laneDepth, laneCursor[lane]);
          laneCursor[lane] = laid.end;
          centers.push(laid.main);
        }
        end = Math.max(...laneCursor);
      } else {
        for (const kid of kids) {
          const laid = layoutSubtree(kid, depth + 1, end);
          end = laid.end;
          centers.push(laid.main);
        }
      }

      // Center the parent on its children; a parent bigger than its
      // block still claims its own extent.
      const main = (Math.min(...centers) + Math.max(...centers)) / 2;
      put(id, depth, main);
      return { end: Math.max(end, cursor + own), main };
    }

    let cursor = 0;
    for (const root of roots) {
      cursor = layoutSubtree(root, 0, cursor).end + GAP;
    }
    return cursor;
  }

  // Root in the middle, one ring per depth. Each subtree gets a
  // sector sized by its leaf count, so dense branches get more arc.
  function radialLayout(tree, pos) {
    const { children, roots } = tree;
    const leavesOf = new Map();
    function countLeaves(id) {
      if (leavesOf.has(id)) return leavesOf.get(id);
      const kids = children.get(id);
      const n = kids.length ? kids.reduce((s, k) => s + countLeaves(k), 0) : 1;
      leavesOf.set(id, n);
      return n;
    }
    const subtreeDepth = subtreeDepthFn(children);
    const totalLeaves = roots.reduce((s, r) => s + countLeaves(r), 0);
    const maxDepth = Math.max(1, ...roots.map(subtreeDepth)) - 1 || 1;
    // The outermost ring must have room for every leaf.
    const ringStep = Math.max(
      320,
      (totalLeaves * 200) / (2 * Math.PI * maxDepth),
    );
    // With several roots, nothing sits in the middle; every root moves
    // out one ring so they don't pile up at the center.
    const depthShift = roots.length > 1 ? 1 : 0;

    function place(id, depth, a0, a1) {
      const angle = (a0 + a1) / 2;
      const r = (depth + depthShift) * ringStep;
      pos.set(id, { x: r * Math.cos(angle), y: r * Math.sin(angle) });
      let a = a0;
      for (const kid of children.get(id)) {
        const span = ((a1 - a0) * countLeaves(kid)) / countLeaves(id);
        place(kid, depth + 1, a, a + span);
        a += span;
      }
    }

    let a = 0;
    for (const root of roots) {
      const span = (2 * Math.PI * countLeaves(root)) / totalLeaves;
      place(root, 0, a, a + span);
      a += span;
    }
  }

  const LAYOUTS = {
    lanes: (tree, pos) => tidyLayout({ laneWrap: true, leafPack: true }, tree, pos),
    columns: (tree, pos) => tidyLayout({}, tree, pos),
    topdown: (tree, pos) => tidyLayout({ vertical: true, leafPack: true }, tree, pos),
    radial: radialLayout,
  };

  function layoutHierarchy() {
    if (!network || !edgesDS || !nodesDS) return;
    const all = [...componentById.keys()];
    if (all.length <= 1) return;
    // Hidden components sit out of the layout: they are not part of
    // the gRPC topology, and a hidden meter pulled into a lane would
    // consume a slot and shift its visible siblings. They go in the
    // stash row below instead.
    const visible = all.filter((id) => !componentById.get(id)?.hidden);
    const tree = buildTree(visible);
    const pos = new Map();
    (LAYOUTS[currentLayout] || LAYOUTS.lanes)(tree, pos);
    // Stash row below everything: hidden components, and nodes the
    // tree walk can't reach (inside a cycle — an invalid graph, but
    // it must still render).
    let maxY = 0;
    for (const p of pos.values()) maxY = Math.max(maxY, p.y);
    let free = 0;
    for (const id of all) {
      if (!pos.has(id)) {
        pos.set(id, { x: free * 220, y: maxY + 100 });
        free += 1;
      }
    }
    for (const [id, p] of pos) {
      network.moveNode(id, p.x, p.y);
    }
  }

  return {
    apply,
    get: (id) => componentById.get(id),
    mainMeterId: () => mainMeterId,
    parentsOf: (id) => (network ? network.getConnectedNodes(id, "from") : []),
    childrenOf: (id) => (network ? network.getConnectedNodes(id, "to") : []),
    selectedIds: () => (network ? network.getSelectedNodes() : []),
    allIds: () => Array.from(componentById.keys()),
    select(ids) {
      if (!network) return;
      // vis-network's selectNodes doesn't fire events, so notify the
      // selection handlers ourselves.
      network.selectNodes(ids.filter((id) => componentById.has(id)));
      notifySelection();
    },
    /// Re-frame the canvas so every visible node fits. vis-network's
    /// auto-fit only fires on stabilization (we have physics off so
    /// it never runs again after the first paint), and the first
    /// paint may have happened while the subview was `display:none`
    /// and the canvas measured 0 × 0. Call this on subview enter and
    /// after container resizes.
    fit() {
      if (!network) return;
      if (applyPendingReveal()) return;
      network.fit({ animation: false });
    },
    /// Center `id` in the visible part of the canvas. `rightInset`
    /// is the width of a panel overlaying the canvas's right edge
    /// (the inspector), so the node lands in the middle of what the
    /// user can actually see instead of possibly behind the panel.
    /// Applies immediately and stays armed for a moment so the
    /// subview-switch refits re-apply it instead of overriding it.
    reveal(id, rightInset = 0) {
      pendingReveal = { id, rightInset, until: Date.now() + 1000 };
      requestAnimationFrame(() => applyPendingReveal());
    },
    /// Temporary highlight for explanation hover: borrows the vis
    /// selection, saving the user's own selection for unhighlight().
    /// Nodes in `subtractedIds` highlight red instead of blue — they
    /// are the terms a formula subtracts.
    highlight(ids, subtractedIds = []) {
      if (!network) return;
      if (highlightStash === null) highlightStash = network.getSelectedNodes();
      // Hovers can follow each other without an unhighlight between
      // (mouseover fires per row); restore the previous red set first
      // or its nodes keep the red style.
      restoreRedHighlights();
      const subs = subtractedIds.filter((id) => componentById.has(id));
      if (subs.length) {
        redHighlighted = subs;
        nodesDS.update(subs.map((id) => nodeFor(componentById.get(id))));
      }
      network.selectNodes(ids.filter((id) => componentById.has(id)));
    },
    unhighlight() {
      if (!network || highlightStash === null) return;
      restoreRedHighlights();
      network.selectNodes(highlightStash.filter((id) => componentById.has(id)));
      highlightStash = null;
      // The effective selection can have changed during the hover
      // (e.g. a refresh dropped a stashed node); tell the panels now
      // instead of waiting for the next gesture.
      notifySelection();
    },
    /// Array of [from, to] edges as rendered. Source-of-truth is the
    /// vis DataSet so this reflects whatever the canvas is actually
    /// showing (post-diff during incremental refreshes).
    connections() {
      if (!edgesDS) return [];
      return edgesDS.get().map((e) => [e.from, e.to]);
    },
    setSelectionHandler(select, deselect) {
      onSelect = select;
      onDeselect = deselect;
    },
    /// Forget the last notified selection, so the next gesture
    /// re-fires the handlers even for an unchanged selection. Call
    /// when a panel the handlers feed (the inspector) was closed
    /// behind notifySelection's back — e.g. a tab switch — so
    /// re-clicking the still-selected node reopens it.
    resetNotify() {
      lastNotified = "";
    },
    /// Live-overlay feed: one WS sample. Cheap — records the value
    /// even while values are off, marks the id dirty, and arms the
    /// 1 Hz flush.
    applySample(ev) {
      const mg = readSelectedMg();
      if (mg == null || (ev.mg_id != null && ev.mg_id !== mg)) return;
      syncLiveMg();
      const e = liveEntry(ev.id);
      e.ts = ev.ts_ms ?? Date.now();
      let drawn = true;
      switch (ev.metric) {
        case "active_power_w": e.p = ev.value; break;
        case "reactive_power_var": e.q = ev.value; break;
        case "soc_pct": e.soc = ev.value; break;
        case "dc_power_w": e.dc = ev.value; break;
        case "energy_wh": e.energy = ev.value; drawn = false; break;
        case "active_power_lower_bound_w": e.pLo = ev.value; drawn = false; break;
        case "active_power_upper_bound_w": e.pHi = ev.value; drawn = false; break;
        case "reactive_power_lower_bound_var": e.qLo = ev.value; drawn = false; break;
        case "reactive_power_upper_bound_var": e.qHi = ev.value; drawn = false; break;
        default: return;
      }
      if (ev.metric === "active_power_lower_bound_w" || ev.metric === "active_power_upper_bound_w") {
        maxAbsBoundW = Math.max(maxAbsBoundW, Math.abs(ev.value));
      }
      // 60 s power history for the hover sparkline; batteries are
      // judged by their DC side.
      const histMetric = componentById.get(ev.id)?.category === "battery" ? "dc_power_w" : "active_power_w";
      if (ev.metric === histMetric && Number.isFinite(ev.value)) {
        e.hist.push([e.ts, ev.value]);
        if (e.hist.length > 60) e.hist.splice(0, e.hist.length - 60);
      }
      if (!drawn) return;
      liveDirty.add(ev.id);
      armLiveFlush();
    },
    /// Apply pending live updates now — subview enter calls this so
    /// a hidden tab's accumulated samples land immediately.
    flushLive,
    /// Smoke-test hook: apply() invocations so far — lets a test
    /// wait for a topology refresh to actually land.
    debugApplyCount() {
      return applyCount;
    },
    /// Smoke-test hook: the width vis-network applied to each pill —
    /// content-derived, stable across flushes. Read off the shape
    /// rather than getBoundingBox(), which ignores a custom shape's
    /// reported dimensions.
    debugNodeWidths() {
      if (!network || !nodesDS) return [];
      return nodesDS.getIds().map((id) => network.body.nodes[id]?.shape?.width ?? null);
    },
    /// Smoke-test hook: the height vis-network applied to each pill.
    /// Read off the shape for the same reason as debugNodeWidths:
    /// getBoundingBox() ignores a custom shape's reported dimensions.
    debugNodeHeights() {
      if (!network || !nodesDS) return [];
      return nodesDS.getIds().map((id) => network.body.nodes[id]?.shape?.height ?? null);
    },
    /// Smoke-test hook: every node's pill model as drawn.
    debugNodeModels() {
      return nodesDS ? nodesDS.get().map((n) => n.pillModel) : [];
    },
    /// Smoke-test hook: every edge's live flow chevron state.
    debugLiveEdges() {
      if (!edgesDS) return [];
      return edgesDS.get().map((e) => ({
        id: e.id,
        width: e.width ?? 1.5,
        middleEnabled: Boolean(e.arrows?.middle?.enabled),
        scaleFactor: e.arrows?.middle?.enabled ? e.arrows.middle.scaleFactor : 0,
        color: e.color?.color ?? null,
        toScale: e.arrows?.to?.scaleFactor ?? null,
      }));
    },
    /// The live-overlay toggle. Off drops every pill's value row
    /// and strips the chevrons in one bulk update, then re-measures
    /// the (now shorter) nodes.
    setValues(on) {
      liveEnabled = Boolean(on);
      // Both directions rebuild every pill: with its value row while
      // live, name-only when off.
      if (nodesDS) {
        nodesDS.update(
          nodesDS.getIds().filter((id) => componentById.has(id)).map((id) => nodeFor(componentById.get(id))),
        );
      }
      if (liveEnabled) {
        localStorage.removeItem(LIVE_KEY);
        for (const id of liveValues.keys()) liveDirty.add(id);
        flushLive();
      } else {
        localStorage.setItem(LIVE_KEY, "0");
        liveDirty.clear();
        if (edgesDS) {
          edgesDS.update(
            edgesDS.get().map((e) => ({ id: e.id, ...edgeRestStyle() })),
          );
        }
      }
      if (!manualArrangement) {
        pendingMeasuredRelayout = true;
        if (network) network.redraw();
      }
    },
    valuesOn() {
      return liveEnabled;
    },
    /// Smoke-test hook: one component's live entry.
    debugLiveEntry(id) {
      return liveValues.get(id) ?? null;
    },
    /// Turns the magnetic drag grid on or off (the canvas header's
    /// "snap" toggle). Off, nodes drag freely; Alt axis locking works
    /// either way.
    setSnap(on) {
      snapEnabled = on;
    },
    /// Drops the manual arrangement and recomputes the layout,
    /// switching the algorithm when a name is given.
    resetLayout(name) {
      if (name && LAYOUTS[name]) currentLayout = name;
      manualArrangement = false;
      layoutHierarchy();
      if (network) network.fit({ animation: false });
    },
    /// Aligns the selected nodes: "row" (same y), "column" (same x),
    /// "spread-h" / "spread-v" (even spacing), "reverse-h" /
    /// "reverse-v" (mirror left–right / top–bottom about the
    /// selection's center), "grid" (rank by flow depth: nodes
    /// the same number of steps from the selection's start line up,
    /// a fan-out spreads its branches on their own lines, and a plain
    /// chain keeps one line), "lanes" / "radial" (the layout
    /// algorithm of the same name, run on the selection only and
    /// kept centered where it was). After the shared coordinate is set,
    /// nodes too close on the free axis are pushed apart by their
    /// real sizes, so an alignment never stacks nodes on top of each
    /// other (e.g. "column" on a horizontal chain turns it vertical
    /// instead of collapsing it onto one point). Counts as a manual
    /// arrangement.
    alignSelection(mode) {
      if (!network) return;
      const ids = network.getSelectedNodes();
      if (ids.length < 2) return;
      const positions = network.getPositions(ids);
      const { width, height } = nodeSizes(ids);
      const pts = ids.map((id) => ({
        id,
        ...positions[id],
        w: width.get(id),
        h: height.get(id),
      }));
      const xs = pts.map((p) => p.x);
      const ys = pts.map((p) => p.y);
      const mean = (a) => a.reduce((s, v) => s + v, 0) / a.length;
      const PAD = 24;
      // The room two nodes need between their centers on `axis`.
      const need = (a, b, axis) =>
        (axis === "x" ? (a.w + b.w) / 2 : (a.h + b.h) / 2) + PAD;

      // Sorts along `axis` (stable on ties via the other axis) and
      // pushes neighbours apart to their needed spacing, then
      // recenters so the group's mean stays put.
      const resolveOverlaps = (axis) => {
        const other = axis === "x" ? "y" : "x";
        pts.sort((a, b) => a[axis] - b[axis] || a[other] - b[other]);
        const before = mean(pts.map((p) => p[axis]));
        for (let i = 1; i < pts.length; i++) {
          const min = pts[i - 1][axis] + need(pts[i - 1], pts[i], axis);
          if (pts[i][axis] < min) pts[i][axis] = min;
        }
        const shift = before - mean(pts.map((p) => p[axis]));
        for (const p of pts) p[axis] += shift;
      };

      // Even spacing over the current span; when the selection has no
      // span on that axis, fan it out by node sizes instead. A final
      // overlap pass covers uneven node sizes, where even steps can
      // still put two big nodes too close.
      const spread = (axis) => {
        const other = axis === "x" ? "y" : "x";
        pts.sort((a, b) => a[axis] - b[axis] || a[other] - b[other]);
        const lo = pts[0][axis];
        const hi = pts[pts.length - 1][axis];
        const minSpan = pts
          .slice(1)
          .reduce((s, p, i) => s + need(pts[i], p, axis), 0);
        const span = Math.max(hi - lo, minSpan);
        const start = mean(pts.map((p) => p[axis])) - span / 2;
        pts.forEach((p, i) => {
          p[axis] = start + (span * i) / (pts.length - 1);
        });
        resolveOverlaps(axis);
      };

      // Structure-aware: split the selection into chains (connected
      // groups), keep each chain as its own line, and line up the
      // k-th node of every chain — two selected meter→inverter→battery
      // chains become two rows with meters, inverters and batteries
      // each sharing a column.
      const grid = () => {
        const inSelection = new Set(pts.map((p) => p.id));
        const byId = new Map(pts.map((p) => [p.id, p]));
        const childIds = new Map(pts.map((p) => [p.id, []]));
        const parentIds = new Map(pts.map((p) => [p.id, []]));
        for (const e of edgesDS.get()) {
          if (inSelection.has(e.from) && inSelection.has(e.to)) {
            childIds.get(e.from).push(e.to);
            parentIds.get(e.to).push(e.from);
          }
        }
        // Connected groups (direction ignored): they pick the
        // orientation, and each group's lines stay together.
        const groups = [];
        const seen = new Set();
        for (const p of pts) {
          if (seen.has(p.id)) continue;
          const group = [];
          const stack = [p.id];
          while (stack.length) {
            const id = stack.pop();
            if (seen.has(id)) continue;
            seen.add(id);
            group.push(byId.get(id));
            stack.push(...childIds.get(id), ...parentIds.get(id));
          }
          groups.push(group);
        }
        // Do the chains run left-to-right (rows) or top-to-bottom
        // (columns)? Compare their total spans.
        const span = (g, axis) =>
          Math.max(...g.map((p) => p[axis])) - Math.min(...g.map((p) => p[axis]));
        const horizontal =
          groups.reduce((s, g) => s + span(g, "x"), 0) >=
          groups.reduce((s, g) => s + span(g, "y"), 0);
        const rankAxis = horizontal ? "x" : "y";
        const lineAxis = horizontal ? "y" : "x";
        const size = (p, axis) => (axis === "x" ? p.w : p.h);

        // A node's rank follows the flow: steps from the selection's
        // start (a node nothing in the selection feeds). A fan-out's
        // children all land on the next rank, not further along one
        // made-up chain.
        const depth = flowDepths(
          pts.map((p) => p.id),
          parentIds,
        );
        const depthOf = (id) => depth.get(id);

        // The leaves get the lines: group by group, each leaf near
        // where it is now, pushed apart by the leaves' node sizes.
        groups.sort(
          (a, b) => mean(a.map((p) => p[lineAxis])) - mean(b.map((p) => p[lineAxis])),
        );
        const leaves = groups.flatMap((g) =>
          g
            .filter((p) => !childIds.get(p.id).length)
            .sort((a, b) => a[lineAxis] - b[lineAxis]),
        );
        const lines = leaves.map((p) => p[lineAxis]);
        for (let i = 1; i < lines.length; i++) {
          const min = lines[i - 1] + need(leaves[i - 1], leaves[i], lineAxis);
          if (lines[i] < min) lines[i] = min;
        }
        leaves.forEach((p, i) => {
          p[lineAxis] = lines[i];
        });
        // A feeding node sits centered on its children, computed from
        // the deepest rank up so the children are already placed. A
        // plain chain thus keeps all its nodes on one line, as before.
        const feeders = pts
          .filter((p) => childIds.get(p.id).length)
          .sort((a, b) => depthOf(b.id) - depthOf(a.id));
        for (const p of feeders) {
          p[lineAxis] = mean(childIds.get(p.id).map((id) => byId.get(id)[lineAxis]));
        }

        // Nodes sharing a rank must not overlap (two parents of one
        // child both center on it): push them apart along the lines.
        const byRank = new Map();
        for (const p of pts) {
          const k = depthOf(p.id);
          if (!byRank.has(k)) byRank.set(k, []);
          byRank.get(k).push(p);
        }
        for (const members of byRank.values()) {
          members.sort((a, b) => a[lineAxis] - b[lineAxis]);
          for (let i = 1; i < members.length; i++) {
            const min =
              members[i - 1][lineAxis] + need(members[i - 1], members[i], lineAxis);
            if (members[i][lineAxis] < min) members[i][lineAxis] = min;
          }
        }

        // Rank k across the groups shares one coordinate (the chains'
        // first, second, third … nodes line up).
        const maxDepth = Math.max(...pts.map((p) => depthOf(p.id)));
        const ranks = [];
        for (let k = 0; k <= maxDepth; k++) {
          const members = byRank.get(k) ?? [];
          ranks.push({
            at: mean(members.map((p) => p[rankAxis])),
            bulk: Math.max(...members.map((p) => size(p, rankAxis))),
          });
        }
        for (let k = 1; k <= maxDepth; k++) {
          const min = ranks[k - 1].at + (ranks[k - 1].bulk + ranks[k].bulk) / 2 + PAD;
          if (ranks[k].at < min) ranks[k].at = min;
        }
        for (const p of pts) {
          p[rankAxis] = ranks[depthOf(p.id)].at;
        }
      };

      // Runs one of the whole-graph layout algorithms (lanes,
      // radial, …) on the selection only, kept centered where the
      // selection was.
      const asLayout = (algorithm) => {
        const tree = buildTree(ids);
        const pos = new Map();
        algorithm(tree, pos);
        const placed = [...pos.values()];
        const center = (values) => (Math.min(...values) + Math.max(...values)) / 2;
        const dx = center(xs) - center(placed.map((p) => p.x));
        const dy = center(ys) - center(placed.map((p) => p.y));
        for (const p of pts) {
          const q = pos.get(p.id);
          if (q) {
            p.x = q.x + dx;
            p.y = q.y + dy;
          }
        }
      };

      if (mode === "row") {
        const y = mean(ys);
        for (const p of pts) p.y = y;
        resolveOverlaps("x");
      } else if (mode === "column") {
        const x = mean(xs);
        for (const p of pts) p.x = x;
        resolveOverlaps("y");
      } else if (mode === "grid") grid();
      else if (mode === "lanes") asLayout(LAYOUTS.lanes);
      else if (mode === "radial") asLayout(LAYOUTS.radial);
      else if (mode === "spread-h") spread("x");
      else if (mode === "spread-v") spread("y");
      else if (mode === "reverse-h" || mode === "reverse-v") {
        // Mirror the selection about its own center on one axis.
        const axis = mode === "reverse-h" ? "x" : "y";
        const values = pts.map((p) => p[axis]);
        const sum = Math.max(...values) + Math.min(...values);
        for (const p of pts) p[axis] = sum - p[axis];
      }
      for (const p of pts) network.moveNode(p.id, p.x, p.y);
      manualArrangement = true;
    },
    /// Scales the distances inside the selection along one axis
    /// ("x" or "y"), about the selection's center: factor > 1 pulls
    /// the nodes apart, factor < 1 pushes them together. Columns stay
    /// columns and rows stay rows — only the gaps change. Counts as a
    /// manual arrangement.
    scaleSelection(axis, factor) {
      if (!network) return;
      const ids = network.getSelectedNodes();
      if (ids.length < 2) return;
      const positions = network.getPositions(ids);
      const values = ids.map((id) => positions[id][axis]);
      const center = values.reduce((s, v) => s + v, 0) / values.length;
      for (const id of ids) {
        const p = positions[id];
        p[axis] = center + (p[axis] - center) * factor;
        network.moveNode(id, p.x, p.y);
      }
      manualArrangement = true;
    },
  };
}

// The Topology subview's canvas: full editing — Ctrl-drag connect
// through the eval/overrides path, and the edit context menu.
export const topology = createGraphCanvas("topology", {
  tooltip: false,
  onConnect(from, to) {
    evalQuoted(`(connect ${from} ${to})`, "Connect failed");
  },
  onContextMenu: showContextMenu,
  onApply(data) {
    // The chrome status pill keeps showing the gRPC-visible count,
    // which is what most operators care about when reasoning about
    // their topology. Hidden meters render on the canvas (dashed)
    // for context but don't bump the official tally.
    const visibleCount = data.components.filter((c) => !c.hidden).length;
    setStatus(
      `${visibleCount} components, ${data.connections.length} connections`,
      "connected",
    );
    // Flip the body's mg-empty flag so the topology canvas's
    // empty-hint overlay shows/hides without a separate JS pass. A
    // microgrid with zero visible components is treated as empty for
    // hint purposes — hidden meters by themselves don't disqualify
    // the overlay.
    if (visibleCount === 0) {
      document.body.dataset.mgEmpty = "1";
    } else {
      delete document.body.dataset.mgEmpty;
    }
  },
});
