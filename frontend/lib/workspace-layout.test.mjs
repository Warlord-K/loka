import { test } from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_LAYOUT, EMPTY_STORE, MAX_CANVASES, layoutFor, layoutKey, readStore, remember } from "./workspace-layout.ts";

const A = "abcdef12-3456-4789-8abc-def012345678";
const B = "bbbbbb12-3456-4789-8abc-def012345678";
const CH = "c0ffee12-0000-4000-8000-000000000001";

test("each canvas keeps its own workspace mode, channels pane and channel", () => {
  assert.equal(layoutKey("u1"), "world.workspace.u1");
  assert.equal(layoutKey(null), "world.workspace.anon");
  let store = remember(EMPTY_STORE, A, { workspace: true }, 1);
  store = remember(store, A, { channels: false, channel: CH, chat: false }, 2);
  store = remember(store, B, { workspace: false }, 3);
  assert.deepEqual(layoutFor(store, A), { workspace: true, channels: false, chat: false, channel: CH });
  assert.deepEqual(layoutFor(store, B), DEFAULT_LAYOUT);
  assert.equal(store.last, A);
  assert.deepEqual(readStore(JSON.stringify(store)), store);
});

test("a new canvas opens with the defaults and leaving a workspace is remembered", () => {
  assert.deepEqual(layoutFor(EMPTY_STORE, A), { workspace: false, channels: true, chat: true, channel: null });
  assert.equal(layoutFor(remember(EMPTY_STORE, A, { workspace: true, chat: false }, 1), B).chat, true, "hiding the chat on one canvas leaves others open");
  assert.deepEqual(layoutFor(EMPTY_STORE, null), DEFAULT_LAYOUT);
  const left = remember(remember(EMPTY_STORE, A, { workspace: true }, 1), A, { workspace: false }, 2);
  assert.equal(layoutFor(left, A).workspace, false);
  assert.equal(left.last, null);
});

test("the last canvas used as a workspace survives other canvases", () => {
  let store = remember(EMPTY_STORE, A, { workspace: true }, 1);
  store = remember(store, B, { workspace: false }, 2);
  assert.equal(store.last, A);
  store = remember(store, B, { workspace: true }, 3);
  assert.equal(store.last, B);
});

test("the store keeps only the most recent canvases", () => {
  let store = EMPTY_STORE;
  for (let i = 0; i < MAX_CANVASES + 5; i += 1) store = remember(store, `canvas-${String(i).padStart(4, "0")}`, { workspace: i === 0 }, i);
  assert.equal(Object.keys(store.canvases).length, MAX_CANVASES);
  assert.equal(store.canvases["canvas-0000"], undefined);
  assert.equal(store.last, null);
});

test("broken, foreign and older stored layouts load safely", () => {
  assert.deepEqual(readStore(null), EMPTY_STORE);
  assert.deepEqual(readStore("not json"), EMPTY_STORE);
  assert.deepEqual(readStore("[1,2]"), EMPTY_STORE);
  assert.deepEqual(readStore(JSON.stringify({ canvases: { "../x": { workspace: true } }, last: "<script>" })), EMPTY_STORE);
  const odd = readStore(JSON.stringify({ canvases: { [A]: { workspace: "yes", channels: 1, channel: "../x" } } }));
  assert.deepEqual(layoutFor(odd, A), DEFAULT_LAYOUT);
  const old = readStore(JSON.stringify({ channels: false, channel: CH, last: A }));
  assert.equal(old.last, A);
  assert.deepEqual(layoutFor(old, A), { workspace: true, channels: false, chat: true, channel: CH });
  assert.equal(remember(EMPTY_STORE, "<script>", { workspace: true }, 1), EMPTY_STORE);
});
