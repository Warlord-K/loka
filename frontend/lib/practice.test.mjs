import { test } from "node:test";
import assert from "node:assert/strict";
import {
  attemptFromItem,
  ATTEMPT_ITEM,
  checkOf,
  cleanSubject,
  EMPTY_INK,
  hasInk,
  inkDoc,
  inkReducer,
  isReadyWord,
  pointerChoice,
  coachLate,
  COACH_WAIT_MS,
  LIMITS,
  practiceOf,
  prepareInk,
  pressureOf,
  rgbHex,
  serdeBytes,
  thinPoints,
  TOO_FULL,
  strokeWidth,
  subjectLabel,
  toBoard,
} from "./practice.ts";

const stroke = (tool = "pen", n = 2) => ({ tool, color: "w-strong", size: 4, points: Array.from({ length: n }, (_, i) => ({ x: i, y: i, p: 0.5, t: i * 10 })) });

test("only a plain ready counts as the ready word", () => {
  for (const said of ["ready", "Ready.", "ready!", "I'm ready", "I’m ready.", "okay, ready", "ready now", "  READY  "]) assert.ok(isReadyWord(said), said);
  for (const said of ["not ready", "ready to go to the shop", "already", "", "is it ready?x", "ready ready"]) assert.ok(!isReadyWord(said), said);
});

test("subjects clean like the server and read as words", () => {
  assert.equal(cleanSubject("  Japanese Kanji "), "japanese-kanji");
  assert.equal(cleanSubject("figure_drawing!"), "figure-drawing");
  assert.equal(cleanSubject("???"), "");
  assert.equal(subjectLabel("figure-drawing"), "Figure Drawing");
});

test("practice and check payloads parse defensively", () => {
  assert.deepEqual(practiceOf({ subject: "japanese", coach_agent_id: "a1" }), { subject: "japanese", coach_agent_id: "a1" });
  assert.deepEqual(practiceOf({ subject: "drawing" }), { subject: "drawing", coach_agent_id: null });
  assert.equal(practiceOf(null), null);
  assert.equal(practiceOf({ subject: "" }), null);
  assert.deepEqual(checkOf({ right: "a", fix: "b", next: "c", score: 72 }), { right: "a", fix: "b", next: "c", score: 72 });
  assert.equal(checkOf({ right: "a", fix: "b" }), null);
  assert.equal(checkOf({ right: "a", fix: "b", next: "c" }).score, null);
});

test("undo, redo and clear walk the history", () => {
  let s = inkReducer(EMPTY_INK, { type: "add", stroke: stroke() });
  s = inkReducer(s, { type: "add", stroke: stroke("marker") });
  assert.equal(s.strokes.length, 2);
  s = inkReducer(s, { type: "undo" });
  assert.equal(s.strokes.length, 1);
  s = inkReducer(s, { type: "redo" });
  assert.equal(s.strokes.length, 2);
  s = inkReducer(s, { type: "clear" });
  assert.equal(s.strokes.length, 0);
  s = inkReducer(s, { type: "undo" });
  assert.equal(s.strokes.length, 2, "clear is undoable");
  s = inkReducer(s, { type: "undo" });
  s = inkReducer(s, { type: "add", stroke: stroke() });
  assert.equal(s.future.length, 0, "a new stroke drops the redo stack");
  assert.equal(inkReducer(EMPTY_INK, { type: "undo" }), EMPTY_INK);
  assert.equal(inkReducer(EMPTY_INK, { type: "add", stroke: stroke("pen", 0) }), EMPTY_INK);
  assert.equal(inkReducer(s, { type: "reset" }), EMPTY_INK);
});

test("a pen in hand ignores the palm unless finger drawing is on", () => {
  const base = { penSeen: true, fingerDraw: false, activeId: null, pointerId: 2 };
  assert.equal(pointerChoice({ ...base, pointerType: "touch" }), "ignore");
  assert.equal(pointerChoice({ ...base, pointerType: "touch", fingerDraw: true }), "draw");
  assert.equal(pointerChoice({ ...base, pointerType: "touch", penSeen: false }), "draw");
  assert.equal(pointerChoice({ ...base, pointerType: "pen" }), "draw");
  assert.equal(pointerChoice({ ...base, pointerType: "pen", activeId: 7 }), "ignore", "a second pointer never joins a stroke");
  assert.equal(pointerChoice({ ...base, pointerType: "mouse", activeId: 2 }), "draw");
  assert.equal(pointerChoice({ ...base, pointerType: "pen", activeId: 7, activeType: "touch" }), "takeover", "a pen press takes over from a resting palm");
  assert.equal(pointerChoice({ ...base, pointerType: "touch", activeId: 7, activeType: "pen" }), "ignore");
});

test("pressure shapes pen width and mice draw at half pressure", () => {
  assert.ok(strokeWidth("pen", 10, 1) > strokeWidth("pen", 10, 0.2));
  assert.equal(strokeWidth("marker", 20, 0.1), strokeWidth("marker", 20, 0.9));
  assert.equal(pressureOf("mouse", 0.5, 1), 0.5);
  assert.equal(pressureOf("pen", 0.8, 1), 0.8);
  assert.equal(pressureOf("pen", 0, 1), 0.5);
  assert.equal(pressureOf("touch", 1, 1), 0.5);
});

test("client points map into board units and stay inside", () => {
  const rect = { left: 10, top: 20, width: 500, height: 500 };
  assert.deepEqual(toBoard(260, 270, rect, { width: 1000, height: 1000 }), { x: 500, y: 500 });
  assert.deepEqual(toBoard(-50, 900, rect, { width: 1000, height: 1000 }), { x: 0, y: 1000 });
});

test("export rounds pressure and time and knows when there is ink", () => {
  const doc = inkDoc(100, 100, [{ tool: "pen", color: "w-strong", size: 4, points: [{ x: 1, y: 2, p: 0.123456, t: 12.7 }] }]);
  assert.deepEqual(doc.strokes[0].points[0], { x: 1, y: 2, p: 0.123, t: 13 });
  assert.ok(hasInk(doc.strokes));
  assert.ok(!hasInk([stroke("eraser")]));
});

test("attempt focus travels as a canvas item", () => {
  assert.equal(attemptFromItem(`${ATTEMPT_ITEM}ABC123`), "abc123");
  assert.equal(attemptFromItem("note-1"), null);
  assert.equal(attemptFromItem(null), null);
});

const line = (n, step = 1, t = 10) => Array.from({ length: n }, (_, i) => ({ x: i * step, y: 0, p: 0.5, t: i * t }));

test("thinning drops close and collinear points but keeps the ends, corners and timing", () => {
  const straight = thinPoints(line(200), 0.35);
  assert.equal(straight.length, 2);
  assert.deepEqual([straight[0].t, straight[1].t], [0, 1990]);
  const corner = [...line(50), ...Array.from({ length: 50 }, (_, i) => ({ x: 49, y: i + 1, p: 0.5, t: 500 + i * 10 }))];
  const kept = thinPoints(corner, 0.35);
  assert.ok(kept.some((pt) => pt.x === 49 && pt.y === 0), "the corner stays");
  assert.equal(kept[kept.length - 1].y, 50);
  assert.equal(thinPoints(line(10, 0.1), 0.35).length, 2, "points under half a unit apart collapse");
  const pressed = line(50).map((pt, i) => ({ ...pt, p: i < 25 ? 0.2 : 0.9 }));
  assert.ok(thinPoints(pressed, 0.35).length > 2, "a pressure change is kept");
});

test("ink over the limits is thinned or refused before anything is uploaded", () => {
  const doc = { width: 1000, height: 1000, strokes: [{ tool: "pen", color: "w-strong", size: 4, points: line(5000, 0.2) }] };
  const ok = prepareInk(doc);
  assert.ok(ok.ok && ok.doc.strokes[0].points.length < 50);
  const many = { ...doc, strokes: Array.from({ length: LIMITS.strokes + 1 }, () => doc.strokes[0]) };
  assert.deepEqual(prepareInk(many), { ok: false, error: TOO_FULL });
  const jagged = { ...doc, strokes: [{ ...doc.strokes[0], points: Array.from({ length: 400 }, (_, i) => ({ x: i * 3, y: (i % 2) * 40, p: 0.5, t: i })) }] };
  assert.deepEqual(prepareInk(jagged, { ...LIMITS, points: 100 }), { ok: false, error: TOO_FULL });
  assert.ok(prepareInk(jagged, { ...LIMITS, bytes: 100 }).ok === false);
});

test("token channels turn into hex and late coaches are spotted", () => {
  assert.equal(rgbHex("38 24 34"), "#261822");
  assert.equal(rgbHex("251, 243, 224"), "#fbf3e0");
  assert.equal(rgbHex("nope"), null);
  const at = "2026-11-22T10:00:00Z";
  const start = Date.parse(at);
  assert.ok(!coachLate(at, null, start + COACH_WAIT_MS - 1));
  assert.ok(coachLate(at, null, start + COACH_WAIT_MS));
  assert.ok(!coachLate(at, start + COACH_WAIT_MS, start + COACH_WAIT_MS + 1000), "a resend restarts the wait");
});

test("practice refs need eight hex characters like the server", async () => {
  const { ATTEMPT_ID } = await import("./practice.ts");
  assert.ok(ATTEMPT_ID.test("abcdef12"));
  assert.ok(ATTEMPT_ID.test("1b70efa3-fe20-4896-8c59-dd31ec4a3e2d"));
  assert.ok(!ATTEMPT_ID.test("abcdef1"));
  assert.ok(!ATTEMPT_ID.test("zzzzzzzz"));
});

test("the stroke budget leaves room for the server's longer numbers and thins harder before refusing", () => {
  assert.ok(LIMITS.bytes < 2 * 1024 * 1024 && LIMITS.bytes > 2 * 1024 * 1024 - 64 * 1024);
  const zigzag = { width: 2000, height: 2000, strokes: [{ tool: "pen", color: "w-strong", size: 4, points: Array.from({ length: 400 }, (_, i) => ({ x: i * 3, y: (i % 2) * 4, p: 0.5, t: i })) }] };
  const kept = prepareInk(zigzag, { ...LIMITS, bytes: 1000 });
  assert.ok(kept.ok && kept.doc.strokes[0].points.length <= 3);
});

function serdeFixtures() {
  const pts = (n, f) => Array.from({ length: n }, (_, i) => f(i));
  return [
    { width: 1024, height: 1024, strokes: [{ tool: "pen", color: "w-strong", rgb: "#261822", size: 8, points: [{ x: 0, y: 0, p: 0, t: 0 }, { x: 512.5, y: 300, p: 1, t: 16 }, { x: 1024, y: 1023.9, p: 0.123, t: 33 }] }] },
    { width: 1280, height: 960, strokes: [{ tool: "marker", color: "w-chart-3", size: 24, points: [{ x: 12.3, y: 45.6, p: 0.5, t: 0 }] }, { tool: "eraser", color: "w-strong", size: 32, points: pts(50, (i) => ({ x: i * 7.5, y: 100 + i, p: 0.5, t: i * 8 })) }] },
    { width: 1024, height: 1024, strokes: pts(40, (s) => ({ tool: "pencil", color: "w-link", rgb: "#0b6f6f", size: 4, points: pts(120, (i) => ({ x: Math.round((s * 13 + i * 3.3) % 1024 * 10) / 10, y: Math.round((i * 7.1 + s) % 1024 * 10) / 10, p: Math.round(((i % 9) / 8) * 1000) / 1000, t: s * 1000 + i * 8 })) })) },
  ];
}

test("the size estimate equals what serde_json writes for the parsed strokes", () => {
  const written = [232, 2169, 207924];
  assert.deepEqual(serdeFixtures().map(serdeBytes), written);
  assert.equal(serdeBytes({ width: 10, height: 10, strokes: [] }), JSON.stringify({ width: 10, height: 10, strokes: [] }).length + 4);
});
