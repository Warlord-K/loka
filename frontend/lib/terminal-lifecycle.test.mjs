import { test } from "node:test";
import assert from "node:assert/strict";
import { NativeShellLifecycle, terminalStartsAllowed } from "./terminal-lifecycle.ts";

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

test("a pending or attached canvas owner protects its native shell", async () => {
  for (const confirmed of [false, true]) {
    const lifecycle = new NativeShellLifecycle();
    const canvas = {};
    const panel = {};
    const lease = await lifecycle.claim("shared", canvas);
    if (confirmed) lifecycle.confirm("shared", canvas, lease);
    let closes = 0;
    const result = await lifecycle.close("shared", panel, async () => {
      closes += 1;
    });
    assert.equal(result.status, "protected");
    assert.equal(closes, 0);
  }
});

test("a close and later attach serialize without closing the replacement", async () => {
  const lifecycle = new NativeShellLifecycle();
  const close = deferred();
  const owner = {};
  const closing = lifecycle.close("native", null, () => close.promise);
  let claimed = false;
  let lease;
  const claim = lifecycle.claim("native", owner).then((nextLease) => {
    lease = nextLease;
    claimed = true;
  });
  await Promise.resolve();
  assert.equal(claimed, false);
  close.resolve();
  assert.equal((await closing).status, "closed");
  await claim;
  lifecycle.confirm("native", owner, lease);
  lease.done();
  assert.equal(lifecycle.occupied("native"), true);
});

test("failed closes retain ownership for a retry", async () => {
  const lifecycle = new NativeShellLifecycle();
  const owner = {};
  const lease = await lifecycle.claim("native", owner);
  lifecycle.confirm("native", owner, lease);
  lease.done();
  let attempts = 0;
  const failed = await lifecycle.close("native", owner, async () => {
    attempts += 1;
    throw new Error("offline");
  });
  assert.equal(failed.status, "failed");
  assert.equal(lifecycle.occupied("native"), true);
  const retried = await lifecycle.close("native", owner, async () => {
    attempts += 1;
  });
  assert.equal(retried.status, "closed");
  assert.equal(attempts, 2);
});

test("device shells start only while their panel is visible and not closing", () => {
  assert.equal(terminalStartsAllowed(true, true, false), true);
  assert.equal(terminalStartsAllowed(true, false, false), false);
  assert.equal(terminalStartsAllowed(true, true, true), false);
  assert.equal(terminalStartsAllowed(false, false, true), true);
});

test("same-id native attachments settle in request order", async () => {
  const lifecycle = new NativeShellLifecycle();
  const home = {};
  const canvas = {};
  const first = await lifecycle.claim("shared", home);
  let second;
  let entered = false;
  const waiting = lifecycle.claim("shared", canvas).then((lease) => {
    second = lease;
    entered = true;
  });
  await Promise.resolve();
  assert.equal(entered, false);
  lifecycle.confirm("shared", home, first);
  first.done();
  await waiting;
  lifecycle.confirm("shared", canvas, second);
  second.done();
  assert.equal(lifecycle.holder("shared"), canvas);
});
