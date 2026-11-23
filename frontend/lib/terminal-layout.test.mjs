import { test } from "node:test";
import assert from "node:assert/strict";
import {
  LOCAL_CHECK_MS,
  MAX_PANES,
  checkUntilAnswered,
  enqueue,
  localRetryIn,
  planRestored,
  queuedOpens,
  waitingNote,
  activeTab,
  addTab,
  adoptShells,
  deviceChoices,
  otherDevices,
  closePane,
  closeTab,
  cycleFocus,
  cycleTab,
  focusToward,
  focusedPane,
  freshLayout,
  layoutKey,
  loadLayout,
  movePane,
  moveTab,
  neighbour,
  newPane,
  paneToTab,
  panesIn,
  parseLayout,
  placePane,
  newPaneProblem,
  panesOn,
  planCommand,
  targetsReady,
  targetsSlow,
  nextLocalCheck,
  blankPane,
  MAX_TABS,
  workshopLayoutKey,
  rectsOf,
  renameTab,
  reorderTab,
  saveLayout,
  setDock,
  setDockSize,
  setRatio,
  splitPane,
  updatePane,
  zoneAt,
} from "./terminal-layout.ts";
import { shortcutKeycaps, shortcutLabel, terminalShortcut, terminalShortcutRows } from "./terminal-keys.ts";

function grid() {
  let layout = freshLayout("mac");
  const a = focusedPane(layout).id;
  const b = newPane("mac");
  const c = newPane("cloud");
  layout = splitPane(layout, a, "row", b);
  layout = splitPane(layout, b.id, "column", c);
  return { layout, a, b: b.id, c: c.id };
}

function memory() {
  const data = new Map();
  return { getItem: (k) => data.get(k) ?? null, setItem: (k, v) => data.set(k, v), removeItem: (k) => data.delete(k), data };
}

test("splitting right then down builds one tree and focuses the new pane", () => {
  const { layout, a, b, c } = grid();
  const tab = activeTab(layout);
  assert.deepEqual(panesIn(tab.root), [a, b, c]);
  assert.equal(tab.focus, c);
  assert.equal(tab.root.dir, "row");
  assert.equal(tab.root.b.dir, "column");
  assert.equal(layout.panes[c].device, "cloud");
});

test("closing a pane gives its space to its sibling and moves focus to a neighbour", () => {
  const { layout, a, b, c } = grid();
  const closed = closePane(layout, c);
  const tab = activeTab(closed);
  assert.deepEqual(panesIn(tab.root), [a, b]);
  assert.equal(tab.focus, b);
  assert.equal(closed.panes[c], undefined);
  assert.equal(tab.root.dir, "row");
});

test("closing the last pane of a tab removes the tab, and closing the last tab leaves no tabs", () => {
  let layout = freshLayout("mac");
  layout = addTab(layout, newPane("cloud"));
  const second = layout.active;
  const first = layout.tabs[0].id;
  layout = closeTab(layout, second);
  assert.equal(layout.tabs.length, 1);
  assert.equal(layout.active, first);
  layout = closePane(layout, focusedPane(layout).id);
  assert.equal(layout.tabs.length, 0);
  assert.deepEqual(layout.panes, {});
});

test("new tabs open after the active one and cycling tabs wraps", () => {
  let layout = freshLayout("mac");
  const first = layout.active;
  layout = addTab(layout, newPane("mac"));
  const second = layout.active;
  layout = cycleTab(layout, 1);
  assert.equal(layout.active, first);
  layout = cycleTab(layout, -1);
  assert.equal(layout.active, second);
});

test("focus moves by direction to the pane that shares the most edge", () => {
  const { layout, a, b, c } = grid();
  const root = activeTab(layout).root;
  assert.equal(neighbour(root, a, "right"), b);
  assert.equal(neighbour(root, c, "left"), a);
  assert.equal(neighbour(root, b, "down"), c);
  assert.equal(neighbour(root, c, "up"), b);
  assert.equal(neighbour(root, a, "left"), null);
  const moved = focusToward(layout, "left");
  assert.equal(activeTab(moved).focus, a);
  assert.equal(focusToward(moved, "up"), moved);
});

test("focus cycles through panes in reading order", () => {
  const { layout, a, b, c } = grid();
  assert.equal(activeTab(cycleFocus(layout, 1)).focus, a);
  assert.equal(activeTab(cycleFocus(layout, -1)).focus, b);
  assert.equal(activeTab(cycleFocus(cycleFocus(layout, 1), 1)).focus, b);
  assert.ok(c);
});

test("dragging a pane onto another pane's edge docks it there, the centre swaps them", () => {
  const { layout, a, b, c } = grid();
  const left = movePane(layout, c, a, "left");
  const tab = activeTab(left);
  assert.deepEqual(panesIn(tab.root), [c, a, b]);
  assert.equal(tab.focus, c);
  const rects = rectsOf(tab.root);
  assert.ok(rects[c].x < rects[a].x);
  const swapped = movePane(layout, a, c, "center");
  assert.deepEqual(panesIn(activeTab(swapped).root), [c, b, a]);
  assert.equal(movePane(layout, a, a, "left"), layout);
});

test("a pane moves between tabs and its old tab closes when empty", () => {
  let layout = freshLayout("mac");
  const first = focusedPane(layout).id;
  const firstTab = layout.active;
  const other = newPane("cloud");
  layout = addTab(layout, other);
  layout = movePane(layout, other.id, first, "bottom");
  assert.equal(layout.tabs.length, 1);
  assert.equal(layout.active, firstTab);
  assert.deepEqual(panesIn(activeTab(layout).root), [first, other.id]);
  assert.equal(activeTab(layout).root.dir, "column");
});

test("a whole tab can be dropped into another tab as a split, and a pane can be pulled out into its own tab", () => {
  let { layout, a, b, c } = grid();
  const lone = newPane("mac");
  layout = addTab(layout, lone);
  const loneTab = layout.active;
  layout = moveTab(layout, loneTab, a, "top");
  assert.equal(layout.tabs.length, 1);
  assert.deepEqual(panesIn(activeTab(layout).root), [lone.id, a, b, c]);
  layout = paneToTab(layout, b);
  assert.equal(layout.tabs.length, 2);
  assert.deepEqual(panesIn(activeTab(layout).root), [b]);
  assert.deepEqual(panesIn(layout.tabs[0].root), [lone.id, a, c]);
});

test("a new terminal goes in a tab, or beside the focused one when tabs are full, or nowhere when nothing fits", () => {
  let layout = freshLayout("mac");
  for (let i = 1; i < MAX_TABS; i += 1) layout = addTab(layout, newPane("mac"));
  assert.equal(layout.tabs.length, MAX_TABS);
  const focus = focusedPane(layout).id;
  const extra = newPane("cloud");
  const placed = placePane(layout, extra, "tab", focus);
  assert.equal(placed.tabs.length, MAX_TABS);
  assert.deepEqual(panesIn(activeTab(placed).root), [focus, extra.id]);
  assert.equal(placePane(layout, newPane("cloud"), "tab", null), null);
  let full = placed;
  while (Object.keys(full.panes).length < MAX_PANES) full = placePane(full, newPane("mac"), "column", focusedPane(full).id);
  assert.equal(placePane(full, newPane("mac"), "tab", focusedPane(full).id), null);
  assert.equal(placePane(full, newPane("mac"), "row", focusedPane(full).id), null);
});

test("a new terminal is refused with the real limit of the place it would run on", () => {
  let layout = freshLayout("mac");
  for (let i = 1; i < 8; i += 1) layout = addTab(layout, newPane("mac"));
  layout = addTab(layout, newPane("box"));
  assert.equal(panesOn(layout, "mac"), 8);
  assert.equal(newPaneProblem(layout, "mac", 8, "This Mac"), "This Mac runs at most 8 terminals at once. Close one to open another.");
  assert.equal(newPaneProblem(layout, "box", 5, "Build box"), null);
  assert.equal(newPaneProblem(layout, null, Infinity, "a device"), null);
  let full = layout;
  while (Object.keys(full.panes).length < MAX_PANES) full = splitPane(full, focusedPane(full).id, "row", newPane("box"));
  assert.match(newPaneProblem(full, "other", 5, "Other"), /up to 16 terminals/);
});

test("tabs reorder and rename with a clean, capped name", () => {
  let layout = freshLayout("mac");
  const first = layout.active;
  layout = addTab(layout, newPane("mac"));
  const second = layout.active;
  layout = reorderTab(layout, second, first);
  assert.deepEqual(layout.tabs.map((t) => t.id), [second, first]);
  layout = renameTab(layout, first, "  build\u0007 logs  ");
  assert.equal(layout.tabs[1].name, "build logs");
  layout = renameTab(layout, first, "   ");
  assert.equal(layout.tabs[1].name, null);
  layout = renameTab(layout, first, "x".repeat(90));
  assert.equal(layout.tabs[1].name.length, 40);
});

test("split ratios are clamped so no pane can vanish", () => {
  const { layout } = grid();
  const tab = activeTab(layout);
  assert.equal(activeTab(setRatio(layout, tab.id, "", 0.01)).root.ratio, 0.12);
  assert.equal(activeTab(setRatio(layout, tab.id, "b", 0.99)).root.b.ratio, 0.88);
  assert.equal(activeTab(setRatio(layout, tab.id, "b", Number.NaN)).root.b.ratio, 0.5);
});

test("the pane and tab caps hold", () => {
  let layout = freshLayout("mac");
  for (let i = 0; i < 40; i += 1) layout = splitPane(layout, focusedPane(layout).id, i % 2 ? "row" : "column", newPane("mac"));
  assert.equal(Object.keys(layout.panes).length, MAX_PANES);
  assert.equal(addTab(layout, newPane("mac")), layout);
});

test("the dock place and size persist per user and come back the same", () => {
  const store = memory();
  let { layout } = grid();
  layout = setDockSize(setDock(layout, "bottom"), 2);
  layout = updatePane(layout, focusedPane(layout).id, { native: "abc-123" });
  saveLayout(store, layoutKey("u1"), layout);
  assert.ok(store.data.has(layoutKey("u1")));
  assert.equal(loadLayout(store, layoutKey("u2")), null);
  assert.equal(loadLayout(store, workshopLayoutKey("s1")), null);
  const back = loadLayout(store, layoutKey("u1"));
  assert.deepEqual(back, layout);
  assert.equal(back.dock, "bottom");
  assert.equal(back.size, 0.8);
  saveLayout(store, layoutKey("u1"), closeTab(back, back.active));
  assert.equal(store.data.has(layoutKey("u1")), false);
});

test("a corrupt saved layout falls back to nothing instead of breaking the terminal", () => {
  const store = memory();
  store.setItem(layoutKey("u1"), "{not json");
  assert.equal(loadLayout(store, layoutKey("u1")), null);
  assert.equal(parseLayout(null), null);
  assert.equal(parseLayout({ v: 2, tabs: [], panes: {} }), null);
  const { layout, a } = grid();
  const dup = JSON.parse(JSON.stringify(layout));
  dup.tabs[0].root.b.a = { kind: "pane", id: a };
  assert.equal(parseLayout(dup), null);
  const missing = JSON.parse(JSON.stringify(layout));
  delete missing.panes[a];
  assert.equal(parseLayout(missing), null);
  const odd = JSON.parse(JSON.stringify(layout));
  odd.dock = "ceiling";
  odd.active = "gone";
  odd.tabs[0].focus = "gone";
  odd.panes[a].device = "../../etc";
  const fixed = parseLayout(odd);
  assert.equal(fixed.dock, "window");
  assert.equal(fixed.active, fixed.tabs[0].id);
  assert.equal(fixed.tabs[0].focus, a);
  assert.equal(fixed.panes[a].device, null);
});

test("drops land on the nearest edge, or the centre away from the edges", () => {
  assert.equal(zoneAt(5, 50, 100, 100), "left");
  assert.equal(zoneAt(95, 50, 100, 100), "right");
  assert.equal(zoneAt(50, 5, 100, 100), "top");
  assert.equal(zoneAt(50, 96, 100, 100), "bottom");
  assert.equal(zoneAt(50, 50, 100, 100), "center");
  assert.equal(zoneAt(1, 1, 0, 0), "center");
});

const key = (k, mods = {}) => ({ key: k, code: "", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...mods });

test("the Mac app uses Ghostty's keys", () => {
  assert.equal(terminalShortcut(key("t", { metaKey: true }), "app"), "new_tab");
  assert.equal(terminalShortcut(key("d", { metaKey: true }), "app"), "split_right");
  assert.equal(terminalShortcut(key("D", { metaKey: true, shiftKey: true }), "app"), "split_down");
  assert.equal(terminalShortcut(key("w", { metaKey: true }), "app"), "close");
  assert.equal(terminalShortcut(key("[", { metaKey: true, code: "BracketLeft" }), "app"), "focus_prev");
  assert.equal(terminalShortcut(key("]", { metaKey: true, code: "BracketRight" }), "app"), "focus_next");
  assert.equal(terminalShortcut(key("{", { metaKey: true, shiftKey: true, code: "BracketLeft" }), "app"), "tab_prev");
  assert.equal(terminalShortcut(key("}", { metaKey: true, shiftKey: true, code: "BracketRight" }), "app"), "tab_next");
  assert.equal(terminalShortcut(key("ArrowLeft", { metaKey: true, altKey: true }), "app"), "focus_left");
  assert.equal(terminalShortcut(key("ArrowDown", { metaKey: true, altKey: true }), "app"), "focus_down");
});

test("in a browser no shortcut is one the browser keeps for itself", () => {
  const reserved = [
    key("t", { metaKey: true }),
    key("w", { metaKey: true }),
    key("T", { ctrlKey: true, shiftKey: true, code: "KeyT" }),
    key("W", { ctrlKey: true, shiftKey: true, code: "KeyW" }),
    key("N", { ctrlKey: true, shiftKey: true, code: "KeyN" }),
    key("ArrowLeft", { metaKey: true, altKey: true }),
    key("{", { metaKey: true, shiftKey: true, code: "BracketLeft" }),
    key("Tab", { ctrlKey: true, shiftKey: true, code: "Tab" }),
  ];
  for (const e of reserved) assert.equal(terminalShortcut(e, "web"), null, JSON.stringify(e));
  assert.equal(terminalShortcut(key("Enter", { ctrlKey: true, shiftKey: true, code: "Enter" }), "web"), "new_tab");
  assert.equal(terminalShortcut(key("D", { ctrlKey: true, shiftKey: true, code: "KeyD" }), "web"), "split_right");
  assert.equal(terminalShortcut(key("E", { ctrlKey: true, shiftKey: true, code: "KeyE" }), "web"), "split_down");
  assert.equal(terminalShortcut(key("X", { ctrlKey: true, shiftKey: true, code: "KeyX" }), "web"), "close");
  assert.equal(terminalShortcut(key("{", { ctrlKey: true, shiftKey: true, code: "BracketLeft" }), "web"), "focus_prev");
  assert.equal(terminalShortcut(key("}", { ctrlKey: true, shiftKey: true, code: "BracketRight" }), "web"), "focus_next");
  assert.equal(terminalShortcut(key("<", { ctrlKey: true, shiftKey: true, code: "Comma" }), "web"), "tab_prev");
  assert.equal(terminalShortcut(key(">", { ctrlKey: true, shiftKey: true, code: "Period" }), "web"), "tab_next");
  assert.equal(terminalShortcut(key("ArrowUp", { ctrlKey: true, shiftKey: true }), "web"), "focus_up");
});

test("terminal keys leave shell and Loka keys alone", () => {
  for (const profile of ["app", "web"]) {
    assert.equal(terminalShortcut(key("d", { ctrlKey: true }), profile), null);
    assert.equal(terminalShortcut(key("c", { ctrlKey: true }), profile), null);
    assert.equal(terminalShortcut(key("k", { metaKey: true }), profile), null);
    assert.equal(terminalShortcut(key("f", { metaKey: true }), profile), null);
    assert.equal(terminalShortcut(key("1", { metaKey: true }), profile), null);
    assert.equal(terminalShortcut(key("ArrowLeft", { metaKey: true }), profile), null);
    assert.equal(terminalShortcut(key("ArrowLeft", { altKey: true }), profile), null);
  }
  assert.equal(terminalShortcut(key("t", { metaKey: true, ctrlKey: true }), "app"), null);
  assert.equal(terminalShortcut(key("d", { metaKey: true }), "web"), null);
});

test("hints show the keys of the place you are in", () => {
  assert.equal(shortcutLabel("new_tab", "app"), "Cmd T");
  assert.equal(shortcutLabel("new_tab", "web"), "Ctrl Shift Enter");
  assert.equal(shortcutKeycaps("split_down", "app"), "⌘ ⇧ D");
  assert.equal(shortcutKeycaps("close", "web"), "⌃ ⇧ X");
});

test("shells already running fill the blank pane first, then get their own tabs, and known ones are skipped", () => {
  const fresh = freshLayout(null);
  const blank = focusedPane(fresh).id;
  const adopted = adoptShells(fresh, ["s1", "s2", "s3"], () => true);
  assert.equal(adopted.panes[blank].native, "s1");
  assert.equal(Object.keys(adopted.panes).length, 3);
  assert.deepEqual(Object.values(adopted.panes).map((p) => p.native).sort(), ["s1", "s2", "s3"]);
  assert.equal(adopted.tabs.length, 3);
  assert.equal(adoptShells(adopted, ["s1", "s2", "s3"], () => true), adopted);
});

test("a pane that already started its own shell is never handed a running one", () => {
  const fresh = freshLayout(null);
  const blank = focusedPane(fresh).id;
  const adopted = adoptShells(fresh, ["s1"], () => false);
  assert.equal(adopted.panes[blank].native, null);
  assert.equal(Object.keys(adopted.panes).length, 2);
  assert.equal(adoptShells(fresh, [], () => true), fresh);
});

test("a new terminal can open on this Mac or any online device, this Mac first and named once", () => {
  const online = [
    { id: "cloud", name: "Cloud box" },
    { id: "mac", name: "Studio Mac" },
  ];
  assert.deepEqual(deviceChoices(online, "mac"), [
    { id: "mac", name: "this Mac" },
    { id: "cloud", name: "Cloud box" },
  ]);
  assert.deepEqual(deviceChoices(online, null), online);
  assert.deepEqual(deviceChoices([], "mac"), [{ id: "mac", name: "this Mac" }]);
  assert.deepEqual(deviceChoices(null, null), []);
});

test("an idle or ended terminal offers every device but the one it was on", () => {
  const choices = deviceChoices([{ id: "cloud", name: "Cloud box" }, { id: "mini", name: "Mac mini" }], "mac");
  assert.deepEqual(otherDevices(choices, "cloud").map((d) => d.id), ["mac", "mini"]);
  assert.deepEqual(otherDevices(choices, null).map((d) => d.id), ["mac", "cloud", "mini"]);
});

test("the shortcuts sheet lists every terminal key of the place you are in, the same keys the hints show", () => {
  for (const profile of ["app", "web"]) {
    const rows = terminalShortcutRows(profile);
    assert.equal(rows.length, 9);
    for (const action of ["new_tab", "split_right", "split_down", "close", "focus_prev", "focus_next", "tab_prev", "tab_next"]) {
      assert.ok(rows.some(([keys]) => keys.join(" ") === shortcutKeycaps(action, profile)), `${profile} ${action}`);
    }
  }
  assert.deepEqual(terminalShortcutRows("app")[0][0], ["⌘", "T"]);
  assert.deepEqual(terminalShortcutRows("web")[0][0], ["⌃", "⇧", "↵"]);
});

test("terminal targets are ready only once devices, this Mac's own shell and running shells are all known", () => {
  const listed = new Set();
  const known = { state: "known", runner: "mac" };
  const checking = { state: "checking", runner: null };
  assert.equal(targetsReady({ devices: null, local: checking, listed }), false);
  assert.equal(targetsReady({ devices: [{ id: "mac" }], local: checking, listed }), false);
  assert.equal(targetsReady({ devices: null, local: known, listed }), false);
  assert.equal(targetsReady({ devices: [], local: known, listed: null }), false);
  assert.equal(targetsReady({ devices: [{ id: "mac" }], local: known, listed }), true);
  assert.equal(targetsReady({ devices: [{ id: "mac" }], local: { state: "known", runner: null }, listed }), true);
});

test("this Mac answers before the slow timer: the late timer changes nothing", () => {
  let check = { state: "checking", runner: null };
  check = nextLocalCheck(check, { kind: "answer", runner: "mac" });
  check = nextLocalCheck(check, { kind: "slow" });
  assert.deepEqual(check, { state: "known", runner: "mac" });
  assert.equal(targetsReady({ devices: [{ id: "mac" }], local: check, listed: new Set() }), true);
});

test("this Mac answers after the slow timer: slow never counts as not local, the late answer makes the targets ready", () => {
  const devices = [{ id: "mac" }, { id: "cloud" }];
  const listed = new Set();
  let check = nextLocalCheck({ state: "checking", runner: null }, { kind: "slow" });
  assert.deepEqual(check, { state: "slow", runner: null });
  assert.equal(targetsReady({ devices, local: check, listed }), false);
  assert.equal(targetsSlow({ devices, local: check, listed }), true);
  assert.equal(planCommand({ ready: false, reuse: true, action: "new_tab", device: "mac", blankFree: true, paneGone: false }), "wait");
  check = nextLocalCheck(check, { kind: "slow" });
  assert.equal(check.state, "slow");
  check = nextLocalCheck(check, { kind: "answer", runner: "mac" });
  assert.deepEqual(check, { state: "known", runner: "mac" });
  assert.equal(targetsReady({ devices, local: check, listed }), true);
  assert.equal(targetsSlow({ devices, local: check, listed }), false);
  assert.equal(planCommand({ ready: true, reuse: true, action: "new_tab", device: "mac", blankFree: true, paneGone: false }), "open_blank");
});

test("fresh start: New terminal on this Mac waits for the targets, then opens in the blank terminal", () => {
  const at = { reuse: true, action: "new_tab", device: "mac", blankFree: true, paneGone: false };
  assert.equal(planCommand({ ...at, ready: false }), "wait");
  assert.equal(planCommand({ ...at, ready: true }), "open_blank");
  assert.equal(planCommand({ ...at, ready: true, blankFree: false }), "place");
  assert.equal(planCommand({ ...at, device: null, ready: false }), "wait");
  assert.equal(planCommand({ ...at, device: null, ready: true }), "focus");
  assert.equal(planCommand({ ...at, device: null, ready: true, blankFree: false }), "place");
});

test("saved layout: a new terminal waits for the targets, then gets its own tab under the device limit", () => {
  const at = { reuse: true, device: "mac", blankFree: false, paneGone: false };
  for (const action of ["new_tab", "split_right", "split_down"]) {
    assert.equal(planCommand({ ...at, action, ready: false }), "wait", action);
    assert.equal(planCommand({ ...at, action, ready: true }), "place", action);
  }
});

test("panel already open: commands run at once, an empty picker terminal is reused and focus never waits", () => {
  assert.equal(planCommand({ ready: true, reuse: true, action: "new_tab", device: "mac", blankFree: false, paneGone: false }), "place");
  assert.equal(planCommand({ ready: true, reuse: true, action: "new_tab", device: "cloud", blankFree: true, paneGone: false }), "open_blank");
  assert.equal(planCommand({ ready: true, reuse: true, action: "split_right", device: "mac", blankFree: true, paneGone: false }), "place");
  assert.equal(planCommand({ ready: true, reuse: false, action: "new_tab", device: null, blankFree: true, paneGone: false }), "place");
  assert.equal(planCommand({ ready: false, reuse: true, action: "focus", device: null, blankFree: true, paneGone: false }), "focus");
  assert.equal(planCommand({ ready: true, reuse: true, action: "focus", device: null, blankFree: false, paneGone: false }), "focus");
});

test("a split pressed while loading is dropped when its terminal was closed meanwhile, a new tab still opens", () => {
  const at = { ready: true, reuse: false, device: null, blankFree: false, paneGone: true };
  assert.equal(planCommand({ ...at, action: "split_right" }), "drop");
  assert.equal(planCommand({ ...at, action: "split_down" }), "drop");
  assert.equal(planCommand({ ...at, action: "new_tab" }), "place");
  assert.equal(planCommand({ ...at, ready: false, action: "split_right" }), "wait");
  assert.equal(planCommand({ ...at, paneGone: false, action: "split_right" }), "place");
});

test("only an idle terminal with no device and no running shell counts as an empty picker to reuse", () => {
  assert.equal(blankPane({ id: "p", device: null, native: null }, true), true);
  assert.equal(blankPane({ id: "p", device: "mac", native: null }, true), false);
  assert.equal(blankPane({ id: "p", device: null, native: "n1" }, true), false);
  assert.equal(blankPane({ id: "p", device: null, native: null }, false), false);
  assert.equal(blankPane(null, true), false);
});

function fakeClock() {
  let now = 0;
  let next = 1;
  const timers = new Map();
  const flush = () => new Promise((resolve) => setImmediate(resolve));
  return {
    timers: {
      set: (run, ms) => {
        const id = next++;
        timers.set(id, { at: now + ms, run });
        return id;
      },
      clear: (id) => timers.delete(id),
    },
    pending: () => timers.size,
    async advance(ms) {
      const end = now + ms;
      for (;;) {
        await flush();
        const due = [...timers.entries()].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        timers.delete(due[0]);
        now = due[1].at;
        due[1].run();
      }
      now = end;
      await flush();
    },
  };
}

function controlledAsk() {
  const calls = [];
  const ask = () =>
    new Promise((resolve, reject) => {
      calls.push({ resolve, reject });
    });
  return { ask, calls };
}

test("this Mac answers in time: one check, never slow, nothing left running", async () => {
  const clock = fakeClock();
  const { ask, calls } = controlledAsk();
  const seen = [];
  checkUntilAnswered(ask, { answer: (v) => seen.push(["answer", v]), slow: () => seen.push(["slow"]) }, clock.timers);
  await clock.advance(0);
  calls[0].resolve("mac");
  await clock.advance(60_000);
  assert.deepEqual(seen, [["answer", "mac"]]);
  assert.equal(calls.length, 1);
  assert.equal(clock.pending(), 0);
});

test("a check that never returns counts as failed at its time limit and is asked again with backoff", async () => {
  const clock = fakeClock();
  const { ask, calls } = controlledAsk();
  const seen = [];
  checkUntilAnswered(ask, { answer: (v) => seen.push(["answer", v]), slow: () => seen.push(["slow"]) }, clock.timers);
  await clock.advance(LOCAL_CHECK_MS - 1);
  assert.deepEqual(seen, []);
  await clock.advance(1);
  assert.deepEqual(seen, [["slow"]]);
  assert.equal(calls.length, 1);
  await clock.advance(localRetryIn(1));
  assert.equal(calls.length, 2);
  await clock.advance(LOCAL_CHECK_MS + localRetryIn(2));
  assert.equal(calls.length, 3);
  calls[2].resolve("mac");
  await clock.advance(120_000);
  assert.deepEqual(seen.at(-1), ["answer", "mac"]);
  assert.equal(seen.filter(([kind]) => kind === "answer").length, 1);
  assert.equal(calls.length, 3);
  assert.equal(clock.pending(), 0);
});

test("a hung check that answers late still counts, and stops the retries", async () => {
  const clock = fakeClock();
  const { ask, calls } = controlledAsk();
  const seen = [];
  checkUntilAnswered(ask, { answer: (v) => seen.push(["answer", v]), slow: () => seen.push(["slow"]) }, clock.timers);
  await clock.advance(LOCAL_CHECK_MS + localRetryIn(1));
  assert.equal(calls.length, 2);
  calls[0].resolve("late");
  await clock.advance(0);
  calls[1].resolve("second");
  calls[0].reject(new Error("ignored"));
  await clock.advance(120_000);
  assert.deepEqual(
    seen.filter(([kind]) => kind === "answer"),
    [["answer", "late"]],
  );
  assert.equal(calls.length, 2);
  assert.equal(clock.pending(), 0);
});

test("failing checks retry with growing waits up to the cap, and closing the terminal stops them", async () => {
  assert.deepEqual([1, 2, 3, 4, 5, 6, 9].map(localRetryIn), [2000, 4000, 8000, 16000, 30000, 30000, 30000]);
  const clock = fakeClock();
  const { ask, calls } = controlledAsk();
  const seen = [];
  const stop = checkUntilAnswered(ask, { answer: (v) => seen.push(["answer", v]), slow: () => seen.push(["slow"]) }, clock.timers);
  await clock.advance(0);
  calls[0].reject(new Error("down"));
  await clock.advance(0);
  assert.deepEqual(seen, [["slow"]]);
  await clock.advance(localRetryIn(1) - 1);
  assert.equal(calls.length, 1);
  await clock.advance(1);
  assert.equal(calls.length, 2);
  stop();
  calls[1].resolve("too late");
  await clock.advance(120_000);
  assert.equal(calls.length, 2);
  assert.equal(seen.some(([kind]) => kind === "answer"), false);
  assert.equal(clock.pending(), 0);
});

test("pressing the same action again while waiting adds nothing, different actions each wait once", () => {
  let queue = [];
  queue = enqueue(queue, { id: 1, action: "new_tab", inside: true });
  const same = enqueue(queue, { id: 2, action: "new_tab", inside: true });
  assert.equal(same, queue);
  queue = enqueue(queue, { id: 3, action: "split_right", inside: true });
  queue = enqueue(queue, { id: 4, action: "split_right", inside: true });
  queue = enqueue(queue, { id: 5, action: "new_tab", device: "cloud" });
  queue = enqueue(queue, { id: 6, action: "new_tab", device: "cloud" });
  queue = enqueue(queue, { id: 7, action: "new_tab", device: "mac" });
  assert.deepEqual(
    queue.map((q) => q.id),
    [1, 3, 5, 7],
  );
});

test("queued actions show one waiting note that names this Mac while it is checked, and none once ready", () => {
  const devices = [{ id: "mac" }];
  const listed = new Set();
  const slow = { state: "slow", runner: null };
  const checking = { state: "checking", runner: null };
  const known = { state: "known", runner: "mac" };
  assert.equal(waitingNote({ devices, local: slow, listed }, 0), null);
  assert.equal(waitingNote({ devices, local: slow, listed }, 1), "Waiting for this Mac. Your new terminal opens as soon as it answers.");
  assert.equal(waitingNote({ devices, local: checking, listed }, 2), "Waiting for this Mac. 2 new terminals open as soon as it answers.");
  assert.equal(waitingNote({ devices: null, local: known, listed }, 1), "Looking for your devices. Your new terminal opens as soon as they are found.");
  assert.equal(waitingNote({ devices, local: known, listed }, 3), null);
});

test("saved terminals while this Mac is slow: only this Mac's own shells wait, every other saved terminal offers Reconnect", () => {
  const online = new Set(["mac", "cloud"]);
  const macShell = { id: "a", device: "mac", native: "n1" };
  const remote = { id: "b", device: "cloud", native: null };
  const offline = { id: "c", device: "gone", native: null };
  const blank = { id: "d", device: null, native: null };
  const slow = { ready: false, online, localRunner: null };
  assert.equal(planRestored({ ...slow, pane: macShell, focused: false }), "wait");
  assert.equal(planRestored({ ...slow, pane: remote, focused: true }), "manual");
  assert.equal(planRestored({ ...slow, pane: remote, focused: false }), "manual");
  assert.equal(planRestored({ ...slow, pane: offline, focused: true }), "manual");
  assert.equal(planRestored({ ...slow, pane: blank, focused: true }), "manual");
  const ready = { ready: true, online, localRunner: "mac" };
  assert.equal(planRestored({ ...ready, pane: macShell, focused: false }), "reattach");
  assert.equal(planRestored({ ...ready, pane: remote, focused: true }), "open");
  assert.equal(planRestored({ ...ready, pane: remote, focused: false }), "manual");
  assert.equal(planRestored({ ...ready, pane: macShell, focused: true, localRunner: null }), "open");
  assert.equal(planRestored({ ...ready, pane: macShell, focused: false, localRunner: null }), "manual");
  for (const pane of [macShell, remote, offline, blank]) for (const focused of [true, false]) assert.notEqual(planRestored({ ...ready, pane, focused }), "wait");
});

test("a focused terminal on another device keeps Reconnect while this Mac is slow or never answers", () => {
  const online = new Set(["mac", "cloud"]);
  const remote = { id: "b", device: "cloud", native: null };
  assert.equal(planRestored({ ready: false, online, localRunner: null, pane: remote, focused: true }), "manual");
  assert.equal(planRestored({ ready: true, online, localRunner: "mac", pane: remote, focused: true }), "open");
});

test("the waiting note only counts queued actions that will really open a terminal", () => {
  const panes = new Set(["p1"]);
  const cmdkPlain = { id: 1, action: "new_tab" };
  const cmdkMac = { id: 2, action: "new_tab", device: "mac" };
  const plus = { id: 3, action: "new_tab", inside: true };
  const split = { id: 4, action: "split_right", inside: true, pane: "gone" };
  assert.equal(queuedOpens([cmdkPlain], { blankFree: true, panes }), 0);
  assert.equal(queuedOpens([cmdkPlain], { blankFree: false, panes }), 1);
  assert.equal(queuedOpens([cmdkMac], { blankFree: true, panes }), 1);
  assert.equal(queuedOpens([plus, split], { blankFree: true, panes }), 1);
  assert.equal(queuedOpens([cmdkPlain, cmdkMac, plus], { blankFree: true, panes }), 2);
  const at = { devices: [{ id: "mac" }], local: { state: "slow", runner: null }, listed: new Set() };
  assert.equal(waitingNote(at, queuedOpens([cmdkPlain], { blankFree: true, panes })), null);
  assert.equal(waitingNote(at, queuedOpens([cmdkPlain, cmdkMac], { blankFree: true, panes })), "Waiting for this Mac. Your new terminal opens as soon as it answers.");
});
