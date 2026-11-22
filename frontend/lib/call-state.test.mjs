import { test } from "node:test";
import assert from "node:assert/strict";
import { callClock, callReducer, NO_CALL } from "./call-state.ts";

const start = (agentId) => callReducer(NO_CALL, { type: "start", agentId });

test("starting a call opens it in full", () => {
  const call = start("a");
  assert.deepEqual(call, { agentId: "a", minimised: false });
});

test("minimising keeps the call running", () => {
  const call = callReducer(start("a"), { type: "minimise" });
  assert.deepEqual(call, { agentId: "a", minimised: true });
});

test("restoring brings the same call back", () => {
  const call = callReducer(callReducer(start("a"), { type: "minimise" }), { type: "restore" });
  assert.deepEqual(call, { agentId: "a", minimised: false });
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
  assert.deepEqual(call, { agentId: "b", minimised: false });
});

test("the clock reads minutes and seconds, then hours", () => {
  assert.equal(callClock(0), "0:00");
  assert.equal(callClock(9.7), "0:09");
  assert.equal(callClock(75), "1:15");
  assert.equal(callClock(3725), "1:02:05");
  assert.equal(callClock(-3), "0:00");
});
