"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowSquareOut, CaretRight, Check, ClockCounterClockwise, Hourglass, Link as LinkIcon, Play, Robot, Scales } from "@phosphor-icons/react";
import { AgentFace, PersonAvatar } from "./channel-ui";
import { cn } from "@/lib/utils";
import { causeLabel, formatUsage, WAITING_FOR_DISK, worldApi, WorldApiError, type AgentMessage, type QuestPart, type QuestRoomPage, type QuestRoomTask, type QuestTiming, type RunEvent, type SessionBrief, type SessionRun, type WorldAgent, type WorldEvent } from "@/lib/world";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, CHUNKY, ICON_BTN, ITEM_CARD, PANEL, ICON_BTN_MD } from "./styles";
import { AskChoices, AttachButton, AttachmentGrid, CornerCopy, DayDivider, MessageStamp, NewMessagesPill, PendingStrip, PrChip, QuestCard, RichText, useAgentHandles, useAttachments, useFollowBottom } from "./chat-kit";
import { ReportCard, ROLE_LABEL, RunTurn, runIsLive, StatusLine, useEscape, useWorldRefresh, type Progress } from "./panels";
import { formatTiming, paceLabel, StatusIcon, statusLabel } from "./quest-ui";
import { Shimmer } from "./work-ui";
import { VoiceField } from "./voice-ui";
import { WSelect } from "./controls";
import { useDraft } from "./drafts";
import { OpenWorkshop } from "./workbench/open-workshop";
import { WatchBrowserButton } from "./browser-view";
import { useFeature } from "./setup-hint";
import { MilestoneList, milestoneProgress } from "./milestones-ui";
import { CloseButton, EmptyState } from "./settings-shell";
import { MessageToolbar } from "./message-row";
import { worldUrl } from "./message-links";
import { useCopy } from "@/lib/copy";
import { githubLine, githubLink, isGithubEvent } from "./github-ui";
import { guardedDetailLoader } from "@/lib/exact-world-navigation";
import { navigationDetails } from "@/lib/navigation-details";
import { acknowledgeRoomSettlements, applyRoomEvent, applyRoomSettlements, buildBoard, crewOf, crewStats, decisionTarget, kindOf, mergeRoomMessages, pairQuote, roomEventMatches, roomEventSettlement, roomMentionHandles, roomScope, ROOM_EVENTS, threadOf, type Asking, type DecisionStatus, type DecisionSummary, type RoomMember, type Thread } from "@/lib/quest-board";
import { roomApi, roomServes } from "@/lib/quest-room-api";
import { scrollWithin } from "@/lib/scroll";
import { AddMember, CrewCard, DecisionsList, MembersBar, NeedsYouStrip, QuestionThread } from "./quest-board-ui";
import { mentionHandle, useMentionPicker, type MentionCandidate } from "./mention-picker";
import {
  attachmentKeysAfterQuestSend,
  cardAfterSettlement,
  draftAfterQuestSend,
  interruptAfterQuestSend,
  questCardKey,
  retainedQuestCard,
  visibleQuestCard,
  type HeldQuestCard,
  type QuestCardIdentity,
  type QuestSendSnapshot,
} from "@/lib/quest-room-state";
import { questAwaitsPlan, questEventTaskId, questNeedsYou, sessionAwaitsPlan } from "@/lib/quest-status";

type RoomRun = QuestRoomPage["room"]["runs"][number];

type Entry =
  | { kind: "message"; at: string; key: string; message: AgentMessage }
  | { kind: "thread"; at: string; key: string; thread: Thread<AgentMessage> }
  | { kind: "event"; at: string; key: string; event: WorldEvent; deliveredSummary: boolean }
  | { kind: "run"; at: string; key: string; run: RoomRun };

export const QUIET_EVENTS = /^(room\.part_added$|run\.(?!failed$|timed_out$|retrying$)|session\.|agent\.)/;

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
  window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: part.id, agentId: part.assigned_agent_id, loaded: true } }));
}

function questLink(taskId: string): string {
  return `quest:${taskId}`;
}

function withoutQuestLink(body: string, taskId: string): string {
  return body.replace(new RegExp(`\\s*${questLink(taskId)}\\s*$`), "").trim();
}

function newestMessageAt(messages: AgentMessage[]): string | undefined {
  return messages.reduce((newest, message) => {
    const at = message.edited_at && message.edited_at > message.created_at ? message.edited_at : message.created_at;
    return at > newest ? at : newest;
  }, "") || undefined;
}

function reviewMiss(review: unknown): string {
  const cause = str(((review ?? {}) as Record<string, unknown>).cause);
  return cause ? causeLabel(cause).toLowerCase() : "";
}

export function eventLine(e: WorldEvent, who: string): string {
  const p = e.payload ?? {};
  if (e.type === "task.finished") {
    const timing = formatTiming(p.timing as QuestTiming | undefined);
    const why = str(p.cause) ? `${causeLabel(str(p.cause))}. ${str(p.reason)}` : str(p.error);
    const miss = reviewMiss(p.review);
    const done = str(p.pr_url) ? `${who} finished and opened a PR` : `${who} finished the quest`;
    const line = why ? `The quest needs attention. ${why}` : miss ? `${done}, but the review ${miss}` : done;
    return timing ? `${line}. Took ${timing}` : line;
  }
  if (isGithubEvent(e.type)) return githubLine(e);
  switch (e.type) {
    case "run.retrying":
      return `${who} hit a snag (${str(p.reason) || causeLabel(str(p.cause))}), so Loka is trying again by itself`;
    case "run.waiting":
      return p.waiting === false ? `There is room on the device again, so ${who}'s work can start` : `The device is low on disk space, so ${who}'s work waits and starts by itself once there is room`;
    case "run.failed":
    case "run.timed_out":
      return `${who}'s run stopped. ${str(p.cause) ? `${causeLabel(str(p.cause))}. ${str(p.reason)}` : str(p.error)}${p.auto_retried ? " Loka had already tried again once." : ""}`;
    case "task.started":
      return p.resumed ? `${who} picked the quest back up` : `${who} started the quest`;
    case "task.merged":
      return "The PR was merged. Quest done.";
    case "action.scheduled":
      return `${who} set up a pull request action: ${str(p.line)}`;
    case "action.ran":
    case "action.skipped":
    case "action.failed":
      return str(p.line);
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
      const miss = reviewMiss(r);
      if (miss) return `The review ${miss}`;
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

function plainDashes(text: string): string {
  return text.replaceAll(" \u2014 ", " - ").replaceAll("\u2014", " - ").replaceAll(" \u2013 ", " - ");
}

function deliveredCompletionEvents(messages: AgentMessage[], events: WorldEvent[], taskId: string): Set<number> {
  const cards = messages.filter((message) => {
    const meta = message.meta;
    return message.author_kind === "system" && meta?.kind === "quest" && meta.status !== "started" && !meta.hidden && meta.task_id === taskId && !!meta.session_id && !!message.body;
  });
  const completions = events.filter((event) => {
    const payload = event.payload ?? {};
    const report = payload.report;
    const eventTaskId = event.task_id || str(payload.task_id);
    const eventSessionId = event.session_id || str(payload.session_id);
    return event.type === "task.finished" && eventTaskId === taskId && !!eventSessionId && !!report && typeof report === "object" && !!str((report as Record<string, unknown>).summary);
  });
  const candidates = cards.flatMap((message) =>
    completions.flatMap((event) => {
      const payload = event.payload ?? {};
      const report = payload.report as Record<string, unknown>;
      const eventSessionId = event.session_id || str(payload.session_id);
      if (message.meta?.session_id !== eventSessionId || message.body !== plainDashes(str(report.summary))) return [];
      return [{ messageId: message.id, eventSeq: event.seq, distance: Math.abs(Date.parse(message.created_at) - Date.parse(event.created_at)) }];
    }),
  ).sort((a, b) => a.distance - b.distance || a.eventSeq - b.eventSeq || a.messageId.localeCompare(b.messageId));
  const matchedMessages = new Set<string>();
  const matchedEvents = new Set<number>();
  for (const candidate of candidates) {
    if (matchedMessages.has(candidate.messageId) || matchedEvents.has(candidate.eventSeq)) continue;
    matchedMessages.add(candidate.messageId);
    matchedEvents.add(candidate.eventSeq);
  }
  return matchedEvents;
}

function eventCards(e: WorldEvent, deliveredSummary = false): { kind: string; data: Record<string, unknown> | null }[] {
  const p = e.payload ?? {};
  const cards: { kind: string; data: Record<string, unknown> | null }[] = [];
  if (e.type === "task.finished" && p.report && typeof p.report === "object") {
    const report = p.report as Record<string, unknown>;
    cards.push({ kind: "report", data: deliveredSummary ? { ...report, summary: "" } : report });
  }
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

function RunCard({
  run,
  session,
  agent,
  handles,
  onOpenAgent,
  progress,
  tick,
  onWithdraw,
  withdrawing,
}: {
  run: RoomRun;
  session: SessionBrief;
  agent: WorldAgent | undefined;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  progress: Progress | null;
  tick: number;
  onWithdraw?: () => void;
  withdrawing: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState<{ prompt: string; output: string | null } | null>(null);
  const [events, setEvents] = useState<RunEvent[] | undefined>(undefined);
  const lastId = useRef<number | undefined>(undefined);
  const live = runIsLive(run);
  const name = agent?.name ?? "The agent";
  const textDone = useRef(false);

  useEffect(() => {
    if (!open) return;
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
  }, [run.id, open, live, run.status]);

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

  const asked = run.kind === "followup" && run.request?.from ? (run.request.text ?? null) : null;
  const usage = formatUsage(run.cost_usd, run.input_tokens + run.cached_input_tokens + run.output_tokens);
  const role = ROLE_LABEL[session.role] ?? session.role;
  const statusText = live ? (run.status === "queued" ? (run.waiting_for === "disk" ? WAITING_FOR_DISK.toLowerCase() : "waiting to start") : "working now") : run.status === "completed" ? "done" : run.status.replace("_", " ");
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
        {onWithdraw && (
          <div className="mt-1 flex justify-end">
            <button type="button" onClick={onWithdraw} disabled={withdrawing} title="Withdraw this run before it starts" className={BTN_SECONDARY_SM}>
              {withdrawing ? "Withdrawing..." : "Withdraw"}
            </button>
          </div>
        )}
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

function MessageRow({ message, taskId, meId, byId, members, handles, onOpenAgent, onOpenSession }: {
  message: AgentMessage;
  taskId: string;
  meId: string | null;
  byId: Map<string, WorldAgent>;
  members?: RoomMember[];
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  onOpenSession: (sessionId: string) => void;
}) {
  const meta = message.meta ?? {};
  if (meta.kind === "quest") return <QuestCard meta={meta} body={message.body} handles={handles} onOpenAgent={onOpenAgent} onOpenSession={onOpenSession} />;
  const human = message.author_kind === "user";
  const mine = human && !!meId && message.author_user_id === meId;
  const agent = byId.get(message.author_agent_id ?? message.agent_id);
  const person = human ? members?.find((member) => member.member_kind === "person" && member.member_id === message.author_user_id) : undefined;
  const who = human ? (mine ? "You" : person?.name ?? "Someone") : message.author_kind === "system" ? "Loka" : agent?.name ?? "Agent";
  const body = withoutQuestLink(message.body, taskId);
  const said = message.kind === "answer" && !human ? `${who} answered` : who;
  return (
    <div data-message-id={message.id} className={cn("group relative max-w-[min(92%,40rem)] select-text rounded-lg border-2 border-w-ink px-3 py-2 text-w-base leading-relaxed", mine ? "ml-auto bg-w-teal/15" : "bg-w-card")}>
      <MessageToolbar actions={{ copy: body, link: message.id }} className="right-2" />
      <p className={cn("mb-0.5 text-w-xs font-semibold", mine ? "text-w-link" : "text-w-muted")}>
        {human ? (meta.room_only ? `${who} in the room` : `${who} to ${byId.get(message.agent_id)?.name ?? "the agent"}`) : said}
      </p>
      {body && <RichText text={body} handles={handles} onOpenAgent={onOpenAgent} />}
      {meta.attachments && meta.attachments.length > 0 && <AttachmentGrid items={meta.attachments} collapsed={meta.media?.collapsed} />}
      <div className="mt-0.5 flex justify-end">
        <MessageStamp createdAt={message.created_at} editedAt={message.edited_at} />
      </div>
    </div>
  );
}

type ComposerTarget = { value: string; label: string; hint?: string };

function ClarificationComposer({ taskId, card, onSettled }: { taskId: string; card: AgentMessage; onSettled: (identity: QuestCardIdentity) => void }) {
  const [text, setText] = useDraft(`quest-room-answer:${taskId}:${card.id}`);
  const files = useAttachments();
  const [sending, setSending] = useState(false);
  const sendingRef = useRef(false);
  const [note, setNote] = useState<string | null>(null);

  const send = async (event: React.FormEvent) => {
    event.preventDefault();
    const answer = text.trim();
    if ((!answer && !files.ready.length) || files.uploading || sendingRef.current) return;
    const draft = text;
    const attachmentKeys = files.pending.filter((pending) => pending.attachment).map((pending) => pending.key);
    const attachments = [...files.ready];
    const identity = { taskId, cardId: card.id };
    sendingRef.current = true;
    setSending(true);
    setNote(null);
    try {
      await worldApi.send(card.agent_id, answer, attachments, undefined, card.id, undefined, taskId);
      setText((current) => current === draft ? "" : current);
      attachmentKeys.forEach(files.remove);
      onSettled(identity);
    } catch (error) {
      if (error instanceof WorldApiError && error.status === 409) {
        onSettled(identity);
      } else {
        setNote(error instanceof WorldApiError ? error.message : "Could not save that answer. Try again.");
      }
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  return (
    <div className="space-y-2">
      <p className="text-w-xs font-semibold text-w-muted">Answer the question above</p>
      <fieldset disabled={sending} className="disabled:opacity-60">
        <AskChoices choices={card.meta?.choices ?? []} answered={false} onAnswer={setText} />
      </fieldset>
      <PendingStrip pending={files.pending} onRemove={files.remove} />
      <form onSubmit={send} className="flex items-end gap-2" {...files.dropProps}>
        <AttachButton onFiles={files.add} />
        <VoiceField composer placeholder="Type your answer" ariaLabel="Answer for the quest" value={text} onChange={setText} />
        <button type="submit" disabled={(!text.trim() && !files.ready.length) || sending || files.uploading} className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-sm font-semibold text-w-strong hover:bg-w-teal2")}>
          {sending ? "Sending..." : "Answer"}
        </button>
      </form>
      {note && <p className="text-w-xs text-w-text2">{note}</p>}
    </div>
  );
}

function QuestComposer({ taskId, canonicalTaskId, targets, executor, main, roomReady, crew, byId, hidden, ready, refresh, stick }: { taskId: string; canonicalTaskId: string; targets: ComposerTarget[]; executor: SessionBrief | null; main: WorldAgent | undefined; roomReady: boolean; crew: ReturnType<typeof crewOf>; byId: Map<string, WorldAgent>; hidden: boolean; ready: boolean; refresh: () => void; stick: () => void }) {
  const [target, setTarget] = useState("");
  const chosen = targets.find((item) => item.value === target) ? target : roomReady ? "room" : (targets.find((item) => item.value === executor?.id && executor?.running)?.value ?? targets.find((item) => item.value === "main")?.value ?? targets[0]?.value ?? "");
  const [text, setText] = useDraft(`quest-room:${taskId}`);
  const [interrupt, setInterrupt] = useState(false);
  const [sending, setSending] = useState(false);
  const sendingRef = useRef(false);
  const files = useAttachments();
  const [note, setNote] = useState<string | null>(null);
  const steering = chosen !== "" && chosen === executor?.id && executor.running;
  const quickAgents = useMemo(() => crew.filter((entry) => entry.kind === "agent").map((entry) => byId.get(entry.id)).filter((agent): agent is WorldAgent => !!agent), [crew, byId]);
  const candidates = useMemo<MentionCandidate[]>(() => {
    const bases = crew.map((entry) => {
      const agent = entry.agentId ? byId.get(entry.agentId) : undefined;
      return (entry.kind === "agent" && agent?.slug) || `${mentionHandle(entry.name) || entry.kind}${entry.kind === "session" ? "helper" : ""}`;
    });
    const unique = roomMentionHandles(bases);
    return crew.map((entry, index) => {
      const agent = entry.agentId ? byId.get(entry.agentId) : undefined;
      const helper = entry.kind === "session" ? entry.detail.split(" · ")[0] : null;
      return {
        key: entry.key,
        handle: unique[index],
        label: helper ? `${entry.name} (${helper})` : entry.name,
        hint: entry.role === "lead" ? "lead" : entry.kind === "session" ? "helper" : entry.kind,
        icon: agent ? <AgentFace agent={agent} size={20} /> : entry.kind === "person" ? <PersonAvatar id={entry.id} name={entry.name} size={20} soft /> : <Robot size={16} className="text-w-muted" />,
        member: true,
      };
    });
  }, [crew, byId]);
  const inputRef = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null);
  const picker = useMentionPicker({ value: text, onChange: setText, inputRef, candidates });

  const send = async (event: React.FormEvent) => {
    event.preventDefault();
    const body = text.trim();
    const destination = chosen;
    const toRoom = destination === "room";
    const toMain = chosen === "main" && !!main;
    const quick = destination.startsWith("ask:") ? quickAgents.find((agent) => agent.id === destination.slice(4)) : undefined;
    const attachments = toMain ? [...files.ready] : [];
    if ((!body && !attachments.length) || sendingRef.current || !chosen || files.uploading) return;
    const snapshot: QuestSendSnapshot = {
      taskId,
      draft: text,
      attachmentKeys: toMain ? files.pending.filter((pending) => pending.attachment).map((pending) => pending.key) : [],
      interrupt,
    };
    sendingRef.current = true;
    setSending(true);
    stick();
    try {
      if (toRoom && roomReady && body) await roomApi.post(canonicalTaskId, body);
      else if (quick && body) await roomApi.post(canonicalTaskId, body, [quick.id]);
      else if (toMain && main) await worldApi.send(main.id, `${body}\n\n${questLink(taskId)}`.trim(), attachments, undefined, undefined, undefined, taskId);
      else if (body) await worldApi.followUp(destination, body, steering && interrupt);
      setText((current) => draftAfterQuestSend(snapshot, taskId, current));
      attachmentKeysAfterQuestSend(snapshot, taskId).forEach(files.remove);
      setInterrupt((current) => interruptAfterQuestSend(snapshot, taskId, current));
      setNote(quick ? `Asked ${quick.name}. The answer shows here.` : toRoom ? "Posted to the room." : destination === "main" ? null : steering ? "Passed into the running work." : "Sent. The session picks it up next.");
      refresh();
    } catch (error) {
      setNote(error instanceof WorldApiError ? error.message : error instanceof Error && /^4\d\d /.test(error.message) ? error.message.slice(4) : "Could not send that.");
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  return (
    <div hidden={hidden} className="space-y-2">
      {targets.length > 0 ? (
        <>
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-w-xs text-w-muted">Send to</span>
            <WSelect ariaLabel="Who gets your message" size="sm" className="w-full flex-none sm:w-64" value={chosen} onChange={setTarget} options={targets} />
            {steering && (
              <label className="flex items-center gap-1.5 text-w-xs text-w-text2">
                <input type="checkbox" checked={interrupt} onChange={(event) => setInterrupt(event.target.checked)} className="accent-w-alert" />
                Stop the current step first
              </label>
            )}
          </div>
          {chosen === "main" && <PendingStrip pending={files.pending} onRemove={files.remove} />}
          <form onSubmit={send} className="relative flex items-end gap-2" {...(chosen === "main" ? files.dropProps : {})}>
            {picker.popover}
            {chosen === "main" && <AttachButton onFiles={files.add} />}
            <VoiceField
              composer
              placeholder={chosen === "room" ? `Write to the room${candidates.length ? ", or @ someone" : ""}` : chosen === "main" ? `Tell ${main?.name ?? "the agent"} about this quest` : chosen.startsWith("ask:") ? "Ask a quick question" : steering ? "Steer the work: a correction, a hint, a question" : "Ask a question or ask for a change"}
              ariaLabel="Message for the quest"
              value={text}
              onChange={setText}
              inputRef={inputRef}
              onKey={picker.onKey}
            />
            <button type="submit" disabled={(!text.trim() && !(chosen === "main" && files.ready.length)) || sending || files.uploading} className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-sm hover:bg-w-teal2 font-semibold text-w-strong")}>
              {sending ? "Sending..." : "Send"}
            </button>
          </form>
        </>
      ) : ready ? (
        <p className="text-w-xs text-w-muted">No agent holds this quest yet, so there is no one to talk to here.</p>
      ) : null}
      {note && <p className="text-w-xs text-w-text2">{note}</p>}
    </div>
  );
}

export function QuestRoom({
  taskId,
  initial,
  initialRequest,
  agents,
  meId,
  onClose,
  onOpenAgent,
  onOpenSession,
  sideToggle,
  focus,
}: {
  taskId: string;
  initial?: QuestRoomPage;
  initialRequest?: number;
  agents: WorldAgent[];
  meId: string | null;
  onClose: () => void;
  onOpenAgent: (id: string) => void;
  onOpenSession: (sessionId: string, agentId: string) => void;
  sideToggle?: React.ReactNode;
  focus?: string | null;
}) {
  useEscape(onClose);
  const currentInitial = initial?.task.id === taskId ? initial : undefined;
  const [task, setTask] = useState<QuestRoomTask | null>(currentInitial?.task ?? null);
  const [sessions, setSessions] = useState<SessionBrief[]>(currentInitial?.room.sessions ?? []);
  const [runs, setRuns] = useState<RoomRun[]>(currentInitial?.room.runs ?? []);
  const [events, setEvents] = useState<WorldEvent[]>(currentInitial?.room.events ?? []);
  const [messages, setMessages] = useState<AgentMessage[]>(currentInitial?.room.messages ?? []);
  const [needsYouMessages, setNeedsYouMessages] = useState<AgentMessage[] | undefined>(currentInitial?.room.needs_you_messages);
  const [decisionSummary, setDecisionSummary] = useState<DecisionSummary | undefined>(currentInitial?.room.decisions);
  const [members, setMembers] = useState<RoomMember[] | undefined>(currentInitial?.room.members);
  const [canonicalTask, setCanonicalTask] = useState<string | undefined>(currentInitial?.room.task_id);
  const [asking, setAsking] = useState<Asking | null>(currentInitial?.room.asking?.mode ?? null);
  const [clarification, setClarification] = useState<AgentMessage | null>(currentInitial?.clarification ?? null);
  const [answerCard, setAnswerCard] = useState<HeldQuestCard<AgentMessage> | null>(
    currentInitial?.clarification ? { taskId, card: currentInitial.clarification } : null,
  );
  const settledCards = useRef(new Set<string>());
  const [failed, setFailed] = useState<string | null>(null);
  const [gone, setGone] = useState(false);
  const [tick, setTick] = useState(0);
  const cursor = useRef(currentInitial?.room.cursor);
  const since = useRef(newestMessageAt(currentInitial?.room.messages ?? []));
  const loadRequest = useRef(0);
  const detailLoader = useRef(guardedDetailLoader()).current;
  const loading = useRef<number | null>(null);
  const again = useRef(false);
  const pending = useRef<number | null>(null);
  const goneRef = useRef(false);
  const activeTask = useRef(taskId);
  activeTask.current = taskId;
  const activeInitialRequest = useRef(initialRequest);
  activeInitialRequest.current = initialRequest;
  const byId = useMemo(() => new Map(agents.map((a) => [a.id, a])), [agents]);
  const handles = useAgentHandles(agents);
  const localSettlements = useRef(new Map<string, DecisionStatus>());
  const settlementTask = useRef(taskId);

  const applyPage = useCallback(({ task: nextTask, clarification: nextClarification, room }: QuestRoomPage) => {
    if (nextTask.id !== activeTask.current) return;
    goneRef.current = false;
    setGone(false);
    acknowledgeRoomSettlements(localSettlements.current, room.messages, room.needs_you_messages);
    setTask(nextTask);
    setClarification((current) => visibleQuestCard(activeTask.current, nextTask.id, nextClarification, current, settledCards.current));
    setAnswerCard((current) => retainedQuestCard(activeTask.current, nextTask.id, nextClarification, current, settledCards.current));
    setSessions(room.sessions);
    setRuns(room.runs);
    setEvents(room.events);
    setMessages(applyRoomSettlements(room.messages, localSettlements.current));
    setNeedsYouMessages(room.needs_you_messages ? applyRoomSettlements(room.needs_you_messages, localSettlements.current) : undefined);
    setDecisionSummary(room.decisions);
    setMembers(room.members);
    setCanonicalTask(room.task_id);
    setAsking(room.asking?.mode ?? null);
    setFailed(null);
    cursor.current = room.cursor;
    since.current = newestMessageAt(room.messages);
    setTick((n) => n + 1);
  }, []);

  const clearUnavailableRoom = useCallback((message: string, terminal: boolean) => {
    goneRef.current = terminal;
    setGone(true);
    setTask(null);
    setSessions([]);
    setRuns([]);
    setEvents([]);
    setMessages([]);
    setNeedsYouMessages(undefined);
    setDecisionSummary(undefined);
    setMembers(undefined);
    setCanonicalTask(undefined);
    setAsking(null);
    setClarification(null);
    setAnswerCard(null);
    setFailed(message);
    cursor.current = undefined;
    since.current = undefined;
    if (terminal) {
      again.current = false;
      if (pending.current !== null) window.clearTimeout(pending.current);
      pending.current = null;
    }
    setTick((n) => n + 1);
  }, []);

  const refresh = useCallback(() => {
    if (activeTask.current !== taskId || goneRef.current) return;
    if (loading.current !== null) {
      again.current = true;
      return;
    }
    const request = ++loadRequest.current;
    const navigationRequest = activeInitialRequest.current;
    const requestedCursor = cursor.current;
    const requestedSince = since.current;
    loading.current = request;
    const intent = detailLoader.begin(() => loading.current === request && activeInitialRequest.current === navigationRequest && activeTask.current === taskId);
    navigationDetails
      .quest(intent, taskId, requestedCursor, requestedSince)
      .then((page) => {
        if (!page || !intent.current()) return;
        const { task: t, clarification: nextClarification, room } = page;
        if (t.id !== activeTask.current) return;
        if (requestedCursor !== undefined && room.cursor < requestedCursor) return;
        goneRef.current = false;
        setGone(false);
        acknowledgeRoomSettlements(localSettlements.current, room.messages, room.needs_you_messages);
        setTask(t);
        setClarification((current) => visibleQuestCard(activeTask.current, t.id, nextClarification, current, settledCards.current));
        setAnswerCard((current) => retainedQuestCard(activeTask.current, t.id, nextClarification, current, settledCards.current));
        setSessions(room.sessions);
        setRuns(room.runs);
        setMembers(room.members);
        const freshMessages = applyRoomSettlements(room.messages, localSettlements.current);
        setNeedsYouMessages(room.needs_you_messages ? applyRoomSettlements(room.needs_you_messages, localSettlements.current) : undefined);
        setDecisionSummary(room.decisions);
        setCanonicalTask(room.task_id);
        setAsking(room.asking?.mode ?? null);
        setFailed(null);
        const first = requestedCursor === undefined;
        cursor.current = room.cursor;
        if (first) setEvents(room.events);
        else if (room.events.length) setEvents((prev) => [...prev, ...room.events.filter((e) => !prev.some((p) => p.seq === e.seq))]);
        if (room.messages.length) {
          const newest = newestMessageAt(room.messages);
          if (newest && (!since.current || newest > since.current)) since.current = newest;
          setMessages((prev) => mergeRoomMessages(prev, freshMessages));
        }
        setTick((n) => n + 1);
      })
      .catch((error: unknown) => {
        if (!intent.current()) return;
        if (error instanceof WorldApiError && (error.status === 403 || error.status === 404)) {
          clearUnavailableRoom(error.status === 403 ? "You cannot open this quest's room." : "This quest no longer exists.", error.status === 404);
          return;
        }
        setFailed("Could not load the room. It will retry on the next update.");
      })
      .finally(() => {
        if (!intent.current()) return;
        loading.current = null;
        if (again.current && !goneRef.current) {
          again.current = false;
          window.setTimeout(refresh, 400);
        }
      });
  }, [taskId, clearUnavailableRoom, detailLoader]);

  const settleClarification = useCallback((identity: QuestCardIdentity) => {
    settledCards.current.add(questCardKey(identity));
    if (activeTask.current !== identity.taskId) return;
    setAnswerCard((current) => {
      if (!current) return null;
      return cardAfterSettlement({ taskId: current.taskId, cardId: current.card.id }, identity) ? current : null;
    });
    setClarification((current) => {
      if (!current) return null;
      return cardAfterSettlement({ taskId: activeTask.current, cardId: current.id }, identity) ? current : null;
    });
    refresh();
  }, [refresh]);

  const initialRef = useRef(initial);
  initialRef.current = initial;

  const refreshSoon = useCallback(() => {
    if (goneRef.current || pending.current !== null) return;
    pending.current = window.setTimeout(() => {
      pending.current = null;
      refresh();
    }, 700);
  }, [refresh]);
  useEffect(() => () => {
    if (pending.current !== null) window.clearTimeout(pending.current);
  }, []);

  const settleQuestion = useCallback((questionId: string, status: DecisionStatus) => {
    localSettlements.current.set(questionId, status);
    setMessages((current) => applyRoomSettlements(current, localSettlements.current));
    setNeedsYouMessages((current) => current ? applyRoomSettlements(current, localSettlements.current) : current);
  }, []);

  const serves = roomServes({ members });
  const visibleSessions = useMemo(() => {
    const exact = sessions.filter((session) => session.task_id === taskId);
    return exact.length ? exact : sessions.filter((session) => !session.task_id);
  }, [sessions, taskId]);
  const executor = useMemo(() => {
    const tasks = [...visibleSessions].reverse().filter((session) => session.role === "task");
    return tasks.find((session) => session.status !== "archived") ?? tasks[0] ?? null;
  }, [visibleSessions]);
  const reviewer = useMemo(() => [...visibleSessions].reverse().find((session) => session.role === "review") ?? null, [visibleSessions]);
  const visibleSessionIds = useMemo(() => new Set(visibleSessions.map((session) => session.id)), [visibleSessions]);
  const liveRun = useMemo(() => [...runs].reverse().find((run) => visibleSessionIds.has(run.session_id) && runIsLive(run)) ?? null, [runs, visibleSessionIds]);
  const scope = useMemo(
    () => roomScope(taskId, canonicalTask, members, task?.parts?.map((part) => part.id) ?? [], sessions),
    [taskId, canonicalTask, members, task?.parts, sessions],
  );
  const match = useCallback(
    (event: WorldEvent) =>
      roomEventMatches(scope, event) ||
      questEventTaskId(event) === taskId ||
      (!serves && !event.task_id && !event.session_id && event.agent_id === task?.assigned_agent_id && (event.type === "message.created" || event.type.startsWith("run.") || event.type.startsWith("session."))),
    [scope, taskId, serves, task?.assigned_agent_id],
  );
  useWorldRefresh(refreshSoon, match);
  useEffect(() => {
    if (settlementTask.current !== taskId) {
      settlementTask.current = taskId;
      localSettlements.current.clear();
    }
    loadRequest.current++;
    detailLoader.invalidate();
    loading.current = null;
    again.current = false;
    goneRef.current = false;
    setGone(false);
    setFailed(null);
    if (initialRef.current?.task.id === taskId) {
      applyPage(initialRef.current);
    } else {
      setTask(null);
      setSessions([]);
      setRuns([]);
      cursor.current = undefined;
      since.current = undefined;
      setEvents([]);
      setMessages([]);
      setNeedsYouMessages(undefined);
      setDecisionSummary(undefined);
      setMembers(undefined);
      setCanonicalTask(undefined);
      setAsking(null);
      setClarification(null);
      setAnswerCard(null);
    }
    refresh();
    return () => {
      detailLoader.invalidate();
      loading.current = null;
      again.current = false;
    };
  }, [taskId, initialRequest, applyPage, detailLoader, refresh]);

  useEffect(() => {
    const onEvent = (ev: Event) => {
      const e = (ev as CustomEvent<WorldEvent>).detail;
      if (!e || !ROOM_EVENTS.includes(e.type)) return;
      const settlement = roomEventSettlement(e, scope.canonicalTaskId);
      if (settlement) settleQuestion(settlement.questionId, settlement.status);
      setMessages((prev) => applyRoomSettlements(applyRoomEvent({ messages: prev, members: null }, e, scope.canonicalTaskId).messages, localSettlements.current));
      setMembers((prev) => applyRoomEvent<AgentMessage>({ messages: [], members: prev }, e, scope.canonicalTaskId).members ?? undefined);
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [scope.canonicalTaskId, settleQuestion]);

  const [steps, setSteps] = useState<Map<string, string>>(() => new Map());
  useEffect(() => {
    const ids = new Set(sessions.map((s) => s.id));
    const onEvent = (ev: Event) => {
      const e = (ev as CustomEvent<WorldEvent>).detail;
      const text = typeof e?.payload?.text === "string" ? e.payload.text.trim() : "";
      if (e?.type !== "agent.progress" || !e.session_id || !ids.has(e.session_id) || !text) return;
      setSteps((prev) => (prev.get(e.session_id!) === text ? prev : new Map(prev).set(e.session_id!, text)));
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [sessions]);

  const [progress, setProgress] = useState<Progress | null>(null);
  const progressTick = useRef(0);
  useEffect(() => {
    if (!liveRun) {
      setProgress(null);
      return;
    }
    const onEvent = (ev: Event) => {
      const e = (ev as CustomEvent<WorldEvent>).detail;
      if (e.run_id ? e.run_id !== liveRun.id : e.session_id !== liveRun.session_id) return;
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
  const clarifying = useMemo(() => new Set(sessions.filter((s) => s.plan_state === "clarifying").map((s) => s.id)), [sessions]);
  const board = useMemo(
    () => buildBoard(messages, { status: task?.status, clarifying, lead: task?.assigned_agent_id, needsYouMessages }),
    [messages, task?.status, clarifying, task?.assigned_agent_id, needsYouMessages],
  );
  const needsYou = !!task && questNeedsYou({ ...task, decisions: board.summary });
  const entries = useMemo(() => {
    const deliveredEvents = deliveredCompletionEvents(messages, events, taskId);
    const list: Entry[] = [
      ...messages
        .filter((m) => m.id !== clarification?.id && !board.folded.has(m.id) && kindOf(m) !== "question")
        .map((m) => ({ kind: "message" as const, at: m.created_at, key: `m${m.id}`, message: m })),
      ...board.threads.map((t) => ({ kind: "thread" as const, at: t.question.created_at, key: `q${t.question.id}`, thread: t })),
      ...events.filter((e) => !QUIET_EVENTS.test(e.type)).map((e) => ({ kind: "event" as const, at: e.created_at, key: `e${e.seq}`, event: e, deliveredSummary: deliveredEvents.has(e.seq) })),
      ...runs.filter((r) => r.kind !== "turn").map((r) => ({ kind: "run" as const, at: r.created_at, key: `r${r.id}`, run: r })),
    ];
    return list.sort((a, b) => (a.at < b.at ? -1 : a.at > b.at ? 1 : 0));
  }, [messages, events, runs, board, taskId, clarification?.id]);

  const endRef = useRef<HTMLDivElement>(null);
  const tailKey = clarification ? `c${clarification.id}` : entries.length ? entries[entries.length - 1].key : undefined;
  const { unseen, away, jump, stick } = useFollowBottom(endRef, tailKey, taskId);

  const main = task?.assigned_agent_id ? byId.get(task.assigned_agent_id) : undefined;
  const owner = !!main && !!meId && main.owner_user_id === meId;
  const canonicalLead = members?.find((member) => member.default && member.member_kind === "agent" && member.role === "lead")?.member_id;
  const roomOwnerId = members?.find((member) => member.default && member.member_kind === "person" && member.role === "owner")?.member_id ?? (canonicalLead ? byId.get(canonicalLead)?.owner_user_id : undefined) ?? main?.owner_user_id ?? null;
  const roomOwner = !!meId && roomOwnerId === meId;
  const roomReady = serves && !!task?.assigned_agent_id;
  const helperLabel = useCallback(
    (s: SessionBrief) => {
      const tasks = sessions.filter((x) => x.role === "task");
      if (s.role !== "task") return ROLE_LABEL[s.role] ?? s.role;
      if (s.attempt) return `Attempt ${s.attempt}`;
      return tasks.length > 1 ? `Helper ${tasks.indexOf(s) + 1}` : "Executor";
    },
    [sessions],
  );
  const crew = useMemo(
    () => crewOf({ members, sessions, agents: byId, lead: task?.assigned_agent_id, threads: board.all, sessionLabel: helperLabel, awaitsPlan: sessionAwaitsPlan }),
    [members, sessions, byId, task?.assigned_agent_id, board.all, helperLabel],
  );
  const stats = useMemo(() => crewStats(crew, board, sessions, decisionSummary), [crew, board, sessions, decisionSummary]);
  const showSide = board.threads.length > 0 || (!!asking && asking !== "off") || sessions.filter((s) => s.role === "task").length > 1;
  const askerOf = useCallback(
    (thread: Thread<AgentMessage>) => {
      if (thread.question.author_kind === "user") return thread.question.author_user_id === meId ? "You" : "Someone in the room";
      const session = sessions.find((s) => s.id === thread.question.asked_by_session);
      const agent = session ? byId.get(session.agent_id) : byId.get(thread.question.author_agent_id ?? "");
      if (session && (session.agent_id === task?.assigned_agent_id || !agent)) return helperLabel(session);
      return agent?.name ?? "A helper";
    },
    [sessions, byId, task?.assigned_agent_id, helperLabel, meId],
  );
  const askerAgentOf = (thread: Thread<AgentMessage>) => {
    const session = sessions.find((s) => s.id === thread.question.asked_by_session);
    return byId.get(session?.agent_id ?? thread.question.author_agent_id ?? "");
  };
  const leadName = main?.name ?? "the agent";
  const answerRoomCard = useCallback(
    async (card: AgentMessage, text: string) => {
      try {
        const sent = await worldApi.send(card.agent_id, text, [], undefined, card.id, undefined, scope.canonicalTaskId);
        setMessages((prev) => mergeRoomMessages(prev, [sent]));
        refreshSoon();
        return true;
      } catch {
        return false;
      }
    },
    [scope.canonicalTaskId, refreshSoon],
  );
  const answerQuestion = useCallback(
    async (questionId: string, text: string) => {
      try {
        await roomApi.answer(questionId, text);
        refreshSoon();
        return null;
      } catch (error) {
        return error instanceof WorldApiError ? error.message : "Not sent. Try again.";
      }
    },
    [refreshSoon],
  );
  const parkingRequests = useRef(new Map<string, Promise<string | null>>());
  const [parking, setParking] = useState<Set<string>>(() => new Set());
  const park = useCallback(
    (questionId: string) => {
      const pending = parkingRequests.current.get(questionId);
      if (pending) return pending;
      setParking((current) => new Set(current).add(questionId));
      const request = roomApi
        .park(questionId)
        .then(({ todo_id }) => {
          settleQuestion(questionId, "parked");
          refreshSoon();
          return todo_id;
        })
        .catch(() => null)
        .finally(() => {
          parkingRequests.current.delete(questionId);
          setParking((current) => {
            const next = new Set(current);
            next.delete(questionId);
            return next;
          });
        });
      parkingRequests.current.set(questionId, request);
      return request;
    },
    [refreshSoon, settleQuestion],
  );
  const changeDecision = useCallback(
    async (thread: Thread<AgentMessage>, text: string) => {
      const recipient = thread.question.agent_id.trim();
      if (!recipient) return "No agent owns this question.";
      try {
        const target = decisionTarget(thread);
        const sent = await worldApi.send(recipient, text, [], undefined, undefined, undefined, scope.canonicalTaskId, undefined, {
          text: pairQuote(thread, askerOf(thread)).slice(0, 2000),
          label: `Decision in quest "${task?.title ?? ""}"`.slice(0, 120),
          source: `msg:${target}`,
        }, target);
        setMessages((prev) => mergeRoomMessages(prev, [sent]));
        refreshSoon();
        return null;
      } catch (error) {
        return error instanceof WorldApiError ? error.message : "Not sent. Try again.";
      }
    },
    [scope.canonicalTaskId, task?.title, askerOf, refreshSoon],
  );
  const feedRef = useRef<HTMLDivElement>(null);
  const jumpTo = (questionId: string) => {
    const target = threadOf(board, questionId)?.question.id ?? questionId;
    const el = feedRef.current?.querySelector(`[data-message-id="${CSS.escape(target)}"]`);
    if (el) scrollWithin(el, { block: "center", smooth: true });
  };
  const [flash, setFlash] = useState<string | null>(null);
  const focused = useRef<string | null>(null);
  useEffect(() => {
    const request = focus ? `${taskId}:${focus}:${initialRequest ?? 0}` : null;
    if (!focus || focused.current === request) return;
    const target = threadOf(board, focus)?.question.id ?? focus;
    const el = feedRef.current?.querySelector(`[data-message-id="${CSS.escape(target)}"]`);
    if (!el) return;
    focused.current = request;
    scrollWithin(el, { block: "center" });
    setFlash(target);
  }, [taskId, focus, initialRequest, board, entries.length]);
  useEffect(() => {
    if (!flash) return;
    const timer = window.setTimeout(() => setFlash(null), 2400);
    return () => window.clearTimeout(timer);
  }, [flash]);
  const [roomNote, setRoomNote] = useState<string | null>(null);
  const removeMember = async (entry: { kind: string; id: string; name: string }) => {
    if (entry.kind !== "agent" && entry.kind !== "person") return;
    try {
      await roomApi.removeMember(scope.canonicalTaskId, entry.kind, entry.id);
      setMembers((prev) => prev?.filter((m) => !(m.member_kind === entry.kind && m.member_id === entry.id)));
      setRoomNote(`${entry.name} left the room.`);
    } catch (error) {
      setRoomNote(error instanceof WorldApiError ? error.message : "Could not remove them.");
    }
  };

  const targets = useMemo(() => {
    const list: ComposerTarget[] = [];
    if (roomReady) list.push({ value: "room", label: "Room", hint: "everyone here can follow along" });
    if (executor && executor.status !== "archived")
      list.push({ value: executor.id, label: executor.running ? "Steer the executor" : "Follow up with the executor", hint: executor.running ? "goes into the running work" : "reopens the quest's session" });
    if (reviewer && reviewer.status !== "archived") list.push({ value: reviewer.id, label: "Ask the reviewer", hint: byId.get(reviewer.agent_id)?.name });
    if (main) list.push({ value: "main", label: `Message ${main.name}`, hint: "main mind, with this quest linked" });
    if (roomReady) {
      for (const entry of crew) {
        if (entry.kind !== "agent" || entry.id === main?.id || !byId.has(entry.id)) continue;
        list.push({ value: `ask:${entry.id}`, label: `Ask ${entry.name}`, hint: "their answer shows here" });
      }
    }
    return list;
  }, [roomReady, executor, reviewer, main, byId, crew]);
  const [note, setNote] = useState<string | null>(null);

  const [resuming, setResuming] = useState(false);
  const [resumeNote, setResumeNote] = useState<string | null>(null);
  const resumeState = !liveRun && task?.resume ? task.resume : null;
  const resumable = !!resumeState?.can;
  const resume = async () => {
    if (goneRef.current || resuming) return;
    setResuming(true);
    setResumeNote(null);
    try {
      const outcome = await worldApi.resumeQuest(taskId);
      if (goneRef.current) return;
      setResumeNote(outcome.result === "resumed" ? "Picking the quest back up." : `${main?.name ?? "The agent"} is opening a fresh session to carry on.`);
      refresh();
    } catch (error) {
      if (!goneRef.current) setResumeNote(error instanceof WorldApiError ? error.message : "Could not resume the quest.");
    } finally {
      setResuming(false);
    }
  };

  const [stopping, setStopping] = useState<"step" | "run" | null>(null);
  const stop = async (mode: "step" | "run") => {
    if (goneRef.current || !liveRun || liveRun.status !== "running" || stopping) return;
    if (mode === "run" && !window.confirm("Stop this run? The work so far stays, and it can be resumed later.")) return;
    setStopping(mode);
    try {
      await worldApi.stopRun(liveRun.id, mode);
      if (goneRef.current) return;
      setNote(mode === "step" ? "Stopping the current step. It carries on with the rest." : "Stopping the run.");
      refresh();
    } catch (error) {
      if (!goneRef.current) setNote(error instanceof Error ? error.message : "Could not stop it.");
    } finally {
      setStopping(null);
    }
  };

  const [withdrawing, setWithdrawing] = useState<string | null>(null);
  const withdraw = async (run: RoomRun) => {
    if (run.status !== "queued" || withdrawing) return;
    if (!window.confirm("Withdraw this waiting run? It will not start.")) return;
    setWithdrawing(run.id);
    try {
      await worldApi.withdrawRun(run.id);
      setNote("Withdrew the waiting run.");
      refresh();
    } catch (error) {
      setNote(error instanceof Error ? error.message : "Could not withdraw it.");
    } finally {
      setWithdrawing(null);
    }
  };

  const nowText = useMemo(() => {
    if (!liveRun) return null;
    const who = byId.get(liveRun.agent_id)?.name ?? "The agent";
    if (liveRun.status === "queued") return liveRun.waiting_for === "disk" ? `${who} is waiting for disk space on its device` : `${who} is waiting for a free slot`;
    return progress?.text ?? `${who} is ${liveRun.kind === "review" ? "reviewing" : "working"}`;
  }, [liveRun, progress, byId]);

  const cost = sessions.reduce((n, s) => n + s.cost_usd, 0);
  const tokens = sessions.reduce((n, s) => n + s.tokens, 0);
  const usage = formatUsage(cost, tokens);
  const openClarification = [...messages]
    .reverse()
    .find((message) => message.meta?.clarification && !messages.some((answer) => answer.meta?.answers === message.id));
  const canAnswerClarification = !!clarification && !!meId;
  const heldAnswerCard = answerCard?.taskId === taskId ? answerCard.card : null;
  const workshopSession = executor ?? reviewer;
  const sideColumn = (
    <>
      <CrewCard stats={stats} crew={crew} byId={byId} sessions={sessions} progress={steps} handles={handles} onOpenAgent={onOpenAgent} />
      <section aria-label="Decisions">
        <p className="mb-1.5 flex items-center gap-1.5 text-w-xs font-semibold text-w-muted">
          <Scales size={14} weight="duotone" /> Decisions{board.decisions.length ? ` (${board.decisions.length})` : ""}
        </p>
        <DecisionsList decisions={board.decisions} askerOf={askerOf} lead={leadName} canAct={owner} onJump={jumpTo} onChange={changeDecision} />
      </section>
    </>
  );
  const browserFeature = useFeature("browser");
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
            <h2 className="mt-1 text-w-xl font-semibold leading-tight text-w-strong">{gone ? "Quest unavailable" : task?.title ?? <Shimmer>Opening the room</Shimmer>}</h2>
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            {!gone && (
              <>
                {main && meId && main.owner_user_id === meId && browserFeature?.state === "ready" && <WatchBrowserButton agentId={main.id} questId={taskId} questTitle={task?.title} className={cn(BTN_SECONDARY_SM, "hidden h-10 sm:inline-flex")} />}
                {workshopSession && <OpenWorkshop detail={{ sessionId: executor?.id ?? null, taskId, agentId: workshopSession.agent_id }} label="Workshop" className="hidden h-10 rounded-lg px-2.5 text-w-xs sm:inline-flex" />}
                <button type="button" onClick={() => void copyLink(worldUrl("quest", taskId))} title={linkCopied ? "Link copied" : "Copy a link to this quest"} aria-label={linkCopied ? "Link copied" : "Copy a link to this quest"} className={ICON_BTN_MD}>
                  {linkCopied ? <Check size={16} weight="bold" className="text-w-ok" /> : <LinkIcon size={16} weight="bold" />}
                </button>
                {executor && (
                  <button type="button" onClick={() => onOpenSession(executor.id, executor.agent_id)} title="Full session history" aria-label="Full session history" className={ICON_BTN_MD}>
                    <ClockCounterClockwise size={16} weight="bold" />
                  </button>
                )}
              </>
            )}
            {sideToggle}
            <CloseButton onClose={onClose} />
          </div>
        </div>
        {task && (
          <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-w-xs text-w-text2">
            {task.pr_url && <PrChip url={task.pr_url} />}
            {task.past_prs?.map((pr) => (
              <span key={pr.url} className="inline-flex items-center gap-1">
                Earlier <PrChip url={pr.url} />
                {pr.state}
              </span>
            ))}
            {task.parent && (
              <button type="button" onClick={() => task.parent && openQuest(task.parent)} className="max-w-[18rem] truncate font-medium text-w-link hover:underline">
                Part of {task.parent.title}
              </button>
            )}
            {clarification || sessions.some((s) => s.plan_state === "clarifying") ? (
              <span>
                Waiting for your answers
                {!clarification && openClarification && byId.get(openClarification.agent_id)?.owner_user_id === meId ? (
                  <> · <RichText text={`[Answer the questions](msg:${openClarification.id})`} handles={handles} onOpenAgent={onOpenAgent} /></>
                ) : !clarification && task.assigned_agent_id && byId.get(task.assigned_agent_id)?.owner_user_id === meId ? (
                  <button type="button" className="ml-1.5 text-w-link hover:underline" onClick={() => onOpenAgent(task.assigned_agent_id!)}>Answer in chat</button>
                ) : null}
              </span>
            ) : questAwaitsPlan(task) ? (
              <span className="font-semibold text-w-alert">Needs you: the plan is waiting for your approval</span>
            ) : needsYou ? (
              <span className="font-semibold text-w-alert">Needs you: a question is waiting for your answer</span>
            ) : task.plan_first ? (
              <span>Plan first</span>
            ) : null}
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
        {resumable && (
          <div className="mt-2 flex flex-wrap items-center gap-2 rounded-lg border-2 border-w-line bg-w-card/70 px-2.5 py-1.5">
            <span className="min-w-0 flex-1 text-w-sm">
              {resumeNote ?? (task?.resume?.mode === "fresh" ? "Its session is gone. Resuming opens a fresh one on the same branch and hands over the last report." : "Resuming carries on in the same session and quest folder.")}
            </span>
            <button type="button" onClick={() => void resume()} disabled={resuming} className={BTN_PRIMARY_SM}>
              <Play size={12} weight="fill" />
              {resuming ? "Resuming..." : "Resume quest"}
            </button>
          </div>
        )}
        {resumeState && !resumeState.can && resumeState.reason && (
          <p className="mt-2 rounded-lg border-2 border-w-line bg-w-card/70 px-2.5 py-1.5 text-w-sm">Cannot resume yet: {resumeState.reason}.</p>
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
        {!gone && <div className="mt-2">
          <MembersBar
            crew={crew}
            byId={byId}
            canRemove={roomOwner && serves}
            onOpenAgent={onOpenAgent}
            onOpenSession={onOpenSession}
            onRemove={(entry) => void removeMember(entry)}
            add={roomOwner && serves ? <AddMember taskId={scope.canonicalTaskId} agents={agents} ownerId={roomOwnerId} crew={crew} onAdded={(text) => { setRoomNote(text); refreshSoon(); }} /> : undefined}
          />
          {roomNote && <p className="mt-1 text-w-xs text-w-text2" role="status">{roomNote}</p>}
        </div>}
        {!gone && <NeedsYouStrip items={board.needsYou} threads={board.all} byId={byId} meId={meId} focus={focus} onAnswer={answerRoomCard} onPark={park} parking={parking} onOpenAgent={onOpenAgent} handles={handles} />}
      </div>
      <div className="@container flex min-h-0 flex-1">
      <div ref={feedRef} className="relative min-h-0 min-w-0 flex-1 overflow-y-auto px-4 py-3 sm:px-5">
        {showSide && (
          <details className="mb-3 rounded-lg border-2 border-w-line bg-w-card/70 px-3 py-2 @4xl:hidden">
            <summary className="cursor-pointer text-w-xs font-semibold text-w-muted">
              Crew and decisions{board.decisions.length ? ` (${board.decisions.length})` : ""}
            </summary>
            <div className="mt-2 space-y-3">{sideColumn}</div>
          </details>
        )}
        <div className="space-y-2">
          {failed && <p className="text-w-sm text-w-danger">{failed}</p>}
          {task && entries.length === 0 && !clarification && <EmptyState icon={Hourglass} title="Nothing has happened on this quest yet" description="Messages, work and reviews show up here as they happen." />}
          {entries.map((entry, i) => {
            const previous = entries[i - 1]?.at;
            const divider = <DayDivider at={entry.at} previous={previous} />;
            if (entry.kind === "message")
              return (
                <div key={entry.key}>
                  {divider}
                  <MessageRow message={entry.message} taskId={taskId} meId={meId} byId={byId} members={members} handles={handles} onOpenAgent={onOpenAgent} onOpenSession={(id) => onOpenSession(id, entry.message.agent_id)} />
                </div>
              );
            if (entry.kind === "thread")
              return (
                <div key={entry.key}>
                  {divider}
                  <QuestionThread
                    highlighted={flash === entry.thread.question.id}
                    byId={byId}
                    members={members}
                    thread={entry.thread}
                    asker={askerOf(entry.thread)}
                    askerAgent={askerAgentOf(entry.thread)}
                    lead={leadName}
                    canAct={owner}
                    handles={handles}
                    onOpenAgent={onOpenAgent}
                    onChange={changeDecision}
                    onAnswer={answerQuestion}
                    onPark={park}
                    parking={parking.has(entry.thread.question.id)}
                    questTitle={task?.title ?? "this quest"}
                  />
                </div>
              );
            if (entry.kind === "event") {
              const e = entry.event;
              const who = (e.agent_id && byId.get(e.agent_id)?.name) || "Someone";
              const cards = eventCards(e, entry.deliveredSummary);
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
                      <ReportCard card={card} handles={handles} onOpenAgent={onOpenAgent} />
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
                  onWithdraw={entry.run.status === "queued" && entry.run.kind !== "turn" ? () => void withdraw(entry.run) : undefined}
                  withdrawing={withdrawing === entry.run.id}
                />
              </div>
            );
          })}
          {clarification && (
            <div className="max-w-[min(92%,40rem)] rounded-xl border-2 border-w-teal bg-w-aqua/50 px-3 py-2.5">
              <p className="text-w-xs font-semibold text-w-link">Your answer is needed before this quest continues</p>
              <div className="mt-1 text-w-sm text-w-text">
                <RichText text={clarification.body} handles={handles} onOpenAgent={onOpenAgent} />
              </div>
            </div>
          )}
          <div ref={endRef} />
        </div>
        {!gone && <NewMessagesPill show={unseen} away={away} onClick={jump} />}
      </div>
      {showSide && (
        <aside aria-label="Crew and decisions" className="hidden w-[20rem] shrink-0 space-y-3 overflow-y-auto border-l-2 border-dashed border-w-line2 px-3 py-3 @4xl:block">
          {sideColumn}
        </aside>
      )}
      </div>
      {!gone && <div className="space-y-2 border-t-2 border-dashed border-w-line2 bg-w-card/70 px-4 py-3 sm:px-5">
        {heldAnswerCard && meId && (
          <div hidden={!canAnswerClarification || clarification?.id !== heldAnswerCard.id}>
            <ClarificationComposer key={`${taskId}:${heldAnswerCard.id}`} taskId={taskId} card={heldAnswerCard} onSettled={settleClarification} />
          </div>
        )}
        {clarification && !meId && <p className="text-w-xs text-w-muted">Loading who can answer this question...</p>}
        <QuestComposer key={taskId} taskId={taskId} canonicalTaskId={scope.canonicalTaskId} targets={targets} executor={executor} main={main} roomReady={roomReady} crew={crew} byId={byId} hidden={!!clarification} ready={!!task} refresh={refresh} stick={stick} />
        {note && <p className="text-w-xs text-w-text2">{note}</p>}
        {task?.pr_url && (
          <a href={task.pr_url} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-w-xs text-w-link underline sm:hidden">
            Open the PR <ArrowSquareOut size={10} weight="bold" />
          </a>
        )}
      </div>}
    </div>
  );
}
