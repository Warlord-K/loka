import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  CLOSE_CHOICES,
  closeChoiceKey,
  closeHint,
  closeShortcutClosesArea,
  listOf,
  loadCloseChoice,
  paneRunning,
  planClose,
  requestClose,
  runningShellKind,
  runningWhere,
  savedNativeInventory,
  saveCloseChoice,
  settleRemoteOpen,
  shouldCloseStaleNative,
  nextPresence,
  presenceMounted,
} from "./terminal-close.ts";
import { clearClosedShellIds, runningTerminals, savedSummaries } from "./terminal-layout.ts";

function recorder() {
  const calls = [];
  return {
    calls,
    effects: {
      quiet: (source) => calls.push(["quiet", source]),
      minimize: () => calls.push(["minimize"]),
      closeAll: () => calls.push(["close_all"]),
      ask: (plan) => calls.push(["ask", plan.count, plan.where]),
    },
  };
}

function memoryStorage() {
  const map = new Map();
  return {
    map,
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => map.set(key, String(value)),
    removeItem: (key) => map.delete(key),
  };
}

test("nothing running closes quietly without asking, whatever was remembered", () => {
  for (const choice of ["ask", "minimize", "close_all"]) {
    for (const source of ["window", "dock", "escape", "outside", "last_terminal"]) {
      const { calls, effects } = recorder();
      const plan = requestClose({ source, running: [], choice }, effects);
      assert.equal(plan.outcome, "quiet");
      assert.deepEqual(calls, [["quiet", source]]);
    }
  }
});

test("ended, idle and remote reconnect panes do not count as running", () => {
  assert.equal(paneRunning({ phase: "ended", native: "n1", local: true }), false);
  assert.equal(paneRunning({ phase: "error", native: null, local: false }), false);
  assert.equal(paneRunning({ phase: "idle", native: null, local: false }), false);
  assert.equal(paneRunning({ phase: "idle", native: "n1", local: false }), false);
  assert.equal(paneRunning({ phase: "elsewhere", native: "n1", local: true }), false);
});

test("live, starting and saved shells on this Mac count as running", () => {
  assert.equal(paneRunning({ phase: "live", native: null, local: false }), true);
  assert.equal(paneRunning({ phase: "connecting", native: null, local: false }), true);
  assert.equal(paneRunning({ phase: "starting", native: null, local: true }), true);
  assert.equal(paneRunning({ phase: "idle", native: "n1", local: true }), true);
});

test("saved Mac inventory keeps matched live and exited ids but ignores foreign ones", () => {
  const inventory = savedNativeInventory(["n1", "n2", "gone"], [
    { id: "n1", exited: false },
    { id: "n2", exited: true },
    { id: "foreign", exited: false },
  ]);
  assert.deepEqual([...inventory], [["n1", false], ["n2", true]]);
  assert.equal(inventory.has("foreign"), false);
  assert.deepEqual([...inventory].filter(([, exited]) => !exited).map(([id]) => id), ["n1"]);
  assert.equal(planClose({ running: [], choice: "close_all" }).outcome, "quiet");
});

test("a pending native reattachment counts only when the Mac still has that shell", () => {
  const inventory = new Map([
    ["live", false],
    ["ended", true],
  ]);
  const free = () => false;
  assert.equal(runningShellKind(true, "live", "live", inventory, free), "saved");
  assert.equal(runningShellKind(true, "ended", "ended", inventory, free), null);
  assert.equal(runningShellKind(true, "missing", "missing", inventory, free), null);
  assert.equal(runningShellKind(true, null, null, inventory, free), "busy");
  assert.equal(runningShellKind(true, null, "live", inventory, free), "saved");
  assert.equal(runningShellKind(true, "live", "live", inventory, () => true), null);
});

test("an explicit close owns a pending native reattachment", () => {
  assert.equal(shouldCloseStaleNative("n1", "n1", false), false);
  assert.equal(shouldCloseStaleNative("n1", "n1", true), true);
  assert.equal(shouldCloseStaleNative("n1", "replacement", false), true);
  assert.equal(shouldCloseStaleNative(null, "new", false), true);
});

test("a delayed remote open is cancelled when its terminal was explicitly closed", async () => {
  let resolve;
  const opening = new Promise((done) => {
    resolve = done;
  });
  let current = true;
  const closed = [];
  const attached = [];
  const settled = settleRemoteOpen(opening, () => current, async (id) => closed.push(id), (opened) => attached.push(opened.session_id));
  current = false;
  resolve({ session_id: "stale", socket: "ws://stale" });
  await settled;
  assert.deepEqual(closed, ["stale"]);
  assert.deepEqual(attached, []);

  const live = { session_id: "live", socket: "ws://live" };
  await settleRemoteOpen(Promise.resolve(live), () => true, async (id) => closed.push(id), (opened) => attached.push(opened.session_id));
  assert.deepEqual(closed, ["stale"]);
  assert.deepEqual(attached, ["live"]);
});

test("the close shortcut asks for the whole terminal on its last tab even when split", () => {
  assert.equal(closeShortcutClosesArea(true, 1), true);
  assert.equal(closeShortcutClosesArea(true, 2), false);
  assert.equal(closeShortcutClosesArea(false, 1), false);
});

test("one running terminal asks and names its device", () => {
  const { calls, effects } = recorder();
  const plan = requestClose({ source: "window", running: [{ device: "this Mac" }], choice: "ask" }, effects);
  assert.equal(plan.outcome, "ask");
  assert.deepEqual(calls, [["ask", 1, "1 terminal on this Mac"]]);
});

test("several terminals across devices give the count and every device once", () => {
  const running = [{ device: "Studio Mac" }, { device: "this Mac" }, { device: "Studio Mac" }];
  assert.equal(runningWhere(running), "3 terminals on Studio Mac and this Mac");
  assert.equal(runningWhere([...running, { device: "Build box" }]), "4 terminals on Studio Mac, this Mac and Build box");
  const { calls, effects } = recorder();
  requestClose({ source: "dock", running, choice: "ask" }, effects);
  assert.deepEqual(calls, [["ask", 3, "3 terminals on Studio Mac and this Mac"]]);
});

test("a remembered choice is used without asking", () => {
  const running = [{ device: "this Mac" }, { device: "Studio Mac" }];
  const minimized = recorder();
  assert.equal(requestClose({ source: "window", running, choice: "minimize" }, minimized.effects).outcome, "minimize");
  assert.deepEqual(minimized.calls, [["minimize"]]);
  const closed = recorder();
  assert.equal(requestClose({ source: "escape", running, choice: "close_all" }, closed.effects).outcome, "close_all");
  assert.deepEqual(closed.calls, [["close_all"]]);
});

test("the docked panel and the full page go through the same decision", () => {
  const running = [{ device: "this Mac" }, { device: "Studio Mac" }];
  for (const choice of ["ask", "minimize", "close_all"]) {
    const outcomes = ["window", "dock", "escape", "outside", "last_terminal"].map((source) => {
      const { calls, effects } = recorder();
      requestClose({ source, running, choice }, effects);
      return calls;
    });
    for (const calls of outcomes) assert.deepEqual(calls, outcomes[0]);
  }
  assert.deepEqual(planClose({ running, choice: "ask" }), { outcome: "ask", count: 2, where: "2 terminals on this Mac and Studio Mac" });
});

test("the remembered choice is stored per person and survives bad values", () => {
  const storage = memoryStorage();
  const mine = closeChoiceKey("u1");
  assert.notEqual(mine, closeChoiceKey("u2"));
  assert.equal(loadCloseChoice(storage, mine), "ask");
  saveCloseChoice(storage, mine, "minimize");
  assert.equal(loadCloseChoice(storage, mine), "minimize");
  assert.equal(loadCloseChoice(storage, closeChoiceKey("u2")), "ask");
  saveCloseChoice(storage, mine, "close_all");
  assert.equal(loadCloseChoice(storage, mine), "close_all");
  saveCloseChoice(storage, mine, "ask");
  assert.equal(storage.map.has(mine), false);
  storage.setItem(mine, "explode");
  assert.equal(loadCloseChoice(storage, mine), "ask");
  assert.equal(loadCloseChoice({ getItem: () => { throw new Error("blocked"); } }, mine), "ask");
  assert.equal(loadCloseChoice(null, mine), "ask");
  assert.equal(mine.startsWith("world.terminals."), false);
});

test("the close button hint says what a remembered choice does", () => {
  assert.equal(closeHint("ask"), "Close the terminal");
  assert.match(closeHint("minimize"), /^Minimizes/);
  assert.match(closeHint("close_all"), /^Closes all terminals/);
  for (const choice of ["ask", "minimize", "close_all"]) assert.ok(closeHint(choice).length <= 64);
  assert.deepEqual(CLOSE_CHOICES.map((c) => c.value), ["ask", "minimize", "close_all"]);
});

test("lists read naturally and the running badge adds up every tab", () => {
  assert.equal(listOf([]), "");
  assert.equal(listOf(["A"]), "A");
  assert.equal(listOf(["A", "B"]), "A and B");
  assert.equal(listOf(["A", "B", "C"]), "A, B and C");
  assert.equal(runningTerminals([]), 0);
  assert.equal(
    runningTerminals([
      { id: "t1", label: "a", device: null, panes: 2, running: 2 },
      { id: "t2", label: "b", device: null, panes: 1, running: 0 },
      { id: "t3", label: "c", device: null, panes: 3, running: 1 },
    ]),
    3,
  );
});

test("hiding the terminal for another window keeps an open one alive and never mounts a closed one", () => {
  const states = ["closed", "open", "minimized"];
  const expect = {
    open: { closed: "open", open: "open", minimized: "open" },
    cover: { closed: "closed", open: "minimized", minimized: "minimized" },
    minimize: { closed: "closed", open: "minimized", minimized: "minimized" },
    close: { closed: "closed", open: "closed", minimized: "closed" },
  };
  for (const [event, after] of Object.entries(expect)) for (const at of states) assert.equal(nextPresence(at, event), after[at], `${at} + ${event}`);
  assert.equal(presenceMounted(nextPresence("closed", "cover")), false);
  assert.equal(presenceMounted(nextPresence(nextPresence("closed", "cover"), "cover")), false);
  assert.equal(presenceMounted(nextPresence("open", "cover")), true);
  assert.equal(presenceMounted("closed"), false);
});

test("the game moves the terminal only through the presence reducer and the panel's closer", () => {
  const game = readFileSync(new URL("../components/world/world-game.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(game, /setTerminalOpen|terminalKept/);
  assert.match(game, /useReducer\(nextPresence, "closed"\)/);
  assert.match(game, /\{presenceMounted\(terminalPresence\) &&/);
  assert.match(game, /closeTerminal: \(\) => \{\s*if \(terminalDockedRef\.current\) return;\s*moveTerminal\("cover"\);/);
  assert.match(game, /terminalFront\) terminalCloser\.current\?\.\("outside"\)/);
});

test("after a reload the saved layout and the Mac's live shells give the running count", () => {
  const layout = {
    v: 1,
    active: "t1",
    dock: "window",
    size: 0.38,
    panes: { p1: { id: "p1", device: "mac", native: "n1" }, p2: { id: "p2", device: "mac", native: "n2" }, p3: { id: "p3", device: "box", native: null } },
    tabs: [
      { id: "t1", name: null, focus: "p1", root: { kind: "split", dir: "row", ratio: 0.5, a: { kind: "pane", id: "p1" }, b: { kind: "pane", id: "p2" } } },
      { id: "t2", name: "build", focus: "p3", root: { kind: "pane", id: "p3" } },
    ],
  };
  const live = [{ id: "n1", exited: false }, { id: "n2", exited: true }, { id: "other", exited: false }];
  assert.deepEqual(savedSummaries(layout, live), [
    { id: "t1", label: "this Mac", device: "mac", panes: 2, running: 1 },
    { id: "t2", label: "build", device: "box", panes: 1, running: 0 },
  ]);
  assert.equal(runningTerminals(savedSummaries(layout, live)), 1);
  assert.deepEqual(savedSummaries(layout, [{ id: "n2", exited: true }]), []);
  assert.deepEqual(savedSummaries(null, live), []);
});

test("close all preserves layout and clears only confirmed closed ids across retries", () => {
  const layout = {
    v: 1,
    active: "t1",
    dock: "right",
    size: 0.42,
    panes: { p1: { id: "p1", device: "mac", native: "n1" }, p2: { id: "p2", device: "box", native: "n2" } },
    tabs: [{ id: "t1", name: "build", focus: "p2", root: { kind: "split", dir: "row", ratio: 0.37, a: { kind: "pane", id: "p1" }, b: { kind: "pane", id: "p2" } } }],
  };
  const first = clearClosedShellIds(
    layout,
    new Map([
      ["p1", { status: "failed" }],
      ["p2", { status: "protected" }],
    ]),
  );
  assert.equal(first, layout);
  assert.deepEqual(clearClosedShellIds(first, new Map([["p1", { status: "closed" }]])), {
    ...layout,
    panes: { p1: { id: "p1", device: "mac", native: null }, p2: layout.panes.p2 },
  });
});

test("the panel resolves native shells and routes last-tab keys through the shared closer", () => {
  const panel = readFileSync(new URL("../components/world/terminal.tsx", import.meta.url), "utf8");
  const shell = readFileSync(new URL("../components/world/terminal-shell.ts", import.meta.url), "utf8");
  assert.match(panel, /wb\.homeTermList\(\)/);
  assert.match(panel, /runningShellKind\(!!shell\?\.busy, pendingNative \?\? null, pane\.native, homeInventory, \(native\) => shellHeld\(native, shell\)\)/);
  assert.match(panel, /homeInventory\.has\(native\)/);
  assert.match(panel, /closeShortcutClosesArea\(asks, current\.tabs\.length\)\) requestCloseTerminals\("last_terminal"\)/);
  assert.match(panel, /await closePanes\(paneIds, homeInventory\)/);
  assert.match(panel, /terminalStartsAllowed\(devicesPanel, front, closingRef\.current\)/);
  assert.ok((panel.match(/terminalStartsAllowed\(devicesPanel, front, closingRef\.current\)/g) ?? []).length >= 4);
  assert.match(shell, /this\.nativeId \?\? this\.pendingNativeId/);
  assert.match(shell, /nativeShells\.close\(info\.id, this, wb\.termClose\)/);
  assert.match(shell, /settleRemoteOpen\(/);
  assert.match(shell, /this\.remoteSessionId && this\.socket\?\.readyState !== WebSocket\.OPEN/);
  assert.match(shell, /get pendingNative\(\)/);
});
