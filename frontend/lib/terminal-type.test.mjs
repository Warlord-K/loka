import { test } from "node:test";
import assert from "node:assert/strict";
import { BRACKET_WAIT_MS, OUTPUT_QUIET_MS, cleanCommand, readyToType, typingFor } from "./terminal-type.ts";

test("a typed command never ends with a newline, so nothing runs until Enter", () => {
  assert.deepEqual(typingFor("git status\n", false), { kind: "type", data: "git status" });
  assert.deepEqual(typingFor("git status\r\n", true), { kind: "type", data: "\x1b[200~git status\x1b[201~" });
});

test("several lines go in as one bracketed paste, or are copied when the shell takes no pastes", () => {
  assert.deepEqual(typingFor("cd app\nnpm test", true), { kind: "type", data: "\x1b[200~cd app\nnpm test\x1b[201~" });
  assert.deepEqual(typingFor("cd app\nnpm test", false), { kind: "copy", text: "cd app\nnpm test" });
});

test("control characters are dropped so a command cannot close the paste early or press keys", () => {
  assert.equal(cleanCommand("echo hi\x1b[201~\x03; rm -rf /tmp/x\x07"), "echo hi[201~; rm -rf /tmp/x");
  assert.equal(cleanCommand("a\u009b31m b\x7f"), "a31m b");
  assert.deepEqual(typingFor("ls\x1b[201~\npwd", true), { kind: "type", data: "\x1b[200~ls[201~\npwd\x1b[201~" });
  assert.deepEqual(typingFor("\x1b\x03\n", true), { kind: "empty" });
});

test("tabs stay inside a paste and become spaces when typed, so they never trigger completion", () => {
  assert.deepEqual(typingFor("printf 'a\tb'", false), { kind: "type", data: "printf 'a b'" });
  assert.deepEqual(typingFor("printf 'a\tb'", true), { kind: "type", data: "\x1b[200~printf 'a\tb'\x1b[201~" });
});

test("a fresh shell is given time to turn on pasting before a command is typed", () => {
  assert.equal(readyToType({ bracketed: true, sinceLive: 0, sinceOutput: 0 }), true);
  assert.equal(readyToType({ bracketed: false, sinceLive: BRACKET_WAIT_MS - 1, sinceOutput: 5000 }), false);
  assert.equal(readyToType({ bracketed: false, sinceLive: BRACKET_WAIT_MS, sinceOutput: OUTPUT_QUIET_MS }), true);
  assert.equal(readyToType({ bracketed: false, sinceLive: BRACKET_WAIT_MS, sinceOutput: 10 }), false);
  assert.equal(readyToType({ bracketed: false, sinceLive: BRACKET_WAIT_MS * 2, sinceOutput: 10 }), true);
});

test("a command for a shell that is still opening is queued, and one for another device opens a new tab instead of ending a shell", async () => {
  const { planRequest } = await import("./terminal-type.ts");
  assert.equal(planRequest({ phase: "starting", current: "mac", target: "mac" }), "queue");
  assert.equal(planRequest({ phase: "connecting", current: "mac", target: "mac" }), "queue");
  assert.equal(planRequest({ phase: "live", current: "mac", target: "mac" }), "type");
  assert.equal(planRequest({ phase: "connecting", current: "mac", target: "cloud" }), "new_tab");
  assert.equal(planRequest({ phase: "live", current: "mac", target: "cloud" }), "new_tab");
  assert.equal(planRequest({ phase: "ended", current: "mac", target: "cloud" }), "open");
  assert.equal(planRequest({ phase: "idle", current: null, target: null }), "none");
});

test("the quote and the note say a copied command was copied, and name the device a typed one went to", async () => {
  const { typedReport } = await import("./terminal-type.ts");
  const typed = typedReport("typed", "Studio Mac");
  assert.match(typed.context, /^Typed at the prompt in the terminal on Studio Mac, not run yet\.$/);
  assert.match(typed.note, /on Studio Mac/);
  const copied = typedReport("copied", "Studio Mac");
  assert.match(copied.context, /^Copied to the clipboard/);
  assert.doesNotMatch(copied.context, /^Typed/);
  assert.match(copied.note, /copied instead/);
  assert.match(typedReport("not_copied", "Studio Mac").context, /^Not typed/);
  assert.equal(typedReport("cancelled", "Studio Mac"), null);
  assert.equal(typedReport("empty", "Studio Mac"), null);
  for (const r of [typed, copied, typedReport("not_copied", "x")]) assert.doesNotMatch(r.note + r.context, /—/);
});
