import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { QUEST_STAGE_LABEL, needsYouFirst, questAwaitsPlan, questEventTaskId, questNeedsYou, questOpen, questStage, sessionAwaitsPlan } from "./quest-status.ts";

test("an open quest needs you while its plan or a passed-up decision waits", () => {
  assert.equal(questNeedsYou({ status: "in_progress" }), false);
  assert.equal(questNeedsYou({ status: "in_progress", plan_waiting: false }), false);
  assert.equal(questNeedsYou({ status: "in_progress", plan_waiting: true }), true);
  assert.equal(questNeedsYou({ status: "todo", plan_waiting: true }), true);
  assert.equal(questNeedsYou({ status: "failed", plan_waiting: true }), true);
  assert.equal(questNeedsYou({ status: "done", plan_waiting: true }), false);
  assert.equal(questNeedsYou({ status: "cancelled", plan_waiting: true }), false);
  assert.equal(questNeedsYou({ status: "in_progress", plan_waiting: null }), false);
  assert.equal(questNeedsYou({ status: "in_progress", decisions: { passed_up: 0 } }), false);
  assert.equal(questNeedsYou({ status: "in_progress", decisions: { passed_up: 1 } }), true);
  assert.equal(questNeedsYou({ status: "review", decisions: { passed_up: 2 } }), true);
  assert.equal(questNeedsYou({ status: "done", decisions: { passed_up: 1 } }), false);
  assert.equal(questNeedsYou({ status: "cancelled", decisions: { passed_up: 1 } }), false);
  assert.equal(questAwaitsPlan({ status: "review", plan_waiting: true }), true);
  assert.equal(questAwaitsPlan({ status: "review", decisions: { passed_up: 1 } }), false);
  assert.equal(questAwaitsPlan({ status: "done", plan_waiting: true }), false);
  assert.equal(questOpen({ status: "review" }), true);
  assert.equal(questOpen({ status: "done" }), false);
});

test("a session waits on a plan only while it is not archived", () => {
  assert.equal(sessionAwaitsPlan({ status: "active", plan_state: "awaiting" }), true);
  assert.equal(sessionAwaitsPlan({ status: "archived", plan_state: "awaiting" }), false);
  assert.equal(sessionAwaitsPlan({ status: "active", plan_state: "planning" }), false);
  assert.equal(sessionAwaitsPlan({ status: "active", plan_state: null }), false);
});

test("quest search puts Needs you first without changing relevance within each group", () => {
  const quests = [
    { id: "first", status: "in_progress" },
    { id: "plan", status: "in_progress", plan_waiting: true },
    { id: "second", status: "review" },
    { id: "question", status: "in_progress", decisions: { passed_up: 1 } },
    { id: "third", status: "todo" },
  ];
  assert.deepEqual(needsYouFirst(quests).map((quest) => quest.id), ["plan", "question", "first", "second", "third"]);
  assert.deepEqual(quests.map((quest) => quest.id), ["first", "plan", "second", "question", "third"]);
});

test("quest stages follow the status and the shared Needs you rule", () => {
  assert.equal(questStage({ status: "in_progress" }), "working");
  assert.equal(questStage({ status: "in_progress", plan_waiting: true }), "needs_you");
  assert.equal(questStage({ status: "in_progress", decisions: { passed_up: 1 } }), "needs_you");
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
          const raw = /\bplan_waiting\b/.test(line) && !/questNeedsYou|questAwaitsPlan|QuestStatus\.needsYou|var plan_waiting|plan_waiting\?:/.test(line);
          if (awaiting || raw) found.push(`${rel}:${i + 1} ${line.trim().slice(0, 100)}`);
        });
    }
  }
  assert.deepEqual(found, [], "Use questNeedsYou, questAwaitsPlan or sessionAwaitsPlan (web) and QuestStatus.needsYou or RoomSession.awaitsPlan (iOS).");
});

test("quest cards refresh on task events only", async () => {
  const { questChanged } = await import("./quest-status.ts");
  const cases = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "fixtures", "quest-refresh.json"), "utf8"));
  assert.ok(cases.length >= 10 && cases.some((c) => c.refresh) && cases.some((c) => !c.refresh));
  assert.ok(cases.some((c) => c.event.type === "run.activity" && c.event.task_id && !c.refresh), "live ticks carry a task id and still never refetch");
  for (const { event, refresh, why } of cases) assert.equal(questChanged(event), refresh, why);
  const routed = cases.find((c) => c.event.type === "task.changed" && c.event.task_id === null).event;
  assert.equal(routed.agent_id, null);
  assert.deepEqual(routed.payload, { task_id: routed.payload.task_id, quiet: true });
  assert.equal(questEventTaskId(routed), routed.payload.task_id, "routing-only changes keep their quest id in the payload");
  const deleted = cases.find((c) => c.event.type === "task.deleted" && c.event.agent_id !== null).event;
  assert.equal(deleted.task_id, null);
  assert.equal(questEventTaskId(deleted), deleted.payload.task_id, "deleted quests keep a routing id after their row is gone");
  const routedDeleted = cases.find((c) => c.event.type === "task.deleted" && c.event.agent_id === null).event;
  assert.notEqual(routed.payload.task_id, deleted.payload.task_id);
  assert.equal(routedDeleted.task_id, null);
  assert.equal(routedDeleted.run_id, null);
  assert.equal(routedDeleted.session_id, null);
  assert.equal(routedDeleted.actor_user_id, null);
  assert.deepEqual(routedDeleted.payload, { task_id: routed.payload.task_id, quiet: true });
});

test("quest cards and the board share reads, refresh from the first event, and isolate accounts", async () => {
  const timers = [];
  const pending = [];
  const schedule = (run) => {
    timers.push(run);
    return timers.length;
  };
  const request = () => new Promise((resolve) => pending.push(resolve));
  const { createQuestStore } = await import("./live-quests.ts");
  const dormant = createQuestStore(request, schedule, () => {});
  dormant.invalidate();
  assert.equal(timers.length, 0, "task events do not fetch quests while no card or board is mounted");
  dormant.dispose();
  const store = createQuestStore(request, schedule, () => {});
  let cards = 0;
  let board = 0;
  store.subscribe(() => cards++);
  store.subscribe(() => board++);
  const cardRead = store.load();
  const boardRead = store.load();
  assert.equal(pending.length, 1, "mounted cards and board coalesce their request");
  pending.shift()([{ id: "a", title: "First" }]);
  await Promise.all([cardRead, boardRead]);
  assert.equal(cards, 1);
  assert.equal(board, 1);

  for (let i = 0; i < 50; i++) store.invalidate();
  assert.equal(timers.length, 1, "steady events keep the timer started by the first event");
  await store.load();
  assert.equal(pending.length, 0, "a card mounted during the burst waits for its first-event timer");
  timers.shift()();
  assert.equal(pending.length, 1);
  store.invalidate();
  timers.shift()();
  pending.shift()([{ id: "stale", title: "Stale" }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(pending.length, 1, "an event during a refresh queues a fresh read");
  assert.equal(store.snapshot()[0].id, "stale", "the current account applies its response during steady traffic");
  pending.shift()([{ id: "fresh", title: "Fresh" }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(store.snapshot()[0].id, "fresh");

  let finishOld;
  const old = createQuestStore(() => new Promise((resolve) => { finishOld = resolve; }), schedule, () => {});
  void old.load();
  old.dispose();
  const next = createQuestStore(() => Promise.resolve([{ id: "next", title: "Next" }]), schedule, () => {});
  await next.load();
  finishOld([{ id: "old", title: "Old" }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(old.snapshot(), undefined, "a signed-out account drops its late response");
  assert.equal(next.snapshot()[0].id, "next", "a late response cannot populate the next account");

  const paused = [];
  const sameAccount = createQuestStore(() => new Promise((resolve) => paused.push(resolve)), schedule, () => {}, false);
  const leave = sameAccount.subscribe(() => {});
  void sameAccount.load();
  leave();
  paused.shift()([{ id: "left", title: "Left" }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  const returnToAccount = sameAccount.subscribe(() => {});
  void sameAccount.load();
  assert.equal(paused.length, 1, "leaving mid-request makes the same account refetch when a card returns");
  paused.shift()([{ id: "returned", title: "Returned" }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(sameAccount.snapshot()[0].id, "returned");
  returnToAccount();

  const retries = [];
  let attempts = 0;
  const failed = createQuestStore(
    () => attempts++ === 0 ? Promise.reject(new Error("offline")) : Promise.resolve([{ id: "recovered", title: "Recovered" }]),
    (run) => {
      retries.push(run);
      return retries.length;
    },
    () => {},
    false,
  );
  const stopFailed = failed.subscribe(() => {});
  await failed.load();
  assert.deepEqual(failed.snapshot(), [], "an initial failure leaves mounted pickers in their empty state");
  failed.invalidate();
  retries.shift()();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(failed.snapshot()[0].id, "recovered", "the next task event retries an initial failure");
  stopFailed();

  const boardSource = readFileSync(join(repo, "frontend/components/world/panels.tsx"), "utf8");
  const cardSource = readFileSync(join(repo, "frontend/components/world/embeds.tsx"), "utf8");
  const dialogSource = readFileSync(join(repo, "frontend/components/world/world-game.tsx"), "utf8");
  const pickerSource = readFileSync(join(repo, "frontend/components/world/canvas-embed.tsx"), "utf8");
  assert.match(boardSource, /const tasks = useLiveQuests\(\) \?\? \[\]/);
  assert.doesNotMatch(boardSource.slice(boardSource.indexOf("export function QuestBoard")), /worldApi\.tasks\(\)/);
  assert.match(cardSource, /const quests = useLiveQuests\(\)/);
  assert.match(dialogSource, /const liveTasks = useLiveQuests\(access\.read\)/);
  assert.doesNotMatch(dialogSource, /worldApi\s*\.\s*tasks\s*\(/);
  assert.match(pickerSource, /const quests = useLiveQuests\(kind === "quest"\)/);
  assert.doesNotMatch(pickerSource, /worldApi\s*\.\s*tasks\s*\(/);
});
