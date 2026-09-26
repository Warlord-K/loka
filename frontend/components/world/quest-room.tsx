"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowSquareOut, CaretRight, Check, ClockCounterClockwise, Hourglass, Link as LinkIcon, Robot } from "@phosphor-icons/react";
import { AgentFace } from "./channel-ui";
import { cn } from "@/lib/utils";
import { causeLabel, formatUsage, worldApi, type AgentMessage, type QuestPart, type QuestRoomPage, type QuestRoomTask, type QuestTiming, type RunEvent, type SessionBrief, type SessionRun, type WorldAgent, type WorldEvent } from "@/lib/world";
import { BTN_SECONDARY_SM, CHUNKY, ICON_BTN, ITEM_CARD, PANEL, ICON_BTN_MD } from "./styles";
import { AttachButton, AttachmentGrid, CornerCopy, DayDivider, MessageStamp, NewMessagesPill, PendingStrip, PrChip, QuestCard, RichText, useAgentHandles, useAttachments, useFollowBottom } from "./chat-kit";
import { ReportCard, ROLE_LABEL, RunTurn, runIsLive, StatusLine, useEscape, useWorldRefresh, type Progress } from "./panels";
import { formatTiming, paceLabel, StatusIcon, statusLabel } from "./quest-ui";
import { Shimmer } from "./work-ui";
import { VoiceField } from "./voice-ui";
import { WSelect } from "./controls";
import { useDraft } from "./drafts";
import { OpenWorkshop } from "./workbench/open-workshop";
import { MilestoneList, milestoneProgress } from "./milestones-ui";
import { CloseButton, EmptyState } from "./settings-shell";
import { MessageToolbar } from "./message-row";
import { worldUrl } from "./message-links";
import { useCopy } from "@/lib/copy";
import { githubLine, githubLink, isGithubEvent } from "./github-ui";

type RoomRun = QuestRoomPage["room"]["runs"][number];

type Entry =
  | { kind: "message"; at: string; key: string; message: AgentMessage }
  | { kind: "event"; at: string; key: string; event: WorldEvent }
  | { kind: "run"; at: string; key: string; run: RoomRun };

const QUIET_EVENTS = /^(run\.(?!failed$|timed_out$|retrying$)|session\.|agent\.)/;

const RUN_LABEL: Record<string, string> = {
  task: "worked on the quest",
  followup: "took a follow-up",
  review: "reviewed the work",
  publish: "published the work",
  turn: "thought it over",
};

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function openQuest(part: QuestPart) {
  window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: part.id, agentId: part.assigned_agent_id } }));
}

function questLink(taskId: string): string {
  return `quest:${taskId}`;
}

function withoutQuestLink(body: string, taskId: string): string {
  return body.replace(new RegExp(`\\s*${questLink(taskId)}\\s*$`), "").trim();
}

function eventLine(e: WorldEvent, who: string): string {
  const p = e.payload ?? {};
  if (e.type === "task.finished") {
    const timing = formatTiming(p.timing as QuestTiming | undefined);
    const why = str(p.cause) ? `${causeLabel(str(p.cause))}. ${str(p.reason)}` : str(p.error);
    const line = why ? `The quest needs attention. ${why}` : str(p.pr_url) ? `${who} finished and opened a PR` : `${who} finished the quest`;
    return timing ? `${line}. Took ${timing}` : line;
  }
  if (isGithubEvent(e.type)) return githubLine(e);
  switch (e.type) {
    case "run.retrying":
      return `${who} hit a snag (${str(p.reason) || causeLabel(str(p.cause))}), so Loka is trying again by itself`;
    case "run.failed":
    case "run.timed_out":
      return `${who}'s run stopped. ${str(p.cause) ? `${causeLabel(str(p.cause))}. ${str(p.reason)}` : str(p.error)}${p.auto_retried ? " Loka had already tried again once." : ""}`;
    case "task.started":
      return p.resumed ? `${who} picked the quest back up` : `${who} started the quest`;
    case "task.merged":
      return "The PR was merged. Quest done.";
    case "task.pr_closed":
      return "The PR was closed without merging";
    case "task.cancelled":
      return `Quest cancelled${str(p.by) ? ` by ${str(p.by)}` : ""}${str(p.reason) ? `: ${str(p.reason)}` : ""}`;
    case "task.updated":
      return str(p.status) ? `Status changed to ${statusLabel(str(p.status))}` : "Quest details changed";
    case "task.assigned":
      return `${who} was given the quest`;
    case "task.review_feedback":
      return "New review comments on the PR went back to the executor";
    case "task.ci_failed": {
      const checks = Array.isArray(p.checks) ? (p.checks as string[]) : [];
      return `Checks failed${checks.length ? ` (${checks.slice(0, 3).join(", ")})` : ""}, sent back to the executor`;
    }
    case "task.plan_ready":
      return `${who} sent a plan, waiting for approval in chat`;
    case "task.plan_approved":
      return `${str(p.by) || "Someone"} approved the plan`;
    case "task.plan_redirected":
      return `${str(p.by) || "Someone"} redirected the plan${str(p.text) && str(p.text) !== "Redirect" ? `: ${str(p.text)}` : ""}`;
    case "task.attempts_settled": {
      const lost = Array.isArray(p.lost) ? p.lost.length : 0;
      const spent = typeof p.lost_cost_usd === "number" && p.lost_cost_usd > 0 ? `, $${p.lost_cost_usd.toFixed(2)} spent on the others` : "";
      return `Attempt ${String(p.winner_attempt ?? "?")} (${str(p.winner_cli)} ${str(p.winner_model)}) won over ${lost} other${lost === 1 ? "" : "s"}${spent}${str(p.why) ? `. ${str(p.why)}` : ""}`;
    }
    case "review.finished": {
      const r = (p.review ?? {}) as Record<string, unknown>;
      return `Review finished${str(r.verdict) ? `: ${str(r.verdict)}` : ""}`;
    }
    case "milestones.updated":
      return "Milestones updated";
    case "attention.requested":
      return `${who} needs your decision: ${str(p.text)}`;
    default:
      return `${who}: ${e.type.replace(/[._]/g, " ")}`;
  }
}

function eventCards(e: WorldEvent): { kind: string; data: Record<string, unknown> | null }[] {
  const p = e.payload ?? {};
  const cards: { kind: string; data: Record<string, unknown> | null }[] = [];
  if (e.type === "task.finished" && p.report && typeof p.report === "object") cards.push({ kind: "report", data: p.report as Record<string, unknown> });
  const review = p.review;
  if ((e.type === "task.finished" || e.type === "review.finished") && review && typeof review === "object") cards.push({ kind: "verdict", data: review as Record<string, unknown> });
  return cards;
}

function duration(run: SessionRun): string {
  const end = run.finished_at ? Date.parse(run.finished_at) : Date.now();
  const secs = Math.max(0, Math.round((end - Date.parse(run.created_at)) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.round((secs % 3600) / 60)}m`;
}

function followupText(prompt: string): string | null {
  return prompt.match(/^\[follow-up from ([^\]\n]+)\]\n([\s\S]*?)\n\nAnswer briefly\./)?.[2]?.trim() ?? null;
}

function RunCard({
  run,
  session,
  agent,
  handles,
  onOpenAgent,
  progress,
  tick,
}: {
  run: RoomRun;
  session: SessionBrief;
  agent: WorldAgent | undefined;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  progress: Progress | null;
  tick: number;
}) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState<{ prompt: string; output: string | null } | null>(null);
  const [events, setEvents] = useState<RunEvent[] | undefined>(undefined);
  const lastId = useRef<number | undefined>(undefined);
  const live = runIsLive(run);
  const name = agent?.name ?? "The agent";
  const textDone = useRef(false);

  useEffect(() => {
    if (run.kind !== "followup" && !open) return;
    if (textDone.current && !live) return;
    let gone = false;
    worldApi
      .runText(run.id)
      .then((t) => {
        if (gone) return;
        setText(t);
        textDone.current = !live;
      })
      .catch(() => {});
    return () => {
      gone = true;
    };
  }, [run.id, run.kind, open, live, run.status]);

  const settled = useRef(false);
  useEffect(() => {
    if (!open || (settled.current && !live)) return;
    let gone = false;
    const after = lastId.current;
    worldApi
      .runEvents(run.id, after)
      .then((fresh) => {
        if (gone) return;
        if (!live) settled.current = true;
        if (fresh.length) lastId.current = fresh[fresh.length - 1].id;
        setEvents((prev) => (after === undefined ? fresh : [...(prev ?? []), ...fresh.filter((e) => e.id > after)]));
      })
      .catch(() => {});
    return () => {
      gone = true;
    };
  }, [open, run.id, tick, live]);

  const asked = run.kind === "followup" && text ? followupText(text.prompt) : null;
  const usage = formatUsage(run.cost_usd, run.input_tokens + run.cached_input_tokens + run.output_tokens);
  const role = ROLE_LABEL[session.role] ?? session.role;
  const statusText = live ? (run.status === "queued" ? "waiting to start" : "working now") : run.status === "completed" ? "done" : run.status.replace("_", " ");
  const fullRun: SessionRun = { ...run, prompt: text?.prompt ?? "", output: text?.output ?? null };

  return (
    <div className="space-y-1.5">
      {asked && (
        <div className="ml-auto max-w-[min(92%,40rem)] rounded-lg border-2 border-w-ink bg-w-teal/15 px-3 py-2 text-w-base">
          <p className="mb-0.5 text-w-xs font-semibold text-w-link">Follow-up to {role.toLowerCase()}</p>
          <RichText text={asked} handles={handles} onOpenAgent={onOpenAgent} />
        </div>
      )}
      <div className={cn("rounded-xl border-2 px-3 py-2", live ? "border-w-teal/60 bg-w-aqua/40" : run.status === "completed" ? "border-w-line bg-w-card/70" : "border-w-danger/40 bg-w-rose/50")}>
        <button type="button" onClick={() => setOpen((o) => !o)} aria-expanded={open} className="flex w-full min-w-0 items-center gap-2 text-left text-w-sm">
          <span className={cn("shrink-0 transition-transform duration-200 motion-reduce:transition-none", open && "rotate-90")}>
            <CaretRight size={10} weight="bold" />
          </span>
          {agent ? <AgentFace agent={agent} size={20} /> : <Robot size={18} className="shrink-0 text-w-muted" />}
          <span className="min-w-0 flex-1 truncate">
            <span className="font-semibold">{name}</span> <span className="text-w-text2">({role.toLowerCase()}) {RUN_LABEL[run.kind] ?? run.kind}</span>
          </span>
          <span className={cn("shrink-0 text-w-2xs", live ? "text-w-link" : run.status === "completed" ? "text-w-muted" : "text-w-danger")}>
            {live ? <Shimmer>{statusText}</Shimmer> : statusText}
          </span>
          <span className="hidden shrink-0 font-mono text-w-2xs text-w-muted sm:inline">
            {duration(run)}
            {usage ? ` · ${usage}` : ""}
          </span>
        </button>
        {!open && live && progress && (
          <div className="mt-1">
            <StatusLine text={progress.text} />
          </div>
        )}
        {!open && !live && run.error && <p className="mt-1 line-clamp-2 text-w-xs text-w-danger">{run.error}</p>}
        {open && (
          <div className="mt-2 space-y-2 border-t-2 border-dashed border-w-line pt-2">
            {!text && !events ? (
              <p className="text-w-sm">
                <Shimmer>Loading the steps</Shimmer>
              </p>
            ) : (
              <RunTurn run={fullRun} session={session} agentName={name} events={events} handles={handles} onOpenAgent={onOpenAgent} progress={progress} />
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function Participant({ agent, label, detail, live, onClick, title = "Open this session's history" }: { agent: WorldAgent | undefined; label: string; detail: string; live: boolean; onClick?: () => void; title?: string }) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={!onClick}
      title={onClick ? title : undefined}
      className={cn(ITEM_CARD, "flex shrink-0 items-center gap-2 px-2.5 py-1.5 text-left text-w-xs outline-none focus-visible:ring-2 focus-visible:ring-w-teal disabled:hover:border-w-line", live && "border-w-teal/60 bg-w-aqua/60")}
    >
      {agent ? <AgentFace agent={agent} size={22} /> : <Robot size={18} className="shrink-0 text-w-muted" />}
      <span className="min-w-0">
        <span className="block max-w-[15rem] truncate font-semibold leading-tight text-w-strong">{agent?.name ?? "Agent"}</span>
        <span className="block max-w-[15rem] truncate leading-tight text-w-muted" title={`${label} · ${detail}`}>
          {label} · {live ? <Shimmer>{detail}</Shimmer> : detail}
        </span>
      </span>
    </button>
  );
}

function MessageRow({ message, taskId, meId, byId, handles, onOpenAgent, onOpenSession }: {
  message: AgentMessage;
  taskId: string;
  meId: string | null;
  byId: Map<string, WorldAgent>;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  onOpenSession: (sessionId: string) => void;
}) {
  const meta = message.meta ?? {};
  if (meta.kind === "quest") return <QuestCard meta={meta} body={message.body} handles={handles} onOpenAgent={onOpenAgent} onOpenSession={onOpenSession} />;
  const mine = message.author_kind === "user";
  const agent = byId.get(message.author_agent_id ?? message.agent_id);
  const who = mine ? (message.author_user_id === meId ? "You" : "Owner") : message.author_kind === "system" ? "Loka" : agent?.name ?? "Agent";
  const body = withoutQuestLink(message.body, taskId);
  return (
    <div data-message-id={message.id} className={cn("group relative max-w-[min(92%,40rem)] select-text rounded-lg border-2 border-w-ink px-3 py-2 text-w-base leading-relaxed", mine ? "ml-auto bg-w-teal/15" : "bg-w-card")}>
      <MessageToolbar actions={{ copy: body, link: message.id }} className="right-2" />
      <p className={cn("mb-0.5 text-w-xs font-semibold", mine ? "text-w-link" : "text-w-muted")}>
        {mine ? `${who} to ${byId.get(message.agent_id)?.name ?? "the agent"}` : who}
      </p>
      {body && <RichText text={body} handles={handles} onOpenAgent={onOpenAgent} />}
      {meta.attachments && meta.attachments.length > 0 && <AttachmentGrid items={meta.attachments} collapsed={meta.media?.collapsed} />}
      <div className="mt-0.5 flex justify-end">
        <MessageStamp createdAt={message.created_at} editedAt={message.edited_at} />
      </div>
    </div>
  );
}

export function QuestRoom({
  taskId,
  agents,
  meId,
  onClose,
  onOpenAgent,
  onOpenSession,
}: {
  taskId: string;
  agents: WorldAgent[];
  meId: string | null;
  onClose: () => void;
  onOpenAgent: (id: string) => void;
  onOpenSession: (sessionId: string, agentId: string) => void;
}) {
  useEscape(onClose);
  const [task, setTask] = useState<QuestRoomTask | null>(null);
  const [sessions, setSessions] = useState<SessionBrief[]>([]);
  const [runs, setRuns] = useState<RoomRun[]>([]);
  const [events, setEvents] = useState<WorldEvent[]>([]);
  const [messages, setMessages] = useState<AgentMessage[]>([]);
  const [failed, setFailed] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const cursor = useRef<number | undefined>(undefined);
  const since = useRef<string | undefined>(undefined);
  const loading = useRef(false);
  const again = useRef(false);
  const byId = useMemo(() => new Map(agents.map((a) => [a.id, a])), [agents]);
  const handles = useAgentHandles(agents);

  const refresh = useCallback(() => {
    if (loading.current) {
      again.current = true;
      return;
    }
    loading.current = true;
    worldApi
      .questRoom(taskId, cursor.current, since.current)
      .then(({ task: t, room }) => {
        setTask(t);
        setSessions(room.sessions);
        setRuns(room.runs);
        setFailed(null);
        const first = cursor.current === undefined;
        cursor.current = room.cursor;
        if (first) setEvents(room.events);
        else if (room.events.length) setEvents((prev) => [...prev, ...room.events.filter((e) => !prev.some((p) => p.seq === e.seq))]);
        if (room.messages.length) {
          const newest = room.messages.reduce((m, x) => {
            const at = x.edited_at && x.edited_at > x.created_at ? x.edited_at : x.created_at;
            return at > m ? at : m;
          }, since.current ?? "");
          since.current = newest || since.current;
          setMessages((prev) => {
            const fresh = new Map(room.messages.map((m) => [m.id, m]));
            return [...prev.map((m) => fresh.get(m.id) ?? m), ...room.messages.filter((m) => !prev.some((p) => p.id === m.id))];
          });
        }
        setTick((n) => n + 1);
      })
      .catch((error: unknown) => {
        const msg = error instanceof Error ? error.message : "";
        setFailed(msg.startsWith("403") ? "You cannot open this quest's room." : msg.startsWith("404") ? "This quest no longer exists." : "Could not load the room. It will retry on the next update.");
      })
      .finally(() => {
        loading.current = false;
        if (again.current) {
          again.current = false;
          window.setTimeout(refresh, 400);
        }
      });
  }, [taskId]);

  useEffect(() => {
    cursor.current = undefined;
    since.current = undefined;
    setEvents([]);
    setMessages([]);
    refresh();
  }, [refresh]);

  const pending = useRef<number | null>(null);
  const refreshSoon = useCallback(() => {
    if (pending.current !== null) return;
    pending.current = window.setTimeout(() => {
      pending.current = null;
      refresh();
    }, 700);
  }, [refresh]);
  useEffect(() => () => {
    if (pending.current !== null) window.clearTimeout(pending.current);
  }, []);

  const sessionIds = useMemo(() => new Set(sessions.map((s) => s.id)), [sessions]);
  const roomAgents = useMemo(() => {
    const ids = new Set<string>(sessions.map((s) => s.agent_id));
    if (task?.assigned_agent_id) ids.add(task.assigned_agent_id);
    return ids;
  }, [sessions, task?.assigned_agent_id]);
  const liveRun = useMemo(() => [...runs].reverse().find(runIsLive) ?? null, [runs]);
  const match = useCallback(
    (e: WorldEvent) =>
      e.task_id === taskId ||
      (!!e.session_id && sessionIds.has(e.session_id)) ||
      (!!e.agent_id && roomAgents.has(e.agent_id) && (e.type === "message.created" || e.type.startsWith("run.") || e.type.startsWith("session."))),
    [taskId, sessionIds, roomAgents],
  );
  useWorldRefresh(refreshSoon, match);

  const [progress, setProgress] = useState<Progress | null>(null);
  const progressTick = useRef(0);
  useEffect(() => {
    if (!liveRun) {
      setProgress(null);
      return;
    }
    const onEvent = (ev: Event) => {
      const e = (ev as CustomEvent<WorldEvent>).detail;
      if (e.agent_id !== liveRun.agent_id) return;
      const p = e.payload ?? {};
      if (e.type === "agent.progress" && typeof p.text === "string" && p.text.trim() && (!p.run_kind || p.run_kind === liveRun.kind)) {
        setProgress({ text: p.text.trim(), at: Date.now() });
        if (Date.now() - progressTick.current > 3000) {
          progressTick.current = Date.now();
          setTick((n) => n + 1);
        }
      }
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [liveRun]);

  const stopReason = useMemo(() => {
    if (task?.status !== "failed") return null;
    const last = [...events].reverse().find((e) => /^(task\.finished|run\.failed|run\.timed_out)$/.test(e.type) && str(e.payload?.cause));
    if (!last) return null;
    const p = last.payload ?? {};
    return { label: causeLabel(str(p.cause)), reason: str(p.reason), retried: Boolean(p.auto_retried) };
  }, [events, task?.status]);
  const entries = useMemo(() => {
    const list: Entry[] = [
      ...messages.map((m) => ({ kind: "message" as const, at: m.created_at, key: `m${m.id}`, message: m })),
      ...events.filter((e) => !QUIET_EVENTS.test(e.type)).map((e) => ({ kind: "event" as const, at: e.created_at, key: `e${e.seq}`, event: e })),
      ...runs.filter((r) => r.kind !== "turn").map((r) => ({ kind: "run" as const, at: r.created_at, key: `r${r.id}`, run: r })),
    ];
    return list.sort((a, b) => (a.at < b.at ? -1 : a.at > b.at ? 1 : 0));
  }, [messages, events, runs]);

  const endRef = useRef<HTMLDivElement>(null);
  const { unseen, away, jump, stick } = useFollowBottom(endRef, entries.length ? entries[entries.length - 1].key : undefined, taskId);

  const executor = useMemo(() => {
    const tasks = [...sessions].reverse().filter((s) => s.role === "task");
    return tasks.find((s) => s.status !== "archived") ?? tasks[0] ?? null;
  }, [sessions]);
  const reviewer = useMemo(() => [...sessions].reverse().find((s) => s.role === "review") ?? null, [sessions]);
  const main = task?.assigned_agent_id ? byId.get(task.assigned_agent_id) : undefined;

  const targets = useMemo(() => {
    const list: { value: string; label: string; hint?: string }[] = [];
    if (executor && executor.status !== "archived")
      list.push({ value: executor.id, label: executor.running ? "Steer the executor" : "Follow up with the executor", hint: executor.running ? "goes into the running work" : "reopens the quest's session" });
    if (reviewer && reviewer.status !== "archived") list.push({ value: reviewer.id, label: "Ask the reviewer", hint: byId.get(reviewer.agent_id)?.name });
    if (main) list.push({ value: "main", label: `Message ${main.name}`, hint: "main mind, with this quest linked" });
    return list;
  }, [executor, reviewer, main, byId]);
  const [target, setTarget] = useState("");
  const chosen = targets.find((t) => t.value === target) ? target : (targets.find((t) => t.value === executor?.id && executor?.running)?.value ?? targets.find((t) => t.value === "main")?.value ?? targets[0]?.value ?? "");
  const [text, setText] = useDraft(`quest-room:${taskId}`);
  const [interrupt, setInterrupt] = useState(false);
  const [sending, setSending] = useState(false);
  const files = useAttachments();
  const [note, setNote] = useState<string | null>(null);
  const steering = chosen !== "" && chosen === executor?.id && executor.running;

  const send = async (e: React.FormEvent) => {
    e.preventDefault();
    const body = text.trim();
    const toMain = chosen === "main" && !!main;
    const attachments = toMain ? files.ready : [];
    if ((!body && !attachments.length) || sending || !chosen || files.uploading) return;
    setSending(true);
    stick();
    try {
      if (toMain && main) {
        await worldApi.send(main.id, `${body}\n\n${questLink(taskId)}`.trim(), attachments, undefined, undefined, undefined, taskId);
        files.clear();
      }
      else if (body) await worldApi.followUp(chosen, body, steering && interrupt);
      setText("");
      setInterrupt(false);
      setNote(chosen === "main" ? null : steering ? "Passed into the running work." : "Sent. The session picks it up next.");
      refresh();
    } catch (error) {
      setNote(error instanceof Error && /^4\d\d /.test(error.message) ? error.message.slice(4) : "Could not send that.");
    } finally {
      setSending(false);
    }
  };

  const [stopping, setStopping] = useState<"step" | "run" | null>(null);
  const stop = async (mode: "step" | "run") => {
    if (!liveRun || liveRun.status !== "running" || stopping) return;
    if (mode === "run" && !window.confirm("Stop this run? The work so far stays, and it can be resumed later.")) return;
    setStopping(mode);
    try {
      await worldApi.stopRun(liveRun.id, mode);
      setNote(mode === "step" ? "Stopping the current step. It carries on with the rest." : "Stopping the run.");
      refresh();
    } catch (error) {
      setNote(error instanceof Error ? error.message : "Could not stop it.");
    } finally {
      setStopping(null);
    }
  };

  const nowText = useMemo(() => {
    if (!liveRun) return null;
    const who = byId.get(liveRun.agent_id)?.name ?? "The agent";
    if (liveRun.status === "queued") return `${who} is waiting for a free slot`;
    return progress?.text ?? `${who} is ${liveRun.kind === "review" ? "reviewing" : "working"}`;
  }, [liveRun, progress, byId]);

  const cost = sessions.reduce((n, s) => n + s.cost_usd, 0);
  const tokens = sessions.reduce((n, s) => n + s.tokens, 0);
  const usage = formatUsage(cost, tokens);
  const workshopSession = executor ?? reviewer;
  const [linkCopied, copyLink] = useCopy();

  return (
    <div role="dialog" aria-modal="true" aria-label="Quest room" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <div className="border-b-2 border-dashed border-w-line2 px-4 py-3 sm:px-5">
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <p className="flex items-center gap-1.5 text-w-xs font-semibold text-w-muted">
              {task && <StatusIcon status={task.status} size={12} />}
              Quest room{task ? ` · ${statusLabel(task.status)}` : ""}
              {task?.repo ? ` · ${task.repo}` : ""}
            </p>
            <h2 className="mt-1 text-w-xl font-semibold leading-tight text-w-strong">{task?.title ?? <Shimmer>Opening the room</Shimmer>}</h2>
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            {workshopSession && <OpenWorkshop detail={{ sessionId: executor?.id ?? null, taskId, agentId: workshopSession.agent_id }} label="Workshop" className="hidden h-10 rounded-lg px-2.5 text-w-xs sm:inline-flex" />}
            <button type="button" onClick={() => void copyLink(worldUrl("quest", taskId))} title={linkCopied ? "Link copied" : "Copy a link to this quest"} aria-label={linkCopied ? "Link copied" : "Copy a link to this quest"} className={ICON_BTN_MD}>
              {linkCopied ? <Check size={16} weight="bold" className="text-w-ok" /> : <LinkIcon size={16} weight="bold" />}
            </button>
            {executor && (
              <button type="button" onClick={() => onOpenSession(executor.id, executor.agent_id)} title="Full session history" aria-label="Full session history" className={ICON_BTN_MD}>
                <ClockCounterClockwise size={16} weight="bold" />
              </button>
            )}
            <CloseButton onClose={onClose} />
          </div>
        </div>
        {task && (
          <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-w-xs text-w-text2">
            {task.pr_url && <PrChip url={task.pr_url} />}
            {task.parent && (
              <button type="button" onClick={() => task.parent && openQuest(task.parent)} className="max-w-[18rem] truncate font-medium text-w-link hover:underline">
                Part of {task.parent.title}
              </button>
            )}
            {task.plan_first && <span>{sessions.some((s) => s.plan_state === "awaiting") ? "Plan waiting for your approval" : "Plan first"}</span>}
            {!!task.attempts && task.attempts > 1 && <span>Best of {task.attempts}</span>}
            {task.pace && task.pace !== "normal" && <span>{paceLabel(task.pace)}</span>}
            {usage && <span className="font-mono text-w-2xs text-w-muted">{usage} so far</span>}
            {task.description && (
              <details className="w-full">
                <summary className="cursor-pointer text-w-xs font-medium text-w-muted">Brief</summary>
                <div className="group/code relative mt-1">
                  <div className="max-h-40 overflow-y-auto rounded-md bg-w-field/60 p-2 pr-10 text-w-sm text-w-text">
                    <RichText text={task.description} handles={handles} onOpenAgent={onOpenAgent} />
                  </div>
                  <CornerCopy text={task.description} label="Copy the brief" className="absolute right-1.5 top-1.5" />
                </div>
              </details>
            )}
          </div>
        )}
        {task && (task.milestones.length > 0 || !!task.parts?.length) && (
          <div className="mt-2 grid gap-x-6 gap-y-2 sm:grid-cols-2">
            {task.milestones.length > 0 && (
              <details open className="min-w-0">
                <summary className="cursor-pointer text-w-xs font-semibold text-w-muted">Checklist {milestoneProgress(task.milestones)}</summary>
                <MilestoneList taskId={task.id} milestones={task.milestones} editable={false} onChanged={() => {}} className="mt-1.5 max-h-40 overflow-y-auto" />
              </details>
            )}
            {!!task.parts?.length && (
              <details open className="min-w-0">
                <summary className="cursor-pointer text-w-xs font-semibold text-w-muted">
                  Parts {task.parts.filter((part) => part.status === "done").length}/{task.parts.filter((part) => part.status !== "cancelled").length}
                </summary>
                <ul className="mt-1.5 max-h-40 space-y-1 overflow-y-auto">
                  {task.parts.map((part) => (
                    <li key={part.id} className="flex items-center gap-2 text-w-xs">
                      <StatusIcon status={part.status} size={12} />
                      <button type="button" onClick={() => openQuest(part)} className="min-w-0 truncate text-left font-medium text-w-link hover:underline">
                        {part.title}
                      </button>
                      <span className="shrink-0 text-w-muted">
                        {statusLabel(part.status)}
                        {part.assigned_agent_id && byId.get(part.assigned_agent_id) ? ` · ${byId.get(part.assigned_agent_id)?.name}` : ""}
                      </span>
                      {part.pr_url && <PrChip url={part.pr_url} />}
                    </li>
                  ))}
                </ul>
              </details>
            )}
          </div>
        )}
        {stopReason && (
          <div className="mt-2 rounded-lg border-2 border-w-ink bg-w-rose px-2.5 py-1.5 text-w-sm">
            <span className="text-w-xs font-semibold text-w-danger">Why it stopped</span>
            <p className="mt-0.5">
              <span className="font-semibold">{stopReason.label}.</span> {stopReason.reason}
              {stopReason.retried && " Loka had already tried again once, so it needs a look."}
            </p>
          </div>
        )}
        {nowText && (
          <div className="mt-2 flex flex-wrap items-center gap-2 rounded-lg border-2 border-w-teal bg-w-aqua/50 px-2.5 py-1.5">
            <span className="text-w-xs font-semibold text-w-link">Now</span>
            <span className="min-w-0 flex-1 truncate text-w-sm" role="status" aria-live="polite">
              <Shimmer>{nowText}</Shimmer>
            </span>
            {liveRun?.status === "running" && (liveRun.kind === "task" || liveRun.kind === "followup") && (
              <button type="button" onClick={() => void stop("step")} disabled={!!stopping} title="Stop the current step, keep the quest going" className={BTN_SECONDARY_SM}>
                {stopping === "step" ? "Stopping..." : "Stop step"}
              </button>
            )}
            {liveRun?.status === "running" && (
              <button type="button" onClick={() => void stop("run")} disabled={!!stopping} className={cn(CHUNKY, "bg-w-alert px-2 py-0.5 text-w-xs text-w-onalert hover:bg-w-coral")}>
                {stopping === "run" ? "Stopping..." : "Stop run"}
              </button>
            )}
          </div>
        )}
        <div className="-mx-1 mt-2 flex gap-1.5 overflow-x-auto px-1 pb-1">
          {main && <Participant agent={main} label="Main mind" detail="holds the quest" live={false} onClick={() => onOpenAgent(main.id)} title="Open this agent" />}
          {sessions
            .filter((s) => s.role !== "main")
            .map((s) => (
              <Participant
                key={s.id}
                agent={byId.get(s.agent_id)}
                label={s.attempt ? `${ROLE_LABEL[s.role] ?? s.role} ${s.attempt}` : (ROLE_LABEL[s.role] ?? s.role)}
                detail={s.running ? "working" : s.plan_state === "awaiting" ? "plan waiting" : s.attempt_state === "won" ? "won" : s.attempt_state === "lost" ? "lost" : s.status}
                live={s.running}
                onClick={() => onOpenSession(s.id, s.agent_id)}
              />
            ))}
        </div>
      </div>
      <div className="relative min-h-0 flex-1 overflow-y-auto px-4 py-3 sm:px-5">
        <div className="space-y-2">
          {failed && <p className="text-w-sm text-w-danger">{failed}</p>}
          {task && entries.length === 0 && <EmptyState icon={Hourglass} title="Nothing has happened on this quest yet" description="Messages, work and reviews show up here as they happen." />}
          {entries.map((entry, i) => {
            const previous = entries[i - 1]?.at;
            const divider = <DayDivider at={entry.at} previous={previous} />;
            if (entry.kind === "message")
              return (
                <div key={entry.key}>
                  {divider}
                  <MessageRow message={entry.message} taskId={taskId} meId={meId} byId={byId} handles={handles} onOpenAgent={onOpenAgent} onOpenSession={(id) => onOpenSession(id, entry.message.agent_id)} />
                </div>
              );
            if (entry.kind === "event") {
              const e = entry.event;
              const who = (e.agent_id && byId.get(e.agent_id)?.name) || "Someone";
              const cards = eventCards(e);
              return (
                <div key={entry.key}>
                  {divider}
                  <div className="flex items-center gap-2 text-w-xs text-w-text2">
                    <span className="h-px w-3 shrink-0 border-t-2 border-dashed border-w-line2" />
                    <span className="min-w-0 flex-1">
                      {eventLine(e, who)}
                      {githubLink(e) && (
                        <a href={githubLink(e) ?? undefined} target="_blank" rel="noopener noreferrer" className="ml-1.5 text-w-link underline-offset-2 hover:underline">
                          Open on GitHub
                        </a>
                      )}
                    </span>
                    <MessageStamp createdAt={e.created_at} />
                  </div>
                  {cards.map((card, j) => (
                    <div key={j} className="max-w-[min(92%,40rem)]">
                      <ReportCard card={card} />
                    </div>
                  ))}
                </div>
              );
            }
            const session = sessions.find((s) => s.id === entry.run.session_id);
            if (!session) return null;
            return (
              <div key={entry.key}>
                {divider}
                <RunCard
                  run={entry.run}
                  session={session}
                  agent={byId.get(entry.run.agent_id)}
                  handles={handles}
                  onOpenAgent={onOpenAgent}
                  progress={entry.run.id === liveRun?.id ? progress : null}
                  tick={runIsLive(entry.run) ? tick : -1}
                />
              </div>
            );
          })}
          <div ref={endRef} />
        </div>
        <NewMessagesPill show={unseen} away={away} onClick={jump} />
      </div>
      <div className="space-y-2 border-t-2 border-dashed border-w-line2 bg-w-card/70 px-4 py-3 sm:px-5">
        {targets.length > 0 ? (
          <>
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-w-xs text-w-muted">Send to</span>
              <WSelect ariaLabel="Who gets your message" size="sm" className="w-full flex-none sm:w-64" value={chosen} onChange={setTarget} options={targets} />
              {steering && (
                <label className="flex items-center gap-1.5 text-w-xs text-w-text2">
                  <input type="checkbox" checked={interrupt} onChange={(e) => setInterrupt(e.target.checked)} className="accent-w-alert" />
                  Stop the current step first
                </label>
              )}
            </div>
            {chosen === "main" && <PendingStrip pending={files.pending} onRemove={files.remove} />}
            <form onSubmit={send} className="flex items-end gap-2" {...(chosen === "main" ? files.dropProps : {})}>
              {chosen === "main" && <AttachButton onFiles={files.add} />}
              <VoiceField
                composer
                placeholder={chosen === "main" ? `Tell ${main?.name ?? "the agent"} about this quest` : steering ? "Steer the work: a correction, a hint, a question" : "Ask a question or ask for a change"}
                ariaLabel="Message for the quest"
                value={text}
                onChange={setText}
              />
              <button type="submit" disabled={(!text.trim() && !(chosen === "main" && files.ready.length)) || sending || files.uploading} className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-sm hover:bg-w-teal2 font-semibold text-w-strong")}>
                {sending ? "Sending..." : "Send"}
              </button>
            </form>
          </>
        ) : (
          task && <p className="text-w-xs text-w-muted">No agent holds this quest yet, so there is no one to talk to here.</p>
        )}
        {note && <p className="text-w-xs text-w-text2">{note}</p>}
        {task?.pr_url && (
          <a href={task.pr_url} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-w-xs text-w-link underline sm:hidden">
            Open the PR <ArrowSquareOut size={10} weight="bold" />
          </a>
        )}
      </div>
    </div>
  );
}
