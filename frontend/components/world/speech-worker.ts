import { pipeline as loadPipeline, env } from "@huggingface/transformers";
import { KokoroTTS } from "kokoro-js";

env.allowLocalModels = false;

const STT_MODEL = "onnx-community/moonshine-base-ONNX";
const TTS_MODEL = "onnx-community/Kokoro-82M-v1.0-ONNX";

type Inbound =
  | { type: "transcribe"; id: number; audio: Float32Array }
  | { type: "speak"; id: number; text: string; voice: string }
  | { type: "warm"; what: "stt" | "tts" };

const ctx = self as unknown as {
  postMessage: (msg: unknown, transfer?: Transferable[]) => void;
  onmessage: ((e: MessageEvent<Inbound>) => void) | null;
};

async function hasWebGPU(): Promise<boolean> {
  const gpu = (navigator as unknown as { gpu?: { requestAdapter: () => Promise<unknown> } }).gpu;
  if (!gpu) return false;
  try {
    return Boolean(await gpu.requestAdapter());
  } catch {
    return false;
  }
}

let devicePromise: Promise<"webgpu" | "wasm"> | null = null;
function device() {
  devicePromise ??= hasWebGPU().then((ok) => (ok ? "webgpu" : "wasm"));
  return devicePromise;
}

function progress(what: "stt" | "tts") {
  return (p: { status?: string; progress?: number; file?: string }) => {
    if (p.status === "progress" && typeof p.progress === "number") {
      ctx.postMessage({ type: "progress", what, progress: p.progress, file: p.file });
    }
  };
}

type Transcriber = (audio: Float32Array) => Promise<{ text?: string } | { text?: string }[]>;
const pipeline = loadPipeline as unknown as (task: string, model: string, options: Record<string, unknown>) => Promise<Transcriber>;

const DISK_FULL = "This device is out of disk space, so the on-device voice model could not be loaded. Free some space and try again.";
const DOWNLOAD_FAILED = "The on-device voice model could not be downloaded. Check your connection and try again.";

function explain(err: unknown): string {
  const name = err instanceof DOMException || err instanceof Error ? err.name : "";
  const text = err instanceof Error ? err.message : String(err);
  if (name === "QuotaExceededError" || /quota|no space|disk (is )?full|ENOSPC/i.test(text)) return DISK_FULL;
  if (/failed to fetch|network|load failed/i.test(text)) return DOWNLOAD_FAILED;
  return text;
}

function retryable<T>(load: () => Promise<T>): () => Promise<T> {
  let current: Promise<T> | null = null;
  return () => {
    if (current) return current;
    const attempt = load();
    current = attempt;
    attempt.catch(() => {
      if (current === attempt) current = null;
    });
    return attempt;
  };
}

const stt = retryable(() =>
  device().then(async (d) => {
    const asr = await pipeline("automatic-speech-recognition", STT_MODEL, {
      device: "wasm",
      dtype: "q8",
      progress_callback: progress("stt"),
    });
    ctx.postMessage({ type: "ready", what: "stt", device: d === "webgpu" ? "webgpu available, wasm in use" : "wasm" });
    return asr;
  }),
);

const tts = retryable(() =>
  device().then(async (d) => {
    const model = await KokoroTTS.from_pretrained(TTS_MODEL, {
      device: "wasm",
      dtype: "q8",
      progress_callback: progress("tts"),
    });
    ctx.postMessage({ type: "ready", what: "tts", device: d === "webgpu" ? "webgpu + wasm" : "wasm" });
    return model;
  }),
);

ctx.onmessage = async (e) => {
  const msg = e.data;
  try {
    if (msg.type === "warm") {
      await (msg.what === "stt" ? stt() : tts());
      return;
    }
    if (msg.type === "transcribe") {
      const asr = await stt();
      const out = await asr(msg.audio);
      const text = (Array.isArray(out) ? out.map((o) => o.text ?? "").join(" ") : out.text ?? "").trim();
      ctx.postMessage({ type: "transcript", id: msg.id, text });
      return;
    }
    if (msg.type === "speak") {
      const model = await tts();
      const raw = await model.generate(msg.text, { voice: msg.voice as never });
      const audio = raw.audio as Float32Array;
      ctx.postMessage({ type: "audio", id: msg.id, audio, sampling_rate: raw.sampling_rate }, [audio.buffer]);
    }
  } catch (err) {
    ctx.postMessage({ type: "error", id: "id" in msg ? msg.id : -1, error: explain(err) });
  }
};
