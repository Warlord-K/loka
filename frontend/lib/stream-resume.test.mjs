import { test } from "node:test";
import assert from "node:assert/strict";
import { newerSeq, resyncHead, streamUrl } from "./stream-resume.ts";

test("a first connection starts live and a reconnection asks for what it missed", () => {
  assert.equal(streamUrl(0), "/api/world/stream");
  assert.equal(streamUrl(4172), "/api/world/stream?after=4172");
});

test("the last seen id only moves forward", () => {
  assert.equal(newerSeq(10, 12), 12);
  assert.equal(newerSeq(10, 7), 10);
  assert.equal(newerSeq(10, undefined), 10);
  assert.equal(newerSeq(10, "99"), 10);
  assert.equal(newerSeq(10, Number.NaN), 10);
});

test("a resync carries the new head and ignores junk", () => {
  assert.equal(resyncHead('{"head":5000}'), 5000);
  assert.equal(resyncHead('{"head":"x"}'), null);
  assert.equal(resyncHead("not json"), null);
});
