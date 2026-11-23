import { holdMusic } from "./music";
import { holdSounds } from "./sounds";
import { CallSpeaker, SoundNotice } from "@/lib/call-speaker";

type Listener = (status: SpeechStatus) => void;

export function holdAudio(): () => void {
  const music = holdMusic();
  const sounds = holdSounds();
  return () => {
    music();
    sounds();
  };
}

export function micProblem(e: unknown): string {
  if (typeof window !== "undefined" && (!window.isSecureContext || !navigator.mediaDevices?.getUserMedia)) {
    return "Voice needs a secure page (https or localhost), so the microphone is off here.";
  }
  const name = e instanceof DOMException || e instanceof Error ? e.name : "";
  const desktop = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  if (name === "NotAllowedError" || name === "SecurityError") {
    return desktop
      ? "Loka cannot use your microphone. Allow it for Loka in System Settings, Privacy and Security, Microphone, then try again."
      : "Your browser blocked the microphone. Allow it from the icon next to the address bar, then try again.";
  }
  if (name === "NotFoundError" || name === "OverconstrainedError") return "No microphone was found. Plug one in or pick one in your sound settings.";
  if (name === "NotReadableError" || name === "AbortError") return "Another app is holding the microphone. Close it and try again.";
  return "Could not start the microphone. Try again.";
}

export const SOUND_BLOCKED = "This device is not playing sound right now. Click anywhere in Loka and check your speaker or headphones.";

export const MODEL_PROBLEM = "On-device voice could not load its speech model. Check your connection and try again.";

export interface SpeechStatus {
  loading: { stt: number | null; tts: number | null };
  device: string | null;
  speaking: boolean;
  error: string | null;
}

const FEMALE = ["af_heart", "af_bella", "af_nicole", "af_sarah", "af_nova", "af_sky", "bf_emma", "bf_isabella", "bf_alice", "bf_lily"];
const MALE = ["am_michael", "am_fenrir", "am_puck", "am_adam", "am_eric", "am_liam", "am_onyx", "bm_george", "bm_fable", "bm_daniel", "bm_lewis"];
const ALL = [...FEMALE, ...MALE];

function hash(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

export function voiceFor(agent: { slug: string; persona: string | null; look: Record<string, unknown> }): string {
  const chosen = typeof agent.look.voice === "string" ? agent.look.voice.trim().toLowerCase() : null;
  if (chosen && ALL.includes(chosen)) return chosen;
  const words = ` ${(agent.persona ?? "").toLowerCase()} `;
  const pool = /\b(she|her|woman|girl|lady|queen)\b/.test(words) ? FEMALE : /\b(he|him|his|man|guy|boy|king)\b/.test(words) ? MALE : ALL;
  return pool[hash(agent.slug) % pool.length];
}

export const VOICES = ALL;

const REF_WORDS: Record<string, string> = { quest: "a quest", report: "a report", todo: "a to-do", msg: "a message" };
const UUID = /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi;
const SPOKEN_LIMIT = 600;

function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return "";
  }
}

export function speakable(text: string, limit = SPOKEN_LIMIT): string {
  const flat = text
    .replace(/<!--[\s\S]*?-->/g, " ")
    .replace(/```[\s\S]*?(```|$)/g, "\nThe code is in the chat.\n")
    .replace(/^[ \t]*\|?[ \t]*:?-{3,}.*$/gm, "")
    .replace(/^[ \t]*\|(.*)\|[ \t]*$/gm, (_, row: string) => row.split("|").map((c) => c.trim()).filter(Boolean).join(", "))
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/<?(https?:\/\/[^\s>)]+)>?/g, (_, url: string) => {
      const host = hostOf(url);
      return host ? `a link to ${host}` : "a link";
    })
    .replace(/\b(quest|report|todo|msg):[\w-]+/g, (_, kind: string) => REF_WORDS[kind] ?? "a link")
    .replace(UUID, "")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/^[ \t]{0,3}#{1,6}[ \t]+/gm, "")
    .replace(/^[ \t]*>[ \t]?/gm, "")
    .replace(/^[ \t]*(?:[-*+\u2022]|\d+[.)])[ \t]+/gm, "")
    .replace(/(\*\*|~~)(\S[\s\S]*?)\1/g, "$2")
    .replace(/(^|[^\w*])\*(\S[^*\n]*?)\*(?!\w)/g, "$1$2")
    .replace(/(^|\W)__?(\S[^_\n]*?)__?(?=\W|$)/g, "$1$2")
    .replace(/(^|\s)[@#](?=[A-Za-z0-9])/g, "$1")
    .replace(/\s+->\s+|\s+=>\s+/g, " to ")
    .replace(/\s&\s/g, " and ")
    .replace(/\p{Extended_Pictographic}\uFE0F?/gu, "")
    .replace(/[*~|<>{}\[\]\\]+/g, " ")
    .replace(/\u2014/g, ", ");
  const lines = flat
    .split(/\n+/)
    .map((l) => l.replace(/\s+/g, " ").trim())
    .filter(Boolean)
    .map((l) => (/[.!?:;,]$/.test(l) ? l : `${l}.`));
  const whole = lines.join(" ").replace(/\s+([.,!?;:])/g, "$1").replace(/([.!?])\.+/g, "$1").trim();
  if (whole.length <= limit) return whole;
  const cut = whole.slice(0, limit);
  const end = Math.max(cut.lastIndexOf(". "), cut.lastIndexOf("! "), cut.lastIndexOf("? "));
  return `${end > limit / 3 ? cut.slice(0, end + 1) : `${cut.slice(0, cut.lastIndexOf(" "))}.`} The rest is in the chat.`;
}

export function sentences(text: string): string[] {
  const parts = text.match(/[^.!?]+[.!?]*["')\]]*\s*/g) ?? [];
  const out: string[] = [];
  for (const raw of parts) {
    const part = raw.trim();
    if (!part) continue;
    if (out.length && (out[out.length - 1].length < 24 || part.length < 12)) out[out.length - 1] += ` ${part}`;
    else out.push(part);
  }
  return out;
}

class Speech {
  private worker: Worker | null = null;
  private next = 1;
  private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private listeners = new Set<Listener>();
  private out: { ctx: AudioContext; speaker: CallSpeaker } | null = null;
  private notice = new SoundNotice(SOUND_BLOCKED);
  private queue: Promise<void> = Promise.resolve();
  private epoch = 0;
  private source: AudioBufferSourceNode | null = null;
  private remaining = 0;
  private held: (() => void) | null = null;
  status: SpeechStatus = { loading: { stt: null, tts: null }, device: null, speaking: false, error: null };

  private ensure(): Worker {
    if (this.worker) return this.worker;
    const w = new Worker(new URL("./speech-worker.ts", import.meta.url), { type: "module" });
    w.onmessage = (e: MessageEvent) => {
      const m = e.data as Record<string, unknown>;
      if (m.type === "progress") {
        const what = m.what as "stt" | "tts";
        this.set({ loading: { ...this.status.loading, [what]: Math.round(m.progress as number) } });
        return;
      }
      if (m.type === "ready") {
        const what = m.what as "stt" | "tts";
        this.set({ loading: { ...this.status.loading, [what]: null }, device: m.device as string });
        return;
      }
      const id = m.id as number;
      const p = this.pending.get(id);
      if (!p) {
        if (m.type === "error") this.set({ error: String(m.error) });
        return;
      }
      this.pending.delete(id);
      if (m.type === "error") {
        this.set({ error: String(m.error) });
        p.reject(new Error(String(m.error)));
      } else p.resolve(m);
    };
    this.worker = w;
    return w;
  }

  private set(patch: Partial<SpeechStatus>) {
    if ("error" in patch) this.notice.setError(patch.error ?? null);
    this.status = { ...this.status, ...patch, error: this.notice.shown() };
    for (const l of this.listeners) l(this.status);
  }

  subscribe(l: Listener): () => void {
    this.listeners.add(l);
    l(this.status);
    return () => this.listeners.delete(l);
  }

  warm(what: "stt" | "tts") {
    this.ensure().postMessage({ type: "warm", what });
  }

  private call<T>(msg: Record<string, unknown>, transfer: Transferable[] = []): Promise<T> {
    const id = this.next++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.ensure().postMessage({ ...msg, id }, transfer);
    });
  }

  async transcribe(audio: Float32Array): Promise<string> {
    const res = await this.call<{ text: string }>({ type: "transcribe", audio }, [audio.buffer]);
    return res.text;
  }

  speak(text: string, voice: string): Promise<void> {
    const parts = sentences(speakable(text));
    if (!parts.length) return Promise.resolve();
    const epoch = this.epoch;
    this.held ??= holdAudio();
    this.remaining += parts.length;
    this.set({ speaking: true, error: null });
    for (const sentence of parts) {
      const job = this.call<{ audio: Float32Array; sampling_rate: number }>({ type: "speak", text: sentence, voice });
      this.queue = this.queue
        .then(async () => {
          const { audio, sampling_rate } = await job;
          if (epoch !== this.epoch) return;
          await this.play(audio, sampling_rate);
        })
        .catch((e: unknown) => {
          if (epoch !== this.epoch) return;
          const why = e instanceof Error && e.message ? ` ${e.message}` : "";
          this.set({ error: `Could not read the reply aloud. It is in the chat.${why}` });
        })
        .then(() => {
          if (epoch !== this.epoch) return;
          this.remaining -= 1;
          if (this.remaining <= 0) this.finish();
        });
    }
    return this.queue;
  }

  stop() {
    this.epoch++;
    const src = this.source;
    this.source = null;
    if (src) {
      try {
        src.stop();
      } catch {}
    }
    this.finish();
  }

  private finish() {
    this.remaining = 0;
    this.held?.();
    this.held = null;
    if (this.status.speaking) this.set({ speaking: false });
  }

  private speaker(): { ctx: AudioContext; speaker: CallSpeaker } {
    if (this.out) return this.out;
    const ctx = new AudioContext();
    const speaker = new CallSpeaker(ctx);
    this.out = { ctx, speaker };
    speaker.subscribe((blocked) => {
      this.notice.setBlocked(blocked);
      this.set({});
    });
    return this.out;
  }

  private play(audio: Float32Array, rate: number): Promise<void> {
    const { ctx, speaker } = this.speaker();
    const buffer = ctx.createBuffer(1, audio.length, rate);
    buffer.copyToChannel(audio as Float32Array<ArrayBuffer>, 0);
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(speaker.input);
    if (speaker.blocked) return Promise.resolve();
    this.source = src;
    const epoch = this.epoch;
    return new Promise((resolve) => {
      let gaveUp = false;
      const current = () => epoch === this.epoch && this.source === src;
      const giveUp = () => {
        gaveUp = true;
        if (this.source === src) this.source = null;
        resolve();
      };
      const wait = window.setTimeout(() => {
        speaker.check();
        giveUp();
      }, 3000);
      src.onended = () => {
        window.clearTimeout(wait);
        if (this.source === src) this.source = null;
        resolve();
      };
      speaker.check();
      void speaker.start().then((ok) => {
        if (gaveUp) return;
        window.clearTimeout(wait);
        if (!ok) speaker.check();
        if (!current() || !ok) {
          giveUp();
          return;
        }
        src.start();
      });
    });
  }

  hear() {
    void this.speaker().speaker.hear();
  }

  clearError() {
    if (this.status.error) this.set({ error: null });
  }

  unlock() {
    void this.speaker().speaker.start();
  }
}

let instance: Speech | null = null;
export function speech(): Speech {
  instance ??= new Speech();
  return instance;
}

export interface Recording {
  stop: () => Promise<Float32Array>;
  cancel: () => void;
}

export async function record(): Promise<Recording> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true } });
  const recorder = new MediaRecorder(stream);
  const chunks: Blob[] = [];
  recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
  recorder.start();
  speech().stop();
  const release = holdAudio();
  const finish = () => {
    stream.getTracks().forEach((t) => t.stop());
    release();
  };
  return {
    cancel: () => {
      recorder.onstop = null;
      if (recorder.state !== "inactive") recorder.stop();
      finish();
    },
    stop: () =>
      new Promise<Float32Array>((resolve, reject) => {
        recorder.onstop = async () => {
          finish();
          try {
            const blob = new Blob(chunks, { type: recorder.mimeType });
            const ctx = new AudioContext({ sampleRate: 16000 });
            const decoded = await ctx.decodeAudioData(await blob.arrayBuffer());
            const mono = decoded.getChannelData(0).slice();
            void ctx.close();
            resolve(mono);
          } catch (e) {
            reject(e);
          }
        };
        recorder.stop();
      }),
  };
}

export interface Capture {
  rate: number;
  stop: () => void;
}

export async function capture(onFrame: (frame: Float32Array) => void): Promise<Capture> {
  const ctx = new AudioContext();
  void ctx.resume();
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true } });
  } catch (e) {
    void ctx.close();
    throw e;
  }
  void ctx.resume();
  speech().stop();
  const release = holdAudio();
  const source = ctx.createMediaStreamSource(stream);
  const node = ctx.createScriptProcessor(4096, 1, 1);
  node.onaudioprocess = (e) => onFrame(e.inputBuffer.getChannelData(0).slice());
  source.connect(node);
  node.connect(ctx.destination);
  let open = true;
  return {
    rate: ctx.sampleRate,
    stop: () => {
      if (!open) return;
      open = false;
      node.onaudioprocess = null;
      node.disconnect();
      source.disconnect();
      stream.getTracks().forEach((t) => t.stop());
      void ctx.close();
      release();
    },
  };
}

export function joinFrames(frames: Float32Array[]): Float32Array {
  const out = new Float32Array(frames.reduce((n, f) => n + f.length, 0));
  let at = 0;
  for (const f of frames) {
    out.set(f, at);
    at += f.length;
  }
  return out;
}

export async function resample(input: Float32Array, from: number, to = 16000): Promise<Float32Array> {
  if (from === to || !input.length) return input;
  try {
    const offline = new OfflineAudioContext(1, Math.ceil((input.length * to) / from), to);
    const buffer = offline.createBuffer(1, input.length, from);
    buffer.copyToChannel(input as Float32Array<ArrayBuffer>, 0);
    const src = offline.createBufferSource();
    src.buffer = buffer;
    src.connect(offline.destination);
    src.start();
    return (await offline.startRendering()).getChannelData(0).slice();
  } catch {
    return downsample(input, from, to);
  }
}

export interface Vad {
  close: () => void;
  level: () => number;
}

export async function listen(opts: {
  onStart: () => void;
  onUtterance: (audio: Float32Array) => void;
  sensitivity: () => number;
}): Promise<Vad> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
  const ctx = new AudioContext();
  const release = holdAudio();
  const source = ctx.createMediaStreamSource(stream);
  const node = ctx.createScriptProcessor(2048, 1, 1);
  const rate = ctx.sampleRate;
  const frameMs = (2048 / rate) * 1000;
  const preroll: Float32Array[] = [];
  let chunks: Float32Array[] = [];
  let speaking = false;
  let voiced = 0;
  let silent = 0;
  let floor = 0.004;
  let level = 0;
  node.onaudioprocess = (e) => {
    const input = e.inputBuffer.getChannelData(0);
    let sum = 0;
    for (let i = 0; i < input.length; i++) sum += input[i] * input[i];
    const rms = Math.sqrt(sum / input.length);
    level = rms;
    const threshold = Math.max(0.012, floor * 3) * opts.sensitivity();
    const frame = input.slice();
    if (!speaking) {
      floor = rms < floor ? floor * 0.9 + rms * 0.1 : floor * 0.995 + rms * 0.005;
      preroll.push(frame);
      if (preroll.length > Math.ceil(350 / frameMs)) preroll.shift();
      voiced = rms > threshold ? voiced + frameMs : 0;
      if (voiced >= 120) {
        speaking = true;
        silent = 0;
        chunks = [...preroll];
        preroll.length = 0;
        opts.onStart();
      }
      return;
    }
    chunks.push(frame);
    silent = rms > threshold * 0.7 ? 0 : silent + frameMs;
    const total = chunks.length * frameMs;
    if (silent >= 1200 || total >= 30000) {
      speaking = false;
      voiced = 0;
      const merged = new Float32Array(chunks.reduce((n, c) => n + c.length, 0));
      let o = 0;
      for (const c of chunks) {
        merged.set(c, o);
        o += c.length;
      }
      chunks = [];
      if (total - silent >= 300) opts.onUtterance(downsample(merged, rate, 16000));
    }
  };
  source.connect(node);
  node.connect(ctx.destination);
  return {
    level: () => level,
    close: () => {
      node.disconnect();
      source.disconnect();
      stream.getTracks().forEach((t) => t.stop());
      void ctx.close();
      release();
    },
  };
}

export function downsample(input: Float32Array, from: number, to: number): Float32Array {
  if (from === to) return input;
  const ratio = from / to;
  const out = new Float32Array(Math.floor(input.length / ratio));
  for (let i = 0; i < out.length; i++) {
    const start = Math.floor(i * ratio);
    const end = Math.min(input.length, Math.floor((i + 1) * ratio));
    let sum = 0;
    for (let j = start; j < end; j++) sum += input[j];
    out[i] = sum / Math.max(1, end - start);
  }
  return out;
}

const REPLY_WAIT_MS = 15 * 60_000;
const awaiting = new Map<string, number>();

export function expectSpokenReply(place: string) {
  awaiting.set(place, Date.now() + REPLY_WAIT_MS);
  speech().warm("tts");
}

export function takeSpokenReply(place: string): boolean {
  const until = awaiting.get(place);
  if (!until) return false;
  awaiting.delete(place);
  return until > Date.now();
}
