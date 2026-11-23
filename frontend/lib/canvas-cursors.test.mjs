import { test } from "node:test";
import assert from "node:assert/strict";
import {
  activityLabel,
  cursorActivity,
  cursorTone,
  CURSOR_TONES,
  dropGone,
  DROP_MS,
  GLIDE_MS,
  gliding,
  itemName,
  IDLE_MS,
  layoutLabels,
  moveCursor,
  nextPhaseIn,
  onScreen,
  phaseAt,
  positionAt,
  PENDING_MS,
  PERSON_GLIDE_MS,
  PRESENCE_EVENT,
  PRESENCE_GAP_MS,
  PRESENCE_MOVE_MIN,
  dropCursor,
  personPresence,
  presenceDue,
  readActivity,
  readyToPlace,
  shortName,
  targetPoint,
  TRAIL_MAX,
} from "./canvas-cursors.ts";

const REPORT = "r1";
const event = (type, payload, agent = "a1") => ({ type, agent_id: agent, payload: { report_id: REPORT, ...payload } });

test("only agent edits and pin answers on this canvas become cursor moves", () => {
  assert.deepEqual(cursorActivity(event("report.updated", { activity: { verb: "add", items: ["n1"] } }), REPORT), { actor: "a1", activity: { verb: "add", items: ["n1"] } });
  assert.equal(cursorActivity(event("report.updated", { activity: { verb: "add", items: ["n1"] } }, null), REPORT), null);
  assert.equal(cursorActivity(event("report.updated", { activity: { verb: "add", items: ["n1"] } }), "other"), null);
  assert.equal(cursorActivity(event("report.updated", {}), REPORT), null);
  assert.deepEqual(cursorActivity(event("canvas.mark_resolved", { item_id: "n2" }), REPORT)?.activity, { verb: "resolve", items: ["n2"] });
  assert.equal(cursorActivity(event("canvas.mark_resolved", { item_id: null }), REPORT), null);
  assert.equal(cursorActivity(event("canvas.mark", { item_id: "n2" }), REPORT), null);
});

test("activity payloads are checked before use", () => {
  assert.equal(readActivity(null), null);
  assert.equal(readActivity({ verb: "dance", items: ["a"] }), null);
  assert.equal(readActivity({ verb: "add", items: [] }), null);
  assert.equal(readActivity({ verb: "add", items: "a" }), null);
  assert.deepEqual(readActivity({ verb: "move", items: ["a", 3, "", "x".repeat(41), "b"] }), { verb: "move", items: ["a", "b"] });
  assert.equal(readActivity({ verb: "add", items: Array.from({ length: 20 }, (_, i) => `n${i}`) }).items.length, 8);
});

test("a cursor points into the touched item, or between the two ends of an arrow", () => {
  const boxes = new Map([
    ["a", { x: 0, y: 0, w: 100, h: 100 }],
    ["b", { x: 300, y: 0, w: 100, h: 100 }],
    ["tall", { x: 0, y: 0, w: 100, h: 1000 }],
  ]);
  const boxOf = (id) => boxes.get(id);
  assert.deepEqual(targetPoint({ verb: "edit", items: ["a"] }, boxOf), { x: 62, y: 40 });
  assert.deepEqual(targetPoint({ verb: "connect", items: ["a", "b"] }, boxOf), { x: 200, y: 50 });
  assert.deepEqual(targetPoint({ verb: "edit", items: ["tall"] }, boxOf), { x: 62, y: 120 });
  assert.deepEqual(targetPoint({ verb: "add", items: ["ghost", "b"] }, boxOf), { x: 362, y: 40 });
  assert.equal(targetPoint({ verb: "add", items: ["ghost"] }, boxOf), null);
});

test("labels say what the agent is doing in plain words", () => {
  const names = new Map([
    ["cart", "Cart"],
    ["pay", "Payment screen"],
  ]);
  const nameOf = (id) => names.get(id) ?? null;
  assert.equal(activityLabel({ verb: "add", items: ["cart"] }, nameOf), "adding Cart");
  assert.equal(activityLabel({ verb: "add", items: ["cart", "pay", "x"] }, nameOf), "adding 3 items");
  assert.equal(activityLabel({ verb: "connect", items: ["cart", "pay"] }, nameOf), "connecting Cart to Payment screen");
  assert.equal(activityLabel({ verb: "disconnect", items: ["cart", "pay"] }, nameOf), "unlinking Cart and Payment screen");
  assert.equal(activityLabel({ verb: "arrange", items: ["cart", "pay"] }, nameOf), "arranging 2 items");
  assert.equal(activityLabel({ verb: "move", items: ["ghost"] }, nameOf), "moving an item");
  assert.equal(activityLabel({ verb: "resize", items: ["cart"] }, nameOf), "resizing Cart");
  assert.equal(activityLabel({ verb: "resolve", items: ["pay"] }, nameOf), "answering a pin on Payment screen");
  assert.equal(shortName("A very long caption that goes on and on and on"), "A very long caption that go…");
  assert.equal(shortName("  two   words "), "two words");
});

test("the newest move wins, glides from where the cursor is and keeps at most three trail points", () => {
  let map = moveCursor(new Map(), "a1", { x: 0, y: 0 }, "adding Cart", 0);
  assert.deepEqual(positionAt(map.get("a1"), 0), { x: 0, y: 0 });
  map = moveCursor(map, "a1", { x: 100, y: 0 }, "editing Cart", 1000);
  const c = map.get("a1");
  assert.equal(c.label, "editing Cart");
  assert.ok(gliding(c, 1000 + GLIDE_MS / 2));
  assert.ok(!gliding(c, 1000 + GLIDE_MS));
  const mid = positionAt(c, 1000 + GLIDE_MS / 2);
  assert.ok(mid.x > 50 && mid.x < 100);
  map = moveCursor(map, "a1", { x: 200, y: 0 }, "moving Cart", 1000 + GLIDE_MS / 2);
  assert.ok(Math.abs(map.get("a1").from.x - mid.x) < 0.001);
  for (let i = 3; i < 10; i += 1) map = moveCursor(map, "a1", { x: i * 100, y: 0 }, "moving", 2000 + i * 1000);
  assert.equal(map.get("a1").trail.length, TRAIL_MAX);
  assert.deepEqual(map.get("a1").trail.map((p) => p.x), [600, 700, 800]);
  map = moveCursor(map, "a1", { x: 900, y: 0 }, "editing", 20000);
  assert.deepEqual(map.get("a1").trail.map((p) => p.x), [600, 700, 800]);
  map = moveCursor(map, "a1", { x: 1000, y: 0 }, "editing", 21000);
  assert.deepEqual(map.get("a1").trail.map((p) => p.x), [700, 800, 900]);
});

test("with reduced motion a cursor jumps and leaves no trail", () => {
  let map = moveCursor(new Map(), "a1", { x: 0, y: 0 }, "adding", 0, true);
  map = moveCursor(map, "a1", { x: 100, y: 0 }, "editing", 10, true);
  const c = map.get("a1");
  assert.deepEqual(positionAt(c, 10, true), { x: 100, y: 0 });
  assert.ok(!gliding(c, 10, true));
  assert.deepEqual(c.trail, []);
});

test("idle cursors fade, then go, and say when to check again", () => {
  const map = moveCursor(new Map(), "a1", { x: 0, y: 0 }, "adding", 0);
  const c = map.get("a1");
  assert.equal(phaseAt(c, IDLE_MS - 1), "live");
  assert.equal(phaseAt(c, IDLE_MS), "idle");
  assert.equal(phaseAt(c, DROP_MS), "gone");
  assert.equal(nextPhaseIn(c, 1000), IDLE_MS - 1000);
  assert.equal(nextPhaseIn(c, IDLE_MS + 1000), DROP_MS - IDLE_MS - 1000);
  assert.equal(nextPhaseIn(c, DROP_MS), null);
  assert.equal(dropGone(map, DROP_MS - 1), null);
  assert.equal(dropGone(map, DROP_MS).size, 0);
});

test("labels that would overlap stack, and labels near the right edge flip", () => {
  const label = { w: 200, h: 30 };
  const spots = layoutLabels(
    [
      { id: "a", x: 100, y: 100 },
      { id: "b", x: 120, y: 110 },
      { id: "c", x: 130, y: 105 },
      { id: "far", x: 600, y: 100 },
      { id: "edge", x: 950, y: 400 },
    ],
    1000,
    label,
  );
  assert.deepEqual(spots.get("a"), { stack: 0, flip: false });
  assert.deepEqual(spots.get("c"), { stack: 1, flip: false });
  assert.deepEqual(spots.get("b"), { stack: 2, flip: false });
  assert.deepEqual(spots.get("far"), { stack: 0, flip: false });
  assert.deepEqual(spots.get("edge"), { stack: 0, flip: true });
  const five = layoutLabels(Array.from({ length: 5 }, (_, i) => ({ id: `n${i}`, x: 300, y: 300 })), 1000, label);
  assert.deepEqual([...five.values()].map((s) => s.stack).sort(), [0, 1, 2, 3, 4]);
});

test("tones are stable per agent and in range, and off-screen cursors are culled", () => {
  assert.equal(cursorTone("agent-1"), cursorTone("agent-1"));
  for (const id of ["a", "b", "c", "d", "e", "f"]) assert.ok(cursorTone(id) >= 1 && cursorTone(id) <= CURSOR_TONES);
  assert.ok(onScreen(10, 10, 100, 100));
  assert.ok(onScreen(-40, 140, 100, 100));
  assert.ok(!onScreen(-60, 10, 100, 100));
  assert.ok(!onScreen(10, 200, 100, 100));
});

test("moves wait for the refreshed canvas so the cursor lands where the item ends up", () => {
  for (const verb of ["add", "move", "resize", "arrange"]) {
    assert.ok(!readyToPlace(verb, 3, 3, 100), `${verb} waits for new items`);
    assert.ok(readyToPlace(verb, 3, 4, 100), `${verb} goes once the canvas refreshed`);
    assert.ok(readyToPlace(verb, 3, 3, PENDING_MS), `${verb} falls back after a while`);
  }
  for (const verb of ["edit", "remove", "connect", "disconnect", "resolve"]) assert.ok(readyToPlace(verb, 3, 3, 0), `${verb} places at once`);
  assert.deepEqual(readActivity({ verb: "resize", items: ["a"] }), { verb: "resize", items: ["a"] });
});

test("events replayed after a reconnect never move a cursor", () => {
  const replayed = { ...event("report.updated", { activity: { verb: "add", items: ["n1"] } }), replayed: true };
  assert.equal(cursorActivity(replayed, REPORT), null);
  assert.equal(personPresence({ type: PRESENCE_EVENT, agent_id: null, replayed: true, payload: { report_id: REPORT, user_id: "u2", x: 1, y: 2 } }, REPORT, "me"), null);
});

test("people on the same canvas show as cursors, never yourself", () => {
  const seen = (payload) => personPresence({ type: PRESENCE_EVENT, agent_id: null, payload: { report_id: REPORT, ...payload } }, REPORT, "me");
  assert.deepEqual(seen({ user_id: "u2", name: "Mei Chen", x: 10, y: 20 }), { actor: "person:u2", userId: "u2", name: "Mei Chen", point: { x: 10, y: 20 } });
  assert.deepEqual(seen({ user_id: "u2", name: "Mei Chen", leaving: true }).point, null);
  assert.equal(seen({ user_id: "me", name: "Me", x: 1, y: 1 }), null);
  assert.equal(seen({ user_id: "", x: 1, y: 1 }), null);
  assert.equal(seen({ user_id: "u2", name: " ", x: 1, y: 1 }).name, "Someone");
  assert.equal(personPresence({ type: PRESENCE_EVENT, agent_id: null, payload: { report_id: "other", user_id: "u2", x: 1, y: 1 } }, REPORT, "me"), null);
});

test("your own presence goes out only when it moved enough or changed item, at most every 150 ms", () => {
  assert.ok(presenceDue(null, { x: 0, y: 0, item: null }, 0));
  const last = { x: 0, y: 0, item: null, at: 1000 };
  assert.ok(!presenceDue(last, { x: 500, y: 0, item: null }, 1000 + PRESENCE_GAP_MS - 1));
  assert.ok(!presenceDue(last, { x: PRESENCE_MOVE_MIN - 1, y: 0, item: null }, 2000));
  assert.ok(presenceDue(last, { x: PRESENCE_MOVE_MIN, y: 0, item: null }, 2000));
  assert.ok(presenceDue(last, { x: 0, y: 0, item: "n1" }, 2000));
});

test("people's cursors glide quickly between updates and leave no trail", () => {
  let map = moveCursor(new Map(), "person:u2", { x: 0, y: 0 }, "", 0, false, PERSON_GLIDE_MS);
  map = moveCursor(map, "person:u2", { x: 100, y: 0 }, "", 1000, false, PERSON_GLIDE_MS);
  assert.ok(gliding(map.get("person:u2"), 1000 + PERSON_GLIDE_MS - 1));
  assert.ok(!gliding(map.get("person:u2"), 1000 + PERSON_GLIDE_MS));
  assert.deepEqual(map.get("person:u2").trail, []);
  assert.equal(dropCursor(map, "person:u2").size, 0);
  assert.equal(dropCursor(map, "person:nobody"), null);
});

test("cursor labels name items in plain words, never markdown or raw refs", () => {
  assert.equal(itemName({ kind: "note", text: "**Payment**\n\nCard and Apple Pay" }, "Note"), "Payment");
  assert.equal(itemName({ kind: "note", caption: " Checkout ", text: "**x**" }, "Note"), "Checkout");
  assert.equal(itemName({ kind: "ref", text: "quest:feed1234" }, "Quest"), "Quest");
  assert.equal(itemName({ kind: "note", text: "## [[Spring plan]] and [docs](https://x.y)" }, "Note"), "Spring plan and docs");
  assert.equal(itemName({ kind: "image" }, "Image"), "Image");
  assert.equal(itemName({ kind: "run", command: "npm test" }, "Step"), "npm test");
});
