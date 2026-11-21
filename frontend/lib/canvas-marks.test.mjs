import { test } from "node:test";
import assert from "node:assert/strict";
import { commitMarkDelete, commitMarkDeleteOnExit, hideMarkDeletes, latestRequests, planMarkDelete, pruneMarkDeletes, restoreEdgeOps, restoreItemOps, settleMarkDelete } from "./canvas-marks.ts";

const mark = (id, kind, item_id) => ({ id, kind, item_id, status: "open", body: id });
const doc = {
  items: [
    { id: "a", kind: "note", x: 0, y: 0, w: 1, h: 1 },
    { id: "s1", kind: "sticky", x: 5, y: 5, w: 1, h: 1, text: "hi", by: "agent" },
  ],
  edges: [
    { id: "e1", from: "a", to: "s1", label: "why" },
    { id: "e2", from: "a", to: "a" },
  ],
};

test("a pin mark plans no item removal", () => {
  const p = planMarkDelete(mark("m1", "pin", "a"), doc);
  assert.equal(p.item, null);
  assert.deepEqual(p.edges, []);
});

test("a sticky mark plans removal of its sticky item and its arrows", () => {
  const p = planMarkDelete(mark("m2", "sticky", "s1"), doc);
  assert.equal(p.item.id, "s1");
  assert.deepEqual(p.edges.map((e) => e.id), ["e1"]);
});

test("a sticky mark hosted on a regular item never removes that item", () => {
  assert.equal(planMarkDelete(mark("m3", "sticky", "a"), doc).item, null);
});

test("hideMarkDeletes hides pending marks, sticky items and their arrows, even after a reload", () => {
  const marks = [mark("m1", "pin", "a"), mark("m2", "sticky", "s1")];
  const pending = [planMarkDelete(marks[1], doc)];
  const shown = hideMarkDeletes(marks, doc, pending);
  assert.deepEqual(shown.marks.map((m) => m.id), ["m1"]);
  assert.deepEqual(shown.doc.items.map((i) => i.id), ["a"]);
  assert.deepEqual(shown.doc.edges.map((e) => e.id), ["e2"]);
  const reloaded = hideMarkDeletes([...marks], { ...doc }, pending);
  assert.deepEqual(reloaded.marks.map((m) => m.id), ["m1"]);
});

test("hideMarkDeletes returns the same objects with nothing pending", () => {
  const marks = [mark("m1", "pin", "a")];
  const shown = hideMarkDeletes(marks, doc, []);
  assert.equal(shown.marks, marks);
  assert.equal(shown.doc, doc);
});

test("restoreItemOps puts the sticky back alone, keeping who wrote it", () => {
  const ops = restoreItemOps(planMarkDelete(mark("m2", "sticky", "s1"), doc));
  assert.deepEqual(ops, [{ op: "add", items: [doc.items[1]] }]);
  assert.equal(ops[0].items[0].by, "agent");
});

test("restoreEdgeOps brings back only arrows whose ends are still on the canvas", () => {
  const p = planMarkDelete(mark("m2", "sticky", "s1"), {
    ...doc,
    items: [...doc.items, { id: "b", kind: "note", x: 0, y: 0, w: 1, h: 1 }],
    edges: [...doc.edges, { id: "e3", from: "s1", to: "b" }],
  });
  const restored = { items: doc.items, edges: [] };
  assert.deepEqual(restoreEdgeOps(p, restored), [{ op: "connect", edges: [{ from: "a", to: "s1", label: "why" }] }]);
  assert.equal(restoreEdgeOps(p, { items: [doc.items[1]], edges: [] }), null);
  assert.equal(restoreEdgeOps(p, { items: doc.items, edges: [{ id: "x", from: "a", to: "s1" }] }), null);
});

const api = (calls, { canvasFails = [], markError = null, gone = false } = {}) => ({
  changeCanvas: async (ops) => {
    calls.push(ops[0].op);
    if (canvasFails.includes(ops[0].op)) throw new Error("canvas");
    return { items: [], edges: [], op: ops[0].op };
  },
  removeMark: async () => {
    calls.push("mark");
    if (markError) throw markError;
  },
  isGone: () => gone,
});

test("commit removes the sticky item, then the mark", async () => {
  const calls = [];
  const out = await commitMarkDelete(planMarkDelete(mark("m2", "sticky", "s1"), doc), api(calls));
  assert.equal(out.ok, true);
  assert.deepEqual(calls, ["remove", "mark"]);
});

test("commit stops before the mark when the sticky item fails to remove", async () => {
  const calls = [];
  const out = await commitMarkDelete(planMarkDelete(mark("m2", "sticky", "s1"), doc), api(calls, { canvasFails: ["remove"] }));
  assert.equal(out.ok, false);
  assert.deepEqual(calls, ["remove"]);
});

const restoring = (calls, { connectFails = false, sticky = true } = {}) => ({
  changeCanvas: async (ops) => {
    calls.push(ops[0].op);
    if (ops[0].op === "remove") return { items: [doc.items[0]], edges: [] };
    if (ops[0].op === "add") return { items: sticky ? doc.items : [doc.items[0]], edges: [], op: "add" };
    if (connectFails) throw new Error("arrow");
    return { items: doc.items, edges: ops[0].edges, op: "connect" };
  },
  removeMark: async () => {
    calls.push("mark");
    throw new Error("500");
  },
  isGone: () => false,
});

test("commit puts the sticky back, then its arrows, when the mark fails to delete", async () => {
  const calls = [];
  const out = await commitMarkDelete(planMarkDelete(mark("m2", "sticky", "s1"), doc), restoring(calls));
  assert.equal(out.ok, false);
  assert.equal(out.doc.op, "connect");
  assert.deepEqual(calls, ["remove", "mark", "add", "connect"]);
});

test("a failed arrow restore still keeps the restored sticky", async () => {
  const calls = [];
  const out = await commitMarkDelete(planMarkDelete(mark("m2", "sticky", "s1"), doc), restoring(calls, { connectFails: true }));
  assert.equal(out.ok, false);
  assert.equal(out.doc.op, "add");
  assert.deepEqual(calls, ["remove", "mark", "add", "connect"]);
});

test("leaving the page sends the mark and sticky deletes together", async () => {
  const started = [];
  let release;
  const gate = new Promise((r) => (release = r));
  const exit = commitMarkDeleteOnExit(planMarkDelete(mark("m2", "sticky", "s1"), doc), {
    removeMark: () => (started.push("mark"), gate),
    changeCanvas: (ops) => (started.push(ops[0].op), gate),
    isGone: () => false,
  });
  assert.deepEqual(started, ["mark", "remove"]);
  release();
  await exit;
});

test("commit treats an already deleted mark as done", async () => {
  const calls = [];
  const out = await commitMarkDelete(planMarkDelete(mark("m1", "pin", "a"), doc), api(calls, { markError: new Error("404"), gone: true }));
  assert.equal(out.ok, true);
  assert.deepEqual(calls, ["mark"]);
});

test("a settled delete stays hidden, so a stale reload cannot bring it back", () => {
  const sticky = mark("m2", "sticky", "s1");
  const pending = new Map([["m2", planMarkDelete(sticky, doc)]]);
  const settled = settleMarkDelete(pending, "m2", true);
  assert.equal(settled.get("m2").done, true);
  assert.equal(settleMarkDelete(settled, "m2", true), settled);
  const stale = hideMarkDeletes([mark("m1", "pin", "a"), sticky], doc, settled.values());
  assert.deepEqual(stale.marks.map((m) => m.id), ["m1"]);
  assert.deepEqual(stale.doc.items.map((i) => i.id), ["a"]);
});

test("a failed or undone delete stops hiding the mark", () => {
  const pending = new Map([["m1", planMarkDelete(mark("m1", "pin", "a"), doc)]]);
  const failed = settleMarkDelete(pending, "m1", false);
  assert.equal(failed.size, 0);
  assert.equal(pending.size, 1);
  assert.equal(settleMarkDelete(failed, "m1", false), failed);
});

test("a settled delete is dropped only once the server no longer has the mark or the sticky", () => {
  const sticky = mark("m2", "sticky", "s1");
  const pending = settleMarkDelete(new Map([["m2", planMarkDelete(sticky, doc)]]), "m2", true);
  assert.equal(pruneMarkDeletes(pending, [sticky], { items: [doc.items[0]], edges: [] }), pending);
  assert.equal(pruneMarkDeletes(pending, [], doc), pending);
  assert.equal(pruneMarkDeletes(pending, [], { items: [doc.items[0]], edges: [] }).size, 0);
});

test("a delete still waiting for its undo window is never pruned", () => {
  const pending = new Map([["m1", planMarkDelete(mark("m1", "pin", "a"), doc)]]);
  assert.equal(pruneMarkDeletes(pending, [], doc), pending);
});

test("latestRequests keeps only the newest response", () => {
  const requests = latestRequests();
  const first = requests.start();
  const second = requests.start();
  assert.equal(requests.isLatest(first), false);
  assert.equal(requests.isLatest(second), true);
});
