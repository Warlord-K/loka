"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BookmarkSimple, Lightning, Phone, Plus, User, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { NavigationRequest } from "@/lib/exact-world-navigation";
import { QUICK_KEEP, QUICK_SKIP, WorldApiError, worldApi, type AgentMessage, type QuickBrief, type QuickChat, type WorldAgent, type WorldEvent } from "@/lib/world";
import { runtimeName } from "@/lib/model-names";
import { latestOnly, startRechecks } from "@/lib/latest";
import { chatWaiting } from "@/lib/thread-wait";
import { AgentFace, PersonAvatar } from "./channel-ui";
import { AskChoices, AttachButton, AttachmentGrid, NewMessagesPill, PendingStrip, RichText, useAgentHandles, useAttachments, useFollowBottom, useRefetchOnResume } from "./chat-kit";
import { useDraft } from "./drafts";
import { MessageAuthor, MessageRow } from "./message-row";
import { PaneResizer, usePaneWidth } from "./pane-resizer";
import { useEscape } from "./panels";
import { QuotedBlock, QuotePreview, quoteProps, useQuote } from "./quote";
import { RelativeTime } from "./relative-time";
import { EmptyState, InfoNote, WindowHeader } from "./settings-shell";
import { BTN_SECONDARY, CHUNKY, NAV_OFF, NAV_ON, PANEL, SIDEBAR } from "./styles";
import { SpokenMark, VoiceField } from "./voice-ui";
import { AgentStatus, Shimmer } from "./work-ui";
import { LocalPathContext } from "./local-path";
import { RunCommandContext, useRunCommand, type RunCommand } from "./run-command";

const CHATS = new Map<string, QuickChat>();
let LIST: QuickBrief[] | null = null;

export const QUICK_CHAT_EVENT = "world:open-quick";

export function openQuickChat(detail: { agentId?: string; sessionId?: string } = {}) {
  window.dispatchEvent(new CustomEvent(QUICK_CHAT_EVENT, { detail }));
}

export function QuickChatWindow({
  agents,
  mainAgentId,
  meId,
  initialAgentId,
  initialSessionId,
  onOpenAgent,
  onBeginNavigation,
  onNavigate,
  onClose,
  call,
  onCall,
  onHangUp,
  onCallSlot,
}: {
  agents: WorldAgent[];
  mainAgentId: string | null;
  meId: string | null;
  initialAgentId?: string | null;
  initialSessionId?: string | null;
  onOpenAgent: (id: string) => void;
  onBeginNavigation: () => NavigationRequest;
  onNavigate: (select: () => void, request?: NavigationRequest) => void;
  onClose: () => void;
  call: { quickId: string | null; minimised: boolean };
  onCall: (agentId: string, sessionId: string) => void;
  onHangUp: () => void;
  onCallSlot: (el: HTMLElement, on: boolean) => void;
}) {
  useEscape(onClose);
  const [agentId, setAgentId] = useState(() => initialAgentId ?? mainAgentId ?? agents[0]?.id ?? "");
  const [sessionId, setSessionId] = useState<string | null>(initialSessionId ?? null);
  const [list, setList] = useState<QuickBrief[] | null>(LIST);
  const [chat, setChat] = useState<QuickChat | null>(() => (initialSessionId ? CHATS.get(initialSessionId) ?? null : null));
  const [loadFailed, setLoadFailed] = useState(false);
  const sessionRef = useRef(sessionId);
  sessionRef.current = sessionId;
  const reads = useRef(latestOnly());
  const pane = usePaneWidth("quick.list", 17, 13, 30);
  const handles = useAgentHandles(agents);

  const loadList = useCallback(() => {
    void worldApi
      .quickList()
      .then((next) => {
        LIST = next;
        setList(next);
      })
      .catch(() => setList((cur) => cur ?? []));
  }, []);

  const loadChat = useCallback((id: string) => {
    const latest = reads.current.begin();
    void worldApi
      .quickRead(id)
      .then((next) => {
        if (!latest()) return;
        CHATS.set(id, next);
        if (sessionRef.current === id) {
          setChat(next);
          setLoadFailed(false);
        }
      })
      .catch(() => {
        if (latest() && sessionRef.current === id) setLoadFailed(true);
      });
  }, []);

  useEffect(() => loadList(), [loadList]);

  useEffect(() => {
    setLoadFailed(false);
    if (!sessionId) {
      setChat(null);
      return;
    }
    setChat(CHATS.get(sessionId) ?? null);
    loadChat(sessionId);
  }, [sessionId, loadChat]);

  useEffect(() => {
    const onEvent = (e: Event) => {
      const ev = (e as CustomEvent<WorldEvent>).detail;
      const quick = typeof ev.payload?.quick_session_id === "string" ? ev.payload.quick_session_id : null;
      const current = sessionRef.current;
      if (quick) loadList();
      if (current && (quick === current || (ev.session_id === current && ev.type.startsWith("run.")))) loadChat(current);
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [loadList, loadChat]);

  useRefetchOnResume(() => {
    loadList();
    if (sessionRef.current) loadChat(sessionRef.current);
  });

  const waiting = chatWaiting(chat?.busy);
  useEffect(() => {
    if (!sessionId || !waiting) return;
    return startRechecks(() => loadChat(sessionId));
  }, [sessionId, waiting, loadChat]);

  const chatAgent = agents.find((a) => a.id === (chat?.agent_id ?? agentId)) ?? null;
  const names = useMemo(() => new Map(agents.map((a) => [a.id, a])), [agents]);

  const leaveCallFor = (next: string | null) => {
    if (call.quickId && call.quickId !== next && !call.minimised) onHangUp();
  };

  const pickChat = (id: string) => {
    leaveCallFor(id);
    setSessionId(id);
  };

  const startNew = (id?: string) => {
    leaveCallFor(null);
    setSessionId(null);
    if (id) setAgentId(id);
  };

  const close = async (id: string) => {
    if (call.quickId === id) onHangUp();
    try {
      await worldApi.quickClose(id);
    } catch {
      return;
    }
    CHATS.delete(id);
    if (sessionRef.current === id) setSessionId(null);
    loadList();
  };

  useEffect(() => {
    if (chat?.status === "archived" && call.quickId === chat.id) onHangUp();
  }, [chat?.status, chat?.id, call.quickId, onHangUp]);

  return (
    <div role="dialog" aria-modal="true" aria-label="Quick chat" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <WindowHeader
        title="Quick chat"
        icon={Lightning}
        subtitle="Talk with an agent's memory on the side. Their work carries on, and nothing is kept unless you say so."
        actions={
          <button type="button" onClick={() => onNavigate(() => startNew())} className={BTN_SECONDARY}>
            <Plus size={14} weight="bold" />
            New chat
          </button>
        }
        onClose={onClose}
      />
      <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
        <div
          style={pane.style}
          className={cn(
            "flex max-h-[30%] shrink-0 flex-col border-b-2 border-dashed border-w-line2 sm:max-h-none sm:border-b-0 sm:border-r-2",
            pane.custom ? "sm:w-[var(--pane-w)]" : "sm:w-[28%] sm:min-w-[14rem] sm:max-w-[20rem]",
            SIDEBAR,
          )}
        >
          <ul className="flex-1 space-y-0.5 overflow-y-auto p-2">
            <li>
              <button type="button" onClick={() => onNavigate(() => startNew())} className={cn("w-full rounded-lg px-2.5 py-2 text-left text-w-sm transition-colors", sessionId === null ? NAV_ON : NAV_OFF)}>
                <span className="flex items-center gap-2 font-semibold">
                  <Plus size={14} weight="bold" />
                  New chat
                </span>
              </button>
            </li>
            {list === null && (
              <li className="px-3 py-3 text-w-sm">
                <Shimmer>Loading chats</Shimmer>
              </li>
            )}
            {list?.map((q) => {
              const a = names.get(q.agent_id);
              return (
                <li key={q.id} className="group relative">
                  <button
                    type="button"
                    onClick={() => onNavigate(() => pickChat(q.id))}
                    className={cn("w-full rounded-lg px-2.5 py-2 pr-8 text-left text-w-sm transition-colors", sessionId === q.id ? NAV_ON : NAV_OFF)}
                  >
                    <span className="block truncate font-semibold text-w-strong">{q.title || "Quick chat"}</span>
                    <span className="mt-0.5 flex min-w-0 items-center gap-1.5 text-w-xs font-normal text-w-muted">
                      {a && <AgentFace agent={a} size={14} />}
                      <span className="truncate">{a?.name ?? "Agent"}</span>
                      <span aria-hidden>·</span>
                      {q.busy ? <Shimmer className="shrink-0">thinking</Shimmer> : <RelativeTime at={q.updated_at} className="shrink-0" />}
                    </span>
                  </button>
                  <button
                    type="button"
                    onClick={() => void close(q.id)}
                    aria-label={`Close ${q.title || "this chat"}`}
                    className="absolute right-1.5 top-2 grid h-6 w-6 place-items-center rounded-md text-w-muted opacity-0 transition-opacity hover:bg-w-paper hover:text-w-text focus-visible:opacity-100 group-hover:opacity-100"
                  >
                    <X size={12} weight="bold" />
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
        <PaneResizer pane={pane} edge="right" label="Resize the chat list" className="hidden sm:block" />
        <Conversation
          key={sessionId ?? `new:${agentId}`}
          agents={agents}
          agent={chatAgent}
          agentId={agentId}
          onPickAgent={(id) => onNavigate(() => setAgentId(id))}
          chat={sessionId ? chat : null}
          sessionId={sessionId}
          loadFailed={loadFailed}
          meId={meId}
          handles={handles}
          onOpenAgent={onOpenAgent}
          calling={call.quickId !== null && call.quickId === sessionId ? (call.minimised ? "away" : "here") : null}
          onCall={() => sessionId && chatAgent && onCall(chatAgent.id, sessionId)}
          onCallSlot={onCallSlot}
          onBeginNavigation={onBeginNavigation}
          onStarted={(id, first, request) => {
            CHATS.delete(id);
            loadList();
            onNavigate(() => {
              setSessionId(id);
              if (first) setChat({ id, agent_id: agentId, title: null, backend: "", model: "", status: "active", thread_root: null, busy: true, messages: [first] });
            }, request);
          }}
          onRefresh={() => sessionId && loadChat(sessionId)}
        />
      </div>
    </div>
  );
}

function Conversation({
  agents,
  agent,
  agentId,
  onPickAgent,
  chat,
  sessionId,
  loadFailed,
  meId,
  handles,
  onOpenAgent,
  calling,
  onCall,
  onCallSlot,
  onBeginNavigation,
  onStarted,
  onRefresh,
}: {
  agents: WorldAgent[];
  agent: WorldAgent | null;
  agentId: string;
  onPickAgent: (id: string) => void;
  chat: QuickChat | null;
  sessionId: string | null;
  loadFailed: boolean;
  meId: string | null;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  calling: "here" | "away" | null;
  onCall: () => void;
  onCallSlot: (el: HTMLElement, on: boolean) => void;
  onBeginNavigation: () => NavigationRequest;
  onStarted: (id: string, first: AgentMessage | null, request: NavigationRequest) => void;
  onRefresh: () => void;
}) {
  const [draft, setDraft] = useDraft(`quick:${sessionId ?? `new:${agentId}`}`);
  const [sending, setSending] = useState(false);
  const [dictated, setDictated] = useState(false);
  const [quote, setQuote] = useQuote(sessionId ? `quick:${sessionId}` : null);
  const [error, setError] = useState<string | null>(null);
  const [answering, setAnswering] = useState<Set<string>>(new Set());
  const inputRef = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const files = useAttachments();
  const messages = useMemo(() => (chat?.messages ?? []).filter((m) => !(m.author_kind === "user" && (m.meta?.answers || m.meta?.voice_request))), [chat]);
  const slotEl = useRef<HTMLElement | null>(null);
  const callSlotRef = useCallback(
    (el: HTMLDivElement | null) => {
      if (el) {
        slotEl.current = el;
        onCallSlot(el, true);
      } else if (slotEl.current) {
        onCallSlot(slotEl.current, false);
        slotEl.current = null;
      }
    },
    [onCallSlot],
  );
  const canCall = !!sessionId && !!chat && chat.status !== "archived" && !chat.thread_root && !!agent && calling !== "here";
  const last = messages[messages.length - 1];
  const follow = useFollowBottom(endRef, last?.id, sessionId);
  const runner = useRunCommand(agent, meId, { where: agent ? (agent.runner_name ? `on ${agent.runner_name}, output goes to ${agent.name}'s main chat` : `on ${agent.name}'s device, output goes to its main chat`) : undefined });
  const thinking = chatWaiting(chat?.busy);

  useEffect(() => {
    if (!window.matchMedia("(pointer: fine)").matches) return;
    const id = window.setTimeout(() => inputRef.current?.focus({ preventScroll: true }), 60);
    return () => window.clearTimeout(id);
  }, [sessionId]);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const text = draft.trim();
    if (!text || sending || files.uploading || !agentId) return;
    setSending(true);
    setError(null);
    setDraft("");
    follow.stick();
    try {
      if (sessionId) {
        await worldApi.quickSend(sessionId, { text, attachments: files.ready, spoken: dictated || undefined, quote });
        onRefresh();
      } else {
        const request = onBeginNavigation();
        const started = await worldApi.quickStart(agentId, { text, attachments: files.ready, spoken: dictated || undefined });
        onStarted(started.session_id, started.message, request);
      }
      files.clear();
      setDictated(false);
      setQuote(null);
    } catch (err) {
      setDraft(text);
      setError(err instanceof WorldApiError ? err.message : "Not sent. Your message is back in the box, try again.");
    } finally {
      setSending(false);
    }
  };

  const answer = async (card: AgentMessage, choice: string) => {
    if (!sessionId || answering.has(card.id)) return;
    setAnswering((cur) => new Set(cur).add(card.id));
    setError(null);
    try {
      await worldApi.quickSend(sessionId, { text: choice, answers: card.id });
      onRefresh();
    } catch (err) {
      setError(err instanceof WorldApiError ? err.message : "Could not save that. Try again.");
      setAnswering((cur) => {
        const next = new Set(cur);
        next.delete(card.id);
        return next;
      });
    }
  };

  const name = agent?.name ?? "your agent";

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="shrink-0 border-b-2 border-dashed border-w-line2 px-4 py-3">
        {agent && (
          <div className="flex items-center gap-3">
            <AgentFace agent={agent} size="2.75rem" rounded="rounded-xl" />
            <div className="min-w-0 flex-1">
              <button type="button" onClick={() => onOpenAgent(agent.id)} className="max-w-full truncate text-left text-w-lg font-semibold leading-tight text-w-strong transition-colors hover:text-w-link">
                {name}
              </button>
              <p className="mt-0.5 flex min-w-0 items-center gap-1.5 text-w-xs text-w-muted">
                <AgentStatus agent={agent} />
                <span aria-hidden>·</span>
                <span className="shrink-0">{runtimeName(chat?.backend ?? agent.cli, chat?.model ?? agent.model)}</span>
              </p>
            </div>
            {canCall && (
              <button
                type="button"
                onClick={onCall}
                aria-label={calling === "away" ? `Back to the call with ${name}` : `Call ${name} from this chat`}
                title={calling === "away" ? "Back to the call" : "Call from this chat"}
                className={cn(CHUNKY, "grid h-10 w-10 shrink-0 place-items-center bg-w-teal text-w-strong hover:bg-w-teal2")}
              >
                <Phone size={17} weight="bold" />
              </button>
            )}
          </div>
        )}
        {!sessionId && agents.length > 1 && (
          <div role="radiogroup" aria-label="Agent for this chat" className="-mx-1 mt-2.5 flex items-center gap-1.5 overflow-x-auto px-1 pb-0.5">
            {agents.map((a) => {
              const on = a.id === agentId;
              return (
                <button
                  key={a.id}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  onClick={() => onPickAgent(a.id)}
                  title={`Chat with ${a.name}`}
                  className={cn(
                    "inline-flex shrink-0 items-center gap-1.5 rounded-lg border-2 py-1 pl-1 pr-2 text-w-xs font-semibold transition-colors",
                    on ? "border-w-teal bg-w-teal/15 text-w-strong" : "border-w-line2 text-w-text2 hover:border-w-ink hover:text-w-strong",
                  )}
                >
                  <AgentFace agent={a} size={20} />
                  {a.name}
                </button>
              );
            })}
          </div>
        )}
      </div>
      {calling === "here" && <div ref={callSlotRef} className="shrink-0" />}
      <div className="relative min-h-0 flex-1 overflow-y-auto pb-2 pt-3">
        <InfoNote className="mx-4 mb-3">
          {`Starts from ${name}'s memory. ${name} keeps working on everything else, and nothing here is remembered unless you tap Remember.`}
        </InfoNote>
        {!sessionId && agents.length === 0 && (
          <EmptyState className="mx-4" icon={Lightning} title="No agents to chat with yet" description="Create an agent first, then come back for a quick chat." />
        )}
        {sessionId && !chat && (
          <p className="px-4 text-w-sm text-w-muted">{loadFailed ? "Could not load this chat." : <Shimmer>Loading the chat</Shimmer>}</p>
        )}
        {messages.map((m) => (
          <QuickRow key={m.id} message={m} sessionId={sessionId} agent={agent} meId={meId} runner={m.author_kind === "agent" ? runner : null} handles={handles} onOpenAgent={onOpenAgent} busy={answering.has(m.id)} onAnswer={(choice) => void answer(m, choice)} />
        ))}
        {thinking && (
          <p className="px-4 py-2 pl-[4.25rem] text-w-sm">
            <Shimmer>{`${name} is thinking`}</Shimmer>
          </p>
        )}
        <NewMessagesPill show={follow.unseen} away={follow.away} onClick={follow.jump} />
        <div ref={endRef} />
      </div>
      {error && <p role="alert" className="mx-4 mb-2 rounded-lg border-2 border-w-danger bg-w-rose p-2 text-w-sm text-w-danger">{error}</p>}
      {chat?.status === "archived" ? (
        <p className="shrink-0 px-4 pb-4 text-w-sm text-w-muted">This chat is closed. Start a new one to keep talking.</p>
      ) : (
        <div className="shrink-0 px-3 pb-3 pt-1" {...files.dropProps}>
          <div className="relative rounded-xl border-2 border-w-line2 bg-w-card transition-colors focus-within:border-w-teal focus-within:ring-2 focus-within:ring-w-teal/25">
            {quote && <QuotePreview quote={quote} onClear={() => setQuote(null)} />}
            <PendingStrip pending={files.pending} onRemove={files.remove} />
            <form onSubmit={submit} className="flex items-end gap-2 p-2">
              <AttachButton onFiles={files.add} />
              <VoiceField
                composer
                inputRef={inputRef}
                value={draft}
                onChange={(v) => {
                  setDraft(v);
                  if (!v) setDictated(false);
                }}
                onDictated={() => setDictated(true)}
                placeholder={sessionId ? `Message ${name}` : `Ask ${name} anything`}
                ariaLabel="Message"
                className="[&_textarea]:border-0 [&_textarea]:bg-transparent [&_textarea]:!shadow-none [&_textarea]:!ring-0"
              />
              <button
                type="submit"
                disabled={sending || files.uploading || !draft.trim() || !agentId}
                className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-base font-semibold text-w-strong hover:bg-w-teal2")}
              >
                Send
              </button>
            </form>
          </div>
        </div>
      )}
    </div>
  );
}

function QuickRow({
  message: m,
  sessionId,
  agent,
  meId,
  runner,
  handles,
  onOpenAgent,
  busy,
  onAnswer,
}: {
  message: AgentMessage;
  sessionId: string | null;
  agent: WorldAgent | null;
  meId: string | null;
  runner: RunCommand | null;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  busy: boolean;
  onAnswer: (choice: string) => void;
}) {
  if (m.author_kind === "system") {
    return (
      <p className="my-2 flex items-center gap-2 px-4 text-w-xs text-w-muted">
        <BookmarkSimple size={13} weight="bold" className="shrink-0 text-w-link" />
        {m.body}
      </p>
    );
  }
  const you = m.author_kind === "user";
  const label = you ? (meId && m.author_user_id === meId ? "You" : "Someone") : agent?.name ?? "Agent";
  const avatar = !you && agent ? (
    <AgentFace agent={agent} size={36} />
  ) : label === "You" ? (
    <span aria-hidden className="grid h-9 w-9 shrink-0 place-items-center rounded-md bg-w-teal text-w-strong">
      <User size={18} weight="bold" />
    </span>
  ) : (
    <PersonAvatar id={m.author_user_id ?? "someone"} name={label} size={36} soft />
  );
  const memory = m.meta?.memory;
  return (
    <MessageRow
      flat
      createdAt={m.created_at}
      avatar={avatar}
      header={
        <MessageAuthor name={label} agent={!you} createdAt={m.created_at} onOpen={!you && agent ? () => onOpenAgent(agent.id) : undefined}>
          {(m.meta?.spoken || m.meta?.kind === "voice") && <SpokenMark />}
        </MessageAuthor>
      }
      actions={{ copy: m.body }}
    >
      {memory ? (
        <MemoryOffer line={memory.line} answered={m.meta?.memory_answered ?? null} busy={busy} onAnswer={onAnswer} />
      ) : (
        <>
          {m.meta?.quote && <QuotedBlock quote={m.meta.quote} />}
          <div {...(sessionId ? quoteProps(`msg:${m.id}`, you ? "Your message" : `${label}'s message`, `quick:${sessionId}`) : {})}>
            <LocalPathContext.Provider value={agent}>
              <RunCommandContext.Provider value={runner}>
                <RichText text={m.body} handles={handles} onOpenAgent={onOpenAgent} className={cn("select-text", m.meta?.failed ? "text-w-danger" : "text-w-text")} />
              </RunCommandContext.Provider>
            </LocalPathContext.Provider>
          </div>
        </>
      )}
      <AttachmentGrid items={m.meta?.attachments ?? []} />
    </MessageRow>
  );
}

export function MemoryOffer({ line, answered, busy, onAnswer }: { line: string; answered: string | null; busy: boolean; onAnswer: (choice: string) => void }) {
  return (
    <div className="mt-1 rounded-lg border-2 border-dashed border-w-line2 bg-w-sand/40 px-3 py-2">
      <p className="flex items-center gap-1.5 text-w-xs font-semibold text-w-muted">
        <BookmarkSimple size={13} weight="bold" />
        {answered === "kept" ? "Remembered" : answered === "skipped" ? "Not kept" : "Remember this?"}
      </p>
      <p className="mt-1 select-text text-w-sm text-w-text">{line}</p>
      {!answered && !busy && <AskChoices choices={[QUICK_KEEP, QUICK_SKIP]} answered={false} onAnswer={onAnswer} />}
      {busy && !answered && <Shimmer className="mt-2 block text-w-xs">Saving</Shimmer>}
    </div>
  );
}
