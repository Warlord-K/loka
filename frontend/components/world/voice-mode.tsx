"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { ChatCircleDots, Microphone, MicrophoneSlash, PhoneDisconnect } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type WorldAgent, type WorldEvent } from "@/lib/world";
import { listen, micProblem, MODEL_PROBLEM, speech, voiceFor, type Vad } from "./speech";
import { Shimmer } from "./work-ui";
import { LiveUnavailable, primeLiveAudio, startLive, type LiveCall } from "./live-voice";
import { useSpeechStatus } from "./voice-ui";
import { CHUNKY, PANEL, PIXEL, SEGMENT, SEGMENT_ITEM, SEGMENT_OFF, SEGMENT_ON } from "./styles";
import { CloseButton } from "./settings-shell";
import { Keycap } from "./keycap";
import { SetupHint, useFeature } from "./setup-hint";
import { escapeTaken } from "@/lib/escape";

type Phase = "starting" | "connecting" | "listening" | "hearing" | "transcribing" | "thinking" | "speaking" | "error";
type Engine = "live" | "device";

const ENGINE_KEY = "world-voice-engine";

export function engineChoice(): Engine {
  try {
    return window.localStorage.getItem(ENGINE_KEY) === "device" ? "device" : "live";
  } catch {
    return "live";
  }
}

const LIVE_PHASES = new Set<Phase>(["starting", "connecting", "hearing", "transcribing", "thinking"]);

const LABEL: Record<Phase, string> = {
  starting: "Waking up the microphone",
  connecting: "Calling",
  listening: "Listening",
  hearing: "Hearing you",
  transcribing: "Catching that",
  thinking: "Thinking",
  speaking: "Speaking. Talk to interrupt.",
  error: "Something went wrong",
};

export function VoiceMode({
  agent,
  portrait,
  onClose,
  onOpenChat,
}: {
  agent: WorldAgent;
  portrait: string;
  onClose: () => void;
  onOpenChat: () => void;
}) {
  const [phase, setPhase] = useState<Phase>("starting");
  const [muted, setMuted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lines, setLines] = useState<{ id: number; key?: string; who: "you" | "them"; text: string; partial?: boolean }[]>([]);
  const transcript = useRef<HTMLUListElement>(null);
  useEffect(() => {
    const el = transcript.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);
  const [level, setLevel] = useState(0);
  const [engine, setEngine] = useState<Engine>(engineChoice);
  const [note, setNote] = useState<string | null>(null);
  const [fellBack, setFellBack] = useState(false);
  const live = useFeature("live_voice");
  const [clock, setClock] = useState({ seconds: 0, usd: 0 });
  const liveRef = useRef<LiveCall | null>(null);
  const status = useSpeechStatus();
  const phaseRef = useRef<Phase>("starting");
  const mutedRef = useRef(false);
  const vadRef = useRef<Vad | null>(null);
  const waiting = useRef(0);
  const seq = useRef(0);
  phaseRef.current = phase;
  mutedRef.current = muted;

  const say = useCallback((who: "you" | "them", text: string) => {
    setLines((prev) => [...prev.slice(-11), { id: ++seq.current, who, text }]);
  }, []);

  const caption = useCallback((key: string, who: "you" | "them", text: string, partial = false) => {
    if (!text.trim()) return;
    setLines((prev) => {
      const at = prev.findIndex((l) => l.key === key);
      if (at >= 0) return prev.map((l, i) => (i === at ? { ...l, text, partial } : l));
      return [...prev.slice(-11), { id: ++seq.current, key, who, text, partial }];
    });
  }, []);

  const pick = useCallback((next: Engine) => {
    if (next === "live") primeLiveAudio();
    setEngine(next);
    setNote(null);
    setError(null);
    setLines([]);
    try {
      window.localStorage.setItem(ENGINE_KEY, next);
    } catch {}
  }, []);

  useEffect(() => {
    if (engine !== "live") return;
    let cancelled = false;
    setPhase("connecting");
    startLive(agent.id, {
      onPhase: (p) => !cancelled && setPhase(p),
      onCaption: (key, who, text, partial) => !cancelled && caption(key, who, text, partial),
      onError: (message) => !cancelled && setError(message),
      onEnd: () => undefined,
    })
      .then((call) => {
        if (cancelled) {
          void call.close();
          return;
        }
        liveRef.current = call;
        call.setMuted(mutedRef.current);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        if (e instanceof LiveUnavailable) {
          setEngine("device");
          setFellBack(true);
          setNote("Live voice is not set up, so this call runs on your device.");
          return;
        }
        setError(e instanceof DOMException ? micProblem(e) : e instanceof Error ? e.message : "Could not start live voice.");
        setPhase("error");
      });
    const tick = window.setInterval(() => {
      const call = liveRef.current;
      if (!call) return;
      setLevel(call.level());
      const seconds = call.elapsed();
      setClock({ seconds, usd: (seconds / 60) * call.usdPerMinute });
    }, 100);
    return () => {
      cancelled = true;
      window.clearInterval(tick);
      void liveRef.current?.close();
      liveRef.current = null;
    };
  }, [engine, agent.id, caption]);

  useEffect(() => {
    liveRef.current?.setMuted(muted);
  }, [muted]);

  const heard = useCallback(
    async (audio: Float32Array) => {
      if (mutedRef.current) return;
      setPhase("transcribing");
      let text = "";
      try {
        text = (await speech().transcribe(audio)).replace(/\[[^\]]*\]/g, " ").trim();
      } catch {
        setError(MODEL_PROBLEM);
        setPhase("listening");
        return;
      }
      try {
        if (!text || /^[\s.,!?-]*$/.test(text)) {
          setPhase(waiting.current ? "thinking" : "listening");
          return;
        }
        setError(null);
        say("you", text);
        waiting.current++;
        setPhase("thinking");
        speech().warm("tts");
        await worldApi.send(agent.id, text, [], undefined, undefined, true);
      } catch {
        waiting.current = Math.max(0, waiting.current - 1);
        setError("Could not send that. Try again.");
        setPhase("listening");
      }
    },
    [agent.id, say],
  );

  useEffect(() => {
    if (engine !== "device") return;
    let closed = false;
    setPhase("starting");
    speech().unlock();
    speech().warm("stt");
    speech().warm("tts");
    listen({
      sensitivity: () => (phaseRef.current === "speaking" ? 2.4 : 1),
      onStart: () => {
        if (mutedRef.current) return;
        if (phaseRef.current === "speaking") speech().stop();
        setPhase("hearing");
      },
      onUtterance: (audio) => void heard(audio),
    })
      .then((vad) => {
        if (closed) {
          vad.close();
          return;
        }
        vadRef.current = vad;
        setPhase("listening");
      })
      .catch((e: unknown) => {
        setError(micProblem(e));
        setPhase("error");
      });
    const tick = window.setInterval(() => setLevel(vadRef.current?.level() ?? 0), 80);
    return () => {
      closed = true;
      window.clearInterval(tick);
      vadRef.current?.close();
      vadRef.current = null;
      speech().stop();
    };
  }, [heard, engine]);

  useEffect(() => {
    if (engine !== "device") return;
    const onEvent = (e: Event) => {
      const ev = (e as CustomEvent<WorldEvent>).detail;
      if (ev.type !== "message.created" || ev.agent_id !== agent.id || ev.payload.author_kind !== "agent" || ev.payload.author_agent_id || ev.payload.quiet) return;
      const body = String(ev.payload.body ?? "").trim();
      if (!body) return;
      waiting.current = Math.max(0, waiting.current - 1);
      say("them", body);
      setPhase("speaking");
      void speech()
        .speak(body, voiceFor(agent))
        .then(() => {
          if (phaseRef.current === "speaking") setPhase(waiting.current ? "thinking" : "listening");
        });
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [agent, say, engine]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (!escapeTaken(e)) onClose();
      } else if (e.key === " ") {
        e.preventDefault();
        setMuted((m) => !m);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const loading = engine === "device" ? (status.loading.stt ?? status.loading.tts) : null;
  const minutes = Math.floor(clock.seconds / 60);
  const elapsed = `${minutes}:${String(Math.floor(clock.seconds % 60)).padStart(2, "0")}`;
  const pulse = Math.min(1, level * 18);
  const active = phase === "hearing" || phase === "speaking";

  return (
    <div role="dialog" aria-modal="true" aria-label={`Voice chat with ${agent.name}`} className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col items-center justify-between gap-4 overflow-hidden p-4 sm:p-6")}>
      <div className="flex w-full shrink-0 items-center justify-between gap-3">
        <div role="radiogroup" aria-label="Voice engine" className={SEGMENT}>
          {(live?.state === "unavailable" ? (["device"] as const) : (["live", "device"] as const)).map((e) => (
            <button
              key={e}
              type="button"
              role="radio"
              aria-checked={engine === e}
              onClick={() => engine !== e && pick(e)}
              title={e === "live" ? "Realtime voice through Grok on fal" : "Free, runs on this device"}
              className={cn(SEGMENT_ITEM, engine === e ? SEGMENT_ON : SEGMENT_OFF)}
            >
              {e === "live" ? "Live" : "On-device"}
            </button>
          ))}
        </div>
        <CloseButton onClose={onClose} label="End voice chat" />
      </div>

      <div className="flex shrink-0 flex-col items-center gap-5 [@media(max-height:640px)]:gap-2">
        <div className="relative grid h-[11rem] w-[11rem] place-items-center [@media(max-height:640px)]:hidden">
          <span
            aria-hidden
            className="absolute rounded-full bg-w-teal/15 transition-[width,height] duration-100 motion-reduce:transition-none"
            style={{ width: `${150 + (active ? pulse * 90 : 0)}px`, height: `${150 + (active ? pulse * 90 : 0)}px` }}
          />
          <span aria-hidden className={cn("absolute h-[8.5rem] w-[8.5rem] rounded-full border-[3px] transition-colors duration-300", phase === "speaking" ? "border-w-gold" : active ? "border-w-teal/60" : "border-w-line2")} />
          <div className="relative rounded-2xl border-[3px] border-w-ink bg-w-aqua p-2 shadow-[0_4px_0_rgb(var(--w-ink))]">
            {portrait ? <img src={portrait} alt="" className="h-[5.75rem] w-auto [image-rendering:pixelated]" /> : <div className="h-[5.75rem] w-[4.75rem]" />}
          </div>
        </div>
        <div className="max-w-md text-center">
          <p className={cn(PIXEL, "text-w-xl leading-none text-w-strong")}>{agent.name}</p>
          <p className="mt-2 text-w-sm text-w-text2" aria-live="polite">
            {error ?? note ?? (muted ? "Muted. Press Space or the mic to talk again." : loading !== null ? <Shimmer>{`Setting up on-device voice (${loading}%)`}</Shimmer> : LIVE_PHASES.has(phase) ? <Shimmer>{LABEL[phase]}</Shimmer> : LABEL[phase])}
          </p>
        </div>
      </div>

      <div className="flex min-h-0 w-full max-w-2xl flex-1 flex-col justify-end gap-3">
        {fellBack && live && live.state === "needs_setup" && <SetupHint feature={live} compact onAction={onClose} />}
        <ul ref={transcript} className="min-h-0 space-y-1.5 overflow-y-auto overscroll-contain text-w-sm">
          {lines.map((l) => (
            <li
              key={l.id}
              className={cn(
                "w-fit max-w-[85%] whitespace-pre-wrap rounded-xl px-3 py-2 leading-relaxed [overflow-wrap:anywhere] motion-safe:animate-w-rise",
                l.who === "you" ? "ml-auto bg-w-aqua text-w-text" : "border-2 border-w-line bg-w-card text-w-text",
                l.partial && "text-w-text2",
              )}
            >
              {l.text}
              {l.partial && <span aria-hidden>...</span>}
            </li>
          ))}
          {phase === "hearing" && !muted && !(lines.length && lines[lines.length - 1].who === "you" && lines[lines.length - 1].partial) && (
            <li aria-hidden className="ml-auto w-fit rounded-xl bg-w-aqua/60 px-3 py-2">
              <Shimmer>Listening</Shimmer>
            </li>
          )}
        </ul>
        <div className="flex shrink-0 items-end justify-center gap-4 pt-2">
          <CallButton label={muted ? "Unmute" : "Mute"} pressed={muted} onClick={() => setMuted((m) => !m)} className={muted ? "bg-w-rose text-w-danger" : "bg-w-card text-w-text hover:bg-w-sand"}>
            {muted ? <MicrophoneSlash size={22} weight="bold" /> : <Microphone size={22} weight="bold" />}
          </CallButton>
          <CallButton label="Chat" onClick={onOpenChat} className="bg-w-card text-w-text hover:bg-w-sand">
            <ChatCircleDots size={22} weight="bold" />
          </CallButton>
          <CallButton label="End" onClick={onClose} className="bg-w-coral text-w-ink hover:brightness-105">
            <PhoneDisconnect size={22} weight="bold" />
          </CallButton>
        </div>
        <p className="flex flex-wrap items-center justify-center gap-x-3 gap-y-1 text-center text-w-xs text-w-muted" aria-live="off">
          <span>{engine === "live" ? `Live ${elapsed}, about $${clock.usd.toFixed(3)}, saved to the chat` : "Transcribed on this device"}</span>
          <span className="flex items-center gap-1">
            <Keycap size="sm">Space</Keycap> mutes
          </span>
          <span className="flex items-center gap-1">
            <Keycap size="sm">Esc</Keycap> ends
          </span>
        </p>
      </div>
    </div>
  );
}

function CallButton({ label, pressed, onClick, className, children }: { label: string; pressed?: boolean; onClick: () => void; className: string; children: React.ReactNode }) {
  return (
    <span className="flex flex-col items-center gap-1.5">
      <button type="button" onClick={onClick} aria-pressed={pressed} aria-label={label} className={cn(CHUNKY, "grid h-14 w-14 place-items-center rounded-xl", className)}>
        {children}
      </button>
      <span aria-hidden className="text-w-xs font-medium text-w-text2">
        {label}
      </span>
    </span>
  );
}
