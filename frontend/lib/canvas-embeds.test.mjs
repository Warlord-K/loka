import { test } from "node:test";
import assert from "node:assert/strict";
import {
  EMBEDS,
  bodyMode,
  embedSlots,
  orphanKeys,
  screenScale,
  stagePlan,
  startsShellBySelf,
  PLACEABLE_EMBEDS,
  currentStep,
  embedBox,
  embedDragData,
  embedItem,
  embedLive,
  nextRun,
  parseTerminalTarget,
  pushTail,
  runProgress,
  readEmbed,
  readEmbedDrag,
  terminalTarget,
} from "./canvas-embeds.ts";

const DEVICE = "6f1c2a7e-0b4d-4c55-9a1e-2f3b4c5d6e7f";

test("the registry knows every kind, but only live-watchable ones can be placed", () => {
  assert.deepEqual(PLACEABLE_EMBEDS, ["terminal", "quest"]);
  assert.equal(EMBEDS.browser.placeable, false);
  assert.deepEqual(readEmbed({ kind: "quest", target: " abc12345 " }), { kind: "quest", target: "abc12345" });
  assert.deepEqual(readEmbed({ kind: "browser", target: "x" }), { kind: "browser", target: "x" });
  assert.equal(readEmbed({ kind: "sheet", target: "x" }), null);
  assert.equal(readEmbed({ kind: "quest", target: "" }), null);
  assert.equal(readEmbed(null), null);
});

test("terminal targets name a device and optionally one shell on it", () => {
  assert.equal(terminalTarget(DEVICE), DEVICE);
  assert.equal(terminalTarget(DEVICE, "0b7e-1"), `${DEVICE}/0b7e-1`);
  assert.deepEqual(parseTerminalTarget(`${DEVICE}/0b7e-1`), { device: DEVICE, shell: "0b7e-1" });
  assert.deepEqual(parseTerminalTarget(DEVICE), { device: DEVICE, shell: null });
  assert.equal(parseTerminalTarget("my-mac"), null);
  assert.equal(parseTerminalTarget(`${DEVICE}/a b`), null);
  assert.equal(parseTerminalTarget(`${DEVICE}/a/b`), null);
});

test("an embed streams only while it is mounted, on screen and the page is visible", () => {
  for (const mounted of [true, false])
    for (const onScreen of [true, false])
      for (const pageVisible of [true, false]) assert.equal(embedLive({ mounted, onScreen, pageVisible }), mounted && onScreen && pageVisible);
});

test("each return to the screen starts a new round so views load the newest state", () => {
  let run = { live: false, round: 0 };
  run = nextRun(run, true);
  assert.deepEqual(run, { live: true, round: 1 });
  assert.equal(nextRun(run, true), run);
  run = nextRun(run, false);
  assert.deepEqual(run, { live: false, round: 1 });
  run = nextRun(run, false);
  run = nextRun(run, true);
  assert.deepEqual(run, { live: true, round: 2 });
});

test("placing centres the default size on the drop point and spreads several side by side", () => {
  assert.deepEqual(embedBox("quest", { x: 0, y: 0 }), { x: -190, y: -150, w: 380, h: 300 });
  assert.deepEqual(embedBox("terminal", { x: 100, y: 100 }, 1), { x: 100 - 280 + 592, y: 100 - 170, w: 560, h: 340 });
  const item = embedItem({ embed: { kind: "quest", target: "abc12345" }, caption: "Ship it" }, { x: 0, y: 0 });
  assert.deepEqual(item, { kind: "embed", embed: { kind: "quest", target: "abc12345" }, caption: "Ship it", x: -190, y: -150, w: 380, h: 300 });
});

test("drag data round trips and refuses kinds that cannot be placed", () => {
  const data = embedDragData({ kind: "terminal", target: DEVICE }, "Build shell");
  assert.deepEqual(readEmbedDrag(data), { embed: { kind: "terminal", target: DEVICE }, caption: "Build shell" });
  assert.deepEqual(readEmbedDrag(embedDragData({ kind: "quest", target: "abc12345" })), { embed: { kind: "quest", target: "abc12345" } });
  assert.equal(readEmbedDrag(embedDragData({ kind: "browser", target: "x" })), null);
  assert.equal(readEmbedDrag("not json"), null);
  assert.equal(readEmbedDrag(""), null);
});

test("the current step is the active milestone, else the first one left", () => {
  const steps = [
    { title: "B", status: "pending", position: 2 },
    { title: "A", status: "done", position: 1 },
    { title: "C", status: "pending", position: 3 },
  ];
  assert.equal(currentStep(steps)?.title, "B");
  assert.equal(currentStep([...steps, { title: "D", status: "active", position: 4 }])?.title, "D");
  assert.equal(currentStep([{ title: "A", status: "done", position: 1 }]), null);
});

test("the log tail keeps the newest few lines and skips repeats", () => {
  let tail = [];
  for (const line of ["one", "two", "two", "  three  ", "four", "five"]) tail = pushTail(tail, line);
  assert.deepEqual(tail, ["two", "three", "four", "five"]);
  assert.equal(pushTail(tail, "   "), tail);
});

test("pausing on scroll or tab hide keeps a terminal's session and only remounts views that reload cheaply", () => {
  const scrolledAway = embedLive({ mounted: false, onScreen: false, pageVisible: true });
  const tabHidden = embedLive({ mounted: true, onScreen: true, pageVisible: false });
  const back = embedLive({ mounted: true, onScreen: true, pageVisible: true });
  for (const live of [scrolledAway, tabHidden, back]) assert.equal(bodyMode("terminal", live), "kept");
  assert.equal(bodyMode("quest", scrolledAway), "paused");
  assert.equal(bodyMode("quest", tabHidden), "paused");
  assert.equal(bodyMode("quest", back), "shown");
  assert.ok(Object.values(EMBEDS).some((spec) => spec.keeps));
});

test("the stage keeps started terminals while they are on the canvas and lets go of removed ones", () => {
  const present = [
    { key: "r:t1", kind: "terminal" },
    { key: "r:q1", kind: "quest" },
    { key: "r:t3", kind: "terminal" },
  ];
  assert.deepEqual(stagePlan(new Set(["r:t1", "r:t2"]), present), { keep: ["r:t1"], gone: ["r:t2"] });
  assert.deepEqual(stagePlan(new Set(), present), { keep: [], gone: [] });
  assert.deepEqual(stagePlan(new Set(["r:q1"]), present), { keep: [], gone: ["r:q1"] });
});

test("only terminals a person placed start a shell on their own", () => {
  assert.equal(startsShellBySelf("person"), true);
  assert.equal(startsShellBySelf("agent"), false);
  assert.equal(startsShellBySelf(undefined), false);
});

test("saved terminals of items no longer on the canvas are found for cleanup", () => {
  const prefix = "world.terminals.embed.r:";
  const stored = [`${prefix}t1`, `${prefix}gone`, "world.terminals.embed.other:t1", "world.terminals.u1"];
  assert.deepEqual(orphanKeys(stored, prefix, new Set(["t1"])), [`${prefix}gone`]);
  assert.deepEqual(orphanKeys(stored, prefix, new Set(["t1", "gone"])), []);
});

test("terminals draw at screen scale within sane bounds", () => {
  assert.equal(screenScale(1), 1);
  assert.equal(screenScale(0.5), 0.5);
  assert.equal(screenScale(2), 2);
  assert.equal(screenScale(0.05), 0.25);
  assert.equal(screenScale(9), 4);
  assert.equal(screenScale(Number.NaN), 1);
  assert.equal(screenScale(0), 1);
});

test("a quest card only takes progress lines from its own run, even when one agent runs two quests", () => {
  const mine = { type: "agent.progress", agent_id: "a1", run_id: "run-1", payload: { text: "Building", run_kind: "task" } };
  const other = { type: "agent.progress", agent_id: "a1", run_id: "run-2", payload: { text: "Other quest", run_kind: "task" } };
  assert.equal(runProgress(mine, "run-1"), "Building");
  assert.equal(runProgress(other, "run-1"), null);
  assert.equal(runProgress(mine, null), null);
  assert.equal(runProgress({ ...mine, run_id: null, payload: { text: "From payload", run_id: "run-1" } }, "run-1"), "From payload");
  assert.equal(runProgress({ ...mine, type: "run.activity" }, "run-1"), null);
  assert.equal(runProgress({ ...mine, payload: { text: "  " } }, "run-1"), null);
});

class FakeDocument {
  constructor() {
    this.active = null;
  }
  holds(node) {
    for (let at = this.active; at; at = at.parentElement) if (at === node) return true;
    return false;
  }
}

class FakeElement {
  constructor(doc, name) {
    this.doc = doc;
    this.name = name;
    this.parentElement = null;
    this.children = [];
  }
  detach() {
    if (!this.parentElement) return;
    if (this.doc.holds(this)) this.doc.active = null;
    this.parentElement.children = this.parentElement.children.filter((c) => c !== this);
    this.parentElement = null;
  }
  appendChild(node) {
    node.detach();
    node.parentElement = this;
    this.children.push(node);
    return node;
  }
  remove() {
    this.detach();
  }
  focus() {
    this.doc.active = this;
  }
}

function effectRunner() {
  let deps = null;
  let cleanup = null;
  return (next, effect) => {
    if (deps && next.length === deps.length && next.every((d, i) => Object.is(d, deps[i]))) return;
    cleanup?.();
    deps = next;
    cleanup = effect() ?? null;
  };
}

function mountedTerminal() {
  const doc = new FakeDocument();
  const holder = new FakeElement(doc, "holder");
  const slot = new FakeElement(doc, "slot");
  const slots = embedSlots(() => new FakeElement(doc, "embed"), (made) => holder.appendChild(made));
  const terminal = new FakeElement(doc, "xterm");
  slots.node("r1:t1").appendChild(terminal);
  const run = effectRunner();
  const layout = (stage) => run([stage.slots, "r1:t1"], () => stage.slots.attach("r1:t1", slot));
  return { doc, holder, slot, slots, terminal, layout };
}

test("a terminal embed keeps typing focus through zoom in and zoom out", () => {
  const { doc, slot, slots, terminal, layout } = mountedTerminal();
  const start = () => {};
  let stage = { start, slots };
  layout(stage);
  assert.equal(slots.node("r1:t1").parentElement, slot);
  terminal.focus();
  for (const scale of [1.5, 2, 1, 0.5, 1]) {
    stage = { start, slots, fontScale: scale };
    layout(stage);
    assert.equal(doc.active, terminal, `focus kept at scale ${scale}`);
    assert.equal(slots.node("r1:t1").parentElement, slot);
  }
});

test("a stage that changed on every zoom moved the terminal out and back, which dropped focus", () => {
  const { doc, holder, slot, terminal } = mountedTerminal();
  const nodes = new Map();
  const legacyStage = (scale) => ({
    scale,
    node: (key) => nodes.get(key) ?? nodes.set(key, terminal.parentElement).get(key),
    park: (node) => holder.appendChild(node),
  });
  const run = effectRunner();
  const legacyLayout = (stage) =>
    run([stage, "r1:t1"], () => {
      const node = stage.node("r1:t1");
      slot.appendChild(node);
      return () => {
        if (node.parentElement === slot) stage.park(node);
      };
    });
  legacyLayout(legacyStage(1));
  terminal.focus();
  legacyLayout(legacyStage(1.5));
  assert.equal(doc.active, null);
});

test("slots reuse one node per key, park it on detach and drop it on removal", () => {
  const { holder, slot, slots } = mountedTerminal();
  const node = slots.node("r1:t1");
  assert.equal(slots.node("r1:t1"), node);
  assert.equal(node.parentElement, holder);
  const detach = slots.attach("r1:t1", slot);
  assert.equal(node.parentElement, slot);
  detach();
  assert.equal(node.parentElement, holder);
  const other = new FakeElement(holder.doc, "other");
  const again = slots.attach("r1:t1", slot);
  other.appendChild(node);
  again();
  assert.equal(node.parentElement, other, "a node already moved elsewhere is left where it is");
  slots.drop("r1:t1");
  assert.equal(node.parentElement, null);
  assert.notEqual(slots.node("r1:t1"), node);
  slots.clear();
  assert.equal(holder.children.length, 0);
});
