import { decode, encode } from "@msgpack/msgpack";
import { holdMusic } from "./music";
import { holdSounds } from "./sounds";
import {
  BargeGuard,
  CallTurns,
  CallUtterances,
  ClientTurns,
  RESERVATION_MS,
  ReplyWait,
  SESSION_WAIT_MS,
  UTTERANCE_SETTLE_MS,
  serverTurnsApplied,
  sessionFallback,
  spokenUpdate,
  type CallEvent,
  type SessionStep,
} from "@/lib/call-turns";
import { AudioSlot, CallSpeaker } from "@/lib/call-speaker";

export type LivePhase = "connecting" | "listening" | "hearing" | "thinking" | "speaking";

export type LiveHandlers = {
  onPhase: (phase: LivePhase) => void;
  onCaption: (key: string, who: "you" | "them", text: string, partial?: boolean) => void;
  onError: (message: string | null) => void;
  onSound: (blocked: boolean) => void;
  onEnd: () => void;
};

export type LiveCall = {
  setMuted: (muted: boolean) => void;
  hear: () => void;
  level: () => number;
  elapsed: () => number;
  usdPerMinute: number;
  maxMinutes: number;
  close: () => Promise<void>;
};

export class LiveUnavailable extends Error {}

const DUCKED_GAIN = 0.15;

const CLIENT_BARGE_MS = 600;

type CallAudio = { ctx: AudioContext; speaker: CallSpeaker };

function openAudio(): CallAudio {
  const ctx = new AudioContext();
  return { ctx, speaker: new CallSpeaker(ctx) };
}

function disposeAudio({ ctx, speaker }: CallAudio) {
  speaker.close();
  void ctx.close().catch(() => undefined);
}

const callAudio = new AudioSlot<CallAudio>(openAudio, (audio) => audio.ctx.state !== "closed", disposeAudio);

export function primeLiveAudio() {
  try {
    const { ctx, speaker } = callAudio.prime();
    void speaker.start();
    const buf = ctx.createBuffer(1, 1, ctx.sampleRate);
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(speaker.input);
    src.start();
  } catch {}
}

async function wireAudio({ ctx, speaker }: CallAudio, stream: MediaStream) {
  void speaker.start();
  await ctx.resume().catch(() => undefined);
  const mic = ctx.createMediaStreamSource(stream);
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 512;
  mic.connect(analyser);
  const workletUrl = URL.createObjectURL(new Blob([WORKLET], { type: "application/javascript" }));
  try {
    await ctx.audioWorklet.addModule(workletUrl);
  } finally {
    URL.revokeObjectURL(workletUrl);
  }
  const tap = new AudioWorkletNode(ctx, "live-tap");
  const sink = ctx.createGain();
  sink.gain.value = 0;
  mic.connect(tap);
  tap.connect(sink);
  sink.connect(ctx.destination);
  const out = ctx.createGain();
  out.connect(speaker.input);
  const outTap = ctx.createAnalyser();
  outTap.fftSize = 512;
  out.connect(outTap);
  return { analyser, tap, out, outTap };
}

function rms(buf: Uint8Array): number {
  let sum = 0;
  for (const v of buf) sum += ((v - 128) / 128) ** 2;
  return Math.sqrt(sum / buf.length);
}

type Session = {
  app: string;
  token: string;
  sample_rate: number;
  usd_per_minute: number;
  max_minutes: number;
  session: Record<string, unknown>;
};

type Event = Record<string, unknown> & { type?: string };

const WORKLET = `class Tap extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch) this.port.postMessage(ch.slice(0));
    return true;
  }
}
registerProcessor("live-tap", Tap);`;

const USER_DONE = new Set(["conversation.item.input_audio_transcription.completed", "conversation.item.input_audio_transcription.done"]);
const USER_FAILED = "conversation.item.input_audio_transcription.failed";
const USER_PARTIAL = new Set(["conversation.item.input_audio_transcription.updated", "conversation.item.input_audio_transcription.delta"]);
const AGENT_DELTA = new Set(["response.output_audio_transcript.delta", "response.audio_transcript.delta", "response.output_text.delta", "response.text.delta"]);
const AGENT_DONE = new Set(["response.output_audio_transcript.done", "response.audio_transcript.done", "response.output_text.done", "response.text.done"]);
const AUDIO_DELTA = new Set(["response.output_audio.delta", "response.audio.delta"]);

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function toBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

function fromBase64(b64: string): Int16Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new Int16Array(bytes.buffer, 0, bytes.length >> 1);
}

async function post(url: string, body?: unknown): Promise<Response> {
  return fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });
}

type Greeting = { text: string; rate: number; pcm: Int16Array };

async function fetchGreeting(base: string): Promise<Greeting | null> {
  const r = await fetch(`${base}/greeting`, { cache: "no-store" }).catch(() => null);
  if (!r?.ok) return null;
  const bytes = new Uint8Array(await r.arrayBuffer());
  if (bytes.length < 2) return null;
  const text = decodeURIComponent(r.headers.get("x-greeting-text") ?? "");
  const rate = Number(r.headers.get("x-greeting-rate")) || 24000;
  return { text, rate, pcm: new Int16Array(bytes.buffer, 0, bytes.length >> 1) };
}

export function startLive(agentId: string, h: LiveHandlers, quickId: string | null = null, about: string | null = null): Promise<LiveCall> {
  return callAudio.use((take) => openLive(agentId, h, quickId, about, take));
}

async function openLive(agentId: string, h: LiveHandlers, quickId: string | null, about: string | null, take: () => CallAudio): Promise<LiveCall> {
  const base = quickId ? `/api/world/quick/${quickId}/voice` : `/api/world/agents/${agentId}/voice`;
  const pendingGreeting = fetchGreeting(base);
  const greeting = await Promise.race([pendingGreeting, new Promise<null>((resolve) => window.setTimeout(() => resolve(null), 900))]);
  const query = new URLSearchParams();
  if (greeting) query.set("greeted", "true");
  if (about && !quickId) query.set("about", about);
  const res = await post(query.size ? `${base}?${query}` : base);
  if (res.status === 503) throw new LiveUnavailable("Live voice is not set up on this server.");
  if (!res.ok) {
    const refusal = quickId ? ((await res.json().catch(() => null)) as { error?: string } | null)?.error : null;
    throw new Error(refusal || `Could not start live voice (${res.status}).`);
  }
  const cfg = (await res.json()) as Session;
  const rate = cfg.sample_rate || 24000;
  const callId = crypto.randomUUID();

  const stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
  let audio: CallAudio;
  let nodes: Awaited<ReturnType<typeof wireAudio>>;
  try {
    audio = take();
    nodes = await wireAudio(audio, stream);
  } catch (e) {
    stream.getTracks().forEach((t) => t.stop());
    throw e;
  }
  const { ctx, speaker } = audio;
  const { analyser, tap, out, outTap } = nodes;

  const releaseSounds = holdSounds();
  const releaseMusic = holdMusic();
  const ws = new WebSocket(`wss://fal.run/${cfg.app}?fal_jwt_token=${encodeURIComponent(cfg.token)}&max_buffering=30`);
  ws.binaryType = "arraybuffer";
  const send = (ev: Event) => {
    if (ws.readyState === WebSocket.OPEN) ws.send(encode(ev));
  };

  let muted = false;
  let closed = false;
  let started = 0;
  let phase: LivePhase = "connecting";
  let hearing = false;
  let responding = false;
  let waiting = 0;
  let playhead = 0;
  const sources = new Set<AudioBufferSourceNode>();
  const turns = new CallTurns();
  const utterances = new CallUtterances(turns);
  let endSent = false;
  const userText = new Map<string, string>();
  const agentText = new Map<string, string>();
  const finished = new Set<string>();
  const calls = new Set<string>();
  let openCalls = 0;
  const updates: string[] = [];
  let settleTimer = 0;
  const replyWait = new ReplyWait();
  let lastHeardAt = 0;
  let dropAudio = false;
  let sessionSent: Record<string, unknown> = cfg.session;
  const sessionTried = new Set<SessionStep>();
  let refused = "";
  let appliedSilence: number | null = null;
  let appliedVoice = "";
  const guard = new BargeGuard();
  let speechStarts = 0;
  let commits = 0;
  let bargeIns = 0;
  let audioChunks = 0;
  let droppedChunks = 0;
  let lastAudioState = "";
  let sessionSettled = false;
  let sessionTimer = 0;
  let clientTurns: ClientTurns<string> | null = null;
  let clientCommits = 0;
  let turnsChecked = false;
  let switchPending = false;
  let expectedCommits = 0;
  let pendingCreate = false;
  let createTimer = 0;
  let clientDropped = "";
  let switchTimer = 0;
  const serverDetection = cfg.session.turn_detection ?? null;
  const report = (message: string | null) => h.onError(message);
  let configured = false;
  let flushTimer = 0;
  let flushDue = 0;
  let stopTimer = 0;
  let uncommitted = false;
  let committing = false;
  let transcribing = true;
  const awaitingUser = new Set<string>();
  let userSeq = 0;
  let agentSeq = 0;
  let saving: Promise<void> = Promise.resolve();

  const setPhase = (p: LivePhase) => {
    if (p !== phase) {
      phase = p;
      h.onPhase(p);
    }
  };
  const settle = () => {
    if (closed) return;
    if (sources.size) setPhase("speaking");
    else if (hearing) setPhase("hearing");
    else if (responding || openCalls || waiting) setPhase("thinking");
    else if (started) setPhase("listening");
  };

  const schedule = (ms: number) => {
    const due = Date.now() + ms;
    if (flushTimer && flushDue <= due) return;
    window.clearTimeout(flushTimer);
    flushDue = due;
    flushTimer = window.setTimeout(() => {
      flushTimer = 0;
      void flush(false);
    }, ms);
  };
  const record = (role: "user" | "agent", key: string, text: string, partial = false) => {
    if (turns.set(role, key, text, partial)) schedule(partial ? 4000 : 400);
  };
  const closeUser = () => {
    window.clearTimeout(settleTimer);
    settleTimer = 0;
    const done = utterances.close();
    if (!done) return;
    h.onCaption(done.key, "you", done.text);
    schedule(400);
  };
  const userBusy = () => (hearing && !muted) || sources.size > 0 || ((committing || awaitingUser.size > 0) && Date.now() - lastHeardAt < RESERVATION_MS);
  const armSettle = () => {
    window.clearTimeout(settleTimer);
    settleTimer = window.setTimeout(() => {
      settleTimer = 0;
      if (!utterances.isOpen()) return;
      if (userBusy()) armSettle();
      else closeUser();
    }, UTTERANCE_SETTLE_MS);
  };
  const settleUser = () => {
    if (!utterances.isOpen()) return;
    if (userBusy()) armSettle();
    else closeUser();
  };
  const afterReply = () => {
    if (replyWait.heard(sources.size > 0, responding)) settleUser();
  };
  const hearUser = (itemKey: string, text: string) => {
    const heard = utterances.hear(itemKey, text);
    if (!heard) return;
    lastHeardAt = Date.now();
    h.onCaption(heard.key, "you", heard.text, heard.open);
    if (heard.open) armSettle();
    else schedule(400);
  };
  const context = () =>
    turns
      .transcript()
      .slice(-30)
      .map((l) => `${l.role === "user" ? "them" : "you"}: ${l.text}`)
      .join("\n");
  const save = async (wantsEnd: boolean, request?: string) => {
    const end = wantsEnd && !endSent;
    if (end) endSent = true;
    const lines = turns.unsaved(Date.now(), !wantsEnd);
    if (!lines.length && !end && !request) {
      if (turns.holding()) schedule(1000);
      return;
    }
    const body = JSON.stringify({
      lines: lines.map((l) => ({ role: l.role, text: l.text, key: l.key, partial: Boolean(l.partial) })),
      call_id: callId,
      request,
      context: request || end ? context() : undefined,
      end,
      seconds: end ? elapsed() : undefined,
      client: end ? ("__TAURI_INTERNALS__" in window ? "desktop" : "web") : undefined,
      diagnostics: end ? diagnostics() : undefined,
    });
    for (let attempt = 0; attempt < (end ? 4 : 1); attempt++) {
      if (attempt) await new Promise((resolve) => window.setTimeout(resolve, 600 * attempt));
      const r = await fetch(`${base}/lines`, { method: "POST", headers: { "content-type": "application/json" }, body, keepalive: end && body.length < 60_000 }).catch(() => null);
      if (r?.ok) {
        turns.markSaved(lines);
        if (turns.holding()) schedule(1000);
        return;
      }
      if (r && r.status >= 400 && r.status < 500 && r.status !== 429) break;
    }
    if (end) report("Part of this call could not be saved to the chat.");
    else if (lines.length) schedule(3000);
  };
  const diagnostics = () => ({
    configured,
    transcribing,
    refused: refused || undefined,
    silence_ms: appliedSilence,
    voice: appliedVoice || str(sessionSent.voice) || undefined,
    speech_started: speechStarts,
    commits,
    barge_ins: bargeIns,
    ...guard.stats(),
    audio_chunks: audioChunks,
    dropped_chunks: droppedChunks,
    audio_state: lastAudioState || ctx.state,
    ...speaker.stats(),
    turns: clientTurns ? "client" : "server",
    client_commits: clientCommits,
    client_dropped: clientDropped || undefined,
  });
  const flush = (end: boolean, request?: string) => {
    const next = saving.then(() => save(end, request));
    saving = next.catch(() => undefined);
    return next;
  };
  const waitForUsers = async (ms: number) => {
    const until = performance.now() + ms;
    const unheard = () => [...userText.entries()].some(([key, text]) => text.trim() && !finished.has(`u-${key}`));
    while (performance.now() < until && ws.readyState === WebSocket.OPEN && (committing || awaitingUser.size || unheard())) {
      await new Promise((resolve) => window.setTimeout(resolve, 100));
    }
  };
  const elapsed = () => (started ? (performance.now() - started) / 1000 : 0);

  const stopPlayback = () => {
    for (const s of sources) {
      try {
        s.stop();
      } catch {}
    }
    sources.clear();
    playhead = ctx.currentTime;
  };
  speaker.subscribe((blocked) => {
    if (!closed) h.onSound(blocked);
  });
  const setGain = (value: number) => out.gain.setTargetAtTime(value, ctx.currentTime, 0.03);
  const play = (b64: string) => {
    const pcm = fromBase64(b64);
    if (!pcm.length) return;
    speaker.check();
    const buf = ctx.createBuffer(1, pcm.length, rate);
    const ch = buf.getChannelData(0);
    for (let i = 0; i < pcm.length; i++) ch[i] = pcm[i] / 32768;
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(out);
    const at = Math.max(ctx.currentTime + 0.02, playhead);
    src.start(at);
    playhead = at + buf.duration;
    sources.add(src);
    src.onended = () => {
      sources.delete(src);
      afterReply();
      settle();
    };
    settle();
  };

  const bargeIn = () => {
    bargeIns++;
    replyWait.interrupted();
    stopPlayback();
    setGain(1);
    if (responding) {
      dropAudio = true;
      send({ type: "response.cancel" });
    }
  };

  const finishAgent = (key: string, text?: string) => {
    if (finished.has(key)) return;
    const full = (text ?? agentText.get(key) ?? "").trim();
    if (!full) return;
    finished.add(key);
    replyWait.replied(dropAudio);
    afterReply();
    h.onCaption(key, "them", full);
    record("agent", key, full);
  };
  const keepCutTurns = () => {
    for (const [key, text] of agentText) if (!finished.has(key)) record("agent", key, text, true);
  };

  const sendUpdates = () => {
    if (responding || openCalls || clientTurns?.isOpen() || !updates.length) return;
    for (const text of updates.splice(0, updates.length)) {
      send({ type: "conversation.item.create", item: { type: "message", role: "user", content: [{ type: "input_text", text: `[update from your full self] ${text}` }] } });
    }
    requestResponse();
  };

  const runTool = async (name: string, callIdOf: string, args: string) => {
    if (calls.has(callIdOf)) return;
    calls.add(callIdOf);
    openCalls++;
    settle();
    let output = "";
    try {
      const parsed = (args ? JSON.parse(args) : {}) as { text?: string };
      if (name === "send_to_agent") {
        await waitForUsers(1500);
        closeUser();
        const text = str(parsed.text).trim();
        await flush(false);
        if (!text) output = JSON.stringify({ ok: false, error: "empty request" });
        else {
          await flush(false, text);
          output = JSON.stringify({ ok: true, note: "Sent. Your full self is on it and will report back in this call." });
        }
      } else if (name === "agent_status") {
        const r = await fetch(`${base}/status`, { cache: "no-store" });
        output = r.ok ? str(((await r.json()) as { text?: string }).text) : JSON.stringify({ ok: false, error: `status ${r.status}` });
      } else output = JSON.stringify({ ok: false, error: `unknown tool ${name}` });
    } catch (e) {
      output = JSON.stringify({ ok: false, error: e instanceof Error ? e.message : "tool failed" });
    }
    send({ type: "conversation.item.create", item: { type: "function_call_output", call_id: callIdOf, output } });
    openCalls--;
    if (!openCalls) requestResponse();
    settle();
  };

  const onEvent = (ev: Event) => {
    const type = str(ev.type);
    if (type === "x-fal-error") {
      if (ev.error !== "TIMEOUT") report(`${str(ev.error)} ${str(ev.reason)}`.trim());
      return;
    }
    if (type === "error") {
      if (closed) return;
      const err = (ev.error ?? {}) as Record<string, unknown>;
      const message = str(err.message) || str(ev.message);
      if (!sessionSettled) {
        const next = sessionFallback(sessionSent, message, sessionTried);
        if (next) {
          sessionTried.add(next.dropped);
          refused = [refused, `${next.dropped}: ${message}`.slice(0, 200)].filter(Boolean).join("; ");
          if (next.dropped === "transcription") transcribing = false;
          if (next.dropped === "turns") useClientTurns();
          sessionSent = next.session;
          send({ type: "session.update", session: sessionSent });
          awaitSession();
          return;
        }
        settleSession();
        report(`The voice service refused this call's settings, so it may answer before you finish. ${message}`.trim());
        return;
      }
      if (clientTurns && /buffer/i.test(message)) {
        if (/commit/i.test(message) && expectedCommits > 0) expectedCommits--;
        return;
      }
      if (switchPending) {
        endSwitch();
        dropClientTurns(`refused: ${message}`.slice(0, 200), false);
        return;
      }
      report(message || "The voice service reported an error.");
      return;
    }
    if (type === "session.updated") {
      configured = true;
      if (switchPending && sessionSettled) {
        endSwitch();
        return;
      }
      if (!turnsChecked) {
        turnsChecked = true;
        const applied = (ev.session ?? {}) as Record<string, unknown>;
        const detection = (applied.turn_detection ?? {}) as Record<string, unknown>;
        appliedSilence = typeof detection.silence_duration_ms === "number" ? detection.silence_duration_ms : null;
        appliedVoice = str(applied.voice);
        if (!clientTurns && !serverTurnsApplied(sessionSent, applied)) switchToClientTurns();
      }
      if (sessionSettled) return;
      settleSession();
      if (!transcribing) report("This call cannot write down what you say, so only the agent's side will show in the chat.");
      return;
    }
    if (type === "input_audio_buffer.committed") {
      commits++;
      if (clientTurns && !closed) {
        if (expectedCommits > 0) expectedCommits--;
        else if (!switchPending) dropClientTurns("provider ended a turn itself", true);
      }
      uncommitted = false;
      committing = false;
      if (str(ev.item_id)) {
        awaitingUser.add(str(ev.item_id));
        utterances.spoken(str(ev.item_id));
        turns.reserve("user", `u-${str(ev.item_id)}`);
      }
      return;
    }
    if (type === "input_audio_buffer.speech_started") {
      speechStarts++;
      hearing = true;
      guard.server();
      if (sources.size || responding) bargeIn();
      else setGain(1);
      settle();
      return;
    }
    if (type === "input_audio_buffer.speech_stopped") {
      hearing = false;
      lastHeardAt = Date.now();
      uncommitted = false;
      window.clearTimeout(waiting);
      waiting = window.setTimeout(() => {
        waiting = 0;
        settle();
      }, 8000);
      settle();
      return;
    }
    if (type === "response.created") {
      dropAudio = false;
      window.clearTimeout(waiting);
      waiting = 0;
      responding = true;
      settle();
      return;
    }
    if (AUDIO_DELTA.has(type)) {
      audioChunks++;
      if (dropAudio) droppedChunks++;
      else play(str(ev.delta));
      return;
    }
    const ids = str(ev.item_id) || str(ev.response_id);
    const itemKey = ids || `turn${userSeq}`;
    if (USER_PARTIAL.has(type)) {
      const text = type.endsWith(".delta") ? (userText.get(itemKey) ?? "") + str(ev.delta) : str(ev.transcript);
      userText.set(itemKey, text);
      hearUser(itemKey, text);
      return;
    }
    if (type === USER_FAILED || USER_DONE.has(type)) {
      awaitingUser.delete(itemKey);
      const text = (type === USER_FAILED ? "" : str(ev.transcript)) || userText.get(itemKey) || "";
      finished.add(`u-${itemKey}`);
      userText.set(itemKey, text);
      hearUser(itemKey, text);
      turns.release(`u-${itemKey}`);
      if (!ids) userSeq++;
      return;
    }
    const replyKey = `a-${str(ev.response_id) || ids || `turn${agentSeq}`}`;
    if (AGENT_DELTA.has(type)) {
      const text = (agentText.get(replyKey) ?? "") + str(ev.delta);
      agentText.set(replyKey, text);
      h.onCaption(replyKey, "them", text);
      if (!finished.has(replyKey)) record("agent", replyKey, text, true);
      return;
    }
    if (AGENT_DONE.has(type)) {
      finishAgent(replyKey, str(ev.transcript) || str(ev.text) || undefined);
      if (!ids) agentSeq++;
      return;
    }
    if (type === "response.function_call_arguments.done") {
      void runTool(str(ev.name), str(ev.call_id), str(ev.arguments));
      return;
    }
    if (type === "response.done") {
      const response = (ev.response ?? {}) as { id?: string; output?: Record<string, unknown>[] };
      for (const item of response.output ?? []) {
        if (item.type === "function_call") void runTool(str(item.name), str(item.call_id), str(item.arguments));
      }
      const key = `a-${response.id ?? str(ev.response_id)}`;
      for (const k of [key, ...agentText.keys()]) if (agentText.has(k)) finishAgent(k);
      if (!ids && !response.id) agentSeq++;
      responding = false;
      if (pendingCreate) sendCreate();
      settle();
      afterReply();
      sendUpdates();
    }
  };

  const settleSession = () => {
    sessionSettled = true;
    window.clearTimeout(sessionTimer);
    sessionTimer = 0;
    begin();
  };
  const awaitSession = () => {
    window.clearTimeout(sessionTimer);
    sessionTimer = window.setTimeout(() => {
      sessionTimer = 0;
      if (sessionSettled) return;
      if (!turnsChecked && !clientTurns) {
        turnsChecked = true;
        switchToClientTurns();
      }
      settleSession();
    }, SESSION_WAIT_MS);
  };
  const useClientTurns = () => {
    clientTurns ??= new ClientTurns<string>();
    guard.cancel();
    setGain(1);
  };
  const endSwitch = () => {
    switchPending = false;
    window.clearTimeout(switchTimer);
    switchTimer = 0;
  };
  const switchToClientTurns = () => {
    useClientTurns();
    switchPending = true;
    window.clearTimeout(switchTimer);
    switchTimer = window.setTimeout(endSwitch, SESSION_WAIT_MS);
    sessionSent = { ...sessionSent, turn_detection: null };
    send({ type: "session.update", session: sessionSent });
    if (!hearing) send({ type: "input_audio_buffer.clear" });
  };
  const dropClientTurns = (why: string, restore: boolean) => {
    if (!clientTurns) return;
    clientTurns = null;
    expectedCommits = 0;
    clientDropped = why;
    if (!restore || !serverDetection || sessionTried.has("turns")) return;
    sessionSent = { ...sessionSent, turn_detection: serverDetection };
    send({ type: "session.update", session: sessionSent });
  };
  const sendCreate = () => {
    window.clearTimeout(createTimer);
    createTimer = 0;
    pendingCreate = false;
    send({ type: "response.create" });
    responding = true;
    settle();
  };
  const requestResponse = () => {
    if (openCalls) return;
    if (!responding) {
      sendCreate();
      return;
    }
    if (pendingCreate) return;
    pendingCreate = true;
    createTimer = window.setTimeout(() => {
      if (pendingCreate) sendCreate();
    }, 2000);
  };
  const endClientTurn = (commit: boolean, respond: boolean) => {
    if (responding) bargeIn();
    if (commit) {
      clientCommits++;
      committing = true;
      uncommitted = false;
      expectedCommits++;
      send({ type: "input_audio_buffer.commit" });
    }
    if (!respond) return;
    requestResponse();
    hearing = false;
    lastHeardAt = Date.now();
    window.clearTimeout(waiting);
    waiting = window.setTimeout(() => {
      waiting = 0;
      settle();
    }, 8000);
    settle();
  };
  const begin = () => {
    if (started || closed) return;
    started = performance.now();
    stopTimer = window.setTimeout(() => void call.close(), cfg.max_minutes * 60_000);
    settle();
  };
  if (greeting) {
    const buf = ctx.createBuffer(1, greeting.pcm.length, greeting.rate);
    const ch = buf.getChannelData(0);
    for (let i = 0; i < greeting.pcm.length; i++) ch[i] = greeting.pcm[i] / 32768;
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(out);
    speaker.check();
    const at = ctx.currentTime + 0.02;
    src.start(at);
    playhead = at + buf.duration;
    sources.add(src);
    src.onended = () => {
      sources.delete(src);
      afterReply();
      settle();
    };
    settle();
    if (greeting.text) {
      h.onCaption("greeting", "them", greeting.text);
      record("agent", "greeting", greeting.text);
    }
  }
  ws.onopen = () => {
    send({ type: "session.update", session: sessionSent });
    awaitSession();
    window.setTimeout(begin, 2500);
  };
  ws.onmessage = async (msg) => {
    try {
      const data = typeof msg.data === "string" ? JSON.parse(msg.data) : decode(new Uint8Array(msg.data as ArrayBuffer));
      if (data && typeof data === "object") onEvent(data as Event);
    } catch {}
  };
  ws.onerror = () => report("Lost the connection to the voice service.");
  ws.onclose = (e) => {
    if (!closed) {
      if (e.code !== 1000) report(e.reason || "The voice call dropped.");
      void call.close();
    }
  };

  let carry = 0;
  let chunk: number[] = [];
  const ratio = ctx.sampleRate / rate;
  tap.port.onmessage = (e: MessageEvent<Float32Array>) => {
    if (muted || ws.readyState !== WebSocket.OPEN || !started) return;
    const input = e.data;
    let pos = carry;
    while (pos < input.length - 1) {
      const i = Math.floor(pos);
      const f = pos - i;
      chunk.push(input[i] * (1 - f) + input[i + 1] * f);
      pos += ratio;
    }
    carry = pos - input.length;
    if (chunk.length >= rate / 10) {
      const pcm = new Int16Array(chunk.length);
      for (let i = 0; i < chunk.length; i++) pcm[i] = Math.max(-32768, Math.min(32767, Math.round(chunk[i] * 32767)));
      chunk = [];
      const audio = toBase64(new Uint8Array(pcm.buffer));
      for (const part of clientTurns ? clientTurns.audio(audio) : [audio]) send({ type: "input_audio_buffer.append", audio: part });
      if (clientTurns ? clientTurns.isOpen() : hearing) uncommitted = true;
    }
  };

  const onWorld = (e: globalThis.Event) => {
    if (closed) return;
    const body = spokenUpdate((e as CustomEvent<CallEvent>).detail, agentId, quickId);
    if (!body) return;
    updates.push(body);
    sendUpdates();
  };
  window.addEventListener("world-event", onWorld);
  const onLeave = () => {
    closeUser();
    keepCutTurns();
    void flush(true);
  };
  window.addEventListener("pagehide", onLeave);

  const levels = new Uint8Array(analyser.fftSize);
  const outLevels = new Uint8Array(outTap.fftSize);
  let floor = 0.01;
  let loudSince = 0;
  let quietSince = 0;
  let localHearing = false;
  const vad = window.setInterval(() => {
    if (closed || muted || !started) return;
    analyser.getByteTimeDomainData(levels);
    const level = rms(levels);
    const playing = sources.size > 0;
    let echo = 0;
    if (playing) {
      outTap.getByteTimeDomainData(outLevels);
      echo = rms(outLevels);
    }
    if (!playing && !localHearing) floor = floor * 0.97 + Math.min(level, 0.05) * 0.03;
    const threshold = Math.max(playing ? 0.045 : 0.025, floor * (playing ? 4 : 2.5), echo * 0.4);
    const now = performance.now();
    if (guard.expired(now)) setGain(1);
    const loud = level > threshold;
    if (clientTurns) {
      const speaking = loud && loudSince > 0 && now - loudSince >= (playing ? CLIENT_BARGE_MS : 120) && (!playing || level > echo * 0.8);
      const step = clientTurns.tick(now, speaking);
      for (const audio of step.flush) send({ type: "input_audio_buffer.append", audio });
      if (step.opened) {
        uncommitted = true;
        if (sources.size || responding) bargeIn();
      }
      if (step.commit || step.respond) endClientTurn(step.commit, step.respond);
    }
    if (loud) {
      quietSince = 0;
      loudSince ||= now;
      if (now - loudSince >= (playing ? 220 : 120)) {
        if (!clientTurns && playing && !guard.ducking() && guard.local(now)) setGain(DUCKED_GAIN);
        if (!localHearing) {
          localHearing = true;
          hearing = true;
          settle();
        }
      }
    } else {
      loudSince = 0;
      quietSince ||= now;
      if (localHearing && now - quietSince > 700) {
        localHearing = false;
        hearing = false;
        settle();
      }
    }
  }, 40);
  const call: LiveCall = {
    usdPerMinute: cfg.usd_per_minute,
    maxMinutes: cfg.max_minutes,
    setMuted: (m) => {
      muted = m;
      chunk = [];
      if (m) clientTurns?.close();
      if (m && guard.ducking()) {
        guard.cancel();
        setGain(1);
      }
      if (m) send({ type: "input_audio_buffer.clear" });
    },
    level: () => {
      analyser.getByteTimeDomainData(levels);
      return rms(levels);
    },
    hear: () => void speaker.hear(),
    elapsed,
    close: async () => {
      if (closed) return;
      closed = true;
      releaseSounds();
      releaseMusic();
      window.clearTimeout(flushTimer);
      window.clearTimeout(settleTimer);
      window.clearTimeout(stopTimer);
      window.clearTimeout(waiting);
      window.clearTimeout(sessionTimer);
      window.clearTimeout(createTimer);
      window.clearTimeout(switchTimer);
      window.clearInterval(vad);
      window.removeEventListener("world-event", onWorld);
      stopPlayback();
      tap.port.onmessage = null;
      stream.getTracks().forEach((t) => t.stop());
      if (ws.readyState === WebSocket.OPEN && started) {
        committing = uncommitted && !muted;
        if (committing) send({ type: "input_audio_buffer.commit" });
        hearing = false;
        await waitForUsers(committing ? 3000 : 2000);
      }
      hearing = false;
      window.removeEventListener("pagehide", onLeave);
      closeUser();
      keepCutTurns();
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) ws.close(1000);
      lastAudioState = ctx.state;
      speaker.close();
      await ctx.close().catch(() => undefined);
      await flush(true);
      h.onEnd();
    },
  };
  return call;
}
