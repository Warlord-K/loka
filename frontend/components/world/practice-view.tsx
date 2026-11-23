"use client";

import { openAcademy } from "@/lib/loka";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { motion } from "framer-motion";
import {
  ArrowLeft,
  ArrowUUpLeft,
  ArrowUUpRight,
  Broom,
  Eraser,
  GridFour,
  HandPointing,
  Highlighter,
  PenNib,
  PencilSimple,
  Plus,
  Student,
  X,
  type Icon as PhosphorIcon,
} from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type WorldAgent } from "@/lib/world";
import {
  attemptFromItem,
  boardSize,
  checkOf,
  coachLate,
  prepareInk,
  cleanSubject,
  hasInk,
  INK_COLOR_LABEL,
  INK_COLORS,
  isReadyWord,
  practiceApi,
  PracticeError,
  PRACTICE_VOICE,
  subjectLabel,
  SUBJECTS,
  TOOL_SIZES,
  type Attempt,
  type CoachOption,
  type InkTool,
  type PracticeDetail,
} from "@/lib/practice";
import type { ReportDetail } from "@/lib/reports";
import { AgentFace } from "./channel-ui";
import { RichText } from "./chat-kit";
import { CopyButton } from "./chat-kit";
import { WSelect } from "./controls";
import { Keycap, isMac } from "./keycap";
import { MODAL_MOTION, SCRIM_MOTION } from "./motion";
import { PracticePad, type PadHandle, type PadState } from "./practice-pad";
import { quoteProps } from "./quote";
import { RelativeTime } from "./relative-time";
import { Shimmer } from "./shimmer";
import { BTN_GHOST, BTN_PRIMARY, BTN_SECONDARY, BTN_SECONDARY_SM, EASE, FOCUS, ICON_BTN, INPUT, MODAL, PILL_INFO, PILL_NEUTRAL, PILL_OK, SCRIM, SEGMENT, SEGMENT_ITEM, SEGMENT_OFF, SEGMENT_ON } from "./styles";
import { VoiceField } from "./voice-ui";
import { MeaningIcon } from "@/lib/meaning-icons";

const TOOLS: { tool: InkTool; label: string; icon: PhosphorIcon }[] = [
  { tool: "pen", label: "Pen", icon: PenNib },
  { tool: "pencil", label: "Pencil", icon: PencilSimple },
  { tool: "marker", label: "Marker", icon: Highlighter },
  { tool: "eraser", label: "Eraser", icon: Eraser },
];

const SWATCH: Record<string, string> = {
  "w-strong": "bg-w-strong",
  "w-danger": "bg-w-danger",
  "w-link": "bg-w-link",
  "w-chart-3": "bg-w-chart-3",
  "w-chart-4": "bg-w-chart-4",
};

const PEN_KEY = "world.practice.pen";

type PenPrefs = { tool: InkTool; color: string; sizes: Record<InkTool, number>; guide: boolean; finger: boolean };

const DEFAULT_PEN: PenPrefs = { tool: "pen", color: "w-strong", sizes: { pen: 1, pencil: 1, marker: 1, eraser: 1 }, guide: true, finger: false };

function loadPen(): PenPrefs {
  try {
    const raw = JSON.parse(window.localStorage.getItem(PEN_KEY) ?? "null") as Partial<PenPrefs> | null;
    if (!raw) return DEFAULT_PEN;
    return {
      tool: TOOLS.some((t) => t.tool === raw.tool) ? (raw.tool as InkTool) : "pen",
      color: INK_COLORS.includes(raw.color as (typeof INK_COLORS)[number]) ? (raw.color as string) : "w-strong",
      sizes: { ...DEFAULT_PEN.sizes, ...(raw.sizes ?? {}) },
      guide: raw.guide !== false,
      finger: raw.finger === true,
    };
  } catch {
    return DEFAULT_PEN;
  }
}

function coachLabel(c: CoachOption): string {
  return c.paused_reason ? `${c.name} (paused)` : c.name;
}

function readyKeys(): string[] {
  return [isMac() ? "⌘" : "Ctrl", "Enter"];
}

function errorText(e: unknown, fallback: string): string {
  return e instanceof PracticeError || e instanceof Error ? e.message || fallback : fallback;
}

function CheckBody({
  attempt,
  coach,
  late,
  resend,
  onOpenAgent,
}: {
  attempt: Attempt;
  coach: WorldAgent | undefined;
  late: boolean;
  resend: { busy: boolean; run: () => void } | null;
  onOpenAgent: (id: string) => void;
}) {
  const check = checkOf(attempt.check);
  const handles = useMemo(() => new Map<string, WorldAgent>(coach ? [[coach.slug.toLowerCase(), coach]] : []), [coach]);
  if (!check && attempt.grade_status === "error") {
    return <p className="text-w-sm text-w-text2">A newer attempt replaced this one, so it was not graded.</p>;
  }
  if (!check && late) {
    return (
      <div className="flex flex-wrap items-center gap-2" aria-live="polite">
        <p className="min-w-0 flex-1 text-w-sm text-w-text2">{coach ? `${coach.name} hasn't answered yet.` : "Coach hasn't answered yet."}</p>
        {resend && (
          <button type="button" onClick={resend.run} disabled={resend.busy} className={cn(BTN_SECONDARY_SM, "disabled:opacity-60")}>
            {resend.busy ? <Shimmer>Resending</Shimmer> : "Resend"}
          </button>
        )}
      </div>
    );
  }
  if (!check) {
    return (
      <p className="text-w-sm text-w-text2" aria-live="polite">
        <Shimmer>{coach ? `${coach.name} is looking at attempt ${attempt.n}` : `Waiting for a coach to look at attempt ${attempt.n}`}</Shimmer>
      </p>
    );
  }
  const parts: [string, string, string][] = [
    ["right", "What is right", check.right],
    ["fix", "What to fix", check.fix],
    ["next", "Next exercise", check.next],
  ];
  const xp = attempt.xp_awarded ?? 0;
  return (
    <div className="space-y-3" aria-live="polite">
      {(attempt.grade_status === "passed" || attempt.grade_status === "failed") && (
        <p className={cn("text-w-sm font-semibold", attempt.grade_status === "passed" ? "text-w-ok" : "text-w-warn")}>
          {attempt.grade_status === "passed" ? "Passed" : "Not passed yet"}
          {xp > 0 && <span className="ml-1.5 font-normal text-w-muted">+{xp} xp</span>}
        </p>
      )}
      {typeof check.score === "number" && (
        <p className="flex items-baseline gap-1.5">
          <span className="text-w-xl font-semibold tabular-nums text-w-strong">{check.score}</span>
          <span className="text-w-xs text-w-muted">out of 100</span>
        </p>
      )}
      {parts.map(([key, heading, text]) => (
        <section key={key}>
          <h4 className={cn("text-w-xs font-semibold", key === "fix" ? "text-w-warn" : key === "right" ? "text-w-ok" : "text-w-link")}>{heading}</h4>
          <RichText text={text} handles={handles} onOpenAgent={onOpenAgent} className="text-w-sm leading-relaxed text-w-text" />
        </section>
      ))}
    </div>
  );
}

export function PracticeView({
  detail,
  agents,
  meId,
  focusItem,
  focusRequest,
  onOpenAgent,
  onChanged,
  embedded = false,
}: {
  detail: ReportDetail;
  agents: WorldAgent[];
  meId: string | null;
  focusItem: string | null;
  focusRequest?: number;
  onOpenAgent: (id: string) => void;
  onChanged: () => void;
  embedded?: boolean;
}) {
  const report = detail.report;
  const [data, setData] = useState<PracticeDetail | null>(null);
  const [coaches, setCoaches] = useState<CoachOption[] | null>(null);
  const [viewing, setViewing] = useState<string | null>(() => attemptFromItem(focusItem));
  const readOnly = meId !== null && report.owner_user_id !== meId;
  const [prefs, setPrefs] = useState<PenPrefs>(() => (typeof window === "undefined" ? DEFAULT_PEN : loadPen()));
  const [pad, setPadState] = useState<PadState>({ count: 0, canUndo: false, canRedo: false, version: 0 });
  const padVersion = useRef(0);
  const setPad = useCallback((next: PadState) => {
    padVersion.current = next.version;
    setPadState(next);
  }, []);
  const [sentVersion, setSentVersion] = useState<number | null>(null);
  const sentRef = useRef<number | null>(null);
  sentRef.current = sentVersion;
  const [locked, setLocked] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const [resentAt, setResentAt] = useState<Record<string, number>>({});
  const [resending, setResending] = useState(false);
  const [resendGone, setResendGone] = useState(false);
  const [penSeen, setPenSeen] = useState(false);
  const [note, setNoteState] = useState("");
  const noteRef = useRef("");
  const setNote = (next: string | ((cur: string) => string)) => {
    const value = typeof next === "function" ? next(noteRef.current) : next;
    noteRef.current = value;
    setNoteState(value);
  };
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [title, setTitle] = useState(report.title);
  const padRef = useRef<PadHandle>(null);
  const root = useRef<HTMLDivElement>(null);
  const subject = data?.practice.subject ?? "";
  const coachId = data?.practice.coach_agent_id ?? null;
  const coach = coachId ? agents.find((a) => a.id === coachId) : undefined;
  const board = useMemo(() => boardSize(subject || "drawing"), [subject]);
  const attempts = data?.attempts ?? [];
  const latest = attempts[0] ?? null;
  const shown = viewing ? attempts.find((a) => a.id.startsWith(viewing)) ?? null : null;
  const panelAttempt = shown ?? latest;
  const displayed = shown ?? (readOnly ? latest : null);
  const readyBlocked = sentVersion !== null && pad.version === sentVersion;
  const shownRef = useRef(shown);
  shownRef.current = shown;
  const readOnlyRef = useRef(readOnly);
  readOnlyRef.current = readOnly;
  const waitingKey = attempts.filter((a) => !checkOf(a.check)).map((a) => a.id).join(",");

  useEffect(() => {
    if (!waitingKey) return;
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 15_000);
    return () => window.clearInterval(t);
  }, [waitingKey]);

  useEffect(() => {
    try {
      window.localStorage.setItem(PEN_KEY, JSON.stringify(prefs));
    } catch {}
  }, [prefs]);
  useEffect(() => setTitle(report.title), [report.title]);
  useEffect(() => {
    const id = attemptFromItem(focusItem);
    if (id) setViewing(id);
  }, [focusItem, focusRequest]);

  const load = useCallback(() => {
    practiceApi
      .get(report.id)
      .then((d) => {
        setData(d);
        setLocked(false);
      })
      .catch((e) => {
        if (e instanceof PracticeError && (e.status === 404 || e.status === 403) && readOnlyRef.current) setLocked(true);
        else setError(errorText(e, "Could not open this practice."));
      });
  }, [report.id]);
  useEffect(() => load(), [load]);

  useEffect(() => {
    if (!subject) return;
    let live = true;
    practiceApi
      .coaches(subject)
      .then((c) => live && setCoaches(c.agents.filter((a) => a.id === coachId || !a.paused_reason)))
      .catch(() => live && setCoaches([]));
    return () => {
      live = false;
    };
  }, [subject, coachId]);

  useEffect(() => {
    const on = (e: Event) => {
      const ev = (e as CustomEvent<{ type: string; payload: Record<string, unknown> }>).detail;
      if (!ev?.type?.startsWith("practice.") || ev.payload?.report_id !== report.id) return;
      load();
    };
    window.addEventListener("world-event", on);
    return () => window.removeEventListener("world-event", on);
  }, [load, report.id]);

  const size = TOOL_SIZES[prefs.tool][prefs.sizes[prefs.tool]] ?? TOOL_SIZES[prefs.tool][1];
  const pen = useMemo(() => ({ tool: prefs.tool, color: prefs.color, size }), [prefs.tool, prefs.color, size]);

  const ready = useCallback(
    async (spokenNote?: string) => {
      const handle = padRef.current;
      if (!handle || sending || !data || readOnlyRef.current || shownRef.current) return;
      setError(null);
      if (sentRef.current !== null && sentRef.current === padVersion.current) {
        setError("This attempt is already with your coach. Change the page or start a new attempt to send another.");
        return;
      }
      const raw = handle.doc();
      if (!hasInk(raw.strokes)) {
        setError("Write or draw something first, then press Ready.");
        return;
      }
      const prepared = prepareInk(raw);
      if (!prepared.ok) {
        setError(prepared.error);
        return;
      }
      const doc = prepared.doc;
      const sentAt = padVersion.current;
      if (!data.practice.coach_agent_id) {
        setError("Pick a coach for this practice first.");
        return;
      }
      setSending(true);
      try {
        const png = await handle.png();
        const upload = await worldApi.upload(png, `practice-${attempts.length + 1}.png`);
        const text = (spokenNote ?? noteRef.current).trim();
        const attempt = await practiceApi.ready(report.id, { image_upload_id: upload.id, strokes: doc, note: text || undefined });
        setSentVersion(sentAt);
        setData((cur) => (cur ? { ...cur, attempts: [attempt, ...cur.attempts.filter((a) => a.id !== attempt.id)] } : cur));
        setNote("");
        setViewing(null);
        onChanged();
      } catch (e) {
        setError(errorText(e, "Could not send this attempt. Try again."));
      } finally {
        setSending(false);
      }
    },
    [sending, data, attempts.length, report.id, onChanged],
  );
  const readyRef = useRef(ready);
  readyRef.current = ready;

  const visible = () => {
    const el = root.current;
    return Boolean(el && el.isConnected && el.getClientRects().length > 0 && !el.closest("[inert],[aria-hidden='true']"));
  };
  const drawing = () => visible() && !readOnlyRef.current && !shownRef.current;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = root.current;
      if (!el || !drawing()) return;
      const typing = e.target instanceof HTMLElement && (e.target.isContentEditable || e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA");
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key === "Enter" && (!typing || el.contains(e.target as Node))) {
        e.preventDefault();
        e.stopPropagation();
        void readyRef.current();
        return;
      }
      if (typing || !mod || e.altKey || e.key.toLowerCase() !== "z") return;
      e.preventDefault();
      e.stopPropagation();
      padRef.current?.act({ type: e.shiftKey ? "redo" : "undo" });
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  useEffect(() => {
    const onVoice = (e: Event) => {
      if (!visible() || readOnlyRef.current) return;
      const text = (e as CustomEvent<{ text: string }>).detail?.text ?? "";
      if (shownRef.current && !isReadyWord(text)) return;
      e.preventDefault();
      if (shownRef.current) return;
      if (isReadyWord(text)) void readyRef.current();
      else setNote((cur) => (cur ? `${cur} ${text}` : text));
    };
    window.addEventListener(PRACTICE_VOICE, onVoice);
    return () => window.removeEventListener(PRACTICE_VOICE, onVoice);
  }, []);

  const changeCoach = async (id: string) => {
    if (!id || id === coachId) return;
    setError(null);
    try {
      await practiceApi.update(report.id, { coach_agent_id: id });
      load();
      onChanged();
    } catch (e) {
      setError(errorText(e, "Could not change the coach."));
    }
  };

  const rename = async () => {
    const next = title.trim();
    if (!next || next === report.title) {
      setTitle(report.title);
      return;
    }
    try {
      await practiceApi.update(report.id, { title: next });
      onChanged();
    } catch (e) {
      setTitle(report.title);
      setError(errorText(e, "Could not rename this practice."));
    }
  };

  const resendAttempt = async (attempt: Attempt) => {
    setResending(true);
    try {
      await practiceApi.resend(attempt.id);
      setResentAt((cur) => ({ ...cur, [attempt.id]: Date.now() }));
      setNow(Date.now());
    } catch (e) {
      if (e instanceof PracticeError && (e.status === 404 || e.status === 405)) setResendGone(true);
      else setError(errorText(e, "Could not resend. Try again."));
    } finally {
      setResending(false);
    }
  };

  const newAttempt = () => {
    padRef.current?.act({ type: "reset" });
    setViewing(null);
    setNote("");
    setError(null);
  };

  const toolbar = (
    <div className="flex flex-wrap items-center gap-1.5" role="toolbar" aria-label="Drawing tools">
      <div className={SEGMENT} role="group" aria-label="Tool">
        {TOOLS.map(({ tool, label, icon: ToolIcon }) => (
          <button
            key={tool}
            type="button"
            aria-pressed={prefs.tool === tool}
            title={label}
            aria-label={label}
            onClick={() => setPrefs((p) => ({ ...p, tool }))}
            className={cn(SEGMENT_ITEM, "grid h-8 w-9 place-items-center px-0 [@media(pointer:coarse)]:h-10 [@media(pointer:coarse)]:w-11", prefs.tool === tool ? SEGMENT_ON : SEGMENT_OFF)}
          >
            <ToolIcon size={16} weight={prefs.tool === tool ? "fill" : "bold"} />
          </button>
        ))}
      </div>
      <div className={SEGMENT} role="group" aria-label="Size">
        {TOOL_SIZES[prefs.tool].map((s, i) => (
          <button
            key={s}
            type="button"
            aria-pressed={prefs.sizes[prefs.tool] === i}
            aria-label={["Thin", "Medium", "Thick"][i]}
            title={["Thin", "Medium", "Thick"][i]}
            onClick={() => setPrefs((p) => ({ ...p, sizes: { ...p.sizes, [p.tool]: i } }))}
            className={cn(SEGMENT_ITEM, "grid h-8 w-8 place-items-center px-0 [@media(pointer:coarse)]:h-10 [@media(pointer:coarse)]:w-10", prefs.sizes[prefs.tool] === i ? SEGMENT_ON : SEGMENT_OFF)}
          >
            <span aria-hidden className="block rounded-full bg-current" style={{ width: `${0.25 + i * 0.2}rem`, height: `${0.25 + i * 0.2}rem` }} />
          </button>
        ))}
      </div>
      {prefs.tool !== "eraser" && (
        <div className={cn(SEGMENT, "gap-0.5")} role="group" aria-label="Colour">
          {INK_COLORS.map((c) => (
            <button
              key={c}
              type="button"
              aria-pressed={prefs.color === c}
              aria-label={INK_COLOR_LABEL[c]}
              title={INK_COLOR_LABEL[c]}
              onClick={() => setPrefs((p) => ({ ...p, color: c }))}
              className={cn("grid h-8 w-8 place-items-center rounded-md transition-colors [@media(pointer:coarse)]:h-10 [@media(pointer:coarse)]:w-10", EASE, FOCUS, "focus-visible:ring-offset-0", prefs.color === c ? "bg-w-sand" : "hover:bg-w-sand/60")}
            >
              <span aria-hidden className={cn("block h-4 w-4 rounded-md border-2", SWATCH[c], prefs.color === c ? "border-w-ink" : "border-w-line2")} />
            </button>
          ))}
        </div>
      )}
      <div className="ml-auto flex items-center gap-1.5">
        <button type="button" onClick={() => padRef.current?.act({ type: "undo" })} disabled={!pad.canUndo} aria-label="Undo" title="Undo" className={cn(ICON_BTN, "disabled:opacity-40")}>
          <ArrowUUpLeft size={15} weight="bold" />
        </button>
        <button type="button" onClick={() => padRef.current?.act({ type: "redo" })} disabled={!pad.canRedo} aria-label="Redo" title="Redo" className={cn(ICON_BTN, "disabled:opacity-40")}>
          <ArrowUUpRight size={15} weight="bold" />
        </button>
        <button type="button" onClick={() => padRef.current?.act({ type: "clear" })} disabled={pad.count === 0} aria-label="Clear" title="Clear the page" className={cn(ICON_BTN, "disabled:opacity-40")}>
          <Broom size={15} weight="bold" />
        </button>
        {subject === "japanese" && (
          <button
            type="button"
            aria-pressed={prefs.guide}
            onClick={() => setPrefs((p) => ({ ...p, guide: !p.guide }))}
            aria-label="Guide grid"
            title={prefs.guide ? "Hide the guide grid" : "Show the four square guide grid"}
            className={cn(ICON_BTN, prefs.guide && "!bg-w-invert !text-w-oninvert")}
          >
            <GridFour size={15} weight="bold" />
          </button>
        )}
        {penSeen && (
          <button
            type="button"
            aria-pressed={prefs.finger}
            onClick={() => setPrefs((p) => ({ ...p, finger: !p.finger }))}
            aria-label="Draw with a finger too"
            title={prefs.finger ? "Fingers draw too" : "Only the pen draws, so your palm can rest on the screen"}
            className={cn(ICON_BTN, prefs.finger && "!bg-w-invert !text-w-oninvert")}
          >
            <HandPointing size={15} weight="bold" />
          </button>
        )}
      </div>
    </div>
  );

  const coachPicker = (
    <WSelect
      value={coachId ?? ""}
      onChange={(v) => void changeCoach(v)}
      options={(coaches ?? []).map((c) => ({ value: c.id, label: coachLabel(c), hint: c.expertise ? c.expertise.slice(0, 28) : undefined }))}
      placeholder="Pick a coach"
      ariaLabel="Coach"
      size="sm"
      icon={coach ? <AgentFace agent={coach} size={18} /> : <Student size={14} weight="duotone" />}
      className="max-w-[14rem]"
    />
  );

  const strip = attempts.length > 0 && (
    <nav aria-label="Attempts" className="min-w-0">
      <ol className="flex gap-2 overflow-x-auto pb-1">
        {attempts.map((a) => {
          const on = displayed ? displayed.id === a.id : false;
          const check = checkOf(a.check);
          return (
            <li key={a.id} className="shrink-0">
              <button
                type="button"
                onClick={() => setViewing(on ? null : a.id)}
                aria-pressed={on}
                aria-label={`Attempt ${a.n}${check ? ", checked" : ", waiting for the coach"}`}
                title={`Attempt ${a.n}`}
                className={cn("relative block h-16 w-16 overflow-hidden rounded-lg border-2 bg-w-field transition-[border-color,transform]", EASE, FOCUS, "focus-visible:ring-offset-0", on ? "border-w-ink" : "border-w-line2 hover:border-w-ink")}
              >
                {a.image_upload_id && <img src={`/api/world/uploads/${a.image_upload_id}`} alt="" loading="lazy" className="h-full w-full object-contain" />}
                <span className="absolute left-0.5 top-0.5 rounded-[0.3rem] bg-w-invert px-1 text-w-3xs font-semibold tabular-nums text-w-oninvert">{a.n}</span>
                {check ? (
                  typeof check.score === "number" && <span className="absolute bottom-0.5 right-0.5 rounded-[0.3rem] bg-w-mint px-1 text-w-3xs font-semibold tabular-nums text-w-ok">{check.score}</span>
                ) : (
                  <span className="absolute inset-x-0 bottom-0 bg-w-paper/90 text-center text-w-3xs">
                    <Shimmer>checking</Shimmer>
                  </span>
                )}
              </button>
            </li>
          );
        })}
      </ol>
    </nav>
  );

  const panel = (
    <aside className="flex min-h-0 flex-col gap-3 @3xl/practice:w-80 @3xl/practice:shrink-0 @3xl/practice:overflow-y-auto">
      <div
        {...(panelAttempt && checkOf(panelAttempt.check)
          ? quoteProps(panelAttempt.coach_message_id ? `msg:${panelAttempt.coach_message_id}` : `report:${panelAttempt.report_id}`, `Attempt ${panelAttempt.n} of ${report.title}`)
          : {})}
        className="order-2 select-text rounded-xl border-2 border-w-line bg-w-card p-3 @3xl/practice:order-none"
      >
        {panelAttempt ? (
          <>
            <div className="mb-2 flex items-center gap-2">
              {coach ? <AgentFace agent={coach} size={22} /> : <Student size={18} weight="duotone" className="text-w-muted" />}
              <h3 className="min-w-0 flex-1 truncate text-w-sm font-semibold text-w-strong">Attempt {panelAttempt.n}</h3>
              <RelativeTime at={panelAttempt.checked_at ?? panelAttempt.created_at} className="shrink-0 text-w-2xs text-w-muted" />
              <CopyButton text={`practice:${panelAttempt.id}`} label="Copy the attempt card to paste in a chat" />
            </div>
            <CheckBody
              attempt={panelAttempt}
              coach={coach}
              late={!checkOf(panelAttempt.check) && coachLate(panelAttempt.created_at, resentAt[panelAttempt.id] ?? null, now)}
              resend={!readOnly && !resendGone ? { busy: resending, run: () => void resendAttempt(panelAttempt) } : null}
              onOpenAgent={onOpenAgent}
            />
            {panelAttempt.note && <p className="mt-3 border-t-2 border-dashed border-w-line pt-2 text-w-xs text-w-text2">{readOnly ? "Note" : "Your note"}: {panelAttempt.note}</p>}
          </>
        ) : locked ? (
          <p className="text-w-sm text-w-text2">Only the person practising can see the attempts on this canvas.</p>
        ) : readOnly ? (
          <p className="text-w-sm text-w-text2">No attempts yet.</p>
        ) : (
          <div className="space-y-1.5">
            <p className="flex items-center gap-1.5 text-w-sm font-semibold text-w-strong">
              <MeaningIcon.tip size={14} weight="duotone" className="text-w-link" /> How it works
            </p>
            <p className="text-w-sm leading-relaxed text-w-text2">
              {subject === "japanese" ? "Write a character in the grid" : "Draw on the page"}, then press Ready or say ready. {coach ? coach.name : "Your coach"} sees the picture and every stroke, and answers with what is right, what to fix and the next exercise.
            </p>
          </div>
        )}
      </div>
      {!shown && !readOnly && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void ready();
          }}
          className="order-1 flex flex-col gap-2 @3xl/practice:order-none"
        >
          <span className="text-w-xs font-semibold text-w-text2" aria-hidden>
            Note for {coach ? coach.name : "your coach"} <span className="font-normal text-w-muted">(optional, or say ready)</span>
          </span>
          <div className="flex">
            <VoiceField
              value={note}
              onChange={setNote}
              onDictated={() => {
                if (!isReadyWord(noteRef.current) || !drawing()) return;
                setNote("");
                void readyRef.current("");
              }}
              placeholder="Stroke 3 felt off"
              ariaLabel={`Note for ${coach ? coach.name : "your coach"}`}
              className="w-full"
            />
          </div>
          <div className="flex items-center gap-2">
            <button type="submit" disabled={sending || !data || readyBlocked} title={readyBlocked ? "Already sent. Change the page or start a new attempt" : undefined} className={cn(BTN_PRIMARY, "flex-1 disabled:opacity-60")}>
              {sending ? <Shimmer>Sending</Shimmer> : readyBlocked ? "Sent" : "Ready"}
            </button>
            <span className="hidden items-center gap-0.5 [@media(pointer:fine)]:inline-flex" aria-hidden>
              {readyKeys().map((k) => (
                <Keycap key={k}>{k}</Keycap>
              ))}
            </span>
          </div>
        </form>
      )}
      {error && (
        <p role="alert" className="order-1 text-w-xs text-w-danger @3xl/practice:order-none">
          {error}
        </p>
      )}
    </aside>
  );

  return (
    <div ref={root} className="flex h-full min-h-0 flex-col @container/practice">
      <div className="flex flex-wrap items-center gap-2 border-b-2 border-dashed border-w-line2 px-3 py-2">
        {readOnly ? (
          <h2 className="min-w-0 flex-1 basis-40 truncate px-1 py-0.5 text-w-md font-semibold text-w-strong">{report.title}</h2>
        ) : (
        <input
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onBlur={() => void rename()}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            if (e.key === "Escape") {
              e.preventDefault();
              setTitle(report.title);
              (e.target as HTMLInputElement).blur();
            }
          }}
          aria-label="Practice title"
          className="min-w-0 flex-1 basis-40 rounded-md bg-transparent px-1 py-0.5 text-w-md font-semibold text-w-strong outline-none hover:bg-w-sand/60 focus:bg-w-field focus:ring-2 focus:ring-w-teal"
        />
        )}
        {subject &&
          (detail.lesson && !readOnly && !embedded ? (
            <button type="button" onClick={() => openAcademy(detail.lesson)} title="Open this lesson's course in the Academy" className={cn(PILL_INFO, "hover:underline")}>
              Lesson · {subjectLabel(subject)}
            </button>
          ) : (
            <span className={PILL_INFO}>{detail.lesson ? "Lesson" : "Practice"} · {subjectLabel(subject)}</span>
          ))}
        {attempts.length > 0 && <span className={PILL_NEUTRAL}>{attempts.length === 1 ? "1 attempt" : `${attempts.length} attempts`}</span>}
        {readOnly ? (
          coach && (
            <span className={cn(PILL_NEUTRAL, "gap-1.5")}>
              <AgentFace agent={coach} size={14} /> {coach.name}
            </span>
          )
        ) : (
          coachPicker
        )}
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-3 @3xl/practice:flex-row @3xl/practice:overflow-hidden">
        <div className="flex min-w-0 shrink-0 flex-col gap-2 @3xl/practice:min-h-0 @3xl/practice:flex-1 @3xl/practice:shrink">
          {displayed ? (
            <div className="flex items-center gap-2">
              {!readOnly && (
                <button type="button" onClick={() => setViewing(null)} className={BTN_SECONDARY_SM}>
                  <ArrowLeft size={13} weight="bold" /> Back to drawing
                </button>
              )}
              <span className="text-w-xs text-w-muted">
                Attempt {displayed.n}, {displayed.stroke_count} {displayed.stroke_count === 1 ? "stroke" : "strokes"}
              </span>
              {checkOf(displayed.check) && <span className={PILL_OK}>Checked</span>}
            </div>
          ) : (
            !readOnly && toolbar
          )}
          <div className={cn("relative w-full shrink-0 @3xl/practice:aspect-auto @3xl/practice:min-h-0 @3xl/practice:flex-1", subject === "japanese" ? "aspect-square" : "aspect-[4/3]")}>
            {!readOnly && (
            <PracticePad
              ref={padRef}
              board={board}
              guide={subject === "japanese" && prefs.guide}
              pen={pen}
              fingerDraw={prefs.finger}
              label={subject === "japanese" ? "Writing page with a four square guide" : "Drawing page"}
              onState={setPad}
              onPen={() => setPenSeen(true)}
              className={cn("absolute inset-0", shown && "invisible")}
            />
            )}
            {displayed ? (
              <div className="absolute inset-0 grid place-items-center">
                {displayed.image_upload_id ? (
                  <img src={`/api/world/uploads/${displayed.image_upload_id}`} alt={`Attempt ${displayed.n}`} className="max-h-full max-w-full rounded-xl border-2 border-w-ink bg-w-field object-contain shadow-[0_3px_0_rgb(var(--w-ink))]" />
                ) : (
                  <p className="text-w-sm text-w-muted">This attempt has no picture.</p>
                )}
              </div>
            ) : (
              readOnly && <div className="absolute inset-0 grid place-items-center rounded-xl border-2 border-dashed border-w-line2 text-w-sm text-w-muted">{locked ? "Private practice" : "No attempts yet"}</div>
            )}
          </div>
          <div className="flex items-end gap-2">
            <div className="min-w-0 flex-1">{strip}</div>
            {!readOnly && !shown && pad.count > 0 && latest && (
              <button type="button" onClick={newAttempt} className={cn(BTN_SECONDARY_SM, "shrink-0")}>
                <Plus size={13} weight="bold" /> New attempt
              </button>
            )}
          </div>
        </div>
        {panel}
      </div>
    </div>
  );
}

export function NewPracticeDialog({ agents, onClose, onCreated }: { agents: WorldAgent[]; onClose: () => void; onCreated: (reportId: string) => void }) {
  const [choice, setChoice] = useState<string>("japanese");
  const [other, setOther] = useState("");
  const [title, setTitle] = useState("");
  const [coaches, setCoaches] = useState<CoachOption[] | null>(null);
  const [coach, setCoach] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const subject = choice === "other" ? cleanSubject(other) : choice;
  const close = useRef(onClose);
  close.current = onClose;
  const busyRef = useRef(busy);
  busyRef.current = busy;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!busyRef.current) close.current();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  useEffect(() => {
    if (!subject) return;
    let live = true;
    const t = window.setTimeout(() => {
      practiceApi
        .coaches(subject)
        .then((c) => {
          if (!live) return;
          const usable = c.agents.filter((a) => !a.paused_reason);
          setCoaches(usable);
          setCoach(c.default_coach_agent_id && usable.some((a) => a.id === c.default_coach_agent_id) ? c.default_coach_agent_id : (usable.find((a) => a.is_main)?.id ?? usable[0]?.id ?? ""));
        })
        .catch(() => live && setCoaches([]));
    }, choice === "other" ? 300 : 0);
    return () => {
      live = false;
      window.clearTimeout(t);
    };
  }, [subject, choice]);

  const picked = coach ? agents.find((a) => a.id === coach) : undefined;

  const create = async () => {
    if (!subject) {
      setError("Name a subject, like japanese or drawing.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const made = await practiceApi.create({ subject, title: title.trim() || undefined, coach: coach || undefined });
      onCreated(made.report.id);
    } catch (e) {
      setError(errorText(e, "Could not start this practice. Try again."));
      setBusy(false);
    }
  };

  return (
    <motion.div
      {...SCRIM_MOTION}
      className={cn(SCRIM, "z-50")}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <motion.form
        {...MODAL_MOTION}
        role="dialog"
        aria-modal="true"
        aria-labelledby="new-practice-title"
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
        className={cn(MODAL, "flex max-h-full max-w-md flex-col gap-4 p-4")}
      >
        <div className="flex items-center gap-2">
          <Student size={20} weight="duotone" className="text-w-link" />
          <h2 id="new-practice-title" className="min-w-0 flex-1 text-w-lg font-semibold text-balance text-w-strong">
            New practice
          </h2>
          <button type="button" onClick={onClose} aria-label="Close" className={BTN_GHOST}>
            <X size={16} weight="bold" />
          </button>
        </div>
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1.5 text-w-xs font-semibold text-w-text2">What are you practising?</legend>
          <div className={cn(SEGMENT, "w-full")}>
            {[...SUBJECTS, { value: "other", label: "Something else" }].map((s) => (
              <button
                key={s.value}
                type="button"
                aria-pressed={choice === s.value}
                onClick={() => setChoice(s.value)}
                className={cn(SEGMENT_ITEM, "flex-1 py-1.5", choice === s.value ? SEGMENT_ON : SEGMENT_OFF)}
              >
                {s.label}
              </button>
            ))}
          </div>
          {choice === "other" && <input autoFocus value={other} onChange={(e) => setOther(e.target.value)} placeholder="Calligraphy, figure drawing, chess notation" aria-label="Subject" maxLength={40} className={INPUT} />}
        </fieldset>
        <label className="flex flex-col gap-1.5">
          <span className="text-w-xs font-semibold text-w-text2">Coach</span>
          <WSelect
            value={coach}
            onChange={setCoach}
            options={(coaches ?? []).map((c) => ({ value: c.id, label: c.name, hint: c.expertise ? c.expertise.slice(0, 28) : undefined }))}
            placeholder={coaches && coaches.length === 0 ? "No agents to coach yet" : "Pick a coach"}
            ariaLabel="Coach"
            icon={picked ? <AgentFace agent={picked} size={20} /> : <Student size={16} weight="duotone" />}
          />
          <span className="text-w-2xs text-w-muted">The last coach you picked for a subject is the default next time.</span>
        </label>
        <label className="flex flex-col gap-1.5">
          <span className="text-w-xs font-semibold text-w-text2">
            Title <span className="font-normal text-w-muted">(optional)</span>
          </span>
          <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder={subject ? `${subjectLabel(subject)} practice` : "Practice"} maxLength={200} className={INPUT} />
        </label>
        {error && (
          <p role="alert" className="text-w-xs text-w-danger">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <button type="button" onClick={onClose} className={BTN_SECONDARY}>
            Cancel
          </button>
          <button type="submit" disabled={busy || !subject} className={cn(BTN_PRIMARY, "disabled:opacity-60")}>
            {busy ? <Shimmer>Starting</Shimmer> : "Start practising"}
          </button>
        </div>
      </motion.form>
    </motion.div>
  );
}
