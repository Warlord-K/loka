"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Microphone, Phone, SpeakerHigh, SpeakerSlash, X } from "@phosphor-icons/react";
import { escapeTaken, takeEscape } from "@/lib/escape";
import { cn } from "@/lib/utils";
import { Keycap, isMac } from "./keycap";
import { anchorAt, shouldCut, splice, spokenText, type DictationAnchor, type Spliced } from "@/lib/dictation";
import { MODEL_PROBLEM, capture, downsample, joinFrames, micProblem, record, resample, speech, type Capture, type Recording, type SpeechStatus } from "./speech";
import { BTN_SECONDARY_SM, HUD_ICON, HUD_SEG } from "./styles";
import { Shimmer } from "./work-ui";

export function useSpeechStatus(): SpeechStatus {
  const [status, setStatus] = useState<SpeechStatus>(speech().status);
  useEffect(() => speech().subscribe(setStatus), []);
  return status;
}

export type TalkState = "idle" | "starting" | "listening" | "transcribing";

const NOISE = /^[\s.,!?-]*$/;

function cleanTranscript(text: string): string {
  const out = text.replace(/\[[^\]]*\]|\([^)]*(?:music|silence|blank|inaudible)[^)]*\)/gi, " ").replace(/\s+/g, " ").trim();
  return NOISE.test(out) ? "" : out;
}

export function useHoldToTalk(onText: (text: string) => void) {
  const rec = useRef<Recording | null>(null);
  const starting = useRef<Promise<void> | null>(null);
  const cancelled = useRef(false);
  const deliver = useRef(onText);
  deliver.current = onText;
  const [state, setState] = useState<TalkState>("idle");
  const [error, setError] = useState<string | null>(null);

  const start = useCallback(() => {
    if (rec.current || starting.current) return;
    setError(null);
    cancelled.current = false;
    speech().unlock();
    speech().warm("stt");
    setState("starting");
    starting.current = record()
      .then((r) => {
        if (cancelled.current) {
          r.cancel();
          setState("idle");
          return;
        }
        rec.current = r;
        setState("listening");
      })
      .catch((e: unknown) => {
        setState("idle");
        setError(micProblem(e));
      })
      .finally(() => {
        starting.current = null;
      });
  }, []);

  const stop = useCallback(async () => {
    if (starting.current) await starting.current;
    const r = rec.current;
    rec.current = null;
    if (!r) return;
    setState("transcribing");
    try {
      const audio = await r.stop();
      if (cancelled.current || audio.length < 16000 * 0.3) {
        setState("idle");
        return;
      }
      const text = cleanTranscript(await speech().transcribe(audio));
      setState("idle");
      if (cancelled.current) return;
      if (text) deliver.current(text);
      else setError("Did not catch that. Try again a little closer to the microphone.");
    } catch {
      setState("idle");
      if (!cancelled.current) setError(MODEL_PROBLEM);
    }
  }, []);

  const cancel = useCallback(() => {
    cancelled.current = true;
    const r = rec.current;
    rec.current = null;
    r?.cancel();
    setState("idle");
  }, []);

  useEffect(() => () => {
    cancelled.current = true;
    rec.current?.cancel();
    rec.current = null;
  }, []);

  return { state, error, start, stop, cancel, clearError: () => setError(null) };
}

type Segment = { frames: Float32Array[]; samples: number; audible: boolean; heard: number; text: string; final: boolean; closed: boolean };

type Dictation = {
  capture: Capture | null;
  rate: number;
  segments: Segment[];
  ended: boolean;
  working: boolean;
  floor: number;
  silentMs: number;
  lastInterim: number;
  total: number;
};

const INTERIM_GAP_MS = 250;
const MIN_SPOKEN_SECONDS = 0.3;
const PREROLL_SECONDS = 0.5;
const QUIET_SPEECH = 0.004;

function newSegment(): Segment {
  return { frames: [], samples: 0, audible: false, heard: 0, text: "", final: false, closed: false };
}

export type DictationHandlers = {
  onText: (firm: string, provisional: string) => void;
  onDone: (text: string, failed: boolean) => void;
  onCancel: () => void;
};

export function useDictation(handlers: DictationHandlers) {
  const live = useRef<Dictation | null>(null);
  const on = useRef(handlers);
  on.current = handlers;
  const [state, setState] = useState<TalkState>("idle");
  const [error, setError] = useState<string | null>(null);

  const emit = (d: Dictation) => {
    const { firm, provisional } = spokenText(d.segments);
    on.current.onText(firm, provisional);
  };

  const finish = (d: Dictation, problem: string | null) => {
    if (live.current !== d) return;
    live.current = null;
    d.capture?.stop();
    const { firm } = spokenText(d.segments.map((s) => ({ text: s.final ? s.text : "", final: true })));
    const spokenSeconds = d.total / d.rate;
    setState("idle");
    on.current.onDone(firm, problem !== null);
    if (problem) setError(problem);
    else if (!firm && spokenSeconds >= MIN_SPOKEN_SECONDS) setError("Did not catch that. Try again a little closer to the microphone.");
  };

  const pump = (d: Dictation) => {
    if (d.working || live.current !== d) return;
    const pending = d.segments.find((s) => s.closed && !s.final);
    const current = d.segments[d.segments.length - 1];
    if (pending) {
      const audio = joinFrames(pending.frames);
      pending.frames = [];
      if (!pending.audible || audio.length < d.rate * MIN_SPOKEN_SECONDS) {
        pending.text = "";
        pending.final = true;
        emit(d);
        pump(d);
        return;
      }
      d.working = true;
      void resample(audio, d.rate)
        .then((pcm) => speech().transcribe(pcm))
        .then(
          (text) => {
            d.working = false;
            if (live.current !== d) return;
            pending.text = cleanTranscript(text);
            pending.final = true;
            emit(d);
            pump(d);
          },
          () => {
            d.working = false;
            finish(d, MODEL_PROBLEM);
          },
        );
      return;
    }
    if (d.ended) {
      finish(d, null);
      return;
    }
    if (!current.audible || current.samples - current.heard < d.rate * MIN_SPOKEN_SECONDS || performance.now() - d.lastInterim < INTERIM_GAP_MS) return;
    current.heard = current.samples;
    d.working = true;
    const audio = downsample(joinFrames(current.frames), d.rate, 16000);
    void speech()
      .transcribe(audio)
      .then(
        (text) => {
          d.working = false;
          d.lastInterim = performance.now();
          if (live.current !== d) return;
          if (!current.final) {
            current.text = cleanTranscript(text);
            emit(d);
          }
          pump(d);
        },
        () => {
          d.working = false;
          finish(d, MODEL_PROBLEM);
        },
      );
  };

  const hear = (d: Dictation, frame: Float32Array) => {
    if (live.current !== d || d.ended) return;
    let sum = 0;
    for (let i = 0; i < frame.length; i++) sum += frame[i] * frame[i];
    const rms = Math.sqrt(sum / frame.length);
    const frameMs = (frame.length / d.rate) * 1000;
    const threshold = Math.max(0.012, d.floor * 3);
    const current = d.segments[d.segments.length - 1];
    const loud = rms > threshold;
    const audible = loud || rms > Math.max(QUIET_SPEECH, d.floor * 2);
    d.total += frame.length;
    current.frames.push(frame);
    current.samples += frame.length;
    if (audible) current.audible = true;
    while (!current.audible && current.frames.length > 1 && current.samples - current.frames[0].length >= d.rate * PREROLL_SECONDS) {
      current.samples -= current.frames.shift()!.length;
    }
    if (!loud) d.floor = rms < d.floor ? d.floor * 0.9 + rms * 0.1 : d.floor * 0.995 + rms * 0.005;
    d.silentMs = audible ? 0 : d.silentMs + frameMs;
    if (current.audible && shouldCut({ segmentMs: (current.samples / d.rate) * 1000, silentMs: d.silentMs })) {
      current.closed = true;
      d.segments.push(newSegment());
    }
    pump(d);
  };

  const start = useCallback(() => {
    if (live.current) return;
    setError(null);
    speech().unlock();
    speech().warm("stt");
    const d: Dictation = { capture: null, rate: 16000, segments: [newSegment()], ended: false, working: false, floor: 0.004, silentMs: 0, lastInterim: 0, total: 0 };
    live.current = d;
    setState("starting");
    capture((frame) => hear(d, frame))
      .then((c) => {
        if (live.current !== d) {
          c.stop();
          return;
        }
        d.capture = c;
        d.rate = c.rate;
        if (d.ended) {
          finish(d, null);
          return;
        }
        setState("listening");
      })
      .catch((e: unknown) => {
        if (live.current !== d) return;
        live.current = null;
        setState("idle");
        on.current.onCancel();
        setError(micProblem(e));
      });
  }, []);

  const stop = useCallback(() => {
    const d = live.current;
    if (!d || d.ended) return;
    d.ended = true;
    if (!d.capture) return;
    d.capture.stop();
    const current = d.segments[d.segments.length - 1];
    current.closed = true;
    setState("transcribing");
    pump(d);
  }, []);

  const cancel = useCallback(() => {
    const d = live.current;
    if (!d) return;
    live.current = null;
    d.capture?.stop();
    setState("idle");
    on.current.onCancel();
  }, []);

  useEffect(
    () => () => {
      live.current?.capture?.stop();
      live.current = null;
    },
    [],
  );

  return { state, error, start, stop, cancel, clearError: () => setError(null) };
}

const TALK_TEXT: Record<Exclude<TalkState, "idle">, string> = {
  starting: "Waking up the microphone",
  listening: "Listening",
  transcribing: "Turning your voice into words",
};

export function TalkStatus({ state, hint, onCancel, className }: { state: TalkState; hint?: React.ReactNode; onCancel: () => void; className?: string }) {
  if (state === "idle") return null;
  return (
    <div role="status" aria-live="polite" className={cn("flex items-center gap-2 text-w-sm", className)}>
      <Microphone size={14} weight="fill" className={cn("shrink-0", state === "listening" ? "text-w-coral" : "text-w-muted")} />
      <Shimmer className="min-w-0 truncate font-semibold">{TALK_TEXT[state]}</Shimmer>
      {state === "listening" && hint && <span className="hidden min-w-0 items-center gap-1 truncate text-w-xs text-w-muted sm:inline-flex">{hint}</span>}
      <button type="button" onClick={onCancel} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md px-1.5 py-0.5 text-w-xs text-w-muted hover:bg-w-sand hover:text-w-strong">
        Cancel <Keycap size="sm">Esc</Keycap>
      </button>
    </div>
  );
}

export function SpokenMark({ call = false, className }: { call?: boolean; className?: string }) {
  return (
    <span title={call ? "Said on a live voice call" : "Sent by voice"} className={cn("inline-flex items-center gap-0.5 text-w-2xs text-w-muted", className)}>
      {call ? <Phone size={11} weight="bold" /> : <Microphone size={11} weight="bold" />}
      {call ? "on a call" : "spoken"}
    </span>
  );
}

export function SpeechPill({ status, talk, onCancel }: { status: SpeechStatus; talk: TalkState; onCancel: () => void }) {
  const load = status.loading.stt ?? status.loading.tts;
  const box = "absolute bottom-24 right-4 z-30 flex max-w-[calc(100%-2rem)] items-center gap-2 rounded-lg border-2 border-w-ink bg-w-paper/95 px-3 py-1.5 text-w-sm shadow-[0_3px_0_rgb(var(--w-ink))] sm:bottom-28";
  if (talk !== "idle") {
    return (
      <TalkStatus
        state={talk}
        onCancel={onCancel}
        hint={
          <>
            Release <Keycap size="sm">V</Keycap> to add the words
          </>
        }
        className={cn(box, "pointer-events-auto")}
      />
    );
  }
  if (load !== null) {
    return (
      <div role="status" className={cn(box, "pointer-events-none")}>
        <Shimmer>{`Setting up on-device voice (${status.loading.stt !== null ? "listening" : "speaking"} model ${load}%)`}</Shimmer>
      </div>
    );
  }
  if (status.speaking) {
    return (
      <div role="status" className={cn(box, "pointer-events-auto")}>
        <SpeakerHigh size={14} weight="fill" className="shrink-0 text-w-teal" />
        <Shimmer className="font-semibold">Speaking</Shimmer>
        <button type="button" onClick={() => speech().stop()} className="ml-1 rounded-md px-1.5 py-0.5 text-w-xs text-w-muted hover:bg-w-sand hover:text-w-strong">
          Stop
        </button>
      </div>
    );
  }
  if (status.error) {
    return (
      <div role="alert" className={cn(box, "pointer-events-auto")}>
        <span className="min-w-0 text-w-text2">{status.error}</span>
        <button type="button" aria-label="Dismiss" onClick={() => speech().clearError()} className="grid h-6 w-6 shrink-0 place-items-center rounded-md text-w-muted hover:bg-w-sand hover:text-w-strong">
          <X size={12} weight="bold" />
        </button>
      </div>
    );
  }
  return null;
}

export function SpeakerToggle({ on, onToggle, grouped = false }: { on: boolean; onToggle: () => void; grouped?: boolean }) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-pressed={on}
      aria-label="Spoken replies"
      title={on ? "Agents speak their replies to your voice messages" : "Spoken replies are off"}
      className={grouped ? HUD_SEG : HUD_ICON}
    >
      {on ? <SpeakerHigh size={20} weight="bold" /> : <SpeakerSlash size={20} weight="bold" className="text-w-muted" />}
    </button>
  );
}

type FieldProps = {
  value: string;
  onChange: (value: string) => void;
  onDictated?: () => void;
  multiline?: boolean;
  composer?: boolean;
  className?: string;
  placeholder?: string;
  ariaLabel?: string;
  inputRef?: React.RefObject<HTMLTextAreaElement | HTMLInputElement | null>;
  onKey?: (e: React.KeyboardEvent<HTMLTextAreaElement>) => boolean;
};

const COMPOSER_MAX_HEIGHT = 200;

function pushKeys(): string[] {
  return isMac() ? ["\u2325", "V"] : ["Alt", "V"];
}

export function PushToTalkKeys({ size = "sm" }: { size?: "sm" | "md" }) {
  return (
    <span className="inline-flex items-center gap-0.5">
      {pushKeys().map((k) => (
        <Keycap key={k} size={size}>
          {k}
        </Keycap>
      ))}
    </span>
  );
}

const TAP_MS = 350;

export function VoiceField({ value, onChange, onDictated, multiline, composer, className, placeholder, ariaLabel, inputRef, onKey }: FieldProps) {
  const latest = useRef(value);
  latest.current = value;
  const area = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    const el = area.current;
    if (!composer || !el) return;
    const fit = () => {
      el.style.height = "";
      if (!el.value || !el.clientWidth) return;
      el.style.height = `${Math.min(el.scrollHeight + 4, COMPOSER_MAX_HEIGHT)}px`;
    };
    fit();
    let width = el.clientWidth;
    const watch = new ResizeObserver(() => {
      if (el.clientWidth === width) return;
      width = el.clientWidth;
      fit();
    });
    watch.observe(el);
    return () => watch.disconnect();
  }, [composer, value]);
  const box = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null);
  const mirror = useRef<HTMLDivElement | null>(null);
  const anchor = useRef<DictationAnchor | null>(null);
  const original = useRef("");
  const caretTo = useRef<number | null>(null);
  const written = useRef<string | null>(null);
  const sendAfter = useRef(false);
  const [shown, setShown] = useState<Spliced | null>(null);
  const attach = (el: HTMLTextAreaElement | HTMLInputElement | null) => {
    box.current = el;
    if (composer) area.current = el as HTMLTextAreaElement | null;
    if (inputRef) inputRef.current = el;
  };
  const write = (next: Spliced) => {
    caretTo.current = next.end;
    written.current = next.value;
    onChange(next.value);
  };
  const { state, start, stop, cancel, error, clearError } = useDictation({
    onText: (firm, provisional) => {
      if (!anchor.current) return;
      const next = splice(anchor.current, firm, provisional);
      setShown(next);
      write(next);
    },
    onDone: (text, failed) => {
      const at = anchor.current;
      anchor.current = null;
      written.current = null;
      if (failed) sendAfter.current = false;
      setShown(null);
      if (!at) return;
      if (!text) {
        caretTo.current = null;
        onChange(original.current);
        return;
      }
      write(splice(at, text, ""));
      onDictated?.();
    },
    onCancel: () => {
      const had = anchor.current;
      anchor.current = null;
      written.current = null;
      sendAfter.current = false;
      setShown(null);
      caretTo.current = null;
      if (had) onChange(original.current);
    },
  });
  const begin = () => {
    if (anchor.current) return;
    const el = box.current;
    const now = latest.current;
    anchor.current = el && document.activeElement === el ? anchorAt(now, el.selectionStart, el.selectionEnd) : anchorAt(now, null, null);
    original.current = now;
    written.current = now;
    start();
  };
  const busy = state !== "idle";
  const sendWhenDone = () => {
    sendAfter.current = true;
    stop();
  };
  useEffect(() => {
    if (!anchor.current || written.current === null || value === written.current) return;
    anchor.current = null;
    written.current = null;
    sendAfter.current = false;
    setShown(null);
    cancel();
  }, [value, cancel]);
  useEffect(() => {
    const form = box.current?.form;
    if (!busy || !form) return;
    const hold = (e: SubmitEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      sendWhenDone();
    };
    form.addEventListener("submit", hold, true);
    return () => form.removeEventListener("submit", hold, true);
  });
  useEffect(() => {
    if (busy || !sendAfter.current) return;
    sendAfter.current = false;
    box.current?.form?.requestSubmit();
  }, [busy, value]);
  useEffect(() => {
    const el = box.current;
    const caret = caretTo.current;
    if (el && caret !== null) {
      caretTo.current = null;
      if (document.activeElement === el) el.setSelectionRange(caret, caret);
      else if (anchor.current && !anchor.current.after) el.scrollTop = el.scrollHeight;
    }
    if (el && mirror.current) {
      mirror.current.scrollTop = el.scrollTop;
      mirror.current.scrollLeft = el.scrollLeft;
    }
  }, [value, shown]);
  const syncMirror = (e: React.UIEvent<HTMLTextAreaElement | HTMLInputElement>) => {
    if (!mirror.current) return;
    mirror.current.scrollTop = e.currentTarget.scrollTop;
    mirror.current.scrollLeft = e.currentTarget.scrollLeft;
  };
  const fitMirror = (div: HTMLDivElement | null) => {
    mirror.current = div;
    const el = box.current;
    if (!div || !el) return;
    const look = getComputedStyle(el);
    for (const side of ["Top", "Right", "Bottom", "Left"] as const) {
      div.style[`border${side}Width`] = look[`border${side}Width`];
      div.style[`padding${side}`] = look[`padding${side}`];
    }
    div.scrollTop = el.scrollTop;
    div.scrollLeft = el.scrollLeft;
  };
  const provisional = shown && shown.provisionalStart < shown.end && shown.value === value ? shown : null;
  const pressedAt = useRef(0);
  const latched = useRef(false);
  const viaKeys = useRef(false);
  const live = useRef(state);
  live.current = state;

  useEffect(() => {
    if (!busy) {
      latched.current = false;
      return;
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || escapeTaken(e)) return;
      takeEscape(e);
      e.stopPropagation();
      cancel();
    };
    const onUp = (e: KeyboardEvent) => {
      if (!viaKeys.current || (e.code !== "KeyV" && e.key !== "Alt")) return;
      viaKeys.current = false;
      void stop();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onUp, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onUp, true);
    };
  }, [busy, cancel, stop]);

  const pushToTalk = (e: React.KeyboardEvent): boolean => {
    if (!e.altKey || e.code !== "KeyV" || e.metaKey || e.ctrlKey) return false;
    e.preventDefault();
    if (e.repeat || busy) return true;
    viaKeys.current = true;
    begin();
    return true;
  };

  const tip = error ?? (state === "transcribing" ? "Finishing the words" : state === "listening" ? "Tap to stop" : `Tap or hold to talk (${pushKeys().join(" ")} while typing)`);
  const field =
    "block w-full rounded-lg border-2 border-w-ink bg-w-field py-2 pl-3 pr-11 text-w-base outline-none placeholder:text-w-faint focus:ring-2 focus:ring-w-teal";
  const shape = composer ? "resize-none overflow-y-auto leading-snug" : multiline ? "min-h-[5rem] resize-y" : "";
  const look = cn(field, shape, provisional && "text-transparent caret-w-ink");
  const hint = viaKeys.current ? (
    <>
      Release <PushToTalkKeys /> to stop
    </>
  ) : (
    "Tap the mic to stop"
  );
  const shared = {
    "aria-label": ariaLabel,
    placeholder: busy ? `${TALK_TEXT[state]}...` : placeholder,
    value,
    readOnly: busy,
    onChange: (e: React.ChangeEvent<HTMLTextAreaElement | HTMLInputElement>) => onChange(e.target.value),
    onScroll: syncMirror,
  };
  return (
    <div className={cn("relative min-w-0 flex-1", className)}>
      {composer ? (
        <textarea
          {...shared}
          ref={attach}
          rows={1}
          onKeyDown={(e) => {
            if (pushToTalk(e)) return;
            if (onKey?.(e)) return;
            if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
            if (e.shiftKey) return;
            e.preventDefault();
            if (busy) {
              sendWhenDone();
              return;
            }
            const el = e.currentTarget;
            if (e.ctrlKey || e.altKey) {
              const { selectionStart: from, selectionEnd: to } = el;
              const next = `${el.value.slice(0, from)}\n${el.value.slice(to)}`;
              onChange(next);
              requestAnimationFrame(() => el.setSelectionRange(from + 1, from + 1));
              return;
            }
            el.form?.requestSubmit();
          }}
          className={look}
        />
      ) : multiline ? (
        <textarea {...shared} ref={attach} onKeyDown={(e) => void pushToTalk(e)} className={look} />
      ) : (
        <input {...shared} ref={attach} onKeyDown={(e) => void pushToTalk(e)} className={look} />
      )}
      {provisional && (
        <div
          ref={fitMirror}
          aria-hidden
          className={cn(
            "pointer-events-none absolute inset-0 overflow-hidden rounded-lg border-solid border-transparent text-w-base",
            composer ? "whitespace-pre-wrap break-words leading-snug" : multiline ? "whitespace-pre-wrap break-words" : "whitespace-pre",
          )}
        >
          {provisional.value.slice(0, provisional.provisionalStart)}
          <span className="text-w-quiet">{provisional.value.slice(provisional.provisionalStart, provisional.end)}</span>
          {provisional.value.slice(provisional.end)}
          {"\u200b"}
        </div>
      )}
      <button
        type="button"
        title={tip}
        aria-label={state === "listening" ? "Stop dictating" : "Talk"}
        aria-pressed={state === "listening"}
        onPointerDown={(e) => {
          e.preventDefault();
          if (state === "transcribing") return;
          if (latched.current) {
            latched.current = false;
            pressedAt.current = 0;
            void stop();
            return;
          }
          e.currentTarget.setPointerCapture(e.pointerId);
          pressedAt.current = Date.now();
          viaKeys.current = false;
          begin();
        }}
        onPointerUp={() => {
          if (!pressedAt.current) return;
          const tapped = Date.now() - pressedAt.current < TAP_MS;
          pressedAt.current = 0;
          if (!tapped) void stop();
          else if (live.current !== "idle") latched.current = true;
        }}
        onPointerCancel={() => {
          pressedAt.current = 0;
          if (!latched.current) void stop();
        }}
        onKeyDown={(e) => {
          if (e.key !== "Enter" && e.key !== " ") return;
          e.preventDefault();
          if (busy) void stop();
          else begin();
        }}
        className={cn(
          "absolute grid place-items-center transition-colors duration-150",
          composer ? "bottom-1 right-1 h-8 w-8 rounded-lg" : "right-1.5 h-7 w-7 rounded-md",
          !composer && (multiline ? "top-1.5" : "top-1/2 -translate-y-1/2"),
          state === "listening" ? "bg-w-coral text-w-strong" : busy ? "bg-w-sand text-w-muted" : "text-w-text2 hover:bg-w-sand hover:text-w-strong",
        )}
      >
        <Microphone size={composer ? 17 : 15} weight={busy ? "fill" : "bold"} />
      </button>
      {busy && (
        <TalkStatus
          state={state}
          onCancel={cancel}
          hint={hint}
          className="absolute inset-x-0 bottom-full z-10 mb-1 rounded-lg border-2 border-w-line2 bg-w-paper px-2.5 py-1 shadow-sm"
        />
      )}
      {!busy && error && (
        <p role="alert" className="absolute inset-x-0 bottom-full z-10 mb-1 flex items-start gap-2 rounded-lg border-2 border-w-line2 bg-w-paper px-2.5 py-1.5 text-w-xs text-w-text2 shadow-sm">
          <Microphone size={13} weight="bold" className="mt-0.5 shrink-0 text-w-warn" />
          <span className="min-w-0 flex-1">{error}</span>
          <button type="button" aria-label="Dismiss" onClick={clearError} className="grid h-5 w-5 shrink-0 place-items-center rounded text-w-muted hover:bg-w-sand hover:text-w-strong">
            <X size={11} weight="bold" />
          </button>
        </p>
      )}
    </div>
  );
}

export function MicCheck() {
  const [state, setState] = useState<"idle" | "checking" | "ok" | string>("idle");
  const check = async () => {
    setState("checking");
    try {
      if (!window.isSecureContext || !navigator.mediaDevices?.getUserMedia) throw new Error("insecure");
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      stream.getTracks().forEach((t) => t.stop());
      setState("ok");
    } catch (e) {
      setState(micProblem(e));
    }
  };
  return (
    <div className="flex flex-col items-end gap-1">
      <button type="button" onClick={() => void check()} disabled={state === "checking"} className={BTN_SECONDARY_SM}>
        <Microphone size={12} weight="bold" />
        {state === "checking" ? <Shimmer>Checking</Shimmer> : "Check microphone"}
      </button>
      {state === "ok" && <span role="status" className="text-w-xs text-w-teal">The microphone works.</span>}
      {state !== "idle" && state !== "checking" && state !== "ok" && (
        <span role="alert" className="max-w-[18rem] text-right text-w-xs text-w-text2">
          {state}
        </span>
      )}
    </div>
  );
}
