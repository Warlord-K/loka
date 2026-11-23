"use client";

import { useEffect, useMemo, useState } from "react";
import { ArrowRight, CaretLeft, CaretRight, HandPalm, ListChecks, PauseCircle, PencilSimple, Plus, Robot, UsersThree, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { SOLO } from "@/lib/edition";
import { formatUsage, worldApi, WorldApiError, type AgentMessage, type SessionBrief, type WorldAgent } from "@/lib/world";
import { cardChoices, crewCardSpec, crewStateText, LIVE_STATES, pairQuote, questionRecipient, whyLabel, YOURS, type CrewEntry, type CrewStats, type RoomMember, type Thread } from "@/lib/quest-board";
import { roomApi } from "@/lib/quest-room-api";
import { AgentFace, PersonAvatar } from "./channel-ui";
import { AskChoices, MessageStamp, RichText } from "./chat-kit";
import { RuntimeBadge } from "./crew-ui";
import { MessageToolbar } from "./message-row";
import { HIGHLIGHT } from "./message-links";
import { MAIN_TARGET, quoteProps, sendQuote } from "./quote";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, FOCUS, ITEM_CARD, PILL_NEUTRAL, PILL_WARN } from "./styles";
import { Shimmer } from "./work-ui";
import { WSelect } from "./controls";
import { UiCard } from "./lazy";

export type AnswerCard = (card: AgentMessage, text: string) => Promise<boolean>;

function failure(error: unknown): string {
  if (error instanceof WorldApiError) return error.message;
  if (error instanceof TypeError) return "Not sent. You seem to be offline. Your words are still here.";
  return "Not sent. Try again.";
}

function stateTone(state: CrewEntry["state"]): string {
  if (state === "paused" || state === "error") return "text-w-danger";
  if (state === "needs_you" || state === "asking") return "text-w-alert";
  if (LIVE_STATES.includes(state)) return "text-w-link";
  return "text-w-muted";
}

function MemberFace({ entry, agent, size }: { entry: CrewEntry; agent: WorldAgent | undefined; size: number }) {
  if (agent) return <AgentFace agent={agent} size={size} />;
  if (entry.kind === "person") return <PersonAvatar id={entry.id} name={entry.name} size={size} soft />;
  return <Robot size={size - 4} className="shrink-0 text-w-muted" />;
}

export function MembersBar({
  crew,
  byId,
  canRemove,
  onOpenAgent,
  onOpenSession,
  onRemove,
  add,
}: {
  crew: CrewEntry[];
  byId: Map<string, WorldAgent>;
  canRemove: boolean;
  onOpenAgent: (id: string) => void;
  onOpenSession: (sessionId: string, agentId: string) => void;
  onRemove: (entry: CrewEntry) => void;
  add?: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-1.5">
    <div role="list" aria-label="People and agents in this room" className="-mx-1 flex min-w-0 flex-1 gap-1.5 overflow-x-auto px-1 pb-1">
      {crew.map((entry) => {
        const agent = entry.agentId ? byId.get(entry.agentId) : undefined;
        const open = entry.sessionId && entry.agentId ? () => onOpenSession(entry.sessionId!, entry.agentId!) : entry.agentId ? () => onOpenAgent(entry.agentId!) : undefined;
        const live = LIVE_STATES.includes(entry.state);
        return (
          <div role="listitem" key={entry.key} className={cn(ITEM_CARD, "group relative flex shrink-0 items-center", live && "border-w-teal/60 bg-w-aqua/60")}>
            <button
              type="button"
              onClick={open}
              disabled={!open}
              title={entry.sessionId ? "Open this helper's history" : entry.agentId ? "Open this agent" : undefined}
              className={cn("flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-w-xs outline-none disabled:cursor-default", FOCUS)}
            >
              <MemberFace entry={entry} agent={agent} size={24} />
              <span className="min-w-0">
                <span className="flex items-center gap-1.5">
                  <span className="max-w-[10rem] truncate font-semibold leading-tight text-w-strong">{entry.name}</span>
                  {entry.runtime && <RuntimeBadge cli={entry.runtime.cli} model={entry.runtime.model} />}
                </span>
                <span className={cn("flex max-w-[16rem] items-center gap-1 truncate leading-tight", stateTone(entry.state))}>
                  {entry.state === "paused" && <PauseCircle size={12} weight="bold" aria-hidden className="shrink-0" />}
                  {live ? <Shimmer>{entry.detail}</Shimmer> : entry.detail}
                </span>
              </span>
            </button>
            {canRemove && entry.removable && (
              <button
                type="button"
                onClick={() => onRemove(entry)}
                aria-label={`Remove ${entry.name} from the room`}
                title={`Remove ${entry.name}`}
                className={cn("mr-1 grid h-6 w-6 place-items-center rounded-md text-w-muted opacity-0 transition-opacity hover:bg-w-sand hover:text-w-strong focus-visible:opacity-100 group-hover:opacity-100", FOCUS)}
              >
                <X size={12} weight="bold" />
              </button>
            )}
          </div>
        );
      })}
    </div>
    {add}
    </div>
  );
}

export function AddMember({
  taskId,
  agents,
  ownerId,
  crew,
  onAdded,
}: {
  taskId: string;
  agents: WorldAgent[];
  ownerId: string | null;
  crew: CrewEntry[];
  onAdded: (note: string) => void;
}) {
  const [people] = useState<{ id: string; name: string }[]>([]);
  const [busy, setBusy] = useState(false);
  const inRoom = new Set(crew.map((c) => (c.kind === "session" ? "" : `${c.kind}:${c.id}`)));
  const options = [
    ...agents
      .filter((a) => a.owner_user_id === ownerId && !a.archived_at && !inRoom.has(`agent:${a.id}`))
      .map((a) => ({ value: `agent:${a.id}`, label: a.name, hint: a.paused_reason ? "agent, paused" : "agent" })),
    ...(!SOLO ? people.filter((p) => !inRoom.has(`person:${p.id}`)).map((p) => ({ value: `person:${p.id}`, label: p.name, hint: "person" })) : []),
  ];
  const pick = async (value: string) => {
    if (!value || busy) return;
    const [kind, id] = value.split(":") as ["agent" | "person", string];
    const name = options.find((o) => o.value === value)?.label ?? "They";
    setBusy(true);
    try {
      await roomApi.addMember(taskId, kind, id);
      onAdded(kind === "agent" ? `${name} joined the room. Choose Ask to get a quick answer.` : `${name} can now open this room.`);
    } catch (error) {
      onAdded(failure(error));
    } finally {
      setBusy(false);
    }
  };
  if (!options.length) return null;
  return (
    <div className="flex shrink-0 items-center">
      <WSelect size="sm" ariaLabel={SOLO ? "Add an agent to this room" : "Add an agent or a person to this room"} value="" placeholder="Add" disabled={busy} onChange={(v) => void pick(v)} className="w-auto whitespace-nowrap" options={options} icon={<Plus size={12} weight="bold" />} />
    </div>
  );
}

export function NeedsYouStrip({
  items,
  threads,
  byId,
  meId,
  focus,
  onAnswer,
  onPark,
  parking,
  onOpenAgent,
  handles,
}: {
  items: AgentMessage[];
  threads: Thread<AgentMessage>[];
  byId: Map<string, WorldAgent>;
  meId: string | null;
  focus?: string | null;
  onAnswer: AnswerCard;
  onPark: (questionId: string) => Promise<string | null>;
  parking: Set<string>;
  onOpenAgent: (id: string) => void;
  handles: Map<string, WorldAgent>;
}) {
  const [index, setIndex] = useState(0);
  const [note, setNote] = useState<string | null>(null);
  const [sending, setSending] = useState<string | null>(null);
  const focusedCard = useMemo(
    () => (focus ? threads.find((thread) => thread.question.id === focus || thread.card?.id === focus || thread.answer?.id === focus)?.card?.id : undefined),
    [threads, focus],
  );
  const ordered = useMemo(() => {
    const hit = focus ? items.find((m) => m.id === focus || m.id === focusedCard) : undefined;
    return hit ? [hit, ...items.filter((m) => m.id !== hit.id)] : items;
  }, [items, focus, focusedCard]);
  useEffect(() => setIndex(0), [focus, focusedCard]);
  const at = Math.min(index, Math.max(0, ordered.length - 1));
  const card = ordered[at];
  useEffect(() => setNote(null), [card?.id]);
  if (!card) return null;
  const thread = threads.find((t) => t.card?.id === card.id) ?? null;
  const isParking = !!thread && parking.has(thread.question.id);
  const agent = byId.get(card.agent_id);
  const mine = !!agent && agent.owner_user_id === meId;
  const about = thread?.about ?? card.meta?.why ?? null;
  const why = whyLabel(about) ?? (card.meta?.clarification ? "Before it starts" : null);
  const suggestion = card.meta?.suggestion?.trim() || null;
  const choices = cardChoices(card.meta);
  const answer = async (text: string) => {
    setSending(card.id);
    setNote(null);
    const ok = await onAnswer(card, text);
    setSending(null);
    if (!ok) setNote("Not sent. You seem to be offline or it was already answered. Your answer is still here.");
  };
  return (
    <section aria-label="Needs you" aria-live="polite" className="mt-2 rounded-lg border-2 border-w-ink bg-w-lemon px-3 py-2 shadow-[0_3px_0_rgb(var(--w-ink))]">
      <div className="flex flex-wrap items-center gap-2 text-w-xs">
        <HandPalm size={14} weight="bold" className="shrink-0 text-w-alert" />
        <span className="font-semibold text-w-strong">Needs you</span>
        {why && <span className={PILL_WARN}>{why}</span>}
        {agent && (
          <button type="button" onClick={() => onOpenAgent(agent.id)} className="text-w-text2 hover:underline">
            from {agent.name}
          </button>
        )}
        {mine && thread && (
          <button
            type="button"
            disabled={isParking}
            onClick={() => void onPark(thread.question.id).then((todo) => setNote(todo ? `Parked as a to-do. ${agent?.name ?? "The helper"} goes on with its assumption.` : "Could not park it."))}
            className={cn("ml-auto inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-w-text2 hover:bg-w-sand hover:text-w-strong disabled:cursor-wait disabled:opacity-60", FOCUS)}
          >
            <ListChecks size={12} weight="bold" /> {isParking ? <Shimmer>Parking as a to-do</Shimmer> : "Not now, park it as a to-do"}
          </button>
        )}
        {ordered.length > 1 && (
          <span className={cn("flex items-center gap-1 text-w-text2", !(mine && thread) && "ml-auto")}>
            <button type="button" aria-label="Previous" disabled={at === 0} onClick={() => setIndex(at - 1)} className={cn("grid h-6 w-6 place-items-center rounded-md hover:bg-w-sand disabled:opacity-40", FOCUS)}>
              <CaretLeft size={12} weight="bold" />
            </button>
            <span className="tabular-nums">
              {at + 1} of {ordered.length}
            </span>
            <button type="button" aria-label="Next" disabled={at >= ordered.length - 1} onClick={() => setIndex(at + 1)} className={cn("grid h-6 w-6 place-items-center rounded-md hover:bg-w-sand disabled:opacity-40", FOCUS)}>
              <CaretRight size={12} weight="bold" />
            </button>
          </span>
        )}
      </div>
      <div {...quoteProps(`msg:${card.id}`, `Question for you in ${agent?.name ?? "the agent"}'s quest`)} className="mt-1 max-h-24 select-text overflow-y-auto text-w-base text-w-text">
        <RichText text={thread?.question.body || card.body} handles={handles} onOpenAgent={onOpenAgent} />
      </div>
      {suggestion && (
        <p className="mt-1 flex items-center gap-1.5 text-w-sm text-w-text2">
          <ArrowRight size={12} weight="bold" className="shrink-0" />
          {agent?.name ?? "The agent"} suggests: <span className="font-semibold text-w-strong">{suggestion}</span>
        </p>
      )}
      {mine ? (
        <div aria-busy={sending === card.id} className={cn(sending === card.id && "pointer-events-none opacity-60")}>
          {sending === card.id && (
            <p className="mt-2 text-w-sm" role="status">
              <Shimmer>Sending your answer</Shimmer>
            </p>
          )}
          <AskChoices key={card.id} choices={choices} answered={false} allowReply onAnswer={(text) => void answer(text)} />
        </div>
      ) : (
        <p className="mt-1 text-w-xs text-w-text2">Waiting for {agent?.name ?? "the agent"}&apos;s owner to answer.</p>
      )}
      {note && <p className="mt-1 text-w-xs text-w-danger" role="alert">{note}</p>}
    </section>
  );
}

function settledLine(thread: Thread<AgentMessage>, asker: string, lead: string): string {
  const who = thread.by?.name || lead;
  if (thread.status === "assumed") return `${asker} went on with an assumption`;
  if (thread.status === "parked") return "Parked as a to-do";
  if (thread.status === "passed_up") return thread.card ? `${who} passed it to you` : `Waiting for ${who}'s owner`;
  if (thread.by?.kind === "person" || (!thread.by && thread.answer?.author_kind === "user")) return thread.by?.name && thread.by.kind === "person" ? `Decided by ${thread.by.name}` : "Decided by you";
  return `Decided by ${who}`;
}

const SLOW_MS = 90_000;

export function ChangeThis({ onSend, label = "Change this" }: { onSend: (text: string) => Promise<string | null>; label?: string }) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  if (!open)
    return (
      <span className="inline-flex items-center gap-2">
        <button type="button" onClick={() => setOpen(true)} className={cn(BTN_SECONDARY_SM, "h-7 px-2 text-w-xs")}>
          <PencilSimple size={12} weight="bold" /> {label}
        </button>
        {note && <span className="text-w-xs text-w-text2">{note}</span>}
      </span>
    );
  return (
    <form
      className="mt-1 flex items-end gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (!text.trim() || busy) return;
        setBusy(true);
        void onSend(text).then((error) => {
          setBusy(false);
          if (error) setNote(error);
          else {
            setText("");
            setOpen(false);
            setNote("Sent. It goes to the helpers that relied on it.");
          }
        });
      }}
    >
      <textarea
        autoFocus
        rows={2}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            setOpen(false);
          }
        }}
        aria-label="What should it be instead?"
        placeholder="What should it be instead?"
        className={cn("min-w-0 flex-1 resize-none rounded-lg border-2 border-w-line2 bg-w-paper px-2.5 py-2 text-w-sm text-w-text", FOCUS)}
      />
      <button type="submit" disabled={!text.trim() || busy} className={BTN_PRIMARY_SM}>
        {busy ? "Sending..." : "Send"}
      </button>
      {note && <span className="text-w-xs text-w-danger">{note}</span>}
    </form>
  );
}

export function QuestionThread({
  highlighted,
  byId,
  members,
  thread,
  asker,
  askerAgent,
  lead,
  canAct,
  handles,
  onOpenAgent,
  onChange,
  onAnswer,
  onPark,
  parking,
  questTitle,
}: {
  thread: Thread<AgentMessage>;
  asker: string;
  askerAgent: WorldAgent | undefined;
  lead: string;
  canAct: boolean;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  onChange: (thread: Thread<AgentMessage>, text: string) => Promise<string | null>;
  onAnswer: (questionId: string, text: string) => Promise<string | null>;
  onPark: (questionId: string) => Promise<string | null>;
  parking: boolean;
  questTitle: string;
  highlighted?: boolean;
  byId: Map<string, WorldAgent>;
  members?: RoomMember[];
}) {
  const q = thread.question;
  const group = thread.group && thread.group.length > 1 ? thread.group : null;
  const to = group ? group.map((g) => questionRecipient(g.question, byId, members, lead)).join(" and ") : questionRecipient(q, byId, members, lead);
  const about = whyLabel(thread.about);
  const yours = !!thread.about && YOURS.includes(thread.about);
  const assumption = q.meta?.question?.assumption?.trim() || null;
  const refs = [...(thread.answer?.meta?.refs ?? []), ...(q.meta?.refs ?? []), ...(q.meta?.question?.refs ?? [])];
  const [now, setNow] = useState(() => Date.now());
  const open = thread.status === "open";
  useEffect(() => {
    if (!open) return;
    const timer = window.setInterval(() => setNow(Date.now()), 15_000);
    return () => window.clearInterval(timer);
  }, [open]);
  const slow = open && now - Date.parse(q.created_at) > SLOW_MS;
  const [answering, setAnswering] = useState(false);
  const [parkNote, setParkNote] = useState<string | null>(null);
  const quote = () =>
    sendQuote(MAIN_TARGET, {
      text: pairQuote(thread, asker).slice(0, 2000),
      label: `Question in quest "${questTitle}"`.slice(0, 120),
      source: `msg:${q.id}`,
    });
  const settledTone = thread.status === "assumed" ? "border-w-warn/50 bg-w-lemon/50" : thread.status === "passed_up" ? "border-w-alert/50 bg-w-card" : "border-w-line bg-w-field/50";
  return (
    <article
      id={`question-${q.id}`}
      data-message-id={q.id}
      aria-label={`${asker} asks ${to}`}
      {...quoteProps(`msg:${q.id}`, `Question in quest "${questTitle}"`)}
      className={cn("group relative max-w-[min(96%,44rem)] select-text rounded-lg border-2 border-w-line2 bg-w-card px-3 py-2 text-w-base", highlighted && HIGHLIGHT)}
    >
      <MessageToolbar actions={{ onQuote: quote, copy: pairQuote(thread, asker), link: q.id }} className="right-2" />
      <p className="flex flex-wrap items-center gap-1.5 text-w-xs text-w-muted">
        {askerAgent ? <AgentFace agent={askerAgent} size={18} /> : <Robot size={16} className="shrink-0" />}
        <span>
          <span className="font-semibold text-w-strong">{asker}</span> {asker === "You" ? "ask" : "asks"} <span className="font-semibold text-w-strong">{to}</span>
        </span>
        {about && <span className={yours ? PILL_WARN : PILL_NEUTRAL}>{about}</span>}
        <MessageStamp createdAt={q.created_at} />
      </p>
      <div className="mt-1">
        <RichText text={q.body} handles={handles} onOpenAgent={onOpenAgent} />
      </div>
      {assumption && thread.status !== "assumed" && <p className="mt-0.5 text-w-xs text-w-text2">Until then it goes on with: {assumption}</p>}
      {group ? (
        <ul className="mt-2 space-y-1.5">
          {group.map((g) => {
            const name = questionRecipient(g.question, byId, members, lead);
            const face = byId.get(g.question.agent_id);
            return (
              <li key={g.question.id} className={cn("flex items-start gap-2 rounded-md border-2 px-2.5 py-1.5 text-w-sm", g.status === "open" ? "border-dashed border-w-line2" : "border-w-line bg-w-field/50")}>
                {face ? <AgentFace agent={face} size={18} /> : <Robot size={16} className="shrink-0 text-w-muted" />}
                <span className="min-w-0 flex-1">
                  <span className="block text-w-xs font-semibold text-w-muted">{name}</span>
                  {g.status === "open" ? (
                    <Shimmer>{`${name} is answering`}</Shimmer>
                  ) : (
                    <RichText text={g.answerText?.trim() || settledLine(g, asker, lead)} handles={handles} onOpenAgent={onOpenAgent} />
                  )}
                </span>
              </li>
            );
          })}
        </ul>
      ) : (
      <div className={cn("mt-2 rounded-md border-2 px-2.5 py-1.5 text-w-sm", open ? "border-dashed border-w-line2 bg-transparent" : settledTone)}>
        {open ? (
          <div className="space-y-1.5">
            <p className="text-w-text2" role="status">
              <Shimmer>{slow ? `${to} is still deciding. ${asker} keeps going meanwhile` : `${to} is deciding`}</Shimmer>
            </p>
            {canAct && !answering && (
              <div className="flex flex-wrap items-center gap-2">
                <button type="button" onClick={() => setAnswering(true)} className={cn(BTN_SECONDARY_SM, "h-7 px-2 text-w-xs")}>
                  Answer it yourself
                </button>
                <button type="button" disabled={parking} onClick={() => void onPark(q.id).then((todo) => setParkNote(todo ? "Parked as a to-do." : "Could not park it."))} className={cn(BTN_SECONDARY_SM, "h-7 px-2 text-w-xs disabled:cursor-wait")}>
                  <ListChecks size={12} weight="bold" /> {parking ? <Shimmer>Parking</Shimmer> : "Park as a to-do"}
                </button>
                {parkNote && <span className="text-w-xs text-w-text2">{parkNote}</span>}
              </div>
            )}
            {canAct && answering && <ChangeThis label="Answer" onSend={(text) => onAnswer(q.id, text)} />}
          </div>
        ) : (
          <>
            <p className={cn("text-w-xs font-semibold", thread.status === "assumed" ? "text-w-warn" : thread.status === "passed_up" ? "text-w-alert" : "text-w-muted")}>{settledLine(thread, asker, lead)}</p>
            {thread.status === "assumed" ? (
              <p className="mt-0.5 text-w-text">{thread.answerText?.trim() || assumption || "It picked the most likely option."}</p>
            ) : thread.status === "passed_up" ? (
              <p className="mt-0.5 text-w-text2">{thread.card ? "Answer it in Needs you at the top." : "Only the agent's owner can answer this question."}</p>
            ) : (
              thread.answerText && (
                <div className="mt-0.5 text-w-text">
                  <RichText text={thread.answerText} handles={handles} onOpenAgent={onOpenAgent} />
                </div>
              )
            )}
            {refs.length > 0 && (
              <div className="mt-1 text-w-xs">
                <RichText text={Array.from(new Set(refs)).join("\n")} handles={handles} onOpenAgent={onOpenAgent} />
              </div>
            )}
            {canAct && (thread.status === "answered" || thread.status === "assumed") && (
              <div className="mt-1.5">
                <ChangeThis onSend={(text) => onChange(thread, text)} />
              </div>
            )}
          </>
        )}
      </div>
      )}
    </article>
  );
}

export function DecisionsList({
  decisions,
  askerOf,
  lead,
  canAct,
  onJump,
  onChange,
}: {
  decisions: Thread<AgentMessage>[];
  askerOf: (thread: Thread<AgentMessage>) => string;
  lead: string;
  canAct: boolean;
  onJump: (questionId: string) => void;
  onChange: (thread: Thread<AgentMessage>, text: string) => Promise<string | null>;
}) {
  if (!decisions.length) return <p className="text-w-xs text-w-muted">Decisions show up here as questions get settled.</p>;
  return (
    <ol className="space-y-2">
      {decisions.map((thread, i) => (
        <li key={thread.question.id} className="rounded-md border-2 border-w-line bg-w-card px-2.5 py-1.5 text-w-sm">
          <button type="button" onClick={() => onJump(thread.question.id)} className={cn("block w-full rounded text-left", FOCUS)}>
            <span className="flex items-baseline gap-1.5">
              <span className="font-mono text-w-2xs text-w-muted">{i + 1}</span>
              <span className="line-clamp-2 min-w-0 flex-1 text-w-text">{thread.answerText?.trim() || thread.question.meta?.question?.assumption || thread.question.body}</span>
            </span>
            <span className={cn("mt-0.5 block text-w-2xs", thread.status === "assumed" ? "text-w-warn" : "text-w-muted")}>{settledLine(thread, askerOf(thread), lead)}</span>
          </button>
          {canAct && thread.status !== "parked" && (
            <div className="mt-1">
              <ChangeThis onSend={(text) => onChange(thread, text)} />
            </div>
          )}
        </li>
      ))}
    </ol>
  );
}

export function CrewCard({
  stats,
  crew,
  byId,
  sessions,
  progress,
  handles,
  onOpenAgent,
}: {
  stats: CrewStats;
  crew: CrewEntry[];
  byId: Map<string, WorldAgent>;
  sessions: SessionBrief[];
  progress: Map<string, string>;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
}) {
  const spec = useMemo(() => crewCardSpec(stats, formatUsage(stats.costUsd, 0).replace(/^Cost /, "") || "$0"), [stats]);
  const helpers = crew.filter((c) => c.kind === "session" || c.role === "helper");
  return (
    <section aria-label="Crew status" className="rounded-lg border-2 border-w-line2 bg-w-card px-3 py-2">
      <p className="mb-1.5 flex items-center gap-1.5 text-w-xs font-semibold text-w-muted">
        <UsersThree size={14} weight="duotone" /> Crew
      </p>
      <UiCard spec={spec} stateKey="quest-crew" handles={handles} onOpenAgent={onOpenAgent} canAnswer={false} answered={false} onAnswer={async () => false} />
      {helpers.length > 0 && (
        <ul className="mt-2 space-y-1.5 border-t-2 border-dashed border-w-line pt-2">
          {helpers.map((entry) => {
            const agent = entry.agentId ? byId.get(entry.agentId) : undefined;
            const session = entry.sessionId ? sessions.find((s) => s.id === entry.sessionId) : undefined;
            const step = entry.sessionId ? progress.get(entry.sessionId) : undefined;
            const live = LIVE_STATES.includes(entry.state);
            return (
              <li key={entry.key} className="flex items-start gap-2 text-w-xs">
                <MemberFace entry={entry} agent={agent} size={20} />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1.5">
                    <span className="truncate font-semibold text-w-strong">{entry.name}</span>
                    {entry.runtime && <RuntimeBadge cli={entry.runtime.cli} model={entry.runtime.model} />}
                  </span>
                  <span className={cn("block truncate", stateTone(entry.state))}>{live ? <Shimmer>{step || crewStateText(entry.state)}</Shimmer> : crewStateText(entry.state)}</span>
                </span>
                {session && session.cost_usd > 0 && <span className="shrink-0 font-mono text-w-2xs text-w-muted">${session.cost_usd.toFixed(2)}</span>}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
