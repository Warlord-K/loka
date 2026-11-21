import { test } from "node:test";
import assert from "node:assert/strict";
import { SEND_RETRY_DELAYS, notSent, sendFailure } from "./send-failure.ts";

class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

test("a dropped connection retries", () => {
  const f = sendFailure(new TypeError("Load failed"));
  assert.equal(f.retry, true);
  assert.match(f.retrying, /retrying/);
});

test("server errors retry as a restart", () => {
  for (const status of [500, 502, 503]) {
    const f = sendFailure(new Error(`${status} Internal Server Error`));
    assert.equal(f.retry, true);
    assert.equal(f.retrying, "Server restarting, retrying...");
  }
});

test("a full disk is named and not retried", () => {
  assert.deepEqual(sendFailure(new ApiError(507, "507 Insufficient Storage")).retry, false);
  assert.match(sendFailure(new ApiError(507, "507 Insufficient Storage")).reason, /^The server is out of disk space\.$/);
  assert.match(sendFailure(new Error("500 could not extend file: No space left on device")).reason, /^The server is out of disk space\.$/);
});

test("rate limits stop and explain", () => {
  const f = sendFailure(new ApiError(429, "429 Too Many Requests"));
  assert.equal(f.retry, false);
  assert.match(f.reason, /too fast/);
});

test("client errors give a plain reason", () => {
  assert.match(sendFailure(new ApiError(401, "401 Unauthorized")).reason, /signed out/);
  assert.match(sendFailure(new ApiError(404, "404 Not Found")).reason, /gone/);
  assert.match(sendFailure(new ApiError(413, "413 Payload Too Large")).reason, /Too large/);
  assert.equal(sendFailure(new ApiError(403, "Only the owner can answer this card")).reason, "Only the owner can answer this card.");
});

test("the message names the reason and keeps the text", () => {
  const text = notSent(sendFailure(new ApiError(507, "507 Insufficient Storage")));
  assert.match(text, /^Not sent\. The server is out of disk space\..*back in the box/);
  assert.doesNotMatch(text, /—/);
});

test("retries cover about half a minute", () => {
  const total = SEND_RETRY_DELAYS.reduce((a, b) => a + b, 0);
  assert.ok(total >= 25000 && total <= 40000);
});
