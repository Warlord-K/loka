import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

class TestWorldApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

function loadRoomApi(fetch) {
  const source = readFileSync(new URL("./quest-room-api.ts", import.meta.url), "utf8");
  const code = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(code, {
    module,
    exports: module.exports,
    fetch,
    require: (id) => id === "./world" ? { WorldApiError: TestWorldApiError } : { cleanVariants: () => null },
  });
  return module.exports.roomApi;
}

function response(status, body = "") {
  return {
    ok: status >= 200 && status < 300,
    status,
    text: async () => body,
    json: async () => body ? JSON.parse(body) : assert.fail("empty response was parsed as JSON"),
  };
}

test("removing a room member accepts 204 and preserves refusal details", async () => {
  const replies = [response(204), response(409, JSON.stringify({ error: "Default members stay in the room." }))];
  const calls = [];
  const roomApi = loadRoomApi(async (...args) => {
    calls.push(args);
    return replies.shift();
  });

  assert.equal(await roomApi.removeMember("quest", "agent", "helper"), undefined);
  await assert.rejects(
    roomApi.removeMember("quest", "agent", "lead"),
    (error) => error instanceof TestWorldApiError && error.status === 409 && error.message === "Default members stay in the room.",
  );
  assert.equal(calls[0][0], "/api/world/tasks/quest/members/agent/helper");
  assert.equal(calls[0][1].method, "DELETE");
});

test("room posts stay quiet unless Quick Ask supplies exact agent IDs", async () => {
  const calls = [];
  const roomApi = loadRoomApi(async (...args) => {
    calls.push(args);
    return response(200, JSON.stringify({ message: "message", asked: [] }));
  });

  await roomApi.post("quest", "Hello @Pip");
  await roomApi.post("quest", "Can you check this?", ["agent-id"]);

  assert.deepEqual(JSON.parse(calls[0][1].body), { body: "Hello @Pip", resolve_mentions: false });
  assert.deepEqual(JSON.parse(calls[1][1].body), { body: "Can you check this?", resolve_mentions: false, to: ["agent-id"] });
});

test("a refused Quick Ask surfaces the server error", async () => {
  const roomApi = loadRoomApi(async () => response(403, JSON.stringify({ error: "That agent is not available for this room." })));
  await assert.rejects(
    roomApi.post("quest", "Can you check this?", ["other-agent"]),
    (error) => error instanceof TestWorldApiError && error.status === 403 && error.message === "That agent is not available for this room.",
  );
});
