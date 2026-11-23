"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import dynamic from "next/dynamic";
import { AnimatePresence } from "framer-motion";
import {
  ArrowRight,
  Books,
  Check,
  CheckCircle,
  Compass,
  Fire,
  Lightbulb,
  LockSimple,
  MapTrifold,
  PaperPlaneRight,
  Star,
  Trophy,
  X,
  XCircle,
  GraduationCap,
  Clock,
  Lamp,
  TShirt,
  BookOpen,
  Student,
} from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { AcademyTarget, AdventureCard, Attempt, ChallengeCard, ChallengeDetail, DialogueLine, NpcLine, Progress, Region, Unlock } from "@/lib/loka";
import type { WorldAgent, WorldEvent } from "@/lib/world";
import { AgentAvatar, usePortrait, type Portrait } from "./channel-ui";
import { useDraft } from "./drafts";
import { lokaApi, LokaFailure, GEAR_ACCESSORY, shareProgress, useCosmetics, useWorldEvent } from "./loka-client";
import { RichText } from "./chat-kit";
import { Shell } from "./places-ui";
import { DISTRICTS } from "./districts";
import { lookOfNpc, staffOf } from "./regions";
import { BTN_PRIMARY, BTN_SECONDARY, CHUNKY, INPUT, ITEM_CARD, PILL_NEUTRAL, SEGMENT, SEGMENT_ITEM, SEGMENT_OFF, SEGMENT_ON } from "./styles";
import { SettingsCard, SettingsGroup, SettingsRow, Switch } from "./settings-shell";
import { Shimmer } from "./work-ui";
import { quoteProps } from "./quote";
import { ATTEMPT_ITEM, keepInReports } from "@/lib/practice";
import { openReport, reportsApi, type ReportDetail } from "@/lib/reports";

const PracticeView = dynamic(() => import("./practice-view").then((m) => m.PracticeView), {
  ssr: false,
  loading: () => <Shimmer>Opening the canvas</Shimmer>,
});
const NewPracticeDialog = dynamic(() => import("./practice-view").then((m) => m.NewPracticeDialog), { ssr: false });
import { EmptyState } from "./settings-shell";
import { MeaningIcon } from "@/lib/meaning-icons";

export type LokaSheet =
  | { kind: "npc"; id: string; name: string; data: Record<string, unknown> }
  | ({ kind: "academy"; tab?: "courses" | "progress" } & AcademyTarget)
  | { kind: "travel" };

interface Refusal {
  text: string;
  fix: "budget" | "keys" | null;
}

function refusalOf(err: unknown): Refusal {
  if (err instanceof LokaFailure) {
    const message = err.message.charAt(0).toUpperCase() + err.message.slice(1);
    const lower = message.toLowerCase();
    if (err.code === "blocked") {
      if (lower.includes("budget")) return { text: message, fix: "budget" };
      if (lower.includes("key")) return { text: message, fix: "keys" };
      return { text: message, fix: lower.includes("pause") ? "budget" : null };
    }
    return { text: message, fix: null };
  }
  return { text: "Could not reach Loka. Check your connection and try again.", fix: null };
}

function RefusalNote({ refusal, onFix }: { refusal: Refusal; onFix: (fix: "budget" | "keys") => void }) {
  return (
    <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg border-2 border-w-danger/60 bg-w-danger/10 px-3 py-2 text-w-base">
      <span className="min-w-0 flex-1">{refusal.text}</span>
      {refusal.fix && (
        <button type="button" onClick={() => onFix(refusal.fix as "budget" | "keys")} className={BTN_SECONDARY}>
          {refusal.fix === "budget" ? "Open budgets" : "Open keys"}
        </button>
      )}
    </div>
  );
}

export function NpcTalkSheet({
  npc,
  meId,
  agents = [],
  onClose,
  onFix,
  onSay,
}: {
  npc: { id: string; name: string; data: Record<string, unknown> };
  meId: string | null;
  agents?: WorldAgent[];
  onClose: () => void;
  onFix: (fix: "budget" | "keys") => void;
  onSay: (text: string) => void;
}) {
  const staff = useMemo(() => staffOf(npc.data, agents) as WorldAgent | null, [npc.data, agents]);
  const staffId = typeof npc.data.agent_id === "string" ? npc.data.agent_id : null;
  const [lines, setLines] = useState<NpcLine[]>([]);
  const [waiting, setWaiting] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [text, setText] = useDraft(`npc:${npc.id}`);
  const dialogue = (Array.isArray(npc.data.dialogue) ? npc.data.dialogue : []) as DialogueLine[];
  const [line, setLine] = useState<DialogueLine | null>(dialogue[0] ?? null);
  const greeting = typeof npc.data.greeting === "string" ? npc.data.greeting : null;
  const npcLook = useMemo(() => lookOfNpc(npc, agents), [npc, agents]);
  const portrait = usePortrait(npcLook, 6);
  const scroller = useRef<HTMLDivElement>(null);
  const waitingRef = useRef(waiting);
  waitingRef.current = waiting;
  const sayRef = useRef(onSay);
  sayRef.current = onSay;
  const load = useCallback(() => {
    void lokaApi
      .npcLines(npc.id)
      .then((list) => {
        const ordered = [...list].sort((a, b) => a.created_at.localeCompare(b.created_at));
        setLines(ordered);
        const w = waitingRef.current;
        const done = w ? ordered.find((l) => l.session_id === w && l.status !== "active") : null;
        if (!done) return;
        if (staffId && done.reply) sayRef.current(done.reply);
        setWaiting((now) => (now === w ? null : now));
      })
      .catch(() => {});
  }, [npc.id, staffId]);
  useEffect(load, [load]);
  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight });
  }, [lines, waiting]);
  useEffect(() => {
    if (!waiting) return;
    const id = window.setInterval(load, 6000);
    return () => window.clearInterval(id);
  }, [waiting, load]);
  useWorldEvent(
    useCallback(
      (ev: WorldEvent) => {
        if (staffId) {
          if (ev.type === "message.created" && ev.agent_id === staffId && ev.payload.author_kind === "agent") load();
          return;
        }
        if (ev.type !== "npc.replied" || ev.payload.npc_id !== npc.id) return;
        const sid = String(ev.payload.session_id ?? "");
        if (ev.payload.ok === false) setRefusal({ text: String(ev.payload.error ?? `${npc.name} could not answer this time.`), fix: null });
        else if (typeof ev.payload.reply === "string") onSay(ev.payload.reply);
        const reply = typeof ev.payload.reply === "string" ? ev.payload.reply : null;
        setLines((prev) => prev.map((l) => (l.session_id === sid ? { ...l, reply, status: ev.payload.ok === false ? "failed" : "completed" } : l)));
        setWaiting((w) => (w === sid ? null : w));
        load();
      },
      [npc.id, npc.name, load, onSay, staffId],
    ),
  );
  const send = async (said: string) => {
    const body = said.trim();
    if (!body || waiting) return;
    setRefusal(null);
    try {
      const res = await lokaApi.talk(npc.id, body);
      if (said === text) setText("");
      setLines((prev) => [...prev, { session_id: res.session_id, said: body, reply: null, status: "active", created_at: new Date().toISOString() }]);
      setWaiting(res.session_id);
    } catch (err) {
      setRefusal(refusalOf(err));
    }
  };
  const pick = (label: string, next: string | null) => {
    const found = next ? dialogue.find((d) => d.id === next) : null;
    if (found) setLine(found);
    else {
      setLine(null);
      void send(label);
    }
  };
  void meId;
  return (
    <Shell eyebrow={staffId ? `Staffed by ${staff?.name ?? "one of your agents"}` : "A local"} title={npc.name} onClose={onClose}>
      <div className="flex min-h-0 flex-col gap-3">
        <div ref={scroller} className="max-h-[min(50vh,26rem)] space-y-3 overflow-y-auto pr-1">
          <Bubble portrait={portrait} name={npc.name}>{greeting ?? `${npc.name} looks up and smiles.`}</Bubble>
          {line && (
            <div className="space-y-2">
              <Bubble portrait={portrait} name={npc.name}>{line.text}</Bubble>
              <div className="flex flex-wrap gap-2 pl-12">
                {line.options.map((o) => (
                  <button key={o.label} type="button" onClick={() => pick(o.label, o.next)} className={BTN_SECONDARY}>
                    {o.label}
                  </button>
                ))}
              </div>
            </div>
          )}
          {lines.map((l) => (
            <div key={l.session_id} className="space-y-2">
              {l.said && <p className="ml-auto w-fit max-w-[85%] whitespace-pre-wrap rounded-lg border-2 border-w-ink bg-w-teal/20 px-3 py-2 text-w-base">{l.said}</p>}
              {l.reply ? (
                <Bubble portrait={portrait} name={npc.name}>{l.reply}</Bubble>
              ) : l.status === "active" || l.session_id === waiting ? (
                <p className="pl-12 text-w-base">
                  <Shimmer>{staffId ? `${staff?.name ?? npc.name} is on it` : `${npc.name} is thinking`}</Shimmer>
                </p>
              ) : staffId ? null : (
                <p className="pl-12 text-w-base text-w-muted">No answer this time.</p>
              )}
            </div>
          ))}
        </div>
        {refusal && <RefusalNote refusal={refusal} onFix={onFix} />}
        <form
          className="flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void send(text);
          }}
        >
          <input value={text} onChange={(e) => setText(e.target.value)} placeholder={`Say something to ${npc.name}`} className={INPUT} aria-label={`Message ${npc.name}`} maxLength={2000} />
          <button type="submit" disabled={!text.trim() || Boolean(waiting)} aria-label="Send" className={BTN_PRIMARY}>
            <PaperPlaneRight size={17} weight="bold" />
          </button>
        </form>
      </div>
    </Shell>
  );
}

function Bubble({ portrait, name, children }: { portrait: Portrait | null; name: string; children: string }) {
  return (
    <div className="flex items-start gap-2">
      <AgentAvatar portrait={portrait} size="2.5rem" rounded="rounded-md" />
      <div className="min-w-0 rounded-lg border-2 border-w-ink bg-w-card px-3 py-2">
        <p className="text-w-xs font-semibold text-w-muted">{name}</p>
        <Md text={children} className="text-w-md leading-relaxed" />
      </div>
    </div>
  );
}

const NO_HANDLES = new Map<string, never>();
const ignore = () => {};

function Md({ text, className }: { text: string; className?: string }) {
  return <RichText text={text} handles={NO_HANDLES} onOpenAgent={ignore} className={className} />;
}

const DIFFICULTY_TONE: Record<string, string> = { easy: "bg-w-ok/20", medium: "bg-w-warn/25", hard: "bg-w-danger/20" };
const TYPE_LABEL: Record<string, string> = { dsa: "DSA", design: "System design", quiz: "Quiz", puzzle: "Puzzle", canvas: "Canvas", reading: "Reading" };

function title(skill: string): string {
  return skill.split(/[-_ ]/).filter(Boolean).map((w) => w[0].toUpperCase() + w.slice(1)).join(" ") || "General";
}

function StateMark({ card }: { card: ChallengeCard }) {
  if (card.state === "passed") return <CheckCircle size={18} weight="fill" className="text-w-ok" aria-label="Passed" />;
  if (card.state === "failed") return <XCircle size={18} weight="bold" className="text-w-danger" aria-label="Not passed yet" />;
  if (card.state === "pending") return <Shimmer className="text-w-xs">Grading</Shimmer>;
  return null;
}

function CardRow({ card, onOpen }: { card: ChallengeCard; onOpen: () => void }) {
  return (
    <button type="button" onClick={onOpen} className={cn(ITEM_CARD, "flex w-full items-center gap-3 px-3.5 py-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-w-teal")}>
      <div className="min-w-0 flex-1">
        <p className="truncate text-w-sm font-semibold text-w-strong">{card.title}</p>
        <p className="mt-1.5 flex flex-wrap items-center gap-x-2.5 gap-y-1 text-w-xs text-w-muted">
          <span className={PILL_NEUTRAL}>{TYPE_LABEL[card.type] ?? card.type}</span>
          <span className={cn(PILL_NEUTRAL, DIFFICULTY_TONE[card.difficulty])}>{card.difficulty}</span>
          <span className="flex items-center gap-1"><Star size={11} weight="fill" className="text-w-warn" />{card.xp} xp</span>
          <span className="flex items-center gap-1"><Clock size={11} weight="bold" />{card.minutes} min</span>
          {card.attempts > 0 && card.best_score !== null && <span>best {card.best_score}</span>}
        </p>
      </div>
      <StateMark card={card} />
    </button>
  );
}

export function AcademySheet({
  meId,
  target,
  initialTab = "courses",
  waitingOn,
  agents,
  onOpenAgent,
  onClose,
  onFix,
  onNavigate,
  navigationCurrent,
}: {
  meId: string | null;
  target: AcademyTarget;
  initialTab?: "courses" | "progress";
  waitingOn: string | null;
  agents: WorldAgent[];
  onOpenAgent: (id: string) => void;
  onClose: () => void;
  onFix: (fix: "budget" | "keys") => void;
  onNavigate: () => number;
  navigationCurrent: (request: number | undefined) => boolean;
}) {
  const [tab, setTab] = useState<"courses" | "progress">(initialTab);
  const [open, setOpen] = useState<string | null>(target.challengeId ?? null);
  const [attempt, setAttempt] = useState<string | null>(target.attemptId ?? null);
  const [resolving, setResolving] = useState<{ reportId: string; request?: number } | null>(target.reportId ? { reportId: target.reportId, request: target.request } : null);
  const [creating, setCreating] = useState<{ request?: number } | null>(target.newPractice ? { request: target.request } : null);
  const [course, setCourse] = useState<string | null>(target.courseId ?? null);
  const [cards, setCards] = useState<ChallengeCard[] | null>(null);
  const [adventures, setAdventures] = useState<AdventureCard[]>([]);
  const [progress, setProgressState] = useState<Progress | null>(null);
  const setProgress = useCallback((p: Progress) => {
    setProgressState(p);
    shareProgress(p);
  }, []);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    Promise.all([lokaApi.challenges(), lokaApi.adventures(), lokaApi.progress()])
      .then(([c, a, p]) => {
        setCards(c);
        setAdventures(a);
        setProgress(p);
        setError(null);
      })
      .catch((err) => setError(refusalOf(err).text));
  }, [setProgress]);
  useEffect(load, [load]);
  useEffect(() => setTab(initialTab), [initialTab]);
  useEffect(() => {
    const linked = target.reportId ? cards?.find((card) => card.report_id === target.reportId) : null;
    setOpen(target.challengeId ?? linked?.id ?? null);
    setAttempt(target.attemptId ?? null);
    setResolving(target.reportId && !linked ? { reportId: target.reportId, request: target.request } : null);
    setCreating(target.newPractice ? { request: target.request } : null);
    setCourse(target.courseId ?? null);
    if (target.challengeId || target.reportId || target.courseId) setTab("courses");
  }, [target]);
  useEffect(() => {
    if (!resolving || cards === null) return;
    if (!navigationCurrent(resolving.request)) {
      setResolving(null);
      return;
    }
    const { reportId, request } = resolving;
    const hit = cards.find((c) => c.report_id === reportId);
    if (hit) {
      setOpen(hit.id);
      setResolving(null);
      return;
    }
    let live = true;
    lokaApi
      .practiceThis(reportId)
      .then((made) => {
        if (!live) return;
        if (!navigationCurrent(request)) {
          setResolving((current) => current === resolving ? null : current);
          return;
        }
        setOpen(made.challenge_id);
        setResolving(null);
        load();
      })
      .catch(() => {
        if (!live) return;
        if (!navigationCurrent(request)) {
          setResolving((current) => current === resolving ? null : current);
          return;
        }
        setResolving(null);
        keepInReports(reportId);
        openReport(reportId, attempt ? `${ATTEMPT_ITEM}${attempt}` : null);
      });
    return () => {
      live = false;
    };
  }, [resolving, cards, attempt, load, navigationCurrent]);
  useWorldEvent(
    useCallback(
      (ev: WorldEvent) => {
        if (ev.payload.owner_user_id && ev.payload.owner_user_id !== meId) return;
        if (["challenge.created", "challenge.graded", "adventure.created", "adventure.completed", "academy.unlocked", "practice.ready", "practice.updated", "report.updated", "item.updated"].includes(ev.type)) load();
      },
      [load, meId],
    ),
  );
  const openCard = (card: ChallengeCard) => {
    onNavigate();
    setResolving(null);
    setAttempt(null);
    setOpen(card.id);
  };
  if (resolving) {
    return (
      <Shell eyebrow="University" title="Academy" onClose={onClose}>
        <Shimmer>Opening the lesson</Shimmer>
      </Shell>
    );
  }
  if (open)
    return (
      <ChallengeView
        key={open}
        id={open}
        meId={meId}
        attempt={attempt}
        focusRequest={target.request}
        agents={agents}
        onOpenAgent={onOpenAgent}
        waitingOn={waitingOn}
        onBack={() => {
          onNavigate();
          setOpen(null);
          setAttempt(null);
          load();
        }}
        onClose={onClose}
        onFix={onFix}
        onProgress={setProgress}
      />
    );
  const ordered = course ? [...adventures].sort((a, b) => Number(b.id === course) - Number(a.id === course)) : adventures;
  const inAdventure = new Set(adventures.flatMap((a) => a.challenges));
  const loose = (cards ?? []).filter((c) => !inAdventure.has(c.id));
  const bySkill = new Map<string, ChallengeCard[]>();
  for (const c of loose) bySkill.set(c.skill, [...(bySkill.get(c.skill) ?? []), c]);
  const cardOf = new Map((cards ?? []).map((c) => [c.id, c]));
  return (
    <Shell eyebrow="University" title="Academy" onClose={onClose}>
      <div className="mb-6 flex items-center gap-3">
        <div role="tablist" aria-label="Academy" className={SEGMENT}>
          {(["courses", "progress"] as const).map((t) => (
            <button key={t} type="button" role="tab" aria-selected={tab === t} onClick={() => { onNavigate(); setResolving(null); setTab(t); }} className={cn(SEGMENT_ITEM, "px-3.5 py-1.5 text-w-sm", tab === t ? SEGMENT_ON : SEGMENT_OFF)}>
              {t === "courses" ? "Courses" : "Progress"}
            </button>
          ))}
        </div>
        {progress && (
          <span className={cn(PILL_NEUTRAL, "ml-auto py-1 tabular-nums")}>
            <Star size={11} weight="fill" className="text-w-warn" />
            Level {progress.level} · {progress.xp} xp
          </span>
        )}
        <button type="button" onClick={() => { const request = onNavigate(); setResolving(null); setCreating({ request }); }} className={cn(BTN_SECONDARY, !progress && "ml-auto")} title="Write or draw on a canvas and a coach agent checks each attempt">
          <Student size={14} weight="bold" />
          <span className="hidden @md:inline">New practice</span>
        </button>
      </div>
      {error && <p className="mb-3 text-w-base text-w-danger">{error}</p>}
      <AnimatePresence>
        {creating && (
          <NewPracticeDialog
            key={`new-practice:${creating.request ?? "initial"}`}
            agents={agents}
            onClose={() => setCreating(null)}
            onCreated={(reportId) => {
              setCreating(null);
              setResolving({ reportId, request: creating.request });
              load();
            }}
          />
        )}
      </AnimatePresence>
      {tab === "progress" ? (
        progress ? <ProgressView progress={progress} meId={meId} onChange={setProgress} /> : <Shimmer>Opening your record</Shimmer>
      ) : cards === null ? (
        <Shimmer>Gathering courses</Shimmer>
      ) : cards.length === 0 && adventures.length === 0 ? (
        <EmptyState
          icon={GraduationCap}
          title="No courses yet"
          description={`Ask your main agent for a course on anything you want to learn, a practice canvas to write or draw on, a coding problem, a system design prompt or a quiz.${progress?.academy_on === false ? " Challenges while you wait are off; turn them on under Progress." : " A mentor may also stop by while a quest runs."}`}
        />
      ) : (
        <div className="space-y-5">
          {ordered.map((a) => (
            <section key={a.id} aria-label={a.title} className={cn(a.id === course && "rounded-lg ring-2 ring-w-teal ring-offset-4 ring-offset-w-paper")}>
              <div className="mb-2 flex items-center gap-2">
                <Compass size={16} weight="bold" className="text-w-warn" />
                <h3 className="text-w-md font-semibold text-w-strong">{a.title}</h3>
                <span className="text-w-xs text-w-muted">
                  Course · {a.passed} of {a.total} passed
                  {a.completed ? " · complete" : ""}
                </span>
              </div>
              {a.story && <p className="mb-2 text-w-base text-w-muted">{a.story}</p>}
              <div className="space-y-2">
                {a.challenges.map((id) => {
                  const c = cardOf.get(id);
                  return c ? <CardRow key={id} card={c} onOpen={() => openCard(c)} /> : null;
                })}
                {a.total === 0 && <p className="text-w-sm text-w-muted">No lessons yet. Ask an agent to add one.</p>}
              </div>
            </section>
          ))}
          {[...bySkill.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([skill, list]) => (
            <section key={skill} aria-label={title(skill)}>
              <div className="mb-2 flex items-center gap-2">
                <Books size={16} weight="bold" className="text-w-ok" />
                <h3 className="text-w-md font-semibold text-w-strong">{title(skill)}</h3>
                {progress?.skills[skill] !== undefined && <span className="text-w-xs text-w-muted">{progress.skills[skill]} xp</span>}
              </div>
              <div className="space-y-2">
                {list.map((c) => (
                  <CardRow key={c.id} card={c} onOpen={() => openCard(c)} />
                ))}
              </div>
            </section>
          ))}
        </div>
      )}
    </Shell>
  );
}

function parseChoices(answer: string): number[] {
  return answer
    .split(/[,\s]+/)
    .map((v) => Number(v))
    .filter((n) => Number.isInteger(n) && n > 0);
}

function LessonCanvas({
  reportId,
  attempt,
  focusRequest,
  agents,
  meId,
  onOpenAgent,
  onLessonChanged,
}: {
  reportId: string;
  attempt: string | null;
  focusRequest?: number;
  agents: WorldAgent[];
  meId: string | null;
  onOpenAgent: (id: string) => void;
  onLessonChanged: () => void;
}) {
  const [detail, setDetail] = useState<ReportDetail | null>(null);
  const [gone, setGone] = useState(false);
  const load = useCallback(() => {
    reportsApi
      .get(reportId)
      .then((d) => {
        setDetail(d);
        setGone(false);
      })
      .catch(() => setGone(true));
  }, [reportId]);
  useEffect(load, [load]);
  useWorldEvent(
    useCallback(
      (ev: WorldEvent) => {
        if (ev.type === "report.updated" && String(ev.payload.report_id ?? ev.payload.id ?? "") === reportId) load();
      },
      [load, reportId],
    ),
  );
  if (!detail) return gone ? <p className="text-w-sm text-w-muted">This lesson's canvas is gone.</p> : <Shimmer>Opening the canvas</Shimmer>;
  return (
    <div className="h-[min(46rem,78vh)] min-h-[28rem] overflow-hidden rounded-xl border-2 border-w-ink bg-w-card">
      <PracticeView embedded detail={detail} agents={agents} meId={meId} focusItem={attempt ? `${ATTEMPT_ITEM}${attempt}` : null} focusRequest={focusRequest} onOpenAgent={onOpenAgent}
        onChanged={() => {
          load();
          onLessonChanged();
        }}
      />
    </div>
  );
}

function ChallengeView({
  id,
  meId,
  attempt,
  focusRequest,
  agents,
  onOpenAgent,
  waitingOn,
  onBack,
  onClose,
  onFix,
  onProgress,
}: {
  id: string;
  meId: string | null;
  attempt: string | null;
  focusRequest?: number;
  agents: WorldAgent[];
  onOpenAgent: (id: string) => void;
  waitingOn: string | null;
  onBack: () => void;
  onClose: () => void;
  onFix: (fix: "budget" | "keys") => void;
  onProgress: (p: Progress) => void;
}) {
  const [detail, setDetail] = useState<ChallengeDetail | null>(null);
  const reportRef = useRef<string | null>(null);
  reportRef.current = detail?.card.report_id ?? null;
  const [hints, setHints] = useState(0);
  const [answer, setAnswer] = useDraft(`academy:${id}`);
  const [pending, setPending] = useState<Attempt | null>(null);
  const [result, setResult] = useState<{ attempt: Attempt; levelUp: boolean; level: number | null; unlocks: Unlock[] } | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [sending, setSending] = useState(false);
  const load = useCallback(() => {
    void lokaApi
      .challenge(id)
      .then((d) => {
        setDetail(d);
        const waiting = d.attempts.find((a) => a.status === "pending");
        setPending((p) => waiting ?? (p && d.attempts.some((a) => a.id === p.id && a.status === "pending") ? p : null));
      })
      .catch((err) => setRefusal(refusalOf(err)));
  }, [id]);
  useEffect(load, [load]);
  const finish = useCallback(
    (attempt: Attempt, extra: { levelUp?: boolean; level?: number | null; unlocks?: Unlock[] } = {}) => {
      setPending(null);
      setResult({ attempt, levelUp: Boolean(extra.levelUp), level: extra.level ?? null, unlocks: extra.unlocks ?? [] });
      if (attempt.status === "passed") setAnswer("");
      void lokaApi.progress().then(onProgress).catch(() => {});
      load();
    },
    [load, onProgress, setAnswer],
  );
  useWorldEvent(
    useCallback(
      (ev: WorldEvent) => {
        if (ev.type === "report.updated" || ev.type === "item.updated") {
          const ref = String(ev.payload.report_id ?? ev.payload.item_id ?? ev.payload.id ?? "");
          if (ref && (ref === id || ref === reportRef.current)) load();
          return;
        }
        if (ev.type !== "challenge.graded" || ev.payload.challenge_id !== id) return;
        if (ev.payload.owner_user_id && ev.payload.owner_user_id !== meId) return;
        const attemptId = String(ev.payload.attempt_id ?? "");
        void lokaApi
          .attempt(attemptId)
          .then((a) => finish(a, { levelUp: Boolean(ev.payload.level_up), level: Number(ev.payload.level) || null, unlocks: (ev.payload.unlocks as Unlock[]) ?? [] }))
          .catch(() => {});
      },
      [id, meId, finish, load],
    ),
  );
  useEffect(() => {
    if (!pending) return;
    const tick = window.setInterval(() => {
      void lokaApi
        .attempt(pending.id)
        .then((a) => {
          if (a.status !== "pending") finish(a);
        })
        .catch(() => {});
    }, 5000);
    return () => window.clearInterval(tick);
  }, [pending, finish]);
  const submit = async () => {
    if (!answer.trim() || sending || pending) return;
    setSending(true);
    setRefusal(null);
    setResult(null);
    try {
      const a = await lokaApi.submit(id, answer);
      if (a.status === "pending") setPending(a);
      else finish(a);
    } catch (err) {
      setRefusal(refusalOf(err));
    } finally {
      setSending(false);
    }
  };
  if (!detail) {
    return (
      <Shell eyebrow="Academy" title="Challenge" onClose={onClose} onBack={onBack}>
        {refusal ? <RefusalNote refusal={refusal} onFix={onFix} /> : <Shimmer>Unrolling the scroll</Shimmer>}
      </Shell>
    );
  }
  const card = detail.card;
  const chosen = new Set(parseChoices(answer));
  const toggle = (n: number) => {
    const next = new Set(chosen);
    if (next.has(n)) next.delete(n);
    else next.add(n);
    setAnswer([...next].sort((a, b) => a - b).join(", "));
  };
  const lang = detail.tests?.language ?? "any language";
  return (
    <Shell eyebrow={title(card.skill) === (TYPE_LABEL[card.type] ?? card.type) ? title(card.skill) : `${title(card.skill)} · ${TYPE_LABEL[card.type] ?? card.type}`} title={card.title} onClose={onClose} onBack={onBack}>
      <div className="space-y-4">
        <p className="flex flex-wrap items-center gap-1.5 text-w-xs text-w-muted">
          <span className={cn("rounded-md border border-w-line2 px-1.5", DIFFICULTY_TONE[card.difficulty])}>{card.difficulty}</span>
          <span>{card.xp} xp</span>
          <span>about {card.minutes} min</span>
          {card.state === "passed" && <span className="text-w-ok">passed before, practice earns no new xp</span>}
        </p>
        {waitingOn && <p className="rounded-lg border-2 border-dashed border-w-line2 px-3 py-2 text-w-sm text-w-muted">Your agent keeps working on {waitingOn}. You will get a note here when it is done, and your answer stays saved.</p>}
        <div {...quoteProps(`challenge:${card.id}`, card.title)}>
          <Md text={detail.prompt || (card.type === "canvas" ? "Draw it on the canvas and press Ready. Your coach checks the picture and every stroke." : "Read it, then tell your coach in your own words what it says.")} className="rounded-lg border-2 border-w-ink bg-w-card p-3 text-w-md leading-relaxed" />
        </div>
        {detail.reading.length > 0 && (
          <div>
            <p className="mb-1.5 text-w-sm text-w-muted">Read first</p>
            <div className="flex flex-wrap gap-2">
              {detail.reading.map((r) => (
                <button key={r.id} type="button" onClick={() => openReport(r.id)} className={cn(BTN_SECONDARY, "max-w-full")}>
                  <BookOpen size={14} weight="bold" className="shrink-0" />
                  <span className="truncate">{r.title}</span>
                </button>
              ))}
            </div>
          </div>
        )}
        {detail.coach && <p className="text-w-sm text-w-muted">Coach: {detail.coach.name}</p>}
        {detail.hints.length > 0 && (
          <div className="space-y-2">
            {detail.hints.slice(0, hints).map((h, i) => (
              <div key={i} className="flex gap-2 rounded-lg bg-w-warn/15 px-3 py-2 text-w-base">
                <Lightbulb size={16} weight="fill" className="mt-0.5 shrink-0 text-w-warn" />
                <Md text={h} className="min-w-0 flex-1" />
              </div>
            ))}
            {hints < detail.hints.length && (
              <button type="button" onClick={() => setHints((n) => n + 1)} className={BTN_SECONDARY}>
                Show a hint ({hints + 1} of {detail.hints.length})
              </button>
            )}
          </div>
        )}
        {card.type === "canvas" ? (
          card.report_id ? (
            <LessonCanvas reportId={card.report_id} attempt={attempt} focusRequest={focusRequest} agents={agents} meId={meId} onOpenAgent={onOpenAgent} onLessonChanged={load} />
          ) : (
            <p className="text-w-sm text-w-muted">This lesson's canvas is gone.</p>
          )
        ) : card.type === "quiz" && detail.choices.length > 0 ? (
          <fieldset className="space-y-2">
            <legend className="mb-1 text-w-sm text-w-muted">Pick every answer that fits</legend>
            {detail.choices.map((c, i) => (
              <label key={i} className={cn(CHUNKY, "flex cursor-pointer items-center gap-3 px-3 py-2", chosen.has(i + 1) ? "bg-w-teal/25" : "bg-w-card hover:bg-w-sand")}>
                <input type="checkbox" checked={chosen.has(i + 1)} onChange={() => toggle(i + 1)} className="h-4 w-4 accent-w-teal" />
                <Md text={c} className="min-w-0 flex-1 text-w-base" />
              </label>
            ))}
          </fieldset>
        ) : (
          <label className="block">
            <span className="mb-1 block text-w-sm text-w-muted">{card.type === "dsa" ? `Your solution in ${lang}` : card.type === "design" ? "Your design" : card.type === "reading" ? "What did you take from it?" : "Your answer"}</span>
            <textarea
              value={answer}
              onChange={(e) => setAnswer(e.target.value)}
              spellCheck={card.type !== "dsa"}
              rows={card.type === "dsa" ? 12 : 8}
              placeholder={card.type === "dsa" ? "Write your code here" : card.type === "design" ? "Components, data flow, trade-offs" : card.type === "reading" ? "What it says, in your own words" : "Type your answer"}
              className={cn(INPUT, "w-full resize-y leading-relaxed", card.type === "dsa" && "font-mono text-w-base")}
              onKeyDown={(e) => {
                if (card.type === "dsa" && e.key === "Tab" && !e.shiftKey) {
                  e.preventDefault();
                  const el = e.currentTarget;
                  const at = el.selectionStart;
                  setAnswer(`${answer.slice(0, at)}  ${answer.slice(el.selectionEnd)}`);
                  window.requestAnimationFrame(() => el.setSelectionRange(at + 2, at + 2));
                }
              }}
            />
          </label>
        )}
        {refusal && <RefusalNote refusal={refusal} onFix={onFix} />}
        {card.type === "canvas" ? null : pending ? (
          <p className="rounded-lg border-2 border-w-ink bg-w-card px-3 py-2 text-w-base">
            <Shimmer>Your answer is being graded</Shimmer>
          </p>
        ) : (
          <button type="button" onClick={() => void submit()} disabled={!answer.trim() || sending} className={BTN_PRIMARY}>
            <Check size={16} weight="bold" />
            {sending ? "Sending..." : "Submit answer"}
          </button>
        )}
        {result && <GradeCard result={result} />}
        {detail.attempts.filter((a) => a.status !== "pending" && a.id !== result?.attempt.id).length > 0 && (
          <details className="text-w-base">
            <summary className="cursor-pointer text-w-muted">Earlier attempts</summary>
            <ul className="mt-2 space-y-2">
              {detail.attempts
                .filter((a) => a.status !== "pending" && a.id !== result?.attempt.id)
                .map((a) => (
                  <li key={a.id} className="rounded-lg border border-w-line2 px-3 py-2">
                    <p className="font-semibold">
                      {a.status === "passed" ? "Passed" : a.status === "error" ? "Could not grade" : "Not yet"}
                      {a.score !== null ? ` · ${a.score}` : ""}
                    </p>
                    {a.feedback && <Md text={a.feedback} className="mt-1 text-w-muted" />}
                  </li>
                ))}
            </ul>
          </details>
        )}
      </div>
    </Shell>
  );
}

function GradeCard({ result }: { result: { attempt: Attempt; levelUp: boolean; level: number | null; unlocks: Unlock[] } }) {
  const a = result.attempt;
  const passed = a.status === "passed";
  return (
    <div role="status" className={cn("space-y-2 rounded-xl border-2 p-4", passed ? "border-w-ok/40 bg-w-ok/10" : a.status === "error" ? "border-w-danger/40 bg-w-danger/10" : "border-w-warn/40 bg-w-warn/10")}>
      <p className="flex items-center gap-2 text-w-lg font-semibold text-w-strong">
        {passed ? <Trophy size={18} weight="fill" className="text-w-warn" /> : <MeaningIcon.tryAgain size={18} weight="bold" />}
        {passed ? "Passed" : a.status === "error" ? "Could not grade this one" : "Not quite yet"}
        {a.score !== null && <span className="text-w-muted">· {a.score} / 100</span>}
      </p>
      {a.feedback && <Md text={a.feedback} className="text-w-base leading-relaxed" />}
      <p className="text-w-base">{a.xp_awarded > 0 ? `+${a.xp_awarded} xp` : passed ? "No new xp for a challenge you already passed." : "Try again whenever you like."}</p>
      {result.levelUp && result.level && <p className="text-w-base font-semibold">You reached level {result.level}.</p>}
      {result.unlocks.length > 0 && <p className="text-w-base">Unlocked: {result.unlocks.map((u) => u.name).join(", ")}. Equip them under Progress.</p>}
    </div>
  );
}

function ProgressView({ progress, meId, onChange }: { progress: Progress; meId: string | null; onChange: (p: Progress) => void }) {
  const [cosmetics, saveCosmetics] = useCosmetics(meId);
  const ready = progress.unlocks.filter((u) => u.kind === "gear" || u.kind === "decoration");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const flip = () => {
    setSaving(true);
    setSaveError(null);
    lokaApi
      .setAcademy(!progress.academy_on)
      .then(onChange)
      .catch((err) => setSaveError(refusalOf(err).text))
      .finally(() => setSaving(false));
  };
  const span = Math.max(1, progress.next_level_xp - progress.level_xp);
  const into = Math.max(0, Math.min(span, progress.xp - progress.level_xp));
  const skills = Object.entries(progress.skills).sort((a, b) => b[1] - a[1]);
  const top = Math.max(1, ...skills.map(([, v]) => v));
  const toggle = (u: Unlock) => {
    const field = u.kind === "gear" ? "gear" : "decor";
    const list = cosmetics[field];
    saveCosmetics({ ...cosmetics, [field]: list.includes(u.id) ? list.filter((x) => x !== u.id) : [...list, u.id] });
  };
  return (
    <div className="space-y-8">
      <div className="flex items-center gap-4 rounded-xl border-2 border-w-line bg-w-card/70 p-4 sm:gap-5 sm:p-5">
        <div className="grid h-16 w-16 shrink-0 place-items-center rounded-2xl bg-w-lemon text-w-warn ring-1 ring-inset ring-w-gold/40">
          <span className="text-[1.75rem] font-bold leading-none tabular-nums">{progress.level}</span>
        </div>
        <div className="min-w-0 flex-1">
          <p className="flex flex-wrap items-baseline gap-x-2 text-w-sm text-w-muted">
            <span className="text-w-md font-semibold text-w-strong">Level {progress.level}</span>
            <span className="tabular-nums">{progress.xp} xp</span>
            <span className="ml-auto tabular-nums">{progress.next_level_xp - progress.xp} xp to level {progress.level + 1}</span>
          </p>
          <div className="mt-2 h-2.5 overflow-hidden rounded-[0.1875rem] border-[1.5px] border-w-ink bg-w-field p-px" role="progressbar" aria-valuemin={0} aria-valuemax={span} aria-valuenow={into} aria-label="Progress to the next level">
            <div className="h-full rounded-[1px] bg-w-teal bg-[repeating-linear-gradient(90deg,transparent_0_5px,rgb(var(--w-ink)/0.18)_5px_6px)] transition-[width] duration-500 motion-reduce:transition-none" style={{ width: `${(into / span) * 100}%` }} />
          </div>
          <p className="mt-2.5 flex flex-wrap gap-x-4 gap-y-1 text-w-sm text-w-text2">
            <span className="flex items-center gap-1.5">
              <Fire size={14} weight="fill" className={progress.streak_alive ? "text-w-warn" : "text-w-faint"} />
              {progress.streak} day streak{progress.best_streak > progress.streak ? ` (best ${progress.best_streak})` : ""}
            </span>
            <span className="flex items-center gap-1.5">
              <Trophy size={14} weight="fill" className="text-w-warn" />
              {progress.solved} solved of {progress.attempts} tries
            </span>
          </p>
        </div>
      </div>
      <SettingsCard rows>
        <SettingsRow
          title="Challenges while you wait"
          description={
            progress.academy_on
              ? "When a quest runs and you sit idle, your main agent writes a short challenge about it. Each one uses a little of your model budget."
              : "Off. Your agents will not write challenges on their own. You can still ask for one in chat."
          }
          control={<Switch checked={progress.academy_on} onChange={flip} label="Challenges while you wait" disabled={saving} />}
        >
          {saveError && <p className="text-w-sm text-w-danger">{saveError}</p>}
        </SettingsRow>
      </SettingsCard>
      <SettingsGroup title="Skills" description="Experience you earned in each area.">
        {skills.length === 0 ? (
          <EmptyState icon={Books} title="No skills yet" description="Pass a challenge to start a skill." />
        ) : (
          <SettingsCard>
            <ul className="space-y-3">
              {skills.map(([skill, xp]) => (
                <li key={skill} className="grid grid-cols-[minmax(6rem,9rem)_minmax(0,1fr)_3.5rem] items-center gap-3 text-w-sm">
                  <span className="truncate font-medium text-w-text">{title(skill)}</span>
                  <span className="h-2.5 overflow-hidden rounded-[0.1875rem] border-[1.5px] border-w-ink bg-w-field p-px">
                    <span className="block h-full rounded-[1px] bg-w-ok transition-[width] duration-500 motion-reduce:transition-none" style={{ width: `${(xp / top) * 100}%` }} />
                  </span>
                  <span className="text-right tabular-nums text-w-muted">{xp} xp</span>
                </li>
              ))}
            </ul>
          </SettingsCard>
        )}
      </SettingsGroup>
      <SettingsGroup title="Unlocks" description="Cosmetics you earned. Equip them to show them off.">
        {ready.length === 0 ? (
          <EmptyState icon={LockSimple} title="Nothing unlocked yet" description="Level 2 unlocks a lantern post, level 3 a scholar's cap. A 3 day streak lights a campfire." />
        ) : (
          <ul className="grid gap-2 sm:grid-cols-2">
            {ready.map((u) => {
              const equippable = u.kind === "gear" ? Boolean(GEAR_ACCESSORY[u.id]) : u.kind === "decoration";
              const on = (u.kind === "gear" ? cosmetics.gear : cosmetics.decor).includes(u.id);
              return (
                <li key={u.id} className={cn(ITEM_CARD, "flex items-center gap-3 px-3.5 py-3", on && "border-w-teal bg-w-aqua/50 hover:border-w-teal")}>
                  <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-w-lemon text-w-warn">
                    {u.kind === "gear" ? <TShirt size={18} weight="duotone" /> : <Lamp size={18} weight="duotone" />}
                  </span>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-w-sm font-semibold text-w-strong">{u.name}</p>
                    <p className="text-w-xs text-w-muted">{u.kind === "gear" ? "Wear it" : "By the Academy door"}</p>
                  </div>
                  {equippable ? (
                    <button type="button" aria-pressed={on} onClick={() => toggle(u)} className={on ? BTN_PRIMARY : BTN_SECONDARY}>
                      {on ? <Check size={14} weight="bold" /> : null}
                      {on ? "Equipped" : "Equip"}
                    </button>
                  ) : (
                    <LockSimple size={15} weight="bold" className="text-w-faint" aria-label="Not equippable yet" />
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </SettingsGroup>
    </div>
  );
}

export function TravelSheet({
  regions,
  current,
  visited,
  onTravel,
  onClose,
}: {
  regions: Region[];
  current: string;
  visited: string[];
  onTravel: (id: string) => void;
  onClose: () => void;
}) {
  const known = regions.filter((r) => r.status === "active");
  const rows = [
    { id: "hearth", name: "Home", theme: "home", description: "Home. Your agents live and work here.", builtIn: true },
    ...DISTRICTS.map((d) => ({ id: d.id, name: d.name, theme: d.theme, description: d.description, builtIn: true })),
    ...known.map((r) => ({ id: r.id, name: r.name, theme: r.theme, description: r.data.description ?? "", builtIn: false })),
  ];
  return (
    <Shell eyebrow="Loka" title="Travel" onClose={onClose}>
      <ul className="space-y-2">
        {rows.map((r) => {
          const here = r.id === current;
          const seen = r.builtIn || visited.includes(r.id);
          return (
            <li key={r.id}>
              <button
                type="button"
                disabled={here || !seen}
                onClick={() => onTravel(r.id)}
                className={cn(CHUNKY, "flex w-full items-center gap-3 p-3 text-left disabled:opacity-100", here ? "bg-w-teal/20" : seen ? "bg-w-card hover:bg-w-sand" : "bg-w-field text-w-muted")}
              >
                <MapTrifold size={20} weight="bold" className="shrink-0" />
                <div className="min-w-0 flex-1">
                  <p className="truncate font-semibold">{seen ? r.name : "Unknown road"}</p>
                  <p className="truncate text-w-xs text-w-muted">{here ? "You are here" : seen ? r.description || r.theme : `Find a portal to ${r.name} to learn the way`}</p>
                </div>
                {!here && seen && <ArrowRight size={16} weight="bold" />}
              </button>
            </li>
          );
        })}
      </ul>
    </Shell>
  );
}

export function OfferCard({
  title: name,
  minutes,
  from,
  onAccept,
  onDismiss,
}: {
  title: string;
  minutes: number;
  from: string;
  onAccept: () => void;
  onDismiss: () => void;
}) {
  return (
    <div role="dialog" aria-label="A challenge while you wait" className="pointer-events-auto w-[min(24rem,calc(100vw-1.5rem))] rounded-xl border-[3px] border-w-ink bg-w-paper p-3 shadow-[0_5px_0_rgb(var(--w-ink))]">
      <div className="flex items-start gap-2">
        <Lightbulb size={20} weight="fill" className="mt-0.5 shrink-0 text-w-warn" />
        <div className="min-w-0 flex-1">
          <p className="text-w-xs font-semibold text-w-ok">{from} has an idea</p>
          <p className="mt-1 text-w-md">
            While your quest runs: <span className="font-semibold">{name}</span>, about {minutes} min.
          </p>
        </div>
        <button type="button" onClick={onDismiss} aria-label="Dismiss" className="p-0.5 text-w-muted hover:text-w-text">
          <X size={15} weight="bold" />
        </button>
      </div>
      <div className="mt-3 flex justify-end gap-2">
        <button type="button" onClick={onDismiss} className={BTN_SECONDARY}>
          Not now
        </button>
        <button type="button" onClick={onAccept} className={BTN_PRIMARY}>
          Accept
        </button>
      </div>
    </div>
  );
}
