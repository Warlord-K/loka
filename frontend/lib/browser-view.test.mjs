import { test } from "node:test";
import assert from "node:assert/strict";
import { keyFrame, MAX_VIEWS, pageToReopen, modifiers, mouseButton, pagePoint, startView, viewKey, viewReducer, viewTitle, withView } from "./browser-view.ts";

const run = (...messages) => messages.reduce(viewReducer, startView());

test("a view waits for the device, then shows frames as they arrive", () => {
  assert.equal(run({ type: "status", state: "connecting" }).phase, "connecting");
  assert.equal(run({ type: "status", state: "live" }).phase, "waiting");
  const live = run({ type: "status", state: "live" }, { type: "frame", data: "AAAA", width: 1280, height: 720 }, { type: "page", url: "https://example.com/" });
  assert.equal(live.phase, "live");
  assert.deepEqual(live.frame, { src: "data:image/jpeg;base64,AAAA", width: 1280, height: 720 });
  assert.equal(live.url, "https://example.com/");
  assert.equal(viewReducer(live, { type: "frame", data: "" }), live);
});

test("a closed browser says so and comes back with the next frame", () => {
  const closed = run({ type: "frame", data: "AAAA", width: 10, height: 10 }, { type: "idle", reason: "closed" });
  assert.equal(closed.phase, "idle");
  assert.equal(closed.idle, "closed");
  assert.ok(closed.frame);
  assert.equal(run({ type: "idle", reason: "not_running" }).idle, "not_running");
  assert.equal(run({ type: "idle", reason: "anything" }).idle, "not_running");
  const back = viewReducer(closed, { type: "frame", data: "BBBB", width: 10, height: 10 });
  assert.equal(back.phase, "live");
  assert.equal(back.idle, null);
});

test("an ended view stays ended and keeps its reason", () => {
  const ended = run({ type: "frame", data: "AAAA", width: 10, height: 10 }, { type: "closed", reason: "idle" });
  assert.equal(ended.phase, "ended");
  assert.equal(ended.ended, "idle");
  assert.equal(viewReducer(ended, { type: "frame", data: "BBBB", width: 10, height: 10 }), ended);
  assert.equal(run({ type: "error", message: "Could not open that page" }).note, "Could not open that page");
});

test("clicks map onto the page through the letterboxed picture", () => {
  const box = { left: 100, top: 50, width: 800, height: 800 };
  const frame = { width: 1600, height: 800 };
  assert.deepEqual(pagePoint({ x: 100, y: 250 }, box, frame), { x: 0, y: 0 });
  assert.deepEqual(pagePoint({ x: 500, y: 450 }, box, frame), { x: 800, y: 400 });
  assert.equal(pagePoint({ x: 500, y: 100 }, box, frame), null);
  assert.equal(pagePoint({ x: 500, y: 450 }, box, { width: 0, height: 0 }), null);
});

test("keys carry text only when they type something", () => {
  const plain = { altKey: false, ctrlKey: false, metaKey: false, shiftKey: false };
  assert.deepEqual(keyFrame("down", { ...plain, key: "a", code: "KeyA", keyCode: 65 }), { type: "key", event: "keyDown", key: "a", code: "KeyA", modifiers: 0, key_code: 65, text: "a" });
  assert.equal(keyFrame("down", { ...plain, key: "Enter", code: "Enter", keyCode: 13 }).text, "\r");
  assert.equal(keyFrame("down", { ...plain, key: "Backspace", code: "Backspace", keyCode: 8 }).text, undefined);
  assert.equal(keyFrame("down", { ...plain, metaKey: true, key: "a", code: "KeyA", keyCode: 65 }).text, undefined);
  assert.equal(keyFrame("up", { ...plain, key: "a", code: "KeyA", keyCode: 65 }).text, undefined);
  assert.equal(keyFrame("down", { ...plain, key: "é", code: "KeyE", keyCode: 0 }).key_code, undefined);
  assert.equal(modifiers({ altKey: true, ctrlKey: true, metaKey: true, shiftKey: true }), 15);
  assert.deepEqual([0, 1, 2, 3].map(mouseButton), ["left", "middle", "right", "none"]);
});

test("one view per agent, newest last, a help ask kept until replaced", () => {
  let views = withView([], { agentId: "a", help: { cardId: "c1", site: "GitHub" } });
  views = withView(views, { agentId: "b" });
  views = withView(views, { agentId: "a" });
  assert.deepEqual(views.map((v) => v.agentId), ["b", "a"]);
  assert.equal(views[1].help?.cardId, "c1");
  for (const id of ["c", "d", "e", "f"]) views = withView(views, { agentId: id });
  assert.equal(views.length, MAX_VIEWS);
  assert.deepEqual(views.map((v) => v.agentId), ["c", "d", "e", "f"]);
});

test("a view opened from a quest watches that quest, apart from the agent's own view", () => {
  let views = withView([], { agentId: "a" });
  views = withView(views, { agentId: "a", questId: "q1" });
  views = withView(views, { agentId: "a", questId: "q2" });
  assert.deepEqual(views.map(viewKey), ["a", "a:q1", "a:q2"]);
  views = withView(views, { agentId: "a", questId: "q1", help: { cardId: "c1", site: "GitHub" } });
  assert.deepEqual(views.map(viewKey), ["a", "a:q2", "a:q1"]);
  assert.equal(views[2].questId, "q1");
  assert.equal(views[2].help?.cardId, "c1");
  views = withView(views, { agentId: "a", questId: "q1" });
  assert.equal(views[2].help?.cardId, "c1", "watching the quest again keeps its help ask");
  assert.equal(views[0].help, null);
});

test("a quest view names its quest, so it never reads like the agent's own view", () => {
  assert.equal(viewTitle("Ada", {}), "Ada's browser");
  assert.equal(viewTitle("Ada", { questId: "q1", questTitle: "  Fix the\n login page " }), 'Ada\'s browser for "Fix the login page"');
  assert.equal(viewTitle("Ada", { questId: "q1" }), "Ada's browser for one quest");
  let views = withView([], { agentId: "a", questId: "q1", questTitle: "Fix login" });
  views = withView(views, { agentId: "a", questId: "q1", help: { cardId: "c1", site: "GitHub" } });
  assert.equal(views[0].questTitle, "Fix login", "a help ask on an open quest view keeps its name");
});

test("a help view reopens the asked page once when the agent's browser has closed", () => {
  const help = { cardId: "c1", site: "GitHub", url: "https://github.com/login" };
  const idle = run({ type: "idle", reason: "closed" });
  assert.equal(pageToReopen(idle, help, false), "https://github.com/login");
  assert.equal(pageToReopen(idle, help, true), null);
  assert.equal(pageToReopen(run({ type: "frame", data: "AAAA", width: 1, height: 1 }), help, false), null);
  assert.equal(pageToReopen(startView(), help, false), null);
  assert.equal(pageToReopen(idle, null, false), null);
  assert.equal(pageToReopen(idle, { cardId: "c2", site: "GitHub" }, false), null);
});
