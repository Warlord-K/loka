import { test } from "node:test";
import assert from "node:assert/strict";
import { chatWaiting, threadWaiting, THINKING_WINDOW_MS } from "./thread-wait.ts";

const now = Date.parse("2026-11-26T06:10:00Z");
const person = { lastIsPerson: true, lastAt: now - 60_000, now };

test("a quick run in progress keeps the thread thinking", () => {
  assert.equal(threadWaiting({ ...person, working: true }), true);
});

test("a finished quick run clears thinking even when the last row is the person's", () => {
  assert.equal(threadWaiting({ ...person, working: false }), false);
  assert.equal(threadWaiting({ ...person, working: false, lastAt: now - 1000 }), false);
});

test("a thread with no quick chat falls back to the person's last reply, for a limited time", () => {
  assert.equal(threadWaiting({ ...person, working: null }), true);
  assert.equal(threadWaiting({ ...person, working: null, lastAt: now - THINKING_WINDOW_MS - 1 }), false);
  assert.equal(threadWaiting({ ...person, working: null, lastIsPerson: false }), false);
});

test("a quick chat is waiting only while the server says a run is open", () => {
  assert.equal(chatWaiting(true), true);
  assert.equal(chatWaiting(false), false);
  assert.equal(chatWaiting(undefined), false);
});
