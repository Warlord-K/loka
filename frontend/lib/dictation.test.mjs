import { test } from "node:test";
import assert from "node:assert/strict";
import { anchorAt, joinWords, shouldCut, splice, spokenText } from "./dictation.ts";

const dictate = (value, from, to, parts) => {
  const { firm, provisional } = spokenText(parts);
  return splice(anchorAt(value, from, to), firm, provisional);
};

test("inserts at the cursor and keeps the text around it", () => {
  const out = dictate("Please fix tomorrow.", 11, 11, [{ text: "the login bug", final: true }]);
  assert.equal(out.value, "Please fix the login bug tomorrow.");
  assert.equal(out.value.slice(out.start, out.end), "the login bug");
});

test("replaces a selection", () => {
  const out = dictate("Call Bob now", 5, 8, [{ text: "Alice", final: true }]);
  assert.equal(out.value, "Call Alice now");
});

test("an empty box gets the words alone", () => {
  const out = dictate("", 0, 0, [{ text: "hello there", final: false }]);
  assert.equal(out.value, "hello there");
  assert.equal(out.provisionalStart, 0);
  assert.equal(out.end, 11);
});

test("provisional words follow the firm ones and are marked", () => {
  const out = dictate("Note:", 5, 5, [
    { text: "First part.", final: true },
    { text: "second", final: false },
  ]);
  assert.equal(out.value, "Note: First part. second");
  assert.equal(out.value.slice(out.provisionalStart, out.end), "second");
  assert.equal(out.value.slice(out.start, out.provisionalStart), "First part. ");
});

test("a final result replaces the interim one in place", () => {
  const anchor = anchorAt("Hi ", 3, 3);
  const live = splice(anchor, "", "this is a tess");
  const done = splice(anchor, "this is a test", "");
  assert.equal(live.value, "Hi this is a tess");
  assert.equal(done.value, "Hi this is a test");
  assert.equal(done.provisionalStart, done.end);
});

test("a later final part stays provisional until the parts before it are final", () => {
  const { firm, provisional } = spokenText([
    { text: "one", final: false },
    { text: "two", final: true },
  ]);
  assert.equal(firm, "");
  assert.equal(provisional, "one two");
});

test("a second dictation appends after the first", () => {
  const first = dictate("Draft", 5, 5, [{ text: "one.", final: true }]);
  const second = dictate(first.value, first.end, first.end, [{ text: "Two.", final: true }]);
  assert.equal(second.value, "Draft one. Two.");
});

test("no extra space before punctuation that follows the cursor", () => {
  const out = dictate("Hello.", 5, 5, [{ text: "there", final: true }]);
  assert.equal(out.value, "Hello there.");
});

test("nothing heard leaves the text as it was", () => {
  const out = dictate("Keep me", 4, 4, [{ text: "  ", final: true }]);
  assert.equal(out.value, "Keep me");
});

test("joinWords keeps single spaces", () => {
  assert.equal(joinWords("a ", "b"), "a b");
  assert.equal(joinWords("a", ", b"), "a, b");
  assert.equal(joinWords("a", "b"), "a b");
});

test("long segments are cut at pauses", () => {
  assert.equal(shouldCut({ segmentMs: 3000, silentMs: 2000 }), false);
  assert.equal(shouldCut({ segmentMs: 9000, silentMs: 600 }), true);
  assert.equal(shouldCut({ segmentMs: 9000, silentMs: 100 }), false);
  assert.equal(shouldCut({ segmentMs: 15000, silentMs: 250 }), true);
  assert.equal(shouldCut({ segmentMs: 25000, silentMs: 0 }), true);
});
