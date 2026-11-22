"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "framer-motion";
import { Microphone, MicrophoneSlash, PhoneDisconnect, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type WorldAgent, type WorldEvent } from "@/lib/world";
import { listen, micProblem, MODEL_PROBLEM, speech, voiceFor, type Vad } from "./speech";
import { Shimmer } from "./work-ui";
import { LiveUnavailable, primeLiveAudio, startLive, type LiveCall } from "./live-voice";
import { useSpeechStatus } from "./voice-ui";
import { CHUNKY, SEGMENT, SEGMENT_ITEM, SEGMENT_OFF, SEGMENT_ON } from "./styles";
import { CloseButton } from "./settings-shell";
import { SetupHint, useFeature } from "./setup-hint";
import { callClock } from "@/lib/call-state";
import { AgentAvatar } from "./channel-ui";
import { NOTICE_MOTION } from "./motion";

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

const PILL_FOCUS = "outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-w-teal";

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
  minimised,
  slot,
  onMinimise,
  onRestore,
  onHangUp,
  onPill,
}: {
  agent: WorldAgent;
  portrait: string;
  minimised: boolean;
  slot: HTMLElement | null;
  onMinimise: () => void;
  onRestore: () => void;
  onHangUp: () => void;
  onPill: (shown: boolean) => void;
}) {
  const [phase, setPhase] = useState<Phase>("starting");
  const [muted, setMuted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lines, setLines] = useState<{ id: number; key?: string; who: "you" | "them"; text: string; partial?: boolean }[]>([]);
  const [engine, setEngine] = useState<Engine>(engineChoice);
  const [note, setNote] = useState<string | null>(null);
  const [fellBack, setFellBack] = useState(false);
  const [settled, setSettled] = useState(false);
  const [ended, setEnded] = useState(false);
  useEffect(() => {
    const id = window.setTimeout(() => setSettled(true), 300);
    return () => window.clearTimeout(id);
  }, []);
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
    setEnded(false);
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
      onEnd: () => {
        if (cancelled) return;
        liveRef.current = null;
        setEnded(true);
      },
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
      const seconds = call.elapsed();
      setClock({ seconds, usd: (seconds / 60) * call.usdPerMinute });
    }, 250);
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
    const startedAt = Date.now();
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
    const tick = window.setInterval(() => setClock({ seconds: (Date.now() - startedAt) / 1000, usd: 0 }), 250);
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

  const loading = engine === "device" ? (status.loading.stt ?? status.loading.tts) : null;
  const elapsed = callClock(clock.seconds);
  const statusText = ended ? (error ? `Call ended. ${error}` : "Call ended") : error ?? note ?? (muted ? "Muted" : loading !== null ? `Setting up on-device voice (${loading}%)` : LABEL[phase]);
  const statusLive = !ended && !error && !note && !muted && (loading !== null || LIVE_PHASES.has(phase));
  const latest = lines[lines.length - 1];
  const docked = !minimised && slot;
  const showPill = !docked && (minimised || settled);
  useEffect(() => onPill(showPill), [showPill, onPill]);
  useEffect(() => () => onPill(false), [onPill]);

  const bar = (
    <section aria-label={`Call with ${agent.name}`} className="flex shrink-0 flex-col gap-2 border-b-2 border-w-line bg-w-aqua/40 px-3 py-2.5 sm:px-4">
      <div className="flex items-center gap-2">
        <div className="min-w-0 flex-1">
          <p className="flex items-baseline gap-2 text-w-sm font-semibold text-w-strong">
            <span className="truncate">{engine === "live" ? "Live call" : "Voice chat"}</span>
            {!ended && <span className="shrink-0 font-normal tabular-nums text-w-text2">{elapsed}</span>}
            {engine === "live" && clock.usd > 0 && <span className="hidden shrink-0 text-w-xs font-normal text-w-muted @md:inline">{`about $${clock.usd.toFixed(3)}`}</span>}
          </p>
          <p className={cn("truncate text-w-xs", error ? "text-w-danger" : "text-w-text2")} aria-live="polite" title={ended ? statusText : undefined}>
            {statusLive ? <Shimmer>{statusText}</Shimmer> : statusText}
          </p>
        </div>
        <div role="radiogroup" aria-label="Voice engine" className={cn(SEGMENT, "hidden shrink-0 @lg:inline-flex")}>
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
        {ended ? (
          <CloseButton onClose={onHangUp} label="Close the ended call" />
        ) : (
          <>
            <button
              type="button"
              onClick={() => setMuted((m) => !m)}
              aria-pressed={muted}
              aria-label={muted ? "Unmute" : "Mute"}
              title={muted ? "Unmute" : "Mute"}
              className={cn(CHUNKY, "grid h-10 w-10 shrink-0 place-items-center", muted ? "bg-w-rose text-w-danger" : "bg-w-card text-w-text hover:bg-w-sand")}
            >
              {muted ? <MicrophoneSlash size={18} weight="bold" /> : <Microphone size={18} weight="bold" />}
            </button>
            <button
              type="button"
              onClick={onHangUp}
              aria-label={`Hang up on ${agent.name}`}
              title="Hang up"
              className={cn(CHUNKY, "flex h-10 shrink-0 items-center gap-1.5 bg-w-coral px-3 text-w-sm font-semibold text-w-ink hover:brightness-105")}
            >
              <PhoneDisconnect size={18} weight="bold" />
              <span className="hidden @sm:inline">Hang up</span>
            </button>
            <CloseButton onClose={onMinimise} label="Minimise the call, it keeps going" />
          </>
        )}
      </div>
      {latest && (
        <p className={cn("line-clamp-2 text-w-sm leading-snug [overflow-wrap:anywhere]", latest.who === "you" ? "text-w-text2" : "text-w-text")}>
          <span className="font-semibold text-w-strong">{latest.who === "you" ? "You" : agent.name}: </span>
          {latest.text}
          {latest.partial && <span aria-hidden>...</span>}
        </p>
      )}
      {fellBack && live && live.state === "needs_setup" && <SetupHint feature={live} compact onAction={onMinimise} />}
    </section>
  );

  return (
    <>
      {docked && createPortal(bar, slot)}
      <AnimatePresence>
        {showPill && (
          <motion.div
            key="pill"
            {...NOTICE_MOTION}
            className="pointer-events-none absolute inset-x-0 top-[max(0.5rem,env(safe-area-inset-top))] z-40 flex justify-center px-3"
          >
            <div
              role="group"
              aria-label={`Call with ${agent.name}`}
              className="pointer-events-auto flex h-11 max-w-full items-stretch overflow-hidden rounded-lg border-2 border-w-ink bg-w-paper text-w-sm text-w-text shadow-[0_3px_0_rgb(var(--w-ink))]"
            >
              <button
                type="button"
                onClick={onRestore}
                aria-label={`Back to the call with ${agent.name}, ${statusText}, ${elapsed}`}
                title="Back to the call"
                className={cn(PILL_FOCUS, "flex min-w-0 items-center gap-2 pl-1.5 pr-3 hover:bg-w-sand")}
              >
                <AgentAvatar portrait={portrait} emoji={agent.avatar_emoji} size="2rem" soft rounded="rounded-md" />
                <span className="min-w-0 max-w-[9rem] truncate font-semibold text-w-strong sm:max-w-[14rem]">{agent.name}</span>
                <span className={cn("shrink-0", ended ? "text-w-danger" : "tabular-nums text-w-text2")}>{ended ? "Call ended" : elapsed}</span>
              </button>
              {!ended && (
                <button
                  type="button"
                  onClick={() => setMuted((m) => !m)}
                  aria-pressed={muted}
                  aria-label={muted ? "Unmute" : "Mute"}
                  title={muted ? "Unmute" : "Mute"}
                  className={cn(PILL_FOCUS, "grid w-11 shrink-0 place-items-center border-l-2 border-w-ink", muted ? "bg-w-rose text-w-danger" : "hover:bg-w-sand")}
                >
                  {muted ? <MicrophoneSlash size={18} weight="bold" /> : <Microphone size={18} weight="bold" />}
                </button>
              )}
              <button
                type="button"
                onClick={onHangUp}
                aria-label={ended ? "Close the ended call" : `Hang up on ${agent.name}`}
                title={ended ? "Close" : "Hang up"}
                className={cn(PILL_FOCUS, "grid w-11 shrink-0 place-items-center border-l-2 border-w-ink", ended ? "hover:bg-w-sand" : "bg-w-coral text-w-ink hover:brightness-105")}
              >
                {ended ? <X size={18} weight="bold" /> : <PhoneDisconnect size={18} weight="bold" />}
              </button>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
}
