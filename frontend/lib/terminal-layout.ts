export type SplitDir = "row" | "column";
export type Zone = "left" | "right" | "top" | "bottom" | "center";
export type Direction = "left" | "right" | "up" | "down";
export type Dock = "window" | "bottom" | "left" | "right";

export type LayoutNode = { kind: "pane"; id: string } | { kind: "split"; dir: SplitDir; ratio: number; a: LayoutNode; b: LayoutNode };
export type TermPane = { id: string; device: string | null; native: string | null };
export type TermTab = { id: string; name: string | null; root: LayoutNode; focus: string };
export type TermLayout = { v: 1; tabs: TermTab[]; active: string; panes: Record<string, TermPane>; dock: Dock; size: number };
export type Rect = { x: number; y: number; w: number; h: number };

export const MAX_PANES = 16;
export const MAX_TABS = 12;
export const MAX_NAME = 40;
export const MIN_RATIO = 0.12;
export const DOCK_SIZE = 0.38;
export const DOCKS: Dock[] = ["window", "bottom", "left", "right"];

const EPS = 1e-6;

let counter = 0;

export function newId(prefix: string): string {
  counter += 1;
  return `${prefix}${Date.now().toString(36)}${counter.toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

export function newPane(device: string | null = null): TermPane {
  return { id: newId("p"), device, native: null };
}

export function freshLayout(device: string | null = null, dock: Dock = "window", size = DOCK_SIZE): TermLayout {
  const pane = newPane(device);
  const tab: TermTab = { id: newId("t"), name: null, root: { kind: "pane", id: pane.id }, focus: pane.id };
  return { v: 1, tabs: [tab], active: tab.id, panes: { [pane.id]: pane }, dock, size };
}

export function panesIn(node: LayoutNode): string[] {
  return node.kind === "pane" ? [node.id] : [...panesIn(node.a), ...panesIn(node.b)];
}

export function activeTab(layout: TermLayout): TermTab | null {
  return layout.tabs.find((t) => t.id === layout.active) ?? layout.tabs[0] ?? null;
}

export function focusedPane(layout: TermLayout): TermPane | null {
  const tab = activeTab(layout);
  return tab ? layout.panes[tab.focus] ?? null : null;
}

export function tabOf(layout: TermLayout, paneId: string): TermTab | null {
  return layout.tabs.find((t) => panesIn(t.root).includes(paneId)) ?? null;
}

export function paneCount(layout: TermLayout): number {
  return Object.keys(layout.panes).length;
}

function clampRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return 0.5;
  return Math.min(1 - MIN_RATIO, Math.max(MIN_RATIO, ratio));
}

function withoutLeaf(node: LayoutNode, id: string): LayoutNode | null {
  if (node.kind === "pane") return node.id === id ? null : node;
  const a = withoutLeaf(node.a, id);
  const b = withoutLeaf(node.b, id);
  if (!a) return b;
  if (!b) return a;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

function mapLeaf(node: LayoutNode, id: string, make: (leaf: LayoutNode) => LayoutNode): LayoutNode {
  if (node.kind === "pane") return node.id === id ? make(node) : node;
  const a = mapLeaf(node.a, id, make);
  const b = mapLeaf(node.b, id, make);
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

function placed(target: LayoutNode, moving: LayoutNode, zone: Exclude<Zone, "center">): LayoutNode {
  const dir: SplitDir = zone === "left" || zone === "right" ? "row" : "column";
  const first = zone === "left" || zone === "top";
  return { kind: "split", dir, ratio: 0.5, a: first ? moving : target, b: first ? target : moving };
}

function nearestIn(order: string[], gone: string, fallback: string): string {
  const at = order.indexOf(gone);
  const rest = order.filter((id) => id !== gone);
  if (!rest.length) return fallback;
  return rest[Math.min(Math.max(at - 1, 0), rest.length - 1)];
}

function replaceTab(layout: TermLayout, tab: TermTab): TermLayout {
  return { ...layout, tabs: layout.tabs.map((t) => (t.id === tab.id ? tab : t)) };
}

export function selectTab(layout: TermLayout, tabId: string): TermLayout {
  return layout.tabs.some((t) => t.id === tabId) && layout.active !== tabId ? { ...layout, active: tabId } : layout;
}

export function cycleTab(layout: TermLayout, delta: number): TermLayout {
  const at = layout.tabs.findIndex((t) => t.id === layout.active);
  if (at < 0 || layout.tabs.length < 2) return layout;
  const next = layout.tabs[(at + delta + layout.tabs.length) % layout.tabs.length];
  return { ...layout, active: next.id };
}

export function focusPane(layout: TermLayout, paneId: string): TermLayout {
  const tab = tabOf(layout, paneId);
  if (!tab) return layout;
  if (tab.focus === paneId && layout.active === tab.id) return layout;
  return { ...replaceTab(layout, { ...tab, focus: paneId }), active: tab.id };
}

export function addTab(layout: TermLayout, pane: TermPane): TermLayout {
  if (layout.tabs.length >= MAX_TABS || paneCount(layout) >= MAX_PANES) return layout;
  const tab: TermTab = { id: newId("t"), name: null, root: { kind: "pane", id: pane.id }, focus: pane.id };
  const at = layout.tabs.findIndex((t) => t.id === layout.active);
  const tabs = [...layout.tabs];
  tabs.splice(at < 0 ? tabs.length : at + 1, 0, tab);
  return { ...layout, tabs, active: tab.id, panes: { ...layout.panes, [pane.id]: pane } };
}

export function splitPane(layout: TermLayout, paneId: string, dir: SplitDir, pane: TermPane): TermLayout {
  const tab = tabOf(layout, paneId);
  if (!tab || paneCount(layout) >= MAX_PANES) return layout;
  const zone = dir === "row" ? "right" : "bottom";
  const root = mapLeaf(tab.root, paneId, (leaf) => placed(leaf, { kind: "pane", id: pane.id }, zone));
  return { ...replaceTab(layout, { ...tab, root, focus: pane.id }), active: tab.id, panes: { ...layout.panes, [pane.id]: pane } };
}

export function placePane(layout: TermLayout, pane: TermPane, how: "tab" | SplitDir, target: string | null): TermLayout | null {
  const placed = how === "tab" ? addTab(layout, pane) : target ? splitPane(layout, target, how, pane) : layout;
  if (placed !== layout) return placed;
  if (how !== "tab" || !target) return null;
  const split = splitPane(layout, target, "row", pane);
  return split === layout ? null : split;
}

export function adoptShells(layout: TermLayout, running: string[], unopened: (paneId: string) => boolean): TermLayout {
  const known = new Set(Object.values(layout.panes).map((p) => p.native));
  const blank = layout.tabs.flatMap((t) => panesIn(t.root)).filter((id) => !layout.panes[id].native && unopened(id));
  let next = layout;
  for (const native of running) {
    if (known.has(native)) continue;
    known.add(native);
    const free = blank.shift();
    next = free ? updatePane(next, free, { native }) : (placePane(next, { ...newPane(null), native }, "tab", null) ?? next);
  }
  return next;
}

export type TerminalDevice = { id: string; name: string };

export function deviceChoices(online: TerminalDevice[] | null, localRunner: string | null, thisMac = "this Mac"): TerminalDevice[] {
  const named = (online ?? []).map((d) => (d.id === localRunner ? { id: d.id, name: thisMac } : d));
  const local = localRunner && !named.some((d) => d.id === localRunner) ? [{ id: localRunner, name: thisMac }] : [];
  return [...local, ...named].sort((a, b) => Number(b.id === localRunner) - Number(a.id === localRunner));
}

export function otherDevices(choices: TerminalDevice[], current: string | null): TerminalDevice[] {
  return choices.filter((d) => d.id !== current);
}

export type LocalCheck = { state: "checking" | "slow" | "known"; runner: string | null };

export type LocalEvent = { kind: "answer"; runner: string | null } | { kind: "slow" };

export function nextLocalCheck(check: LocalCheck, event: LocalEvent): LocalCheck {
  if (event.kind === "answer") return { state: "known", runner: event.runner };
  return check.state === "known" ? check : { ...check, state: "slow" };
}

export const LOCAL_CHECK_MS = 5000;
export const LOCAL_RETRY_MS = 2000;
export const LOCAL_RETRY_MAX_MS = 30000;

export function localRetryIn(failures: number): number {
  return Math.min(LOCAL_RETRY_MAX_MS, LOCAL_RETRY_MS * 2 ** Math.max(0, failures - 1));
}

export type CheckTimers = { set: (run: () => void, ms: number) => number; clear: (id: number) => void };

const windowTimers: CheckTimers = { set: (run, ms) => window.setTimeout(run, ms), clear: (id) => window.clearTimeout(id) };

export function checkUntilAnswered<T>(ask: () => Promise<T>, on: { answer: (value: T) => void; slow: () => void }, timers: CheckTimers = windowTimers): () => void {
  let done = false;
  let failures = 0;
  const pending = new Set<number>();
  const later = (run: () => void, ms: number) => {
    const id = timers.set(() => {
      pending.delete(id);
      if (!done) run();
    }, ms);
    pending.add(id);
    return id;
  };
  const stop = () => {
    done = true;
    for (const id of pending) timers.clear(id);
    pending.clear();
  };
  const attempt = () => {
    let settled = false;
    const fail = () => {
      if (settled || done) return;
      settled = true;
      timers.clear(limit);
      pending.delete(limit);
      failures += 1;
      on.slow();
      later(attempt, localRetryIn(failures));
    };
    const limit = later(fail, LOCAL_CHECK_MS);
    Promise.resolve()
      .then(ask)
      .then((value) => {
        if (done) return;
        stop();
        on.answer(value);
      }, fail);
  };
  attempt();
  return stop;
}

export type TerminalTargets = { devices: readonly unknown[] | null; local: LocalCheck; listed: ReadonlySet<string> | null };

export function targetsReady(at: TerminalTargets): boolean {
  return at.devices !== null && at.listed !== null && at.local.state === "known";
}

export function targetsSlow(at: TerminalTargets): boolean {
  return at.devices !== null && at.listed !== null && at.local.state === "slow";
}

export type CommandAction = "new_tab" | "split_right" | "split_down" | "focus";

export type QueuedAction = { action: CommandAction; device?: string; pane?: string; inside?: boolean; tab?: string };

function sameQueued(a: QueuedAction, b: QueuedAction): boolean {
  return a.action === b.action && (a.device ?? null) === (b.device ?? null) && (a.pane ?? null) === (b.pane ?? null) && !!a.inside === !!b.inside && (a.tab ?? null) === (b.tab ?? null);
}

export function enqueue<T extends QueuedAction>(queue: T[], next: T): T[] {
  return queue.some((queued) => sameQueued(queued, next)) ? queue : [...queue, next];
}

export function queuedOpens(queue: readonly QueuedAction[], at: { blankFree: boolean; panes: ReadonlySet<string> }): number {
  return queue.filter((queued) => {
    const plan = planCommand({ ready: true, reuse: !queued.inside, action: queued.action, device: queued.device ?? null, blankFree: at.blankFree, paneGone: !!queued.pane && !at.panes.has(queued.pane) });
    return plan === "place" || plan === "open_blank";
  }).length;
}

export function waitingNote(at: TerminalTargets, count: number): string | null {
  if (count <= 0 || targetsReady(at)) return null;
  const opens = count === 1 ? "Your new terminal opens" : `${count} new terminals open`;
  return at.local.state === "known" ? `Looking for your devices. ${opens} as soon as they are found.` : `Waiting for this Mac. ${opens} as soon as it answers.`;
}

export type RestoredPlan = "reattach" | "open" | "wait" | "manual";

export function planRestored(at: { ready: boolean; pane: TermPane; focused: boolean; online: ReadonlySet<string>; localRunner: string | null }): RestoredPlan {
  const { pane } = at;
  if (!at.ready) return pane.device && pane.native ? "wait" : "manual";
  if (pane.native && at.localRunner && pane.device === at.localRunner) return "reattach";
  return pane.device && at.focused && at.online.has(pane.device) ? "open" : "manual";
}

export type CommandPlan = "wait" | "focus" | "open_blank" | "place" | "drop";

export function planCommand(at: { ready: boolean; reuse: boolean; action: CommandAction; device: string | null; blankFree: boolean; paneGone: boolean }): CommandPlan {
  if (at.action === "focus") return "focus";
  if (!at.ready) return "wait";
  if (at.paneGone && at.action !== "new_tab") return "drop";
  if (!at.reuse || at.action !== "new_tab" || !at.blankFree) return "place";
  return at.device ? "open_blank" : "focus";
}

export function blankPane(pane: TermPane | null, idle: boolean): boolean {
  return !!pane && idle && !pane.device && !pane.native;
}

export const MAC_TERMINALS_PER_PLACE = 8;
export const DEVICE_TERMINALS = 5;

export function panesOn(layout: TermLayout, device: string | null): number {
  return Object.values(layout.panes).filter((pane) => pane.device === device).length;
}

export function newPaneProblem(layout: TermLayout, device: string | null, cap: number, place: string): string | null {
  if (paneCount(layout) >= MAX_PANES) return `Loka keeps up to ${MAX_PANES} terminals open. Close one to open another.`;
  if (panesOn(layout, device) >= cap) return `${place} runs at most ${cap} terminals at once. Close one to open another.`;
  return null;
}

export function closePane(layout: TermLayout, paneId: string): TermLayout {
  const tab = tabOf(layout, paneId);
  if (!tab) return layout;
  const panes = { ...layout.panes };
  delete panes[paneId];
  const root = withoutLeaf(tab.root, paneId);
  if (!root) {
    const tabs = layout.tabs.filter((t) => t.id !== tab.id);
    const at = layout.tabs.findIndex((t) => t.id === tab.id);
    const active = layout.active === tab.id ? tabs[Math.min(at, tabs.length - 1)]?.id ?? "" : layout.active;
    return { ...layout, tabs, panes, active };
  }
  const focus = tab.focus === paneId ? nearestIn(panesIn(tab.root), paneId, panesIn(root)[0]) : tab.focus;
  return { ...replaceTab(layout, { ...tab, root, focus }), panes };
}

export function closeTab(layout: TermLayout, tabId: string): TermLayout {
  const tab = layout.tabs.find((t) => t.id === tabId);
  if (!tab) return layout;
  return panesIn(tab.root).reduce((next, id) => closePane(next, id), layout);
}

export function movePane(layout: TermLayout, paneId: string, targetId: string, zone: Zone): TermLayout {
  if (paneId === targetId || !layout.panes[paneId] || !layout.panes[targetId]) return layout;
  const from = tabOf(layout, paneId);
  const to = tabOf(layout, targetId);
  if (!from || !to) return layout;
  if (zone === "center") {
    const swap = (node: LayoutNode): LayoutNode =>
      node.kind === "pane"
        ? node.id === paneId
          ? { kind: "pane", id: targetId }
          : node.id === targetId
            ? { kind: "pane", id: paneId }
            : node
        : { ...node, a: swap(node.a), b: swap(node.b) };
    const tabs = layout.tabs.map((t) => (t.id === from.id || t.id === to.id ? { ...t, root: swap(t.root), focus: t.focus === paneId ? targetId : t.focus === targetId ? paneId : t.focus } : t));
    return focusPane({ ...layout, tabs }, paneId);
  }
  const rest = withoutLeaf(from.root, paneId);
  let tabs = layout.tabs;
  if (!rest) tabs = tabs.filter((t) => t.id !== from.id);
  else tabs = tabs.map((t) => (t.id === from.id ? { ...t, root: rest, focus: t.focus === paneId ? nearestIn(panesIn(from.root), paneId, panesIn(rest)[0]) : t.focus } : t));
  tabs = tabs.map((t) => (t.id === to.id ? { ...t, root: mapLeaf(t.id === from.id && rest ? rest : t.root, targetId, (leaf) => placed(leaf, { kind: "pane", id: paneId }, zone)), focus: paneId } : t));
  return { ...layout, tabs, active: to.id };
}

export function moveTab(layout: TermLayout, tabId: string, targetId: string, zone: Zone): TermLayout {
  const tab = layout.tabs.find((t) => t.id === tabId);
  const to = tabOf(layout, targetId);
  if (!tab || !to || to.id === tab.id) return layout;
  const tabs = layout.tabs
    .filter((t) => t.id !== tab.id)
    .map((t) => (t.id === to.id ? { ...t, root: mapLeaf(t.root, targetId, (leaf) => placed(leaf, tab.root, zone === "center" ? "right" : zone)), focus: tab.focus } : t));
  return { ...layout, tabs, active: to.id };
}

export function paneToTab(layout: TermLayout, paneId: string, beforeTab: string | null = null): TermLayout {
  const from = tabOf(layout, paneId);
  if (!from) return layout;
  if (from.root.kind === "pane") return reorderTab(layout, from.id, beforeTab);
  if (layout.tabs.length >= MAX_TABS) return layout;
  const rest = withoutLeaf(from.root, paneId);
  if (!rest) return layout;
  const focus = from.focus === paneId ? nearestIn(panesIn(from.root), paneId, panesIn(rest)[0]) : from.focus;
  const tab: TermTab = { id: newId("t"), name: null, root: { kind: "pane", id: paneId }, focus: paneId };
  const tabs = layout.tabs.map((t) => (t.id === from.id ? { ...t, root: rest, focus } : t));
  const at = beforeTab ? tabs.findIndex((t) => t.id === beforeTab) : -1;
  tabs.splice(at < 0 ? tabs.length : at, 0, tab);
  return { ...layout, tabs, active: tab.id };
}

export function reorderTab(layout: TermLayout, tabId: string, beforeTab: string | null): TermLayout {
  const tab = layout.tabs.find((t) => t.id === tabId);
  if (!tab || tabId === beforeTab) return layout;
  const tabs = layout.tabs.filter((t) => t.id !== tabId);
  const at = beforeTab ? tabs.findIndex((t) => t.id === beforeTab) : -1;
  tabs.splice(at < 0 ? tabs.length : at, 0, tab);
  return { ...layout, tabs, active: tabId };
}

export function renameTab(layout: TermLayout, tabId: string, name: string): TermLayout {
  const tab = layout.tabs.find((t) => t.id === tabId);
  if (!tab) return layout;
  const clean = name.replace(/[\u0000-\u001f\u007f]/g, "").trim().slice(0, MAX_NAME);
  return replaceTab(layout, { ...tab, name: clean || null });
}

export function setRatio(layout: TermLayout, tabId: string, path: string, ratio: number): TermLayout {
  const tab = layout.tabs.find((t) => t.id === tabId);
  if (!tab) return layout;
  const walk = (node: LayoutNode, rest: string): LayoutNode => {
    if (node.kind === "pane") return node;
    if (!rest) return { ...node, ratio: clampRatio(ratio) };
    return rest[0] === "a" ? { ...node, a: walk(node.a, rest.slice(1)) } : { ...node, b: walk(node.b, rest.slice(1)) };
  };
  return replaceTab(layout, { ...tab, root: walk(tab.root, path) });
}

export function updatePane(layout: TermLayout, paneId: string, change: Partial<Omit<TermPane, "id">>): TermLayout {
  const pane = layout.panes[paneId];
  if (!pane) return layout;
  const next = { ...pane, ...change };
  if (next.device === pane.device && next.native === pane.native) return layout;
  return { ...layout, panes: { ...layout.panes, [paneId]: next } };
}

export function clearClosedShellIds(layout: TermLayout, results: ReadonlyMap<string, { status: string }>): TermLayout {
  let next = layout;
  for (const [id, result] of results) if (result.status === "closed") next = updatePane(next, id, { native: null });
  return next;
}

export function setDock(layout: TermLayout, dock: Dock): TermLayout {
  return layout.dock === dock ? layout : { ...layout, dock };
}

export function setDockSize(layout: TermLayout, size: number): TermLayout {
  const clean = Number.isFinite(size) ? Math.min(0.8, Math.max(0.18, size)) : DOCK_SIZE;
  return { ...layout, size: clean };
}

export function rectsOf(node: LayoutNode, area: Rect = { x: 0, y: 0, w: 1, h: 1 }): Record<string, Rect> {
  if (node.kind === "pane") return { [node.id]: area };
  const r = node.ratio;
  const first = node.dir === "row" ? { ...area, w: area.w * r } : { ...area, h: area.h * r };
  const second = node.dir === "row" ? { ...area, x: area.x + area.w * r, w: area.w * (1 - r) } : { ...area, y: area.y + area.h * r, h: area.h * (1 - r) };
  return { ...rectsOf(node.a, first), ...rectsOf(node.b, second) };
}

function overlap(a0: number, a1: number, b0: number, b1: number): number {
  return Math.max(0, Math.min(a1, b1) - Math.max(a0, b0));
}

export function neighbour(root: LayoutNode, from: string, dir: Direction): string | null {
  const rects = rectsOf(root);
  const cur = rects[from];
  if (!cur) return null;
  let best: { id: string; gap: number; shared: number } | null = null;
  for (const [id, r] of Object.entries(rects)) {
    if (id === from) continue;
    const horizontal = dir === "left" || dir === "right";
    const gap =
      dir === "left" ? cur.x - (r.x + r.w) : dir === "right" ? r.x - (cur.x + cur.w) : dir === "up" ? cur.y - (r.y + r.h) : r.y - (cur.y + cur.h);
    if (gap < -EPS) continue;
    const shared = horizontal ? overlap(cur.y, cur.y + cur.h, r.y, r.y + r.h) : overlap(cur.x, cur.x + cur.w, r.x, r.x + r.w);
    if (shared <= EPS) continue;
    if (!best || gap < best.gap - EPS || (Math.abs(gap - best.gap) <= EPS && shared > best.shared)) best = { id, gap, shared };
  }
  return best?.id ?? null;
}

export function focusToward(layout: TermLayout, dir: Direction): TermLayout {
  const tab = activeTab(layout);
  if (!tab) return layout;
  const next = neighbour(tab.root, tab.focus, dir);
  return next ? focusPane(layout, next) : layout;
}

export function cycleFocus(layout: TermLayout, delta: number): TermLayout {
  const tab = activeTab(layout);
  if (!tab) return layout;
  const order = panesIn(tab.root);
  if (order.length < 2) return layout;
  const at = order.indexOf(tab.focus);
  return focusPane(layout, order[(at + delta + order.length) % order.length]);
}

export function zoneAt(x: number, y: number, w: number, h: number): Zone {
  if (w <= 0 || h <= 0) return "center";
  const fx = x / w;
  const fy = y / h;
  const edges: [Zone, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["top", fy],
    ["bottom", 1 - fy],
  ];
  const [zone, dist] = edges.reduce((best, cur) => (cur[1] < best[1] ? cur : best));
  return dist < 0.28 ? zone : "center";
}

const ID = /^[A-Za-z0-9_-]{1,64}$/;

function str(value: unknown): string | null {
  return typeof value === "string" && ID.test(value) ? value : null;
}

function parseNode(raw: unknown, seen: Set<string>, depth: number): LayoutNode | null {
  if (!raw || typeof raw !== "object" || depth > 12) return null;
  const node = raw as Record<string, unknown>;
  if (node.kind === "pane") {
    const id = str(node.id);
    if (!id || seen.has(id)) return null;
    seen.add(id);
    return { kind: "pane", id };
  }
  if (node.kind !== "split" || (node.dir !== "row" && node.dir !== "column")) return null;
  const a = parseNode(node.a, seen, depth + 1);
  const b = parseNode(node.b, seen, depth + 1);
  if (!a || !b) return null;
  return { kind: "split", dir: node.dir, ratio: clampRatio(typeof node.ratio === "number" ? node.ratio : 0.5), a, b };
}

export function parseLayout(raw: unknown): TermLayout | null {
  if (!raw || typeof raw !== "object") return null;
  const data = raw as Record<string, unknown>;
  if (data.v !== 1 || !Array.isArray(data.tabs) || !data.panes || typeof data.panes !== "object") return null;
  const seen = new Set<string>();
  const panes: Record<string, TermPane> = {};
  const tabs: TermTab[] = [];
  for (const rawTab of data.tabs.slice(0, MAX_TABS)) {
    if (!rawTab || typeof rawTab !== "object") return null;
    const t = rawTab as Record<string, unknown>;
    const id = str(t.id);
    const root = parseNode(t.root, seen, 0);
    if (!id || !root) return null;
    const ids = panesIn(root);
    for (const paneId of ids) {
      const p = (data.panes as Record<string, unknown>)[paneId] as Record<string, unknown> | undefined;
      if (!p || typeof p !== "object") return null;
      panes[paneId] = { id: paneId, device: str(p.device), native: typeof p.native === "string" && p.native.length <= 64 ? p.native : null };
    }
    const focus = typeof t.focus === "string" && ids.includes(t.focus) ? t.focus : ids[0];
    const name = typeof t.name === "string" ? t.name.trim().slice(0, MAX_NAME) || null : null;
    tabs.push({ id, name, root, focus });
  }
  if (!tabs.length || Object.keys(panes).length > MAX_PANES) return null;
  const active = typeof data.active === "string" && tabs.some((t) => t.id === data.active) ? data.active : tabs[0].id;
  const dock = DOCKS.includes(data.dock as Dock) ? (data.dock as Dock) : "window";
  const size = typeof data.size === "number" && Number.isFinite(data.size) ? Math.min(0.8, Math.max(0.18, data.size)) : DOCK_SIZE;
  return { v: 1, tabs, active, panes, dock, size };
}

export function layoutKey(userId: string | null): string {
  return `world.terminals.${userId ?? "anon"}`;
}

export function workshopLayoutKey(sessionKey: string): string {
  return `world.terminals.workshop.${sessionKey}`;
}

export function embedLayoutKey(key: string): string {
  return `world.terminals.embed.${key}`;
}

export function loadLayout(storage: Pick<Storage, "getItem"> | null, key: string): TermLayout | null {
  try {
    const raw = storage?.getItem(key);
    return raw ? parseLayout(JSON.parse(raw)) : null;
  } catch {
    return null;
  }
}

export function saveLayout(storage: Pick<Storage, "setItem" | "removeItem"> | null, key: string, layout: TermLayout | null): void {
  try {
    if (!layout || !layout.tabs.length) storage?.removeItem(key);
    else storage?.setItem(key, JSON.stringify(layout));
  } catch {
    return;
  }
}

export type TerminalSummary = { id: string; label: string; device: string | null; panes: number; running: number };

export function runningTerminals(list: readonly TerminalSummary[]): number {
  return list.reduce((sum, summary) => sum + summary.running, 0);
}

let summaries: TerminalSummary[] = [];
const summaryListeners = new Set<() => void>();

export function terminalSummaries(): TerminalSummary[] {
  return summaries;
}

export function watchTerminals(listener: () => void): () => void {
  summaryListeners.add(listener);
  return () => {
    summaryListeners.delete(listener);
  };
}

export function publishTerminals(next: TerminalSummary[]): void {
  if (JSON.stringify(next) === JSON.stringify(summaries)) return;
  summaries = next;
  for (const listener of summaryListeners) listener();
}

export function savedSummaries(layout: TermLayout | null, live: readonly { id: string; exited: boolean }[], thisMac = "this Mac"): TerminalSummary[] {
  if (!layout) return [];
  const alive = new Set(live.filter((term) => !term.exited).map((term) => term.id));
  const tabs = layout.tabs.map((t) => {
    const ids = panesIn(t.root);
    const running = ids.filter((id) => {
      const native = layout.panes[id]?.native;
      return !!native && alive.has(native);
    }).length;
    return { id: t.id, label: t.name ?? thisMac, device: layout.panes[t.focus]?.device ?? null, panes: ids.length, running };
  });
  return tabs.some((t) => t.running > 0) ? tabs : [];
}
