import { test } from "node:test";
import assert from "node:assert/strict";
import { QUOTE_CONTEXT_CHARS, QUOTE_TEXT_CHARS, TERMINAL_LINES, attachesByDefault, joinRows, tailWithin, terminalQuote } from "./terminal-context.ts";

const row = (text, wrapped = false) => ({ text, wrapped });

test("wrapped rows join into one line and blank edges go", () => {
  assert.deepEqual(joinRows([row(""), row("$ ls   "), row("a.txt  b.t"), row("xt", true), row(""), row("  ")]), ["$ ls", "a.txt  b.txt"]);
});

test("control characters are dropped", () => {
  assert.deepEqual(joinRows([row("ok\u0007\u001b done")]), ["ok done"]);
});

test("tails keep whole lines within the budget and the line cap", () => {
  assert.deepEqual(tailWithin(["aaa", "bb", "c"], 4), ["bb", "c"]);
  assert.deepEqual(tailWithin(["x".repeat(10)], 4), ["xxxx"]);
  const many = Array.from({ length: 500 }, (_, i) => `line ${i}`);
  const tail = tailWithin(many, 1_000_000);
  assert.equal(tail.length, TERMINAL_LINES);
  assert.equal(tail.at(-1), "line 499");
});

test("a selection is the quote and recent output is its context", () => {
  const quote = terminalQuote({ selection: "error: boom\n", rows: [row("$ cargo build"), row("error: boom")], device: "Mac mini" });
  assert.equal(quote.text, "error: boom");
  assert.equal(quote.label, "Terminal selection on Mac mini");
  assert.equal(quote.context, "$ cargo build\nerror: boom");
});

test("without a selection the recent output is sent, clearly labelled", () => {
  const rows = Array.from({ length: 600 }, (_, i) => row(`output line number ${i}`));
  const quote = terminalQuote({ selection: "  ", rows, device: "Mac mini" });
  assert.ok(quote.text.length <= QUOTE_TEXT_CHARS);
  assert.ok(quote.context.length <= QUOTE_CONTEXT_CHARS);
  assert.ok(quote.text.endsWith("output line number 599"));
  assert.ok(quote.context.endsWith(quote.text));
  assert.match(quote.label, /^Recent terminal output on Mac mini \(last \d+ lines\)$/);
});

test("short output is sent once and empty terminals send nothing", () => {
  const quote = terminalQuote({ selection: "", rows: [row("$ pwd"), row("/Users/me")], device: "This Mac" });
  assert.equal(quote.text, "$ pwd\n/Users/me");
  assert.equal(quote.context, undefined);
  assert.equal(quote.label, "Recent terminal output on This Mac (last 2 lines)");
  assert.equal(terminalQuote({ selection: "", rows: [row(""), row(" ")], device: "x" }), null);
});

test("terminal output is attached by default only to the viewer's own agents", () => {
  assert.equal(attachesByDefault("me", "me"), true);
  assert.equal(attachesByDefault("someone-else", "me"), false);
  assert.equal(attachesByDefault(null, "me"), false);
  assert.equal(attachesByDefault("me", null), false);
  assert.equal(attachesByDefault(undefined, undefined), false);
});
