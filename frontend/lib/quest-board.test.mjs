import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { sessionAwaitsPlan } from "./quest-status.ts";
import { acknowledgeRoomSettlements, applyRoomEvent, applyRoomSettlements, boardQuests, buildBoard, cleanVariants, decisionTarget, mixProblem, questionRecipient, resolveMix, roomAsk, roomEventMatches, roomEventSettlement, roomMentionHandles, roomScope, threadOf, crewCardSpec, crewOf, crewStats, mergeRoomMessages, pairQuote, summaryText } from "./quest-board.ts";
import { qk } from "./query-keys.ts";

const TASK = "task-1";
let clock = 0;
const at = () => `2026-11-27T10:${String(clock++).padStart(2, "0")}:00Z`;

function msg(id, fields = {}) {
  return {
    id,
    agent_id: "lead",
    author_kind: "agent",
    author_user_id: null,
    author_agent_id: "lead",
    body: id,
    created_at: at(),
    kind: "chat",
    meta: { task_id: TASK },
    ...fields,
  };
}

const question = (id, extra = {}) => msg(id, { kind: "question", decision_status: "open", asked_by_session: "s1", routes_to: { kind: "agent", id: "lead", name: "Abhi" }, ...extra });
const reply = (id, kind, q, extra = {}) => msg(id, { kind, meta: { task_id: TASK, answers_question: q }, ...extra });

test("questions pair with their answers and the answers fold into the thread", () => {
  const board = buildBoard([question("q1"), reply("a1", "decision", "q1", { answered_by: { kind: "agent", id: "lead", name: "Abhi" } }), msg("chat")]);
  assert.equal(board.threads.length, 1);
  assert.equal(board.threads[0].status, "answered");
  assert.equal(board.threads[0].answer.id, "a1");
  assert.deepEqual([...board.folded], ["a1"]);
  assert.equal(board.summary.answered_by_agent, 1);
  assert.equal(board.decisions.length, 1);
});

test("an assumed answer is a decision the helper made on its own", () => {
  const board = buildBoard([question("q1", { meta: { task_id: TASK, question: { about: "fact", assumption: "Use the v2 API" } } }), reply("x1", "assumed", "q1")]);
  assert.equal(board.threads[0].status, "assumed");
  assert.equal(board.summary.assumed, 1);
  assert.equal(board.decisions[0].question.meta.question.assumption, "Use the v2 API");
});

test("a passed up question needs you until a person answers its card", () => {
  const q = question("q1", { decision_status: "passed_up", meta: { task_id: TASK, question: { about: "taste" } } });
  const card = reply("n1", "needs_you", "q1", { meta: { task_id: TASK, answers_question: "q1", suggestion: "B", options: ["A", "B"] } });
  let board = buildBoard([q, card]);
  assert.deepEqual(board.needsYou.map((m) => m.id), ["n1"]);
  assert.equal(board.threads[0].status, "passed_up");
  assert.equal(board.threads[0].about, "taste");
  assert.equal(board.summary.passed_up, 1);

  const answer = msg("u1", { author_kind: "user", author_user_id: "me", author_agent_id: null, body: "B", meta: { answers: "n1", task_id: TASK } });
  board = buildBoard([q, card, answer]);
  assert.deepEqual(board.needsYou, []);
  assert.equal(board.threads[0].status, "answered");
  assert.equal(board.threads[0].answer.id, "u1");
  assert.equal(board.summary.answered_by_person, 1);
  assert.equal(board.summary.passed_up, 0);
  assert.ok(board.folded.has("u1") && board.folded.has("n1"));
});

test("the authoritative Needs you snapshot fills capped history and replaces pending pairs", () => {
  const oldQuestion = question("q-old", { decision_status: "passed_up" });
  const oldCard = reply("n-old", "needs_you", "q-old");
  const nextQuestion = question("q-next", { decision_status: "passed_up" });
  const nextCard = reply("n-next", "needs_you", "q-next");
  const timeline = Array.from({ length: 205 }, (_, i) => msg(`chat-${i}`));

  const capped = buildBoard(timeline, { needsYouMessages: [oldQuestion, oldCard] });
  assert.deepEqual(capped.needsYou.map((message) => message.id), ["n-old"]);
  assert.equal(threadOf(capped, "q-old").card.id, "n-old");

  const overlap = buildBoard([...timeline, oldQuestion, oldCard, nextQuestion, nextCard], { needsYouMessages: [oldQuestion, oldCard, nextQuestion, nextCard] });
  assert.equal(overlap.all.filter((thread) => thread.question.id === "q-old").length, 1);
  assert.deepEqual(overlap.needsYou.map((message) => message.id), ["n-old", "n-next"]);

  const newerTimelineCard = reply("n-old-newer", "needs_you", "q-old");
  const exactSnapshot = buildBoard([oldQuestion, newerTimelineCard], { needsYouMessages: [oldQuestion, oldCard] });
  assert.equal(threadOf(exactSnapshot, "q-old").card.id, "n-old");
  assert.deepEqual(exactSnapshot.needsYou.map((message) => message.id), ["n-old"]);

  const replaced = buildBoard([...timeline, oldQuestion, oldCard, nextQuestion, nextCard], { needsYouMessages: [nextQuestion, nextCard] });
  assert.deepEqual(replaced.needsYou.map((message) => message.id), ["n-next"]);
  assert.equal(replaced.summary.passed_up, 1);

  const cleared = buildBoard([...timeline, oldQuestion, oldCard, nextQuestion, nextCard], { needsYouMessages: [] });
  assert.deepEqual(cleared.needsYou, []);
  assert.equal(cleared.summary.passed_up, 0);

  const legacy = buildBoard([oldQuestion, oldCard]);
  assert.deepEqual(legacy.needsYou.map((message) => message.id), ["n-old"]);

  const visibleWithoutPrivateCard = buildBoard([], { needsYouMessages: [oldQuestion] });
  assert.equal(visibleWithoutPrivateCard.summary.passed_up, 1);
  assert.deepEqual(visibleWithoutPrivateCard.needsYou, []);
  assert.equal(threadOf(visibleWithoutPrivateCard, "q-old").card, null);
});

test("timeline settlements beat a stale pending snapshot and leave other pending cards focused", () => {
  const settledQuestion = question("q-settled", { decision_status: "passed_up" });
  const settledCard = reply("n-settled", "needs_you", "q-settled");
  const answer = msg("u-settled", { author_kind: "user", author_user_id: "me", author_agent_id: null, body: "Ship B", meta: { answers: "n-settled", task_id: TASK } });
  const pendingQuestion = question("q-pending", { decision_status: "passed_up" });
  const pendingCard = reply("n-pending", "needs_you", "q-pending");
  const loose = msg("n-loose", { kind: "needs_you", meta: { task_id: TASK, clarification: { task_id: TASK, session_id: "s1" } } });
  const board = buildBoard([answer, loose], { clarifying: new Set(["s1"]), needsYouMessages: [settledQuestion, settledCard, pendingQuestion, pendingCard, loose] });

  assert.equal(threadOf(board, "q-settled").status, "answered");
  assert.deepEqual(board.needsYou.map((message) => message.id), ["n-pending", "n-loose"]);
  assert.equal(threadOf(board, "q-pending").card.id, "n-pending");
  assert.deepEqual(buildBoard([loose], { clarifying: new Set(["s1"]), needsYouMessages: [] }).needsYou, []);
  assert.deepEqual(buildBoard([loose], { clarifying: new Set(["s1"]) }).needsYou.map((message) => message.id), ["n-loose"]);
  assert.deepEqual(buildBoard([], { clarifying: new Set(["s1"]), needsYouMessages: [loose] }).needsYou.map((message) => message.id), ["n-loose"]);
  assert.deepEqual(buildBoard([], { clarifying: new Set(), needsYouMessages: [loose] }).needsYou, []);
  const legacyReply = msg("u-loose", { author_kind: "user", author_user_id: "me", body: "Use B", meta: { answers: "n-loose", task_id: TASK } });
  assert.deepEqual(buildBoard([legacyReply], { clarifying: new Set(["s1"]), needsYouMessages: [loose] }).needsYou, []);
});

test("local settlements close snapshot-only questions across overlapping pages", () => {
  const pendingQuestion = question("q-snapshot", { decision_status: "passed_up" });
  const pendingCard = reply("n-snapshot", "needs_you", "q-snapshot");
  const settlements = new Map([["q-snapshot", "parked"]]);
  const settledSnapshot = applyRoomSettlements([pendingQuestion, pendingCard], settlements);
  const immediate = buildBoard([], { needsYouMessages: settledSnapshot });
  assert.equal(threadOf(immediate, "q-snapshot").status, "parked");
  assert.deepEqual(immediate.needsYou, []);

  const stalePage = applyRoomSettlements([pendingQuestion, pendingCard], settlements);
  const afterOverlap = buildBoard([], { needsYouMessages: stalePage });
  assert.equal(threadOf(afterOverlap, "q-snapshot").status, "parked");
  assert.deepEqual(afterOverlap.needsYou, []);
  acknowledgeRoomSettlements(settlements, [], [pendingQuestion, pendingCard]);
  assert.equal(settlements.get("q-snapshot"), "parked");

  const correctedQuestion = { ...pendingQuestion, decision_status: "answered" };
  const corrected = applyRoomSettlements([correctedQuestion, pendingCard], settlements);
  assert.equal(threadOf(buildBoard([], { needsYouMessages: corrected }), "q-snapshot").status, "answered");
  acknowledgeRoomSettlements(settlements, [correctedQuestion], [pendingQuestion, pendingCard]);
  assert.equal(settlements.has("q-snapshot"), false);

  settlements.set("q-snapshot", "parked");
  acknowledgeRoomSettlements(settlements, [], []);
  assert.equal(settlements.has("q-snapshot"), false);

  const event = { type: "session.answered", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q-snapshot", status: "answered" } };
  assert.deepEqual(roomEventSettlement(event, TASK), { questionId: "q-snapshot", status: "answered" });
  assert.equal(roomEventSettlement(event, "other"), null);

  const changedEvent = { type: "room.decision", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q-snapshot", status: "changed" } };
  assert.deepEqual(roomEventSettlement(changedEvent, TASK), { questionId: "q-snapshot", status: "answered" });
  const changedLive = applyRoomEvent({ messages: [pendingQuestion, pendingCard], members: [] }, changedEvent, TASK);
  assert.equal(threadOf(buildBoard([], { needsYouMessages: changedLive.messages }), "q-snapshot").status, "answered");
  assert.deepEqual(buildBoard([], { needsYouMessages: changedLive.messages }).needsYou, []);
  assert.equal(roomEventSettlement({ ...changedEvent, payload: { ...changedEvent.payload, status: "passed_up" } }, TASK), null);

  settlements.set("q-snapshot", "answered");
  acknowledgeRoomSettlements(settlements, [correctedQuestion], [pendingQuestion, pendingCard]);
  assert.equal(settlements.has("q-snapshot"), false);
});

test("a closed quest pins nothing as needs you", () => {
  const q = question("q1", { decision_status: "passed_up" });
  const card = reply("n1", "needs_you", "q1");
  assert.deepEqual(buildBoard([q, card], { status: "done" }).needsYou, []);
  assert.deepEqual(buildBoard([q, card], { status: "cancelled" }).needsYou, []);
  assert.equal(buildBoard([q, card], { status: "in_progress" }).needsYou.length, 1);
});

test("a parked question stays parked even when an answer arrives later", () => {
  const board = buildBoard([question("q1", { decision_status: "parked" }), reply("a1", "answer", "q1")]);
  assert.equal(board.threads[0].status, "parked");
  assert.equal(board.summary.parked, 1);
});

test("only the newest clarification card of a session still asking is pinned", () => {
  const old = msg("c1", { meta: { clarification: { session_id: "s1", task_id: TASK }, task_id: TASK } });
  const fresh = msg("c2", { meta: { clarification: { session_id: "s1", task_id: TASK }, task_id: TASK } });
  assert.deepEqual(buildBoard([old, fresh], { clarifying: new Set(["s1"]) }).needsYou.map((m) => m.id), ["c2"]);
  assert.deepEqual(buildBoard([old, fresh], { clarifying: new Set() }).needsYou, []);
  const answered = msg("u1", { author_kind: "user", meta: { answers: "c2" } });
  assert.deepEqual(buildBoard([old, fresh, answered], { clarifying: new Set(["s1"]) }).needsYou, []);
});

test("the room event reducer places a new question in the open room without a refetch", () => {
  const live = { messages: [msg("chat")], members: [] };
  const asked = {
    type: "session.asked",
    task_id: TASK,
    agent_id: "helper-agent",
    created_at: at(),
    payload: { task_id: TASK, session_id: "s2", question_id: "q9", question: "Which store keeps marks?", about: "choice", to: { kind: "agent", id: "lead", name: "Abhi" } },
  };
  const next = applyRoomEvent(live, asked, TASK);
  assert.equal(next.messages.length, 2);
  const q = next.messages[1];
  assert.equal(q.kind, "question");
  assert.equal(q.decision_status, "open");
  assert.equal(q.asked_by_session, "s2");
  assert.equal(q.meta.question.about, "choice");
  assert.equal(applyRoomEvent(next, asked, TASK), next, "a replayed event adds nothing");
  assert.equal(applyRoomEvent(live, { ...asked, task_id: "other", payload: { ...asked.payload, task_id: "other" } }, TASK), live, "other rooms are ignored");

  const answered = applyRoomEvent(next, { type: "session.answered", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q9", status: "answered", answer: "canvas_marks", by: { kind: "agent", id: "lead", name: "Abhi" } } }, TASK);
  const board = buildBoard(answered.messages);
  assert.equal(board.threads[0].status, "answered");
  assert.equal(board.threads[0].answerText, "canvas_marks");
  assert.equal(board.threads[0].by.name, "Abhi");

  const passed = applyRoomEvent(next, { type: "room.decision", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q9", status: "passed_up", by: null } }, TASK);
  assert.equal(buildBoard(passed.messages).threads[0].status, "passed_up");
  assert.equal(applyRoomEvent(next, { type: "room.decision", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q9", status: "bogus" } }, TASK), next);
});

test("member events add and remove explicit members, and are ignored before the server lists members", () => {
  const added = { type: "room.member_added", task_id: TASK, created_at: at(), payload: { task_id: TASK, member_kind: "agent", member_id: "pip", role: "guest", name: "Pip" } };
  const none = { messages: [], members: undefined };
  assert.equal(applyRoomEvent(none, added, TASK), none);
  const live = { messages: [], members: [{ member_kind: "agent", member_id: "lead", role: "lead", name: "Abhi", default: true }] };
  const withPip = applyRoomEvent(live, added, TASK);
  assert.deepEqual(withPip.members.map((m) => m.member_id), ["lead", "pip"]);
  assert.deepEqual(applyRoomEvent(withPip, added, TASK).members.length, 2);
  const removed = applyRoomEvent(withPip, { type: "room.member_removed", task_id: TASK, created_at: at(), payload: { task_id: TASK, member_kind: "agent", member_id: "pip" } }, TASK);
  assert.deepEqual(removed.members.map((m) => m.member_id), ["lead"]);
  const keepsDefault = applyRoomEvent(removed, { type: "room.member_removed", task_id: TASK, created_at: at(), payload: { task_id: TASK, member_kind: "agent", member_id: "lead" } }, TASK);
  assert.equal(keepsDefault, removed);
});

test("a refetch that has not caught up keeps the answer the event already showed", () => {
  const q = question("q1");
  const live = applyRoomEvent({ messages: [q], members: null }, { type: "session.answered", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q1", status: "answered", answer: "yes" } }, TASK);
  const merged = mergeRoomMessages(live.messages, [{ ...q }]);
  assert.equal(buildBoard(merged).threads[0].answerText, "yes");
  const settled = mergeRoomMessages(merged, [{ ...q, decision_status: "answered" }, reply("a1", "answer", "q1", { body: "yes, from memory" })]);
  assert.equal(buildBoard(settled).threads[0].answerText, "yes, from memory");
});

test("room refreshes preserve settled questions until an authoritative settled row arrives", () => {
  const q = question("q1");
  const parked = applyRoomEvent({ messages: [q], members: null }, { type: "room.decision", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q1", status: "parked" } }, TASK);
  const stale = mergeRoomMessages(parked.messages, [{ ...q }]);
  assert.equal(buildBoard(stale).threads[0].status, "parked");

  const parkedFromServer = mergeRoomMessages([q], [{ ...q, decision_status: "parked" }, reply("a1", "answer", "q1", { body: "Kept working" })]);
  assert.equal(buildBoard(parkedFromServer).threads[0].status, "parked");

  const corrected = mergeRoomMessages(parkedFromServer, [{ ...q, decision_status: "answered" }, reply("a2", "decision", "q1", { body: "Use the corrected value", answered_by: { kind: "person", id: "me", name: "You" } })]);
  const correctedBoard = buildBoard(corrected);
  assert.equal(correctedBoard.threads[0].status, "answered");
  assert.equal(correctedBoard.threads[0].answerText, "Use the corrected value");
});

test("the quest card summary reads like a sentence, skips zeros and credits only the lead's own decisions to it", () => {
  assert.equal(summaryText({ open: 0, answered_by_agent: 2, answered_by_lead: 2, answered_by_person: 0, passed_up: 1, assumed: 0, parked: 0 }, "Abhi"), "2 decided by Abhi, 1 needs you");
  assert.equal(summaryText({ open: 0, answered_by_agent: 3, answered_by_lead: 2, answered_by_person: 0, passed_up: 0, assumed: 0, parked: 0 }, "Abhi"), "2 decided by Abhi, 1 decided by another agent");
  assert.equal(summaryText({ open: 0, answered_by_agent: 2, answered_by_person: 0, passed_up: 0, assumed: 0, parked: 0 }, "Abhi"), "2 decided by agents");
  assert.equal(summaryText({ open: 1, answered_by_agent: 0, answered_by_person: 1, passed_up: 2, assumed: 1, parked: 1 }, "Abhi"), "1 decided by you, 1 assumed, 1 parked as a to-do, 1 question open, 2 need you");
  assert.equal(summaryText({ open: 0, answered_by_agent: 1, answered_by_lead: 1, answered_by_person: 0, passed_up: 0, assumed: 0, parked: 0 }, null), "1 decided by the lead");
  assert.equal(summaryText({ open: 0, answered_by_agent: 0, answered_by_person: 0, passed_up: 0, assumed: 0, parked: 0 }, "Abhi"), "");
  assert.equal(summaryText(undefined, "Abhi"), "");
});

test("the crew lists the lead first with live states, and falls back to sessions without members", () => {
  const agents = new Map([
    ["lead", { id: "lead", name: "Abhi", cli: "claude", model: "opus", status: "idle" }],
    ["pip", { id: "pip", name: "Pip", cli: "codex", model: "gpt-6.2-sol", status: "thinking", paused_reason: "Paused by owner" }],
  ]);
  const sessions = [
    { id: "s1", agent_id: "lead", role: "task", status: "active", running: true, backend: "codex", model: "gpt-6.2-sol", cost_usd: 0.4 },
    { id: "s2", agent_id: "lead", role: "task", status: "active", running: false, backend: "claude", model: "sonnet", cost_usd: 0.1 },
  ];
  const board = buildBoard([question("q1", { asked_by_session: "s2" })]);
  const label = () => "Executor";
  const fallback = crewOf({ members: undefined, sessions, agents, lead: "lead", threads: board.threads, sessionLabel: label, awaitsPlan: sessionAwaitsPlan });
  assert.deepEqual(fallback.map((c) => [c.key, c.state]), [["agent:lead", "deciding"], ["session:s1", "working"], ["session:s2", "waiting"]]);
  assert.deepEqual(fallback[1].runtime, { cli: "codex", model: "gpt-6.2-sol" });

  const members = [
    { member_kind: "person", member_id: "me", role: "owner", name: "Ana", default: true },
    { member_kind: "agent", member_id: "pip", role: "guest", name: "Pip" },
    { member_kind: "agent", member_id: "lead", role: "lead", name: "Abhi", default: true },
  ];
  const crew = crewOf({ members, sessions, agents, lead: "lead", threads: [], sessionLabel: label, awaitsPlan: sessionAwaitsPlan });
  assert.deepEqual(crew.map((c) => c.key), ["agent:lead", "agent:pip", "person:me"]);
  assert.equal(crew[1].state, "paused");
  assert.equal(crew[1].removable, true);
  assert.equal(crew[2].removable, false);

  const stats = crewStats(fallback, board, sessions);
  assert.deepEqual(stats, { helpers: 2, working: 1, open: 1, decided: 0, needsYou: 0, costUsd: 0.5 });
  const spec = crewCardSpec(stats, "$0.50");
  assert.equal(spec.elements.bar.props.value, 0);
  assert.equal(spec.elements.cost.props.value, "$0.50");
});

test("crew metrics use the scoped room aggregate beyond capped history", () => {
  const cappedBoard = buildBoard(Array.from({ length: 205 }, (_, i) => msg(`metric-chat-${i}`)));
  const scoped = { open: 7, answered_by_agent: 18, answered_by_person: 4, passed_up: 2, assumed: 3, parked: 1 };
  assert.deepEqual(crewStats([], cappedBoard, [], scoped), { helpers: 0, working: 0, open: 7, decided: 22, needsYou: 0, costUsd: 0 });

  const legacyBoard = buildBoard([question("q-legacy")]);
  assert.equal(crewStats([], legacyBoard, []).open, 1);
});

test("a canonical crew room keeps part sessions live without widening unrelated events", () => {
  const agents = new Map([
    ["lead", { id: "lead", name: "Abhi", cli: "claude", model: "opus", status: "idle" }],
    ["helper", { id: "helper", name: "Pip", cli: "codex", model: "gpt-6.2-sol", status: "idle" }],
  ]);
  const sessions = [
    { id: "parent-session", task_id: "parent", agent_id: "lead", role: "task", status: "active", running: true, backend: "claude", model: "opus", cost_usd: 0.5 },
    { id: "part-session", task_id: "part-1", agent_id: "helper", role: "task", status: "active", running: true, backend: "codex", model: "gpt-6.2-sol", cost_usd: 0.75 },
  ];
  const members = [
    { quest_id: "parent", member_kind: "agent", member_id: "lead", role: "lead", name: "Abhi", default: true },
    { quest_id: "parent", member_kind: "session", member_id: "part-session", role: "helper", name: "Pip", default: true, cli: "codex", model: "gpt-6.2-sol" },
  ];
  const crew = crewOf({ members, sessions, agents, lead: "lead", threads: [], sessionLabel: (session) => session.task_id === "part-1" ? "Part helper" : "Executor", awaitsPlan: sessionAwaitsPlan });
  const helper = crew.find((entry) => entry.sessionId === "part-session");
  assert.equal(helper.state, "working");
  assert.equal(helper.detail, "Part helper · working");
  assert.deepEqual(helper.runtime, { cli: "codex", model: "gpt-6.2-sol" });
  assert.equal(crewStats(crew, buildBoard([]), sessions).costUsd, 1.25);

  const scope = roomScope("parent", "parent", members, ["part-1"], sessions);
  assert.equal(scope.canonicalTaskId, "parent");
  assert.equal(roomEventMatches(scope, { task_id: "part-1" }), true);
  assert.equal(roomEventMatches(scope, { session_id: "part-session" }), true);
  assert.equal(roomEventMatches(scope, { task_id: "other" }), false);
  assert.equal(roomScope("part-1", null, members, [], sessions).canonicalTaskId, "parent");
  const beforePartSession = roomScope("parent", "parent", members, [], []);
  assert.equal(roomEventMatches(beforePartSession, { task_id: "parent" }), true);
});

test("quoting a pair keeps the question and who decided it", () => {
  const board = buildBoard([question("q1", { body: "Header A or B?" }), reply("a1", "decision", "q1", { body: "B", answered_by: { kind: "agent", id: "lead", name: "Abhi" } })]);
  assert.equal(pairQuote(board.threads[0], "Pip"), "Pip asked: Header A or B?\nAbhi decided: B");
});

test("the palette offers the quest boards that need you first", () => {
  const d = (over) => ({ open: 0, answered_by_agent: 0, answered_by_person: 0, passed_up: 0, assumed: 0, parked: 0, ...over });
  const quests = [
    { id: "a", status: "in_progress", decisions: d({ answered_by_agent: 2 }) },
    { id: "b", status: "in_progress", decisions: d({ passed_up: 1 }) },
    { id: "c", status: "done", decisions: d({ passed_up: 3 }) },
    { id: "d", status: "review", asking: "crew" },
    { id: "e", status: "in_progress" },
  ];
  assert.deepEqual(boardQuests(quests).map((q) => q.id), ["b", "d", "a"]);
  assert.deepEqual(boardQuests([{ id: "x", status: "todo" }, { id: "y", status: "in_progress" }]).map((q) => q.id), ["y"]);
  assert.deepEqual(boardQuests([{ id: "z", status: "done" }]), []);
});

test("a needs you notification opens the room on its card, and answers find their thread", () => {
  assert.deepEqual(roomAsk({ type: "attention.requested", task_id: TASK, payload: { question_id: "q1", message_id: "n1" } }), { taskId: TASK, focus: "n1" });
  assert.equal(roomAsk({ type: "attention.requested", task_id: TASK, payload: { text: "plain ask" } }), null);
  assert.equal(roomAsk({ type: "message.created", task_id: TASK, payload: { question_id: "q1" } }), null);
  const board = buildBoard([question("q1", { decision_status: "passed_up" }), reply("n1", "needs_you", "q1")]);
  assert.equal(threadOf(board, "n1").question.id, "q1");
  assert.equal(threadOf(board, "q1").question.id, "q1");
  assert.equal(threadOf(board, "zz"), null);
});

test("focused retired cards and replaced decisions resolve to their paired thread", () => {
  const replacedQuestion = question("q-replaced", { decision_status: "answered" });
  const oldDecision = reply("d-old", "decision", "q-replaced");
  const newDecision = reply("d-new", "decision", "q-replaced");
  const retiredQuestion = question("q-retired", { decision_status: "answered" });
  const retiredCard = reply("n-retired", "needs_you", "q-retired");
  const pendingQuestion = question("q-pending-focus", { decision_status: "passed_up" });
  const pendingCard = reply("n-pending-focus", "needs_you", "q-pending-focus");
  const board = buildBoard([replacedQuestion, oldDecision, newDecision, retiredQuestion, retiredCard], { needsYouMessages: [pendingQuestion, pendingCard] });

  assert.equal(threadOf(board, oldDecision.id).question.id, replacedQuestion.id);
  assert.equal(threadOf(board, oldDecision.id).answer.id, newDecision.id);
  assert.equal(threadOf(board, retiredCard.id).question.id, retiredQuestion.id);
  assert.deepEqual(board.needsYou.map((message) => message.id), [pendingCard.id]);
});

test("one message asked of several agents is one thread with each answer inline, live and after a refetch", () => {
  const asked = (qid, agent, name) => ({
    type: "session.asked",
    task_id: TASK,
    created_at: at(),
    payload: { task_id: TASK, question_id: qid, question: "Is the cache safe to ship?", about: "fact", post_id: "post-1", to: { kind: "agent", id: agent, name } },
  });
  let live = { messages: [msg("hello")], members: [] };
  live = applyRoomEvent(live, asked("q1", "pip", "Pip"), TASK);
  live = applyRoomEvent(live, asked("q2", "quill", "Quill"), TASK);
  let board = buildBoard(live.messages);
  assert.equal(board.threads.length, 1);
  assert.deepEqual(board.threads[0].group.map((t) => t.question.routes_to.name), ["Pip", "Quill"]);
  assert.ok(board.folded.has("q2"));
  assert.equal(board.all.length, 2);
  assert.equal(board.summary.open, 2);

  live = applyRoomEvent(live, { type: "session.answered", task_id: TASK, created_at: at(), payload: { task_id: TASK, question_id: "q2", status: "answered", answer: "Yes, behind the flag", by: { kind: "agent", id: "quill", name: "Quill" } } }, TASK);
  board = buildBoard(live.messages);
  const [pip, quill] = board.threads[0].group;
  assert.equal(pip.status, "open");
  assert.equal(quill.status, "answered");
  assert.equal(quill.answerText, "Yes, behind the flag");
  assert.equal(threadOf(board, "q2").question.id, "q1");

  const fetched = [
    question("q1", { routes_to: { kind: "agent", id: "pip", name: "Pip" }, meta: { task_id: TASK, question: { post_id: "post-1" } } }),
    question("q2", { decision_status: "answered", routes_to: { kind: "agent", id: "quill", name: "Quill" }, meta: { task_id: TASK, question: { post_id: "post-1" } } }),
    reply("a2", "answer", "q2", { body: "Yes, behind the flag" }),
    question("q3", { meta: { task_id: TASK, question: { post_id: "post-2" } } }),
  ];
  board = buildBoard(fetched);
  assert.deepEqual(board.threads.map((t) => t.group.map((g) => g.question.id)), [["q1", "q2"], ["q3"]]);
  assert.equal(board.threads[0].group[1].answer.id, "a2");
});

test("a crew mix slot left empty uses the server preview after quest and agent choices", () => {
  const lead = { cli: "claude", model: "opus" };
  const preview = [lead, { cli: "codex", model: "gpt-6.2-sol" }, { cli: "cursor", model: "composer-2.5" }];
  const mix = resolveMix(3, lead, [null, { model: "haiku" }, {}], [{ cli: "codex", model: "gpt-6.2-sol" }, null, null], preview);
  assert.deepEqual(mix.map((m) => [m.source, m.runtime.cli, m.runtime.model]), [["agent", "codex", "gpt-6.2-sol"], ["quest", "claude", "haiku"], ["loka", "cursor", "composer-2.5"]]);
  assert.deepEqual(resolveMix(3, lead, [], [], [lead, lead, lead]).map((m) => m.runtime), [lead, lead, lead]);
  assert.deepEqual(resolveMix(2, lead, [], [], []).map((m) => m.runtime), [null, null]);
  assert.deepEqual(cleanVariants([{ cli: " codex ", model: "gpt-6.2-sol" }, { cli: "", model: "" }, { model: "haiku" }], 3), [{ cli: "codex", model: "gpt-6.2-sol" }, null, { model: "haiku" }]);
  assert.deepEqual(cleanVariants([null, {}], 2), []);
  assert.deepEqual(cleanVariants([{ cli: "codex", model: "x" }, null, null, { cli: "cursor", model: "y" }], 2), [{ cli: "codex", model: "x" }, null]);
});

test("crew preview data is scoped to the selected agent and its runtime identity", () => {
  assert.notDeepEqual(qk.agent.crewPreview("a", "device", "claude", "opus"), qk.agent.crewPreview("b", "device", "claude", "opus"));
  assert.notDeepEqual(qk.agent.crewPreview("a", "device", "claude", "opus"), qk.agent.crewPreview("a", "device", "codex", "gpt-6.2-sol"));
});

test("decision corrections target an answer when there is one and the question for an assumption", () => {
  const settled = { question: question("q1"), answer: reply("a1", "decision", "q1") };
  const assumed = { question: question("q2"), answer: null };
  assert.equal(decisionTarget(settled), "a1");
  assert.equal(decisionTarget(assumed), "q2");
});

test("a passed-up question keeps the original recipient from its agent id", () => {
  const passedUp = question("q1", { agent_id: "guest", decision_status: "passed_up", routes_to: { kind: "person", id: "me", name: "You" } });
  assert.equal(questionRecipient(passedUp, new Map([["guest", { name: "Bramble" }]]), [], "Abhi"), "Bramble");
  assert.equal(questionRecipient(passedUp, new Map(), [{ member_kind: "agent", member_id: "guest", role: "guest", name: "Bramble" }], "Abhi"), "Bramble");
  assert.equal(questionRecipient(passedUp, new Map(), [], "Abhi"), "Abhi");
});

test("the room counts which decisions the lead made and which a guest agent made", () => {
  const messages = [
    question("q1"),
    reply("a1", "decision", "q1", { answered_by: { kind: "agent", id: "lead", name: "Abhi" } }),
    question("q2", { routes_to: { kind: "agent", id: "pip", name: "Pip" } }),
    reply("a2", "answer", "q2", { answered_by: { kind: "agent", id: "pip", name: "Pip" } }),
    question("q3"),
    reply("a3", "decision", "q3"),
  ];
  const board = buildBoard(messages, { lead: "lead" });
  assert.equal(board.summary.answered_by_agent, 3);
  assert.equal(board.summary.answered_by_lead, 2);
  assert.equal(summaryText(board.summary, "Abhi"), "2 decided by Abhi, 1 decided by another agent");
  assert.equal(buildBoard(messages).summary.answered_by_lead, undefined);
});

test("a helper slot with a CLI but no model blocks sending until it has one", () => {
  assert.equal(mixProblem([{ cli: "codex", model: "gpt-6.2-sol" }, null, { model: "haiku" }], 3), null);
  assert.equal(mixProblem([null, { cli: "cursor", model: " " }], 3), "Pick a model for helper 2, or set it back to Loka picks.");
  assert.equal(mixProblem([null, null, null, { cli: "codex", model: "" }], 3), null);
  assert.deepEqual(cleanVariants([{ cli: "codex", model: "gpt-6.2-sol" }, null], 2), [{ cli: "codex", model: "gpt-6.2-sol" }, null]);
  const saved = cleanVariants([null, { model: "sonnet" }, null, { cli: "cursor", model: "composer-2.5" }], 4);
  const edited = saved.map((slot, i) => i === 0 ? { model: "haiku" } : slot);
  assert.deepEqual(cleanVariants(edited, 4), [{ model: "haiku" }, { model: "sonnet" }, null, { cli: "cursor", model: "composer-2.5" }]);
  assert.equal(mixProblem([null, null, null, { cli: "cursor", model: "" }], 4), "Pick a model for helper 4, or set it back to Loka picks.");
});

test("the shared send path keeps guest decisions in the canonical room", () => {
  const api = readFileSync(new URL("./world.ts", import.meta.url), "utf8");
  const crew = readFileSync(new URL("./quest-room-api.ts", import.meta.url), "utf8");
  const crewUi = readFileSync(new URL("../components/world/crew-ui.tsx", import.meta.url), "utf8");
  const room = readFileSync(new URL("../components/world/quest-room.tsx", import.meta.url), "utf8");
  assert.match(crew, /DEFAULT_CREW = 3/);
  assert.match(crew, /MAX_CREW = CREW_SIZES\[CREW_SIZES\.length - 1\]/);
  assert.match(crewUi, /preview\.isSuccess \? preview\.data\.defaults : \[\]/);
  assert.match(api, /quote: quote \?\? undefined, changes/);
  assert.match(room, /const recipient = thread\.question\.agent_id\.trim\(\);/);
  assert.match(room, /worldApi\.send\(recipient, text, \[\], undefined, undefined, undefined, scope\.canonicalTaskId,/);
  assert.match(room, /worldApi\.send\(card\.agent_id, text, \[\], undefined, card\.id, undefined, scope\.canonicalTaskId\)/);
  assert.doesNotMatch(room, /worldApi\.send\(main\.id, text,/);
  assert.match(room, /source: `msg:\$\{target\}`/);
  assert.match(room, /}, target\);/);
});

test("room chat stays quiet and quick asks are explicit", () => {
  const room = readFileSync(new URL("../components/world/quest-room.tsx", import.meta.url), "utf8");
  const boardUi = readFileSync(new URL("../components/world/quest-board-ui.tsx", import.meta.url), "utf8");
  assert.match(room, /value: "room", label: "Room"/);
  assert.match(room, /canonicalTaskId=\{scope\.canonicalTaskId\}/);
  assert.match(room, /await roomApi\.post\(canonicalTaskId, body\);/);
  assert.match(room, /await roomApi\.post\(canonicalTaskId, body, \[quick\.id\]\);/);
  assert.doesNotMatch(room, /mentionedIn/);
  assert.match(room, /key: entry\.key/);
  assert.match(room, /entry\.kind === "person" \? <PersonAvatar/);
  assert.match(room, /const human = message\.author_kind === "user";/);
  assert.match(room, /const mine = human && !!meId && message\.author_user_id === meId;/);
  assert.match(room, /const person = human \? members\?\.find/);
  assert.match(room, /human \? \(meta\.room_only/);
  assert.match(room, /mine \? "ml-auto bg-w-teal\/15" : "bg-w-card"/);
  assert.match(room, /meta\.room_only \? `\$\{who\} in the room`/);
  assert.match(room, /room\\\.part_added\$/);
  assert.match(room, /setNeedsYouMessages\(room\.needs_you_messages \? applyRoomSettlements\(room\.needs_you_messages, localSettlements\.current\) : undefined\)/);
  assert.match(room, /setDecisionSummary\(room\.decisions\)/);
  assert.match(room, /crewStats\(crew, board, sessions, decisionSummary\)/);
  assert.match(room, /localSettlements\.current\.set\(questionId, status\)/);
  assert.match(room, /setNeedsYouMessages\(\(current\) => current \? applyRoomSettlements/);
  assert.match(boardUi, /thread\.question\.id === focus \|\| thread\.card\?\.id === focus \|\| thread\.answer\?\.id === focus/);
  assert.match(boardUi, /thread\.card \? `\$\{who\} passed it to you` : `Waiting for \$\{who\}'s owner`/);
  assert.match(boardUi, /thread\.card \? "Answer it in Needs you at the top\." : "Only the agent's owner can answer this question\."/);
  assert.match(boardUi, /if \(!text\.trim\(\) \|\| busy\) return;/);
  assert.match(boardUi, /void onSend\(text\)\.then/);
  assert.deepEqual(roomMentionHandles(["pip", "piphelper", "piphelper", "piphelper"]), ["pip", "piphelper", "piphelper2", "piphelper3"]);
});

test("room membership follows edition and canonical ownership", () => {
  const room = readFileSync(new URL("../components/world/quest-room.tsx", import.meta.url), "utf8");
  const boardUi = readFileSync(new URL("../components/world/quest-board-ui.tsx", import.meta.url), "utf8");
  assert.match(room, /member\.default && member\.member_kind === "person" && member\.role === "owner"/);
  assert.match(room, /ownerId=\{roomOwnerId\}/);
  assert.match(boardUi, /\.\.\.\(!SOLO \? people\.filter/);
  assert.match(boardUi, /SOLO \? "Add an agent to this room"/);
  assert.match(boardUi, /Choose Ask to get a quick answer\./);
});
