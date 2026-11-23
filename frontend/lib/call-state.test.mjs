import { test } from "node:test";
import assert from "node:assert/strict";
import { callClock, callReducer, isCallOf, NO_CALL } from "./call-state.ts";

const start = (agentId) => callReducer(NO_CALL, { type: "start", agentId });

test("starting a call opens it in full", () => {
  const call = start("a");
  assert.deepEqual(call, { agentId: "a", quickId: null, about: null, minimised: false });
});

test("minimising keeps the call running", () => {
  const call = callReducer(start("a"), { type: "minimise" });
  assert.deepEqual(call, { agentId: "a", quickId: null, about: null, minimised: true });
});

test("restoring brings the same call back", () => {
  const call = callReducer(callReducer(start("a"), { type: "minimise" }), { type: "restore" });
  assert.deepEqual(call, { agentId: "a", quickId: null, about: null, minimised: false });
});

test("only hanging up ends the call", () => {
  let call = start("a");
  for (const type of ["minimise", "restore", "minimise"]) {
    call = callReducer(call, { type });
    assert.equal(call.agentId, "a");
  }
  assert.deepEqual(callReducer(call, { type: "hang_up" }), NO_CALL);
});

test("minimise, restore and hang up do nothing without a call", () => {
  for (const type of ["minimise", "restore", "hang_up"]) assert.equal(callReducer(NO_CALL, { type }), NO_CALL);
});

test("unchanged actions keep the same state object", () => {
  const open = start("a");
  assert.equal(callReducer(open, { type: "restore" }), open);
  const small = callReducer(open, { type: "minimise" });
  assert.equal(callReducer(small, { type: "minimise" }), small);
});

test("starting a call while one is minimised opens the new one in full", () => {
  const call = callReducer(callReducer(start("a"), { type: "minimise" }), { type: "start", agentId: "b" });
  assert.deepEqual(call, { agentId: "b", quickId: null, about: null, minimised: false });
});

test("the clock reads minutes and seconds, then hours", () => {
  assert.equal(callClock(0), "0:00");
  assert.equal(callClock(9.7), "0:09");
  assert.equal(callClock(75), "1:15");
  assert.equal(callClock(3725), "1:02:05");
  assert.equal(callClock(-3), "0:00");
});

test("a call from a quick chat belongs to that chat, not the agent's main chat", () => {
  const call = callReducer(NO_CALL, { type: "start", agentId: "a", quickId: "q1" });
  assert.deepEqual(call, { agentId: "a", quickId: "q1", about: null, minimised: false });
  assert.equal(isCallOf(call, "a", "q1"), true);
  assert.equal(isCallOf(call, "a", null), false);
  assert.equal(isCallOf(call, "a", "q2"), false);
  assert.equal(isCallOf(start("a"), "a", null), true);
  assert.deepEqual(callReducer(callReducer(call, { type: "minimise" }), { type: "restore" }), call);
  assert.deepEqual(callReducer(call, { type: "hang_up" }), NO_CALL);
});

test("a call started from a canvas carries the canvas, and hanging up forgets it", () => {
  const call = callReducer(NO_CALL, { type: "start", agentId: "a", about: "report:abcdef12" });
  assert.deepEqual(call, { agentId: "a", quickId: null, about: "report:abcdef12", minimised: false });
  assert.ok(isCallOf(call, "a", null));
  assert.deepEqual(callReducer(call, { type: "hang_up" }), NO_CALL);
});
