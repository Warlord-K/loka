import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { QUEST_STAGE_LABEL, questNeedsYou, questOpen, questStage, sessionAwaitsPlan } from "./quest-status.ts";

test("a quest needs you only while its plan waits and it is still open", () => {
  assert.equal(questNeedsYou({ status: "in_progress" }), false);
  assert.equal(questNeedsYou({ status: "in_progress", plan_waiting: false }), false);
  assert.equal(questNeedsYou({ status: "in_progress", plan_waiting: true }), true);
  assert.equal(questNeedsYou({ status: "todo", plan_waiting: true }), true);
  assert.equal(questNeedsYou({ status: "failed", plan_waiting: true }), true);
  assert.equal(questNeedsYou({ status: "done", plan_waiting: true }), false);
  assert.equal(questNeedsYou({ status: "cancelled", plan_waiting: true }), false);
  assert.equal(questNeedsYou({ status: "in_progress", plan_waiting: null }), false);
  assert.equal(questOpen({ status: "review" }), true);
  assert.equal(questOpen({ status: "done" }), false);
});

test("a session waits on a plan only while it is not archived", () => {
  assert.equal(sessionAwaitsPlan({ status: "active", plan_state: "awaiting" }), true);
  assert.equal(sessionAwaitsPlan({ status: "archived", plan_state: "awaiting" }), false);
  assert.equal(sessionAwaitsPlan({ status: "active", plan_state: "planning" }), false);
  assert.equal(sessionAwaitsPlan({ status: "active", plan_state: null }), false);
});

test("quest stages follow the status and the shared Needs you rule", () => {
  assert.equal(questStage({ status: "in_progress" }), "working");
  assert.equal(questStage({ status: "in_progress", plan_waiting: true }), "needs_you");
  assert.equal(questStage({ status: "done", plan_waiting: true }), "done");
  assert.equal(questStage({ status: "cancelled", plan_waiting: true }), "cancelled");
  assert.equal(questStage({ status: "review" }), "in_review");
  assert.equal(questStage({ status: "plan_review" }), "in_review");
  assert.equal(questStage({ status: "review", plan_waiting: true }), "needs_you");
  assert.equal(questStage({ status: "failed", plan_waiting: true }), "needs_you");
  for (const status of ["todo", "in_progress", "review", "plan_review", "failed", "done", "cancelled"]) {
    for (const plan_waiting of [true, false, null, undefined]) {
      assert.equal(questStage({ status, plan_waiting }) === "needs_you", questNeedsYou({ status, plan_waiting }), `${status} ${plan_waiting}`);
    }
  }
  assert.equal(QUEST_STAGE_LABEL.in_review, "In review");
  assert.equal(questStage({ status: "failed" }), "stuck");
  assert.equal(questStage({ status: "todo" }), "waiting");
  assert.equal(QUEST_STAGE_LABEL.needs_you, "Needs you");
});

const here = fileURLToPath(import.meta.url);
const repo = join(dirname(here), "..", "..");
const VIEWS = ["frontend/components", "frontend/app", "frontend/lib", "ios/App", "ios/Shared"];
const SKIP = new Set(["node_modules", ".next", "build", "DerivedData"]);
const HELPERS = new Set(["frontend/lib/quest-status.ts", "frontend/lib/world.ts"]);

function* files(dir) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIP.has(entry.name)) yield* files(path);
    } else if ([".ts", ".tsx", ".swift"].includes(extname(entry.name)) && !entry.name.includes(".test.")) {
      yield path;
    }
  }
}

test("quest status views read the plan state only through the shared helpers", () => {
  const found = [];
  for (const root of VIEWS) {
    for (const path of files(join(repo, root))) {
      const rel = relative(repo, path);
      if (HELPERS.has(rel)) continue;
      readFileSync(path, "utf8")
        .split("\n")
        .forEach((line, i) => {
          const awaiting = /plan_state\s*={2,3}\s*"awaiting"/.test(line) && !/var awaitsPlan\b/.test(line);
          const raw = /\bplan_waiting\b/.test(line) && !/questNeedsYou|QuestStatus\.needsYou|var plan_waiting|plan_waiting\?:/.test(line);
          if (awaiting || raw) found.push(`${rel}:${i + 1} ${line.trim().slice(0, 100)}`);
        });
    }
  }
  assert.deepEqual(found, [], "Use questNeedsYou or sessionAwaitsPlan (web) and QuestStatus.needsYou or RoomSession.awaitsPlan (iOS).");
});

test("quest cards refresh on task events only", async () => {
  const { questChanged } = await import("./quest-status.ts");
  const cases = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "fixtures", "quest-refresh.json"), "utf8"));
  assert.ok(cases.length >= 10 && cases.some((c) => c.refresh) && cases.some((c) => !c.refresh));
  assert.ok(cases.some((c) => c.event.type === "run.activity" && c.event.task_id && !c.refresh), "live ticks carry a task id and still never refetch");
  for (const { event, refresh, why } of cases) assert.equal(questChanged(event), refresh, why);
});
