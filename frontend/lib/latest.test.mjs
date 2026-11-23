import { test } from "node:test";
import assert from "node:assert/strict";
import { applyLatest, latestOnly } from "./latest.ts";

test("only the newest request counts as current", () => {
  const reads = latestOnly();
  const first = reads.begin();
  assert.equal(first(), true);
  const second = reads.begin();
  assert.equal(first(), false);
  assert.equal(second(), true);
});

test("an older to-do read cannot replace the new selection", async () => {
  const requests = latestOnly();
  const { promise, resolve } = Promise.withResolvers();
  const opened = [];
  const older = applyLatest(() => promise, requests.begin(), (todo) => opened.push(todo.id));
  await applyLatest(() => ({ id: "newer" }), requests.begin(), (todo) => opened.push(todo.id));
  resolve({ id: "older" });
  await older;
  assert.deepEqual(opened, ["newer"]);
});

test("a late search response cannot replace hits for the current query", async () => {
  const requests = latestOnly();
  const older = Promise.withResolvers();
  let shown = null;
  const search = (query, read) => applyLatest(read, requests.begin(), (results) => {
    shown = { query, results };
  });
  const first = search("old", () => older.promise);
  await search("current", () => ["current hit"]);
  older.resolve(["old hit"]);
  await first;
  assert.deepEqual(shown, { query: "current", results: ["current hit"] });
});

test("identical report requests are independent and a declined edit guard keeps the current view", async () => {
  const requests = latestOnly();
  const { promise, resolve } = Promise.withResolvers();
  let report = "A";
  const select = (id, guard) => applyLatest(guard, requests.begin(), (allowed) => {
    if (allowed) report = id;
  });
  await select("B", () => true);
  const first = select("A", () => promise);
  await select("A", () => false);
  resolve(true);
  await first;
  assert.equal(report, "B");
  await select("A", () => true);
  assert.equal(report, "A");
});

import { recheckDelay, startRechecks } from "./latest.ts";

function fakeClock() {
  const queue = [];
  return {
    schedule: (fn, ms) => {
      const entry = { fn, ms, live: true };
      queue.push(entry);
      return entry;
    },
    cancel: (entry) => {
      if (entry) entry.live = false;
    },
    fire() {
      const entry = queue.find((e) => e.live);
      if (!entry) return null;
      entry.live = false;
      entry.fn();
      return entry.ms;
    },
  };
}

test("a chat stuck on thinking is refetched again and again with backoff, not once", () => {
  const clock = fakeClock();
  let fetches = 0;
  startRechecks(() => fetches++, clock.schedule, clock.cancel);
  const waits = [];
  for (let i = 0; i < 6; i++) waits.push(clock.fire());
  assert.equal(fetches, 6);
  assert.equal(waits[0], 20_000);
  assert.ok(waits[1] > waits[0]);
  assert.equal(waits[5], 60_000);
});

test("stopping when the answer arrives cancels the pending refetch", () => {
  const clock = fakeClock();
  let fetches = 0;
  const stop = startRechecks(() => fetches++, clock.schedule, clock.cancel);
  clock.fire();
  stop();
  assert.equal(clock.fire(), null);
  assert.equal(fetches, 1);
});

test("rechecks are bounded", () => {
  assert.equal(recheckDelay(0), 20_000);
  assert.equal(recheckDelay(100), null);
  const clock = fakeClock();
  let fetches = 0;
  startRechecks(() => fetches++, clock.schedule, clock.cancel);
  while (clock.fire() !== null);
  assert.ok(fetches > 10 && fetches < 40);
});
