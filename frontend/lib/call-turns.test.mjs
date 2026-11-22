import { test } from "node:test";
import assert from "node:assert/strict";
import { CallTurns, RESERVATION_MS } from "./call-turns.ts";

test("each turn is saved once, in order", () => {
  const turns = new CallTurns();
  turns.set("agent", "greet", "Hey Sam, what's up?");
  turns.set("user", "u-1", "Check the build");
  const first = turns.unsaved();
  assert.deepEqual(
    first.map((l) => [l.role, l.text]),
    [
      ["agent", "Hey Sam, what's up?"],
      ["user", "Check the build"],
    ],
  );
  turns.markSaved(first);
  assert.deepEqual(turns.unsaved(), []);
});

test("an agent turn is saved while it is still being said, then finished in place", () => {
  const turns = new CallTurns();
  turns.set("agent", "a-1", "Sure, I am", true);
  const early = turns.unsaved();
  assert.deepEqual(early, [{ role: "agent", key: "a-1", text: "Sure, I am", partial: true }]);
  turns.markSaved(early);
  turns.set("agent", "a-1", "Sure, I am looking", true);
  assert.equal(turns.unsaved()[0].text, "Sure, I am looking");
  turns.markSaved(turns.unsaved());
  turns.set("agent", "a-1", "Sure, I am looking at it now.");
  assert.deepEqual(turns.unsaved(), [{ role: "agent", key: "a-1", text: "Sure, I am looking at it now." }]);
  assert.equal(turns.transcript().length, 1);
});

test("a call cut mid sentence still has the partial agent turn to save", () => {
  const turns = new CallTurns();
  turns.set("user", "u-1", "What did the review say?");
  turns.markSaved(turns.unsaved());
  turns.set("agent", "a-2", "Rex found two things, the first", true);
  assert.deepEqual(turns.unsaved(), [{ role: "agent", key: "a-2", text: "Rex found two things, the first", partial: true }]);
});

test("a failed save is retried with the latest text", () => {
  const turns = new CallTurns();
  turns.set("agent", "a-1", "One", true);
  const lost = turns.unsaved();
  turns.set("agent", "a-1", "One two", true);
  assert.deepEqual(turns.unsaved(), [{ role: "agent", key: "a-1", text: "One two", partial: true }]);
  assert.equal(lost[0].text, "One");
});

test("a longer transcript of the same spoken turn replaces it", () => {
  const turns = new CallTurns();
  turns.set("user", "u-1", "Open the");
  turns.markSaved(turns.unsaved());
  assert.equal(turns.set("user", "u-1", "Open the report on sprites"), true);
  assert.equal(turns.unsaved()[0].text, "Open the report on sprites");
  assert.equal(turns.set("user", "u-1", "  "), false);
  assert.equal(turns.set("user", "u-1", "Open the report on sprites"), false);
});

test("a question whose transcript arrives late is still saved before the answer", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-2", 1000);
  turns.set("agent", "a-2", "The build is green", true);
  assert.deepEqual(turns.unsaved(1500), [], "the answer waits for the question");
  assert.equal(turns.holding(), true);
  turns.set("user", "u-2", "Is the build green?");
  assert.deepEqual(
    turns.unsaved(1600).map((l) => [l.role, l.text]),
    [
      ["user", "Is the build green?"],
      ["agent", "The build is green"],
    ],
  );
  assert.equal(turns.holding(), false);
});

test("a question with no words never blocks the answer for long", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-3", 1000);
  turns.set("agent", "a-3", "Did you say something?", true);
  assert.deepEqual(turns.unsaved(1000 + RESERVATION_MS + 1).map((l) => l.key), ["a-3"]);
  assert.equal(turns.holding(), false);
});

test("released and final saves skip empty reservations", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-4", 1000);
  turns.release("u-4");
  assert.equal(turns.holding(), false);
  turns.reserve("user", "u-5", 1000);
  turns.set("agent", "a-5", "Bye for now", true);
  assert.deepEqual(turns.unsaved(1001, false).map((l) => l.key), ["a-5"], "hanging up saves what the agent said");
  assert.deepEqual(turns.transcript().map((l) => l.key), ["a-5"]);
});

test("releasing a filled turn keeps it", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-6", 1000);
  turns.set("user", "u-6", "Thanks");
  turns.release("u-6");
  assert.deepEqual(turns.transcript().map((l) => l.text), ["Thanks"]);
});
