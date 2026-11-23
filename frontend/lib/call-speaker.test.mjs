import { test } from "node:test";
import assert from "node:assert/strict";
import { AudioSlot, BLOCK_AFTER_MS, CallSpeaker, SoundNotice } from "./call-speaker.ts";

class Emitter {
  listeners = new Map();
  addEventListener(type, fn) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type).add(fn);
  }
  removeEventListener(type, fn) {
    this.listeners.get(type)?.delete(fn);
  }
  emit(type) {
    for (const fn of [...(this.listeners.get(type) ?? [])]) fn();
  }
  count(type) {
    return this.listeners.get(type)?.size ?? 0;
  }
}

function node(name) {
  return {
    name,
    targets: [],
    connect(target) {
      this.targets.push(target);
    },
    disconnect() {
      this.targets = [];
    },
  };
}

function fakeContext({ state = "running", streams = true, resumes = true } = {}) {
  const ctx = new Emitter();
  ctx.state = state;
  ctx.resumes = resumes;
  ctx.destination = node("destination");
  ctx.gains = [];
  ctx.createGain = () => {
    const gain = node("gain");
    ctx.gains.push(gain);
    return gain;
  };
  ctx.resume = async () => {
    if (ctx.resumes && ctx.state !== "running") {
      ctx.state = "running";
      ctx.emit("statechange");
    }
  };
  if (streams) ctx.createMediaStreamDestination = () => ({ ...node("stream"), stream: { id: "call-stream" } });
  return ctx;
}

function fakeElement(outcomes = []) {
  const el = new Emitter();
  el.paused = true;
  el.autoplay = false;
  el.srcObject = null;
  el.plays = 0;
  el.outcomes = outcomes;
  el.play = async () => {
    el.plays++;
    const outcome = el.outcomes.length ? el.outcomes.shift() : "ok";
    if (outcome !== "ok") throw { name: outcome };
    if (!el.paused) return;
    el.paused = false;
    el.emit("playing");
  };
  el.pause = () => {
    if (el.paused) return;
    el.paused = true;
    el.emit("pause");
  };
  return el;
}

function fakeEnv(element) {
  const env = { gestures: new Set(), devices: new Set(), timers: [] };
  env.element = () => element;
  env.onGesture = (fn) => {
    env.gestures.add(fn);
    return () => env.gestures.delete(fn);
  };
  env.onDeviceChange = (fn) => {
    env.devices.add(fn);
    return () => env.devices.delete(fn);
  };
  env.later = (fn, ms) => {
    const timer = { fn, ms, live: true };
    env.timers.push(timer);
    return () => {
      timer.live = false;
    };
  };
  env.runTimers = () => {
    for (const timer of env.timers.splice(0)) if (timer.live) timer.fn();
  };
  env.gesture = () => {
    for (const fn of [...env.gestures]) fn();
  };
  env.deviceChange = () => {
    for (const fn of [...env.devices]) fn();
  };
  return env;
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("the call sound plays through a media element fed by the audio graph", async () => {
  const ctx = fakeContext();
  const el = fakeElement();
  const speaker = new CallSpeaker(ctx, fakeEnv(el));
  assert.equal(speaker.route, "element");
  assert.equal(el.autoplay, true);
  assert.deepEqual(el.srcObject, { id: "call-stream" });
  assert.equal(speaker.input.targets[0].name, "stream");
  assert.equal(await speaker.start(), true);
  assert.equal(el.paused, false);
  assert.equal(speaker.healthy(), true);
});

test("without media stream support the sound goes straight to the speakers", async () => {
  const ctx = fakeContext({ streams: false });
  const speaker = new CallSpeaker(ctx, fakeEnv(fakeElement()));
  assert.equal(speaker.route, "direct");
  assert.equal(speaker.input.targets[0], ctx.destination);
  assert.equal(await speaker.start(), true);
});

test("a suspended audio graph is resumed when sound is about to play", async () => {
  const ctx = fakeContext({ state: "suspended" });
  const el = fakeElement();
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  speaker.check();
  await settle();
  assert.equal(ctx.state, "running");
  assert.equal(speaker.healthy(), true);
  env.runTimers();
  assert.equal(speaker.blocked, false);
});

test("a refused play shows the tap to hear recovery and a tap brings the sound back", async () => {
  const ctx = fakeContext();
  const el = fakeElement(["NotAllowedError", "NotAllowedError"]);
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  const seen = [];
  speaker.subscribe((blocked) => seen.push(blocked));
  assert.equal(await speaker.start(), false);
  speaker.check();
  await settle();
  assert.deepEqual(seen, [false]);
  env.runTimers();
  assert.deepEqual(seen, [false, true]);
  assert.equal(speaker.blocked, true);
  assert.equal(env.gestures.size, 1);
  assert.equal(speaker.route, "element");
  assert.equal(await speaker.hear(), true);
  assert.deepEqual(seen, [false, true, false]);
  assert.equal(env.gestures.size, 0);
  assert.equal(speaker.stats().sound_blocked, true);
  assert.equal(speaker.stats().speaker_error, "NotAllowedError");
});

test("any click or key anywhere also retries while the sound is blocked", async () => {
  const ctx = fakeContext({ state: "suspended", resumes: false });
  const el = fakeElement();
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  const seen = [];
  speaker.subscribe((blocked) => seen.push(blocked));
  speaker.check();
  await settle();
  env.runTimers();
  assert.deepEqual(seen, [false, true]);
  ctx.resumes = true;
  env.gesture();
  await settle();
  assert.deepEqual(seen, [false, true, false]);
  assert.equal(speaker.blocked, false);
});

test("a media element that cannot play falls back to the speakers without blocking", async () => {
  const ctx = fakeContext();
  const el = fakeElement(["NotSupportedError"]);
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  const seen = [];
  speaker.subscribe((blocked) => seen.push(blocked));
  assert.equal(await speaker.start(), true);
  assert.equal(speaker.route, "direct");
  assert.equal(speaker.input.targets.length, 1);
  assert.equal(speaker.input.targets[0], ctx.destination);
  assert.equal(el.srcObject, null);
  env.runTimers();
  assert.deepEqual(seen, [false]);
});

test("the element pausing on its own, such as a headphone switch, starts it again", async () => {
  const ctx = fakeContext();
  const el = fakeElement();
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  await speaker.start();
  el.pause();
  await settle();
  assert.equal(el.paused, false);
  assert.equal(el.plays, 2);
  env.runTimers();
  assert.equal(speaker.blocked, false);
});

test("an output device change kicks the sound again", async () => {
  const ctx = fakeContext();
  const el = fakeElement();
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  await speaker.start();
  ctx.state = "interrupted";
  ctx.resumes = true;
  env.deviceChange();
  await settle();
  assert.equal(ctx.state, "running");
  assert.equal(speaker.healthy(), true);
});

test("the blocked notice waits for the grace period", async () => {
  const ctx = fakeContext({ state: "suspended", resumes: false });
  const env = fakeEnv(fakeElement());
  const speaker = new CallSpeaker(ctx, env);
  speaker.check();
  speaker.check();
  assert.equal(env.timers.length, 1);
  assert.equal(env.timers[0].ms, BLOCK_AFTER_MS);
});

test("closing stops the element and every listener", async () => {
  const ctx = fakeContext();
  const el = fakeElement(["NotAllowedError"]);
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  await speaker.start();
  speaker.check();
  env.runTimers();
  speaker.close();
  assert.equal(env.gestures.size, 0);
  assert.equal(env.devices.size, 0);
  assert.equal(ctx.count("statechange"), 0);
  assert.equal(el.count("pause"), 0);
  assert.equal(el.srcObject, null);
  assert.equal(speaker.input.targets.length, 0);
  assert.equal(await speaker.hear(), false);
});

test("a resume that never settles still shows the tap to hear recovery", async () => {
  const ctx = fakeContext({ state: "suspended" });
  ctx.resume = () => new Promise(() => undefined);
  const el = fakeElement();
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  const seen = [];
  speaker.subscribe((blocked) => seen.push(blocked));
  void speaker.start();
  speaker.check();
  await settle();
  assert.deepEqual(seen, [false]);
  env.runTimers();
  assert.deepEqual(seen, [false, true]);
  assert.equal(speaker.blocked, true);
  assert.equal(env.gestures.size, 1);
});

test("an output device change that leaves the element stopped reattaches its stream", async () => {
  const ctx = fakeContext();
  const el = fakeElement();
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  await speaker.start();
  const stream = el.srcObject;
  const assigned = [];
  let current = stream;
  Object.defineProperty(el, "srcObject", {
    get: () => current,
    set: (value) => {
      assigned.push(value);
      current = value;
    },
  });
  el.paused = true;
  el.outcomes = ["NotAllowedError"];
  env.deviceChange();
  await settle();
  await settle();
  assert.deepEqual(assigned, [null, stream]);
  assert.equal(el.paused, false);
  assert.equal(speaker.healthy(), true);
});

test("a healthy element is left alone on an output device change", async () => {
  const ctx = fakeContext();
  const el = fakeElement();
  const env = fakeEnv(el);
  const speaker = new CallSpeaker(ctx, env);
  await speaker.start();
  const stream = el.srcObject;
  env.deviceChange();
  await settle();
  assert.equal(el.srcObject, stream);
  assert.equal(el.plays, 2);
});

test("a block that happened before anyone subscribed reaches the new subscriber at once", async () => {
  const ctx = fakeContext({ state: "suspended", resumes: false });
  const env = fakeEnv(fakeElement());
  const speaker = new CallSpeaker(ctx, env);
  speaker.check();
  await settle();
  env.runTimers();
  assert.equal(speaker.blocked, true);
  const seen = [];
  const stop = speaker.subscribe((blocked) => seen.push(blocked));
  assert.deepEqual(seen, [true]);
  speaker.check();
  assert.deepEqual(seen, [true]);
  ctx.resumes = true;
  env.gesture();
  await settle();
  assert.deepEqual(seen, [true, false]);
  stop();
  speaker.check();
  assert.deepEqual(seen, [true, false]);
});

test("the sound notice shows on top of the real error and gives it back on unblock", async () => {
  const notice = new SoundNotice("no sound");
  const ctx = fakeContext({ state: "suspended", resumes: false });
  const env = fakeEnv(fakeElement());
  const speaker = new CallSpeaker(ctx, env);
  speaker.subscribe((blocked) => notice.setBlocked(blocked));
  notice.setError("model failed to load");
  speaker.check();
  await settle();
  env.runTimers();
  assert.equal(notice.shown(), "no sound");
  notice.setError("no sound");
  assert.equal(notice.shown(), "no sound");
  ctx.resumes = true;
  env.gesture();
  await settle();
  assert.equal(notice.shown(), "model failed to load");
  notice.setError(null);
  assert.equal(notice.shown(), null);
});

test("clearing errors while blocked keeps the sound notice", () => {
  const notice = new SoundNotice("no sound");
  notice.setBlocked(true);
  notice.setError(null);
  assert.equal(notice.shown(), "no sound");
  notice.setBlocked(false);
  assert.equal(notice.shown(), null);
});

function audioSlot() {
  const opened = [];
  const disposed = [];
  const slot = new AudioSlot(
    () => {
      const audio = { id: opened.length + 1, closed: false };
      opened.push(audio);
      return audio;
    },
    (audio) => !audio.closed,
    (audio) => {
      audio.closed = true;
      disposed.push(audio.id);
    },
  );
  return { slot, opened, disposed };
}

test("priming reuses the open audio and replaces a closed one", () => {
  const { slot, opened } = audioSlot();
  const first = slot.prime();
  assert.equal(slot.prime(), first);
  first.closed = true;
  const second = slot.prime();
  assert.notEqual(second, first);
  assert.equal(opened.length, 2);
});

test("a call that fails before taking the audio disposes the primed audio", async () => {
  const { slot, disposed } = audioSlot();
  slot.prime();
  await assert.rejects(slot.use(async () => {
    throw new Error("microphone denied");
  }));
  assert.deepEqual(disposed, [1]);
  slot.drop();
  assert.deepEqual(disposed, [1]);
});

test("a call that fails after taking the audio disposes what it took", async () => {
  const { slot, disposed, opened } = audioSlot();
  slot.prime();
  await assert.rejects(slot.use(async (take) => {
    take();
    throw new Error("worklet failed");
  }));
  assert.deepEqual(disposed, [1]);
  await assert.rejects(slot.use(async (take) => {
    take();
    throw new Error("worklet failed");
  }));
  assert.equal(opened.length, 2);
  assert.deepEqual(disposed, [1, 2]);
});

test("a call that starts keeps its audio and the next call opens a fresh one", async () => {
  const { slot, disposed } = audioSlot();
  slot.prime();
  const audio = await slot.use(async (take) => take());
  assert.equal(audio.id, 1);
  slot.drop();
  assert.deepEqual(disposed, []);
  assert.equal(slot.prime().id, 2);
});
