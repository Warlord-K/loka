import { test } from "node:test";
import assert from "node:assert/strict";
import {
  BARGE_CONFIRM_MS,
  BargeGuard,
  CallTurns,
  CallUtterances,
  ClientTurns,
  ECHO_LIMIT,
  PREROLL_CHUNKS,
  RESERVATION_MS,
  ReplyWait,
  TURN_MAX_MS,
  TURN_SILENCE_MS,
  joinSegments,
  serverTurnsApplied,
  sessionFallback,
  spokenUpdate,
} from "./call-turns.ts";

test("each turn is saved once, in order", () => {
  const turns = new CallTurns();
  turns.set("agent", "greet", "Hey Sam, what's up?");
  turns.set("user", "u-1", "Check the build");
  const first = turns.unsaved();
  assert.deepEqual(
    first.map((l) => [l.role, l.text]),
    [
      ["agent", "Hey Sam, what's up?"],
      ["user", "Check the build"],
    ],
  );
  turns.markSaved(first);
  assert.deepEqual(turns.unsaved(), []);
});

test("an agent turn is saved while it is still being said, then finished in place", () => {
  const turns = new CallTurns();
  turns.set("agent", "a-1", "Sure, I am", true);
  const early = turns.unsaved();
  assert.deepEqual(early, [{ role: "agent", key: "a-1", text: "Sure, I am", partial: true }]);
  turns.markSaved(early);
  turns.set("agent", "a-1", "Sure, I am looking", true);
  assert.equal(turns.unsaved()[0].text, "Sure, I am looking");
  turns.markSaved(turns.unsaved());
  turns.set("agent", "a-1", "Sure, I am looking at it now.");
  assert.deepEqual(turns.unsaved(), [{ role: "agent", key: "a-1", text: "Sure, I am looking at it now." }]);
  assert.equal(turns.transcript().length, 1);
});

test("a call cut mid sentence still has the partial agent turn to save", () => {
  const turns = new CallTurns();
  turns.set("user", "u-1", "What did the review say?");
  turns.markSaved(turns.unsaved());
  turns.set("agent", "a-2", "Rex found two things, the first", true);
  assert.deepEqual(turns.unsaved(), [{ role: "agent", key: "a-2", text: "Rex found two things, the first", partial: true }]);
});

test("a failed save is retried with the latest text", () => {
  const turns = new CallTurns();
  turns.set("agent", "a-1", "One", true);
  const lost = turns.unsaved();
  turns.set("agent", "a-1", "One two", true);
  assert.deepEqual(turns.unsaved(), [{ role: "agent", key: "a-1", text: "One two", partial: true }]);
  assert.equal(lost[0].text, "One");
});

test("a longer transcript of the same spoken turn replaces it", () => {
  const turns = new CallTurns();
  turns.set("user", "u-1", "Open the");
  turns.markSaved(turns.unsaved());
  assert.equal(turns.set("user", "u-1", "Open the report on sprites"), true);
  assert.equal(turns.unsaved()[0].text, "Open the report on sprites");
  assert.equal(turns.set("user", "u-1", "  "), false);
  assert.equal(turns.set("user", "u-1", "Open the report on sprites"), false);
});

test("a question whose transcript arrives late is still saved before the answer", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-2", 1000);
  turns.set("agent", "a-2", "The build is green", true);
  assert.deepEqual(turns.unsaved(1500), [], "the answer waits for the question");
  assert.equal(turns.holding(), true);
  turns.set("user", "u-2", "Is the build green?");
  assert.deepEqual(
    turns.unsaved(1600).map((l) => [l.role, l.text]),
    [
      ["user", "Is the build green?"],
      ["agent", "The build is green"],
    ],
  );
  assert.equal(turns.holding(), false);
});

test("a question with no words never blocks the answer for long", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-3", 1000);
  turns.set("agent", "a-3", "Did you say something?", true);
  assert.deepEqual(turns.unsaved(1000 + RESERVATION_MS + 1).map((l) => l.key), ["a-3"]);
  assert.equal(turns.holding(), false);
});

test("released and final saves skip empty reservations", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-4", 1000);
  turns.release("u-4");
  assert.equal(turns.holding(), false);
  turns.reserve("user", "u-5", 1000);
  turns.set("agent", "a-5", "Bye for now", true);
  assert.deepEqual(turns.unsaved(1001, false).map((l) => l.key), ["a-5"], "hanging up saves what the agent said");
  assert.deepEqual(turns.transcript().map((l) => l.key), ["a-5"]);
});

test("releasing a filled turn keeps it", () => {
  const turns = new CallTurns();
  turns.reserve("user", "u-6", 1000);
  turns.set("user", "u-6", "Thanks");
  turns.release("u-6");
  assert.deepEqual(turns.transcript().map((l) => l.text), ["Thanks"]);
});

test("pauses in one utterance never save a prefix, only the whole thing once", () => {
  const turns = new CallTurns();
  const said = new CallUtterances(turns);
  turns.reserve("user", "u-i1", 1000);
  said.hear("i1", "Okay and");
  said.hear("i1", "Okay and one");
  assert.deepEqual(turns.unsaved(1100), [], "nothing is saved while the person is still talking");
  turns.reserve("user", "u-i2", 1200);
  said.hear("i2", "more thing,");
  said.hear("i2", "more thing, check the build");
  turns.release("u-i2");
  turns.set("agent", "a-1", "Sure, checking", true);
  assert.deepEqual(turns.unsaved(1300), [], "the answer waits behind the open question");
  assert.equal(turns.holding(), true);
  assert.deepEqual(said.close(), { key: "u-i1", text: "Okay and one more thing, check the build", open: false });
  const lines = turns.unsaved(1400);
  assert.deepEqual(
    lines.map((l) => [l.role, l.key, l.text]),
    [
      ["user", "u-i1", "Okay and one more thing, check the build"],
      ["agent", "a-1", "Sure, checking"],
    ],
  );
  turns.markSaved(lines);
  assert.deepEqual(turns.unsaved(1500), []);
  assert.equal(said.close(), null);
});

test("the agent hears the open utterance in full before it is saved", () => {
  const turns = new CallTurns();
  const said = new CallUtterances(turns);
  said.hear("i1", "I mean you should be");
  said.hear("i2", "able to open it from the side chat");
  assert.deepEqual(turns.transcript(), [{ role: "user", key: "u-i1", text: "I mean you should be able to open it from the side chat" }]);
});

test("a cumulative or repeated transcript replaces the segments instead of doubling them", () => {
  const turns = new CallTurns();
  const said = new CallUtterances(turns);
  said.hear("i1", "I would say");
  said.hear("i2", "I would say ship it today");
  said.hear("i1", "I would say");
  assert.equal(said.close()?.text, "I would say ship it today");
});

test("a later, longer transcript of a finished utterance updates it in place", () => {
  const turns = new CallTurns();
  const said = new CallUtterances(turns);
  said.hear("i1", "Okay and one");
  said.close();
  turns.markSaved(turns.unsaved());
  assert.deepEqual(said.hear("i1", "Okay and one last thing"), { key: "u-i1", text: "Okay and one last thing", open: false });
  assert.deepEqual(turns.unsaved(), [{ role: "user", key: "u-i1", text: "Okay and one last thing" }]);
  said.hear("i2", "Thanks");
  assert.equal(said.isOpen(), true);
  assert.deepEqual(turns.unsaved(), [{ role: "user", key: "u-i1", text: "Okay and one last thing" }], "the next utterance waits until it is finished");
});

test("hanging up saves an utterance that was still open", () => {
  const turns = new CallTurns();
  const said = new CallUtterances(turns);
  said.hear("i1", "Bye for");
  assert.deepEqual(turns.unsaved(Date.now(), false), [{ role: "user", key: "u-i1", text: "Bye for" }]);
});

test("segments join in the order they were spoken, even when transcripts arrive out of order", () => {
  const turns = new CallTurns();
  const said = new CallUtterances(turns);
  said.spoken("i1");
  said.spoken("i2");
  said.hear("i2", "check the build");
  said.hear("i1", "Okay and one more thing,");
  assert.equal(said.close()?.text, "Okay and one more thing, check the build");
});

test("pieces that only share a first word are both kept", () => {
  assert.equal(joinSegments(["No.", "not today, maybe tomorrow"]), "No. not today, maybe tomorrow");
  assert.equal(joinSegments(["So", "something broke in the build"]), "So something broke in the build");
  assert.equal(joinSegments(["Okay and one more thing,", "Okay?"]), "Okay and one more thing, Okay?");
  assert.equal(joinSegments(["I would say", "I would say ship it today"]), "I would say ship it today");
  assert.equal(joinSegments(["Yes", "Yes"]), "Yes Yes");
});

test("talking over the agent's answer keeps one message for the whole utterance", () => {
  const turns = new CallTurns();
  const said = new CallUtterances(turns);
  const wait = new ReplyWait();
  said.hear("i1", "Okay and one");
  wait.replied(false);
  assert.equal(wait.heard(true, false), false, "the answer is still playing, so the turn stays open");
  assert.deepEqual(turns.unsaved(), []);
  wait.interrupted();
  said.hear("i2", "more thing, check the build");
  assert.equal(wait.heard(false, false), false, "a cut answer never closes the turn");
  wait.replied(false);
  assert.equal(wait.heard(false, true), false, "still generating");
  assert.equal(wait.heard(false, false), true, "played out uncut");
  said.close();
  assert.deepEqual(turns.unsaved(), [{ role: "user", key: "u-i1", text: "Okay and one more thing, check the build" }]);
});

test("a cut answer never closes the turn even after it finishes playing", () => {
  const wait = new ReplyWait();
  wait.replied(true);
  assert.equal(wait.heard(false, false), false);
});

test("talking over the agent ducks it at once and the provider confirms the barge-in", () => {
  const guard = new BargeGuard();
  assert.equal(guard.local(0), true);
  assert.equal(guard.ducking(), true);
  guard.server();
  assert.equal(guard.ducking(), false);
  assert.equal(guard.expired(BARGE_CONFIRM_MS * 2), false);
  assert.deepEqual(guard.stats(), { echoes: 0, confirmed: 1, local_off: false });
});

test("the agent's own echo only ducks it briefly and stops ducking after repeats", () => {
  const guard = new BargeGuard();
  let now = 0;
  for (let i = 0; i < ECHO_LIMIT; i++) {
    assert.equal(guard.local(now), true);
    assert.equal(guard.expired(now + BARGE_CONFIRM_MS - 1), false);
    now += BARGE_CONFIRM_MS;
    assert.equal(guard.expired(now), true, "the volume comes back when nobody really spoke");
    now += 10;
  }
  assert.equal(guard.local(now), false, "echo cannot quiet the agent again");
  assert.deepEqual(guard.stats(), { echoes: ECHO_LIMIT, confirmed: 0, local_off: true });
  guard.server();
  assert.equal(guard.local(now), true, "a real interruption turns local ducking back on");
});

const SESSION = {
  voice: "liora",
  turn_detection: { type: "server_vad", silence_duration_ms: 1200 },
  audio: { input: { format: { type: "audio/pcm" }, transcription: { model: "grok-transcribe" } }, output: { format: { type: "audio/pcm" } } },
};

test("a refused voice is dropped first and every other setting is kept", () => {
  const next = sessionFallback(SESSION, "Invalid voice: liora", new Set());
  assert.equal(next.dropped, "voice");
  assert.equal("voice" in next.session, false);
  assert.deepEqual(next.session.turn_detection, SESSION.turn_detection);
  assert.deepEqual(next.session.audio, SESSION.audio);
});

test("other refusals drop transcription, then the voice, then give up", () => {
  const tried = new Set();
  const first = sessionFallback(SESSION, "bad request", tried);
  assert.equal(first.dropped, "transcription");
  assert.equal(first.session.audio.input.transcription, undefined);
  assert.deepEqual(first.session.audio.input.format, SESSION.audio.input.format);
  assert.deepEqual(first.session.turn_detection, SESSION.turn_detection);
  tried.add(first.dropped);
  const second = sessionFallback(first.session, "bad request", tried);
  assert.equal(second.dropped, "voice");
  assert.deepEqual(second.session.turn_detection, SESSION.turn_detection);
  tried.add(second.dropped);
  const third = sessionFallback(second.session, "bad request", tried);
  assert.equal(third.dropped, "turns");
  assert.equal(third.session.turn_detection, null);
  tried.add(third.dropped);
  assert.equal(sessionFallback(third.session, "bad request", tried), null);
});

test("refused turn settings switch to client turns first and keep the voice and transcription", () => {
  const next = sessionFallback(SESSION, "Unknown field turn_detection.silence_duration_ms", new Set());
  assert.equal(next.dropped, "turns");
  assert.equal(next.session.turn_detection, null);
  assert.equal(next.session.voice, "liora");
  assert.deepEqual(next.session.audio, SESSION.audio);
});

test("server turns count only when the provider confirms at least the asked silence", () => {
  const sent = { turn_detection: { type: "server_vad", silence_duration_ms: 1200 } };
  assert.equal(serverTurnsApplied(sent, { turn_detection: { type: "server_vad", silence_duration_ms: 1200 } }), true);
  assert.equal(serverTurnsApplied(sent, { turn_detection: { type: "server_vad", silence_duration_ms: 1500 } }), true);
  assert.equal(serverTurnsApplied(sent, { turn_detection: { type: "server_vad", silence_duration_ms: 200 } }), false);
  assert.equal(serverTurnsApplied(sent, { turn_detection: { type: "server_vad" } }), false);
  assert.equal(serverTurnsApplied(sent, {}), false);
  assert.equal(serverTurnsApplied({ turn_detection: null }, sent), false);
});

test("client turns stay open through short pauses and commit after a real pause", () => {
  const gate = new ClientTurns();
  assert.deepEqual(gate.audio("before"), []);
  const start = gate.tick(0, true);
  assert.equal(start.opened, true);
  assert.deepEqual(start.flush, ["before"], "the words just before the turn opened are kept");
  assert.deepEqual(gate.audio("during"), ["during"]);
  assert.equal(gate.tick(100, false).commit, false);
  assert.equal(gate.tick(100 + TURN_SILENCE_MS - 1, false).commit, false, "a pause under the limit keeps the turn open");
  assert.equal(gate.tick(1400, true).commit, false);
  assert.equal(gate.tick(1500, false).commit, false, "speaking again restarts the pause");
  const end = gate.tick(1500 + TURN_SILENCE_MS, false);
  assert.equal(end.commit, true);
  assert.equal(end.respond, true);
  assert.equal(gate.isOpen(), false);
  assert.equal(gate.tick(9000, false).commit, false, "nothing to commit while quiet");
});

test("client turns keep only the latest audio before speech and cap a turn's length", () => {
  const gate = new ClientTurns();
  for (let i = 0; i < PREROLL_CHUNKS + 3; i++) gate.audio(i);
  const start = gate.tick(0, true);
  assert.equal(start.flush.length, PREROLL_CHUNKS);
  assert.equal(start.flush[PREROLL_CHUNKS - 1], PREROLL_CHUNKS + 2);
  assert.equal(gate.tick(TURN_MAX_MS - 1, true).commit, false);
  const long = gate.tick(TURN_MAX_MS, true);
  assert.equal(long.commit, true, "a long turn sends what it has");
  assert.equal(long.respond, false, "but the agent waits for the pause");
  assert.equal(gate.isOpen(), true);
  assert.equal(gate.tick(TURN_MAX_MS + 40, true).commit, false, "nothing new to send yet");
  gate.audio("more");
  const quiet = TURN_MAX_MS + 80;
  assert.equal(gate.tick(quiet, false).commit, false, "the pause timer starts at the first quiet moment");
  const done = gate.tick(quiet + TURN_SILENCE_MS, false);
  assert.deepEqual([done.commit, done.respond], [true, true]);
  gate.close();
  assert.equal(gate.isOpen(), false);
  assert.deepEqual(gate.tick(TURN_MAX_MS * 3, true).flush, []);
});

test("a turn with no audio sent never asks the agent to answer", () => {
  const gate = new ClientTurns();
  assert.equal(gate.tick(0, true).opened, true);
  gate.tick(10, false);
  const end = gate.tick(10 + TURN_SILENCE_MS, false);
  assert.deepEqual([end.commit, end.respond], [false, false]);
  assert.equal(gate.isOpen(), false);
});

test("a long turn that ends right after a mid-turn send still gets an answer", () => {
  const gate = new ClientTurns();
  gate.audio("a");
  gate.tick(0, true);
  assert.equal(gate.tick(TURN_MAX_MS, true).commit, true);
  gate.tick(TURN_MAX_MS + 10, false);
  const end = gate.tick(TURN_MAX_MS + 10 + TURN_SILENCE_MS, false);
  assert.deepEqual([end.commit, end.respond], [false, true]);
});

test("muting while ducked drops the duck without counting it as echo", () => {
  const guard = new BargeGuard();
  guard.local(0);
  guard.cancel();
  assert.equal(guard.ducking(), false);
  assert.equal(guard.expired(BARGE_CONFIRM_MS * 2), false);
  assert.deepEqual(guard.stats(), { echoes: 0, confirmed: 0, local_off: false });
});

const reply = (payload, agent = "a") => ({ type: "message.created", agent_id: agent, payload: { author_kind: "agent", body: " Done. ", ...payload } });

test("a main chat call speaks the agent's own replies and never a quick chat's", () => {
  assert.equal(spokenUpdate(reply({}), "a", null), "Done.");
  assert.equal(spokenUpdate(reply({ quick_session_id: "q1", quiet: true }), "a", null), null);
  assert.equal(spokenUpdate(reply({ author_agent_id: "b" }), "a", null), null);
  assert.equal(spokenUpdate(reply({}, "b"), "a", null), null);
  assert.equal(spokenUpdate(reply({ author_kind: "user" }), "a", null), null);
  assert.equal(spokenUpdate(reply({ body: "  " }), "a", null), null);
});

test("a quick chat call speaks only that quick chat's replies, quiet or not", () => {
  assert.equal(spokenUpdate(reply({ quick_session_id: "q1", quiet: true }), "a", "q1"), "Done.");
  assert.equal(spokenUpdate(reply({ quick_session_id: "q2", quiet: true }), "a", "q1"), null);
  assert.equal(spokenUpdate(reply({}), "a", "q1"), null);
  assert.equal(spokenUpdate({ type: "run.finished", agent_id: "a", payload: { quick_session_id: "q1" } }, "a", "q1"), null);
});
