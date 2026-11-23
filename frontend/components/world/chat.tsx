"use client";

import { Fragment, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { ArrowLeft, CaretRight, ChatsCircle, Hash, PencilSimple, Plus, Star, UserCircle, UsersThree, X } from "@phosphor-icons/react";
import { latestOnly } from "@/lib/latest";
import { conversationIsForeground, conversationIsForegroundNow, guardedDetailLoader, type ConversationForeground, type NavigationRequest } from "@/lib/exact-world-navigation";
import { navigationDetails } from "@/lib/navigation-details";
import { useTeams } from "@/lib/teams";
import { cn } from "@/lib/utils";
import { Shimmer, StatusMark } from "./work-ui";
import {
  matchesQuery,
  worldApi,
  type ChannelDetail,
  type ChannelMessage,
  type ChannelSummary,
  type Person,
  type WorldAgent,
  type WorldEvent,
} from "@/lib/world";
import { BTN_SECONDARY, CHUNKY, FOCUS, INPUT, NAV_GLIDE, NAV_HEADING, NAV_ITEM, NAV_OFF, NAV_ON, NAV_PILL, PANEL, SIDEBAR, UNREAD } from "./styles";
import Glide from "./glide";
import { Keycap } from "./keycap";
import { WSelect } from "./controls";
import { FilterField } from "./filter-field";
import { PushToTalkKeys, SpokenMark, VoiceField } from "./voice-ui";
import { expectSpokenReply } from "./speech";
import { useDraft, useDraftKeys } from "./drafts";
import { channelRef, useMessageJump } from "./message-links";
import { QuotedBlock, QuotePreview, quoteProps, useQuote } from "./quote";
import { AttachButton, AttachmentGrid, DayDivider, MessageEditor, NewMessagesPill, PendingStrip, RichText, useAgentHandles, useAttachments, useFollowBottom, usePersonHandles, useRefetchOnResume } from "./chat-kit";
import { AgentAvatar, AgentFace, PersonAvatar, portraitOf, startsGroup, useArtVersion } from "./channel-ui";
import { mentionHandle, useMentionPicker, type MentionCandidate } from "./mention-picker";
import { EmptyState, GHOST_ICON_BTN, WindowHeader } from "./settings-shell";
import { escapeTaken } from "@/lib/escape";
import { TypingLine } from "./typing";
import { ChannelSeen } from "./seen";
import { RepliesButton, ThreadPanel, type ThreadItem } from "./thread-panel";
import { MessageAuthor, MessageRow } from "./message-row";
import { PaneResizer, usePaneWidth } from "./pane-resizer";
import { RunCommandContext, runCommandFor, type RunCommand } from "./run-command";

const SECTIONS_KEY = "world.chat.sections";

function ChatSection({ id, title, count, unread = 0, collapsed, onToggle, action, children }: { id: string; title: string; count: number; unread?: number; collapsed: boolean; onToggle: (id: string) => void; action?: ReactNode; children: ReactNode }) {
  return (
    <div className="mb-3">
      <div className={cn(NAV_HEADING, "justify-between pr-1")}>
        <button type="button" onClick={() => onToggle(id)} aria-expanded={!collapsed} className="-ml-1 flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-1 py-0.5 text-left transition-colors hover:text-w-text">
          <CaretRight size={10} weight="bold" className={cn("shrink-0 transition-transform motion-reduce:transition-none", !collapsed && "rotate-90")} />
          <span className="truncate">{title}</span>
          {collapsed && count > 0 && unread === 0 && <span className="tabular-nums text-w-faint">{count}</span>}
        </button>
        {collapsed && unread > 0 && <span className={UNREAD}>{unread}</span>}
        {action}
      </div>
      {!collapsed && children}
    </div>
  );
}

const chatCache: { channels: ChannelSummary[]; people: Person[] } = { channels: [], people: [] };

function handleOf(name: string): string {
  return name.replace(/[^A-Za-z0-9]/g, "").toLowerCase();
}

function personName(p: Person): string {
  return p.name || p.email?.split("@")[0] || "someone";
}

export function ChatPanel({
  agents,
  canPost = true,
  initialChannel,
  initialAgent = null,
  talkable = [],
  mainId = null,
  renderAgent,
  onClose,
  onOpenAgent,
  onUnreadChange,
  meId,
  agentUnread = {},
  focusAgent = null,
  focusChannel = null,
  onSelect,
  onNavigate,
  onBeginNavigation,
  foreground: rootForeground,
  onClearMessageFocus,
  compact = false,
}: {
  foreground: ConversationForeground;
  onClearMessageFocus?: () => void;
  compact?: boolean;
  meId: string | null;
  agents: WorldAgent[];
  canPost?: boolean;
  initialChannel?: string | null;
  initialAgent?: string | null;
  talkable?: WorldAgent[];
  mainId?: string | null;
  renderAgent?: (agent: WorldAgent, nav: { back: () => void; open: (id: string) => void; foreground: ConversationForeground }) => ReactNode;
  onClose: () => void;
  onOpenAgent: (id: string) => void;
  onUnreadChange: () => void;
  agentUnread?: Record<string, number>;
  focusAgent?: { id: string; nonce: number } | null;
  focusChannel?: { id: string; nonce: number; thread?: { root: number; reply: number }; message?: ChannelMessage; detail?: ChannelDetail } | null;
  onSelect?: (selection: { agentId: string | null; channelId: string | null }) => void;
  onNavigate?: (target: { kind: "channel" | "chat-agent" | "chat-person"; id: string; loaded?: boolean; detail?: ChannelDetail }, request?: NavigationRequest) => void;
  onBeginNavigation?: () => NavigationRequest;
}) {
  const [channels, setChannelState] = useState<ChannelSummary[]>(() => chatCache.channels);
  const [people, setPeopleState] = useState<Person[]>(() => chatCache.people);
  const setChannels = useCallback((list: ChannelSummary[]) => {
    chatCache.channels = list;
    setChannelState(list);
  }, []);
  const setPeople = useCallback((list: Person[]) => {
    chatCache.people = list;
    setPeopleState(list);
  }, []);
  const [current, setCurrentChannel] = useState<string | null>(initialChannel ?? null);
  const [agentId, setAgentId] = useState<string | null>(initialChannel ? null : initialAgent);
  const selectionVersion = useRef(0);
  const activeChannel = useRef<string | null>(null);
  activeChannel.current = agentId ? null : current;
  const [listOpen, setListOpen] = useState(false);
  const narrow = useSyncExternalStore(
    useCallback((notify) => {
      const query = window.matchMedia("not all and (min-width: 640px)");
      query.addEventListener("change", notify);
      return () => query.removeEventListener("change", notify);
    }, []),
    () => window.matchMedia("not all and (min-width: 640px)").matches,
    () => false,
  );
  const mobile = compact || narrow;
  const conversationForeground = { ...rootForeground, mobileList: mobile && listOpen };
  const conversationVisible = conversationIsForeground(conversationForeground, "chat");
  const foreground = useRef(conversationForeground);
  foreground.current = conversationForeground;
  const wasForeground = useRef(conversationVisible);
  const backToChats = useCallback(() => {
    onBeginNavigation?.();
    setListOpen(true);
  }, [onBeginNavigation]);
  const listPane = usePaneWidth("chat.list", 18, 12, 30);
  const threadPane = usePaneWidth("chat.thread", 26, 18, 44);
  const setCurrent = useCallback((id: string | null) => {
    selectionVersion.current++;
    setCurrentChannel(id);
    setAgentId(null);
    setListOpen(false);
  }, []);
  const openAgentChat = useCallback(
    (id: string) => {
      if (!talkable.some((a) => a.id === id)) return onOpenAgent(id);
      selectionVersion.current++;
      setAgentId(id);
      setListOpen(false);
    },
    [talkable, onOpenAgent],
  );
  const selectAgent = (id: string) => {
    if (onNavigate && talkable.some((agent) => agent.id === id)) onNavigate({ kind: "chat-agent", id });
    else openAgentChat(id);
  };
  const focusedNonce = useRef(focusAgent?.nonce);
  useEffect(() => {
    if (!focusAgent || focusAgent.nonce === focusedNonce.current) return;
    focusedNonce.current = focusAgent.nonce;
    openAgentChat(focusAgent.id);
  }, [focusAgent, openAgentChat]);
  const unreadRef = useRef(onUnreadChange);
  unreadRef.current = onUnreadChange;
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  useEffect(() => {
    onSelectRef.current?.({ agentId, channelId: agentId ? null : current });
  }, [agentId, current]);
  const selectedAgent = agentId ? talkable.find((a) => a.id === agentId) ?? null : null;
  const [channelDetail, setDetail] = useState<ChannelDetail | null>(focusChannel?.detail ?? null);
  const focusChannelRef = useRef(focusChannel);
  focusChannelRef.current = focusChannel;
  const usedDetail = useRef<number | undefined>(undefined);
  const [channelMessages, setMessages] = useState<ChannelMessage[]>([]);
  const [deletedMessageKeys, setDeletedMessageKeys] = useState<Set<string>>(() => new Set());
  const detail = channelDetail?.id === current ? channelDetail : null;
  const loadedMessages = useMemo(
    () => channelMessages.filter((m) => m.channel_id === current && !deletedMessageKeys.has(`${m.channel_id}:${m.id}`)),
    [channelMessages, current, deletedMessageKeys],
  );
  const focusedSnapshot = focusChannel && focusChannel.id === detail?.id ? focusChannel.message : undefined;
  const focusedMessage = focusedSnapshot && !deletedMessageKeys.has(`${focusedSnapshot.channel_id}:${focusedSnapshot.id}`) ? focusedSnapshot : undefined;
  const messages = useMemo(() => {
    if (!focusedMessage || loadedMessages.some((message) => message.id === focusedMessage.id)) return loadedMessages;
    return [...loadedMessages, focusedMessage].sort((a, b) => a.id - b.id);
  }, [focusedMessage, loadedMessages]);
  const [draft, setDraft] = useDraft(`channel:${current ?? "none"}`);
  const [creating, setCreating] = useState(false);
  const [picking, setPicking] = useState(false);
  const [newName, setNewName] = useState("");
  const [newTopic, setNewTopic] = useState("");
  const [adding, setAdding] = useState("");
  const [editing, setEditing] = useState<number | null>(null);
  const [thread, setThread] = useState<{ channel: string; root: number } | null>(() =>
    focusChannel?.thread && focusChannel.id === initialChannel ? { channel: focusChannel.id, root: focusChannel.thread.root } : null,
  );
  const openLocalThread = (message: ChannelMessage) => {
    onClearMessageFocus?.();
    setThread({ channel: message.channel_id, root: message.id });
  };
  const closeThread = () => {
    setThread(null);
    onClearMessageFocus?.();
  };
  const endRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null);

  useEffect(() => {
    if (!conversationVisible) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !escapeTaken(e) && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [conversationVisible, onClose]);

  const [channelLists] = useState(latestOnly);
  const [channelReads] = useState(latestOnly);
  const [channelCreates] = useState(latestOnly);
  const channelDetailReads = useRef(guardedDetailLoader()).current;
  const loadChannels = useCallback(function loadChannels() {
    const isLatest = channelLists.begin();
    const selection = selectionVersion.current;
    worldApi
      .channels()
      .then((list) => {
        if (!isLatest()) return;
        if (selection !== selectionVersion.current) {
          loadChannels();
          return;
        }
        setChannels(list);
        const hasChannel = (id: string) => list.some((channel) => channel.id === id);
        if (activeChannel.current && !hasChannel(activeChannel.current)) {
          activeChannel.current = null;
          channelReads.begin();
        }
        setCurrentChannel((cur) => {
          if (cur && hasChannel(cur)) return cur;
          return list.find((c) => c.name === cur?.toLowerCase())?.id ?? list.find((c) => c.kind === "channel" && c.name === "general")?.id ?? list[0]?.id ?? null;
        });
        setThread((open) => open && hasChannel(open.channel) ? open : null);
        setDetail((detail) => detail && hasChannel(detail.id) ? detail : null);
        setMessages((messages) => messages.length && !hasChannel(messages[0].channel_id) ? [] : messages);
      })
      .catch(() => {});
  }, [channelLists, channelReads, setChannels]);

  const focusedChannelNonce = useRef(focusChannel?.nonce);
  useEffect(() => {
    if (!focusChannel || focusChannel.nonce === focusedChannelNonce.current) return;
    focusedChannelNonce.current = focusChannel.nonce;
    setCurrent(focusChannel.id);
    if (focusChannel.detail) setDetail(focusChannel.detail);
    if (!channels.some((channel) => channel.id === focusChannel.id)) loadChannels();
    setThread(focusChannel.thread ? { channel: focusChannel.id, root: focusChannel.thread.root } : null);
  }, [channels, focusChannel, loadChannels, setCurrent]);


  useEffect(() => {
    loadChannels();
    worldApi.people().then(setPeople).catch(() => {});
    return () => { channelLists.begin(); };
  }, [channelLists, loadChannels, setPeople]);

  const removeCommittedMessage = useCallback((channel: string, id: number) => {
    const key = `${channel}:${id}`;
    setDeletedMessageKeys((current) => current.has(key) ? current : new Set(current).add(key));
    setMessages((current) => current.filter((message) => message.channel_id !== channel || message.id !== id));
  }, []);

  const loadCurrent = useCallback(() => {
    if (!current || agentId || activeChannel.current !== current) return;
    const isLatest = channelReads.begin();
    const stillOpen = () => isLatest() && activeChannel.current === current;
    const detailIntent = channelDetailReads.begin(stillOpen);
    const focused = focusChannelRef.current;
    if (focused?.detail?.id === current && usedDetail.current !== focused.nonce) {
      usedDetail.current = focused.nonce;
      setDetail(focused.detail);
    } else {
      navigationDetails.channel(detailIntent, current).then((detail) => {
        if (detail && detailIntent.current()) setDetail(detail);
      }).catch(() => {});
    }
    worldApi
      .channelMessages(current)
      .then((m) => {
        if (!stillOpen()) return;
        setMessages((cur) => {
          const oldest = m[0]?.id;
          if (oldest === undefined) return m;
          const keep = cur.filter((x) => x.channel_id === current && x.id < oldest);
          return keep.length ? [...keep, ...m] : m;
        });
        const last = m[m.length - 1];
        if (last && conversationIsForegroundNow(foreground.current, "chat")) void worldApi.markRead(current, last.id).then(() => {
          loadChannels();
          unreadRef.current();
        });
      })
      .catch(() => {});
  }, [current, agentId, channelReads, channelDetailReads, loadChannels]);

  useEffect(() => {
    loadCurrent();
    return () => {
      channelReads.begin();
      channelDetailReads.invalidate();
    };
  }, [channelReads, channelDetailReads, loadCurrent]);
  useEffect(() => {
    const revealed = conversationVisible && !wasForeground.current;
    wasForeground.current = conversationVisible;
    if (revealed) loadCurrent();
  }, [conversationVisible, loadCurrent]);
  useRefetchOnResume(() => {
    loadChannels();
    loadCurrent();
  });

  useEffect(() => {
    const onEvent = (e: Event) => {
      const ev = (e as CustomEvent<WorldEvent>).detail;
      if (ev.type.startsWith("team.")) {
        loadChannels();
        loadCurrent();
        unreadRef.current();
        return;
      }
      if (ev.type === "channel.deleted") {
        const gone = Number(ev.payload.message_id);
        const channel = typeof ev.payload.channel_id === "string" ? ev.payload.channel_id : "";
        if (channel && Number.isFinite(gone)) removeCommittedMessage(channel, gone);
      }
      if (ev.type !== "channel.message" && ev.type !== "channel.edited" && ev.type !== "channel.deleted") return;
      if (ev.payload.channel_id === current) loadCurrent();
      else loadChannels();
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [current, loadCurrent, loadChannels, removeCommittedMessage]);

  const lastId = messages[messages.length - 1]?.id;
  const follow = useFollowBottom(endRef, lastId, current);

  const messagesRef = useRef(loadedMessages);
  messagesRef.current = loadedMessages;
  const loadOlder = useCallback(async () => {
    const oldest = messagesRef.current[0];
    if (!current || !oldest || oldest.channel_id !== current) return false;
    const page = await worldApi.channelMessagesBefore(current, oldest.id);
    if (!page.length || activeChannel.current !== current) return false;
    setMessages((cur) => {
      const seen = new Set(cur.map((m) => m.id));
      return [...page.filter((m) => !seen.has(m.id)), ...cur];
    });
    return true;
  }, [current]);
  const messageIds = useMemo(() => (agentId ? [] : messages.filter((m) => m.channel_id === current).map((m) => channelRef(m.id))), [messages, current, agentId]);
  const highlighted = useMessageJump({ channelId: !conversationVisible || agentId ? null : current }, messageIds, loadOlder);

  useEffect(() => {
    if (!current || !detail || window.matchMedia("(pointer: coarse)").matches) return;
    const active = document.activeElement as HTMLElement | null;
    if (active && active !== document.body && active.closest("textarea, [contenteditable=true]")) return;
    inputRef.current?.focus({ preventScroll: true });
  }, [current, detail?.id]);

  const [trash, setTrash] = useState<{ channel: string; id: number } | null>(null);
  const [trashError, setTrashError] = useState<string | null>(null);
  const trashRef = useRef(trash);
  trashRef.current = trash;
  const commitDelete = useCallback((item: { channel: string; id: number }) => {
    worldApi
      .deleteChannelMessage(item.channel, item.id)
      .then(() => removeCommittedMessage(item.channel, item.id))
      .catch((e: Error) => setTrashError(e.message === "409" ? "That message has replies now, so it stays." : "Could not delete that message. Try again."));
  }, [removeCommittedMessage]);
  const removeMessage = (channel: string, id: number) => {
    if (trashRef.current) commitDelete(trashRef.current);
    setTrashError(null);
    setTrash({ channel, id });
  };
  useEffect(() => {
    if (!trash) return;
    const t = window.setTimeout(() => {
      commitDelete(trash);
      setTrash(null);
    }, 5000);
    return () => window.clearTimeout(t);
  }, [trash, commitDelete]);
  useEffect(
    () => () => {
      if (trashRef.current) commitDelete(trashRef.current);
    },
    [commitDelete],
  );
  const visibleMessages = useMemo(() => (trash ? messages.filter((m) => m.id !== trash.id) : messages), [messages, trash]);

  const saveEdit = async (messageId: number, text: string) => {
    if (!current) return;
    const updated = await worldApi.editChannelMessage(current, messageId, text);
    setMessages((cur) => cur.map((m) => (m.id === updated.id ? updated : m)));
    setEditing(null);
  };

  const files = useAttachments();
  const [dictated, setDictated] = useState(false);
  const [quote, setQuote] = useQuote(current ? `channel:${current}` : null);

  const send = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!current || (!draft.trim() && !files.ready.length) || files.uploading) return;
    const body = draft.trim();
    const attached = files.ready;
    setDraft("");
    files.clear();
    follow.stick();
    const spoken = dictated;
    setDictated(false);
    const quoted = quote;
    setQuote(null);
    const sent = await worldApi.postChannel(current, body, attached, undefined, spoken || undefined, quoted).catch(() => {
      setDraft(body);
      setQuote(quoted);
      return null;
    });
    if (sent && spoken) expectSpokenReply(`channel:${current}`);
    loadCurrent();
    follow.stick();
  };

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newName.trim()) return;
    const creationCurrent = channelCreates.begin();
    const request = onBeginNavigation?.();
    const ch = await worldApi.createChannel(newName.trim(), newTopic.trim()).catch(() => null);
    if (ch) {
      loadChannels();
      if (!creationCurrent() || (request && !request.current())) return;
      setCreating(false);
      setNewName("");
      setNewTopic("");
      if (onNavigate) onNavigate({ kind: "channel", id: ch.id, loaded: true, detail: ch }, request);
      else setCurrent(ch.id);
    }
  };

  const dm = async (p: Person) => {
    if (onNavigate) {
      onNavigate({ kind: "chat-person", id: p.id });
      return;
    }
    const ch = await worldApi.openDm(p.id).catch(() => null);
    if (ch) {
      loadChannels();
      setCurrent(ch.id);
    }
  };

  const addMember = async (value: string) => {
    if (!current || !value) return;
    const [kind, id] = value.split(":") as ["user" | "agent", string];
    await worldApi.addMember(current, kind, id).catch(() => {});
    setAdding("");
    loadCurrent();
  };

  const mention = (name: string) => {
    setDraft((d) => `${d}${d && !d.endsWith(" ") ? " " : ""}@${handleOf(name)} `);
    inputRef.current?.focus();
  };

  const drafted = useDraftKeys([...talkable.map((a) => `agent:${a.id}:talk`), ...channels.map((c) => `channel:${c.id}`)]);
  const draftMark = (key: string, selected: boolean) =>
    drafted.has(key) && !selected ? (
      <PencilSimple size={12} weight="bold" className="shrink-0 text-w-muted" aria-label="Unsent draft" />
    ) : null;
  const [filter, setFilter] = useState("");
  const asideRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || e.metaKey || e.ctrlKey || (e.key !== "ArrowUp" && e.key !== "ArrowDown")) return;
      const rows = [...(asideRef.current?.querySelectorAll<HTMLButtonElement>("button[data-chat-item]:not([data-picker])") ?? [])].filter((b) => b.getClientRects().length > 0);
      if (!rows.length) return;
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      const at = rows.findIndex((b) => b.dataset.current !== undefined);
      for (let i = 1; i <= rows.length; i++) {
        const next = rows[(at + step * i + rows.length * 2) % rows.length];
        if (!e.shiftKey || next.dataset.unread !== undefined) {
          next.click();
          return;
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  const [collapsed, setCollapsed] = useState<string[]>(() => {
    try {
      const saved = JSON.parse(localStorage.getItem(SECTIONS_KEY) ?? "[]");
      return Array.isArray(saved) ? saved.filter((k): k is string => typeof k === "string") : [];
    } catch {
      return [];
    }
  });
  const folded = (id: string) => !filter && collapsed.includes(id);
  const toggleSection = (id: string) =>
    setCollapsed((prev) => {
      const next = prev.includes(id) ? prev.filter((k) => k !== id) : [...prev, id];
      try {
        localStorage.setItem(SECTIONS_KEY, JSON.stringify(next));
      } catch {}
      return next;
    });
  const shownAgents = talkable.filter((a) => matchesQuery(filter, a.name, a.slug, a.expertise, ...(a.tags ?? [])));
  const agentRow = (a: WorldAgent) => {
    const on = agentId === a.id;
    const unread = !on ? agentUnread[a.id] ?? 0 : 0;
    const working = (a.jobs?.length ?? 0) > 0;
    const dot = a.status === "error" || a.status === "waiting" || working;
    const state = a.status === "error" ? "Needs a fix" : a.status === "waiting" ? "Needs you" : a.status === "thinking" ? "Thinking" : working ? `${a.jobs!.length > 1 ? `${a.jobs!.length} quests` : "On a quest"}` : "Free";
    return (
      <li key={a.id}>
        <button type="button" onClick={() => selectAgent(a.id)} title={`${a.name}, ${state.toLowerCase()}`} data-chat-item data-current={on || undefined} data-unread={unread > 0 || undefined} className={cn(NAV_ITEM, on ? NAV_ON : NAV_OFF)}>
          <span className="relative shrink-0">
            <AgentFace agent={a} size={22} />
            {dot && (
              <span className={cn("absolute -bottom-1 -right-1 grid place-items-center rounded-full p-px", on ? "bg-w-paper" : "bg-w-sand")}>
                <StatusMark status={a.status === "thinking" ? "idle" : a.status} working={working} size={10} />
              </span>
            )}
          </span>
          <span className={cn("min-w-0 flex-1 truncate", (a.status === "waiting" || unread > 0) && "font-semibold text-w-strong")}>
            {a.status === "thinking" ? <Shimmer>{a.name}</Shimmer> : a.name}
          </span>
          {draftMark(`agent:${a.id}:talk`, on)}
          {a.id === mainId && <Star size={12} weight="fill" className="shrink-0 text-w-warn" aria-label="Main agent" />}
          {unread > 0 ? (
            <span className={UNREAD} aria-label={`${unread} unread`}>{unread > 99 ? "99+" : unread}</span>
          ) : a.status === "waiting" ? (
            <span className="shrink-0 text-w-3xs font-semibold text-w-alert">Needs you</span>
          ) : null}
        </button>
      </li>
    );
  };
  const allOwnAgents = shownAgents.filter((a) => !a.owner_user_id || a.owner_user_id === meId);
  const { teams } = useTeams(!!meId);
  const teamGroups = useMemo(() => {
    const placed = new Set<string>();
    const groups: { id: string; name: string; list: WorldAgent[] }[] = [];
    for (const t of teams) {
      if (t.owner_user_id !== meId) continue;
      const ids = new Set(t.members.filter((m) => m.member_kind === "agent").map((m) => m.member_id));
      const list = allOwnAgents.filter((a) => ids.has(a.id) && !placed.has(a.id));
      if (list.length === 0) continue;
      list.forEach((a) => placed.add(a.id));
      groups.push({ id: t.id, name: t.name, list });
    }
    return { groups, placed };
  }, [teams, allOwnAgents, meId]);
  const ownAgents = allOwnAgents.filter((a) => !teamGroups.placed.has(a.id));
  const otherGroups = useMemo(() => {
    const groups = new Map<string, WorldAgent[]>();
    for (const a of shownAgents) {
      if (!a.owner_user_id || a.owner_user_id === meId) continue;
      groups.set(a.owner_user_id, [...(groups.get(a.owner_user_id) ?? []), a]);
    }
    return [...groups.entries()];
  }, [shownAgents, meId]);
  const ownerLabel = (owner: string) => {
    const person = people.find((p) => p.id === owner);
    return `${person ? personName(person) : "Someone"}'s agents`;
  };
  const artVersion = useArtVersion();
  const portraits = useMemo(() => Object.fromEntries(agents.map((a) => [a.id, portraitOf(a)])), [agents, artVersion]);
  const candidates = useMemo<MentionCandidate[]>(() => {
    const members = new Set((detail?.members ?? []).map((m) => `${m.member_kind}:${m.member_id}`));
    const list: MentionCandidate[] = [];
    for (const a of agents) {
      if (!(a.visibility === "public" || a.owner_user_id === meId || a.is_main)) continue;
      const owner = a.owner_user_id && a.owner_user_id !== meId ? people.find((p) => p.id === a.owner_user_id) : undefined;
      list.push({
        key: `agent:${a.id}`,
        handle: mentionHandle(a.slug || a.name),
        label: a.name,
        hint: a.is_main && owner ? `${personName(owner)}'s main agent` : owner ? `${personName(owner)}'s agent` : "agent",
        icon: <AgentAvatar portrait={portraits[a.id]} emoji={a.avatar_emoji} size={22} soft />,
        member: members.has(`agent:${a.id}`),
      });
    }
    for (const p of people) {
      if (p.id === meId) continue;
      list.push({
        key: `user:${p.id}`,
        handle: mentionHandle(p.name || p.email?.split("@")[0] || ""),
        label: personName(p),
        hint: "person",
        icon: <PersonAvatar id={p.id} name={personName(p)} size={22} soft />,
        member: members.has(`user:${p.id}`),
      });
    }
    if (detail?.kind === "channel" || detail?.kind === "team") {
      const at = <span className="inline-flex h-[1.375rem] w-[1.375rem] items-center justify-center rounded-md bg-w-gold/25 text-w-2xs font-bold text-w-warn">@</span>;
      list.push({ key: "here", handle: "here", label: "Everyone here", hint: "agents that are around now", icon: at });
      list.push({ key: "channel", handle: "channel", label: "The whole channel", hint: "every agent, away ones too", icon: at });
    }
    return list.filter((c) => c.handle);
  }, [agents, people, detail, meId, portraits]);
  const picker = useMentionPicker({ value: draft, onChange: setDraft, inputRef, candidates });
  const channelsOnly = channels.filter((c) => c.kind === "channel" && matchesQuery(filter, c.name, c.topic));
  const dms = channels.filter((c) => c.kind === "dm" && matchesQuery(filter, c.peers ?? "Direct message"));
  const teamChannels = channels.filter((c) => c.kind === "team" && matchesQuery(filter, c.name, c.topic));
  const memberKeys = useMemo(() => new Set((detail?.members ?? []).map((m) => `${m.member_kind}:${m.member_id}`)), [detail]);
  const agentsByHandle = useAgentHandles(agents);
  const peopleByHandle = usePersonHandles(people);
  const openPerson = (id: string) => {
    const person = people.find((p) => p.id === id);
    if (person && id !== meId) void dm(person);
  };
  const openThread = current && !agentId && thread?.channel === current ? thread.root : null;
  const runners = useMemo(() => {
    const map = new Map<string, RunCommand>();
    for (const a of agents) {
      const runner = runCommandFor(a, meId, { where: a.runner_name ? `on ${a.runner_name}, output goes to ${a.name}'s chat` : `on ${a.name}'s device, output goes to its chat` });
      if (runner) map.set(a.id, runner);
    }
    return map;
  }, [agents, meId]);
  const runnerOf = (m: ChannelMessage) => (m.author_kind === "agent" && m.author_agent_id ? runners.get(m.author_agent_id) ?? null : null);
  const threadItem = (m: ChannelMessage): ThreadItem => {
    const agent = m.author_agent_id ? agents.find((a) => a.id === m.author_agent_id) : undefined;
    const name = m.author_name ?? "Someone";
    return {
      key: String(m.id),
      name,
      agent: m.author_kind === "agent",
      avatar: m.author_kind === "agent" ? <AgentAvatar portrait={agent ? portraits[agent.id] : null} emoji={m.author_emoji} soft /> : <PersonAvatar id={m.author_user_id ?? name} name={name} soft />,
      body: m.body,
      createdAt: m.created_at,
      editedAt: m.edited_at,
      attachments: m.meta?.attachments ?? [],
      spoken: !!m.meta?.spoken,
      quote: m.meta?.quote,
      quoteSource: `msg:${channelRef(m.id)}`,
      runner: runnerOf(m),
      ...(canPost && m.author_kind === "user" && !!meId && m.author_user_id === meId
        ? {
            onEdit: async (body: string) => {
              await worldApi.editChannelMessage(m.channel_id, m.id, body);
            },
            onDelete: m.reply_to ? () => worldApi.deleteChannelMessage(m.channel_id, m.id) : undefined,
          }
        : {}),
    };
  };
  const dmTitle = (c: { id: string; peers?: string | null }) => {
    if (detail && detail.id === c.id && detail.kind === "dm") {
      const others = detail.members.map((m) => m.name);
      return others.join(", ");
    }
    return c.peers || "Direct message";
  };

  return (
    <div
      role={compact ? "region" : "dialog"}
      aria-modal={compact ? undefined : true}
      aria-label={compact ? "Channels" : "Chat"}
      className={cn(compact ? "flex h-full w-full flex-col overflow-hidden" : PANEL, !compact && "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}
    >
      {!compact && <WindowHeader title="Chat" subtitle="Channels, teams, agents and direct messages. @mention an agent to pull it in" onClose={onClose} />}
      <div className="relative flex min-h-0 flex-1">
        <aside
          ref={asideRef}
          onKeyDown={(e) => {
            if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
            const rows = [...(asideRef.current?.querySelectorAll<HTMLButtonElement>("button[data-chat-item]") ?? [])];
            const at = rows.indexOf(document.activeElement as HTMLButtonElement);
            if (at < 0) return;
            e.preventDefault();
            rows[at + (e.key === "ArrowDown" ? 1 : -1)]?.focus();
          }}
          style={listPane.style}
          className={cn(
            "min-h-0 shrink-0 flex-col overflow-y-auto border-r-2 border-dashed border-w-line2 px-2.5 pb-4 pt-3 text-w-sm",
            !compact && "sm:flex",
            !compact && (listPane.custom ? "sm:w-[var(--pane-w)]" : "sm:w-64 lg:w-72"),
            NAV_GLIDE,
            SIDEBAR,
            listOpen ? "flex w-full" : "hidden",
          )}
        >
          <Glide hover=".nav-off" className={NAV_PILL} />
          <FilterField
            value={filter}
            onChange={setFilter}
            placeholder="Find a chat"
            className="mb-4 shrink-0"
            onEnter={() => asideRef.current?.querySelector<HTMLButtonElement>("button[data-chat-item]")?.click()}
            onArrowDown={() => asideRef.current?.querySelector<HTMLButtonElement>("button[data-chat-item]")?.focus()}
          />
          <ChatSection id="channels" unread={channelsOnly.reduce((n, c) => n + c.unread, 0)} title="Channels" count={channelsOnly.length} collapsed={folded("channels")} onToggle={toggleSection} action={
            <button type="button" onClick={() => { onBeginNavigation?.(); setCreating((current) => !current); }} aria-label="New channel" title="New channel" className="grid h-6 w-6 place-items-center rounded-md transition-colors hover:bg-w-paper hover:text-w-text">
              <Plus size={13} weight="bold" />
            </button>
          }>
          {creating && (
            <form onSubmit={create} className="mb-2 space-y-1.5 px-1">
              <input className={cn(INPUT, "w-full py-1 text-w-sm")} placeholder="name" value={newName} onChange={(e) => setNewName(e.target.value)} />
              <VoiceField placeholder="topic (optional)" ariaLabel="Channel topic" value={newTopic} onChange={setNewTopic} />
              <button type="submit" className={cn(CHUNKY, "w-full bg-w-teal py-1 text-w-xs hover:bg-w-teal2 font-semibold text-w-strong")}>Create</button>
            </form>
          )}
          <ul className="space-y-0.5">
            {channelsOnly.map((c) => (
              <li key={c.id}>
                <button type="button" onClick={() => onNavigate ? onNavigate({ kind: "channel", id: c.id, loaded: true }) : setCurrent(c.id)} data-chat-item data-current={(!agentId && current === c.id) || undefined} data-unread={c.unread > 0 || undefined} className={cn(NAV_ITEM, !agentId && current === c.id ? NAV_ON : NAV_OFF)}>
                  <span className="grid h-[1.375rem] w-[1.375rem] shrink-0 place-items-center text-w-muted">
                    <Hash size={15} weight="bold" />
                  </span>
                  <span className={cn("min-w-0 flex-1 truncate", c.unread > 0 && "font-semibold text-w-strong")}>{c.name}</span>
                  {draftMark(`channel:${c.id}`, !agentId && current === c.id)}
                  {c.unread > 0 && <span className={UNREAD}>{c.unread}</span>}
                </button>
              </li>
            ))}
          </ul>
          </ChatSection>
          {teamChannels.length > 0 && (
            <ChatSection id="teams" unread={teamChannels.reduce((n, c) => n + c.unread, 0)} title="Teams" count={teamChannels.length} collapsed={folded("teams")} onToggle={toggleSection}>
            <ul className="space-y-0.5">
              {teamChannels.map((c) => (
                <li key={c.id}>
                  <button type="button" onClick={() => onNavigate ? onNavigate({ kind: "channel", id: c.id, loaded: true }) : setCurrent(c.id)} data-chat-item data-current={(!agentId && current === c.id) || undefined} data-unread={c.unread > 0 || undefined} className={cn(NAV_ITEM, !agentId && current === c.id ? NAV_ON : NAV_OFF)}>
                    <span className="grid h-[1.375rem] w-[1.375rem] shrink-0 place-items-center text-w-muted">
                      <UsersThree size={15} weight="bold" />
                    </span>
                    <span className={cn("min-w-0 flex-1 truncate", c.unread > 0 && "font-semibold text-w-strong")}>{c.name}</span>
                    {draftMark(`channel:${c.id}`, !agentId && current === c.id)}
                    {c.unread > 0 && <span className={UNREAD}>{c.unread}</span>}
                  </button>
                </li>
              ))}
            </ul>
            </ChatSection>
          )}
          {shownAgents.length > 0 && (
            <ChatSection id="agents" title="Agents" count={shownAgents.length} collapsed={folded("agents")} onToggle={toggleSection}>
              <ul className="mb-2 space-y-0.5">{ownAgents.map(agentRow)}</ul>
              {teamGroups.groups.map((g) => (
                <details key={g.id} className="group mb-3" open>
                  <summary className="flex cursor-pointer list-none items-center gap-1.5 rounded-lg px-2.5 py-1 text-w-xs font-medium text-w-muted transition-colors hover:text-w-text">
                    <CaretRight size={10} weight="bold" className="transition-transform group-open:rotate-90 motion-reduce:transition-none" />
                    <UsersThree size={13} weight="bold" className="shrink-0" />
                    <span className="min-w-0 flex-1 truncate">{g.name}</span>
                    <span className="tabular-nums">{g.list.length}</span>
                  </summary>
                  <ul className="mt-0.5 space-y-0.5">{g.list.map(agentRow)}</ul>
                </details>
              ))}
              {otherGroups.map(([owner, list]) => (
                <details key={owner} className="group mb-3" open={!!filter || list.some((a) => a.id === agentId)}>
                  <summary className="flex cursor-pointer list-none items-center gap-1.5 rounded-lg px-2.5 py-1 text-w-xs font-medium text-w-muted transition-colors hover:text-w-text">
                    <CaretRight size={10} weight="bold" className="transition-transform group-open:rotate-90 motion-reduce:transition-none" />
                    <span className="min-w-0 flex-1 truncate">{ownerLabel(owner)}</span>
                    <span className="tabular-nums">{list.length}</span>
                  </summary>
                  <ul className="mt-0.5 space-y-0.5">{list.map(agentRow)}</ul>
                </details>
              ))}
            </ChatSection>
          )}
        </aside>

        <section className={cn("min-w-0 flex-1 flex-col", listOpen ? cn("hidden", !compact && "sm:flex") : "flex")}>
          {selectedAgent && renderAgent ? (
            <div key={selectedAgent.id} className="flex min-h-0 flex-1 animate-[fade-in_160ms_ease-out] flex-col">
              {renderAgent(selectedAgent, { back: backToChats, open: selectAgent, foreground: conversationForeground })}
            </div>
          ) : detail ? (
            <>
              <header className="flex items-center gap-3 border-b-2 border-dashed border-w-line2 px-3 py-3 sm:px-5">
                <button type="button" onClick={backToChats} aria-label="Back to chats" className={cn(GHOST_ICON_BTN, !compact && "sm:hidden")}>
                  <ArrowLeft size={18} weight="bold" />
                </button>
                {detail.kind === "dm" ? (
                  <PersonAvatar id={detail.id} name={dmTitle(detail)} size={36} soft />
                ) : (
                  <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-w-aqua text-w-link ring-1 ring-inset ring-w-teal/30">
                    {detail.kind === "team" ? <UsersThree size={18} weight="duotone" /> : <Hash size={18} weight="duotone" />}
                  </span>
                )}
                <div className="min-w-0 flex-1">
                  <p className="truncate text-w-lg font-semibold leading-tight text-w-strong">{detail.kind === "dm" ? dmTitle(detail) : detail.name}</p>
                  {detail.topic ? (
                    <p className="truncate text-w-xs text-w-muted">{detail.topic}</p>
                  ) : (
                    detail.kind !== "dm" && <p className="text-w-xs text-w-faint">{detail.members.length} {detail.members.length === 1 ? "member" : "members"}</p>
                  )}
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  <div className="hidden -space-x-1.5 sm:flex">
                    {detail.members.slice(0, 6).map((m) => (
                      <button
                        key={`${m.member_kind}:${m.member_id}`}
                        type="button"
                        onClick={() => mention(m.name)}
                        title={`Mention ${m.name}`}
                        className={cn("group/face relative rounded-md hover:z-10 focus-visible:z-10", FOCUS, "focus-visible:ring-offset-0")}
                      >
                        <span className="block rounded-md ring-2 ring-w-paper transition-transform duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] group-hover/face:-translate-y-1 group-focus-visible/face:-translate-y-1 motion-reduce:transform-none">
                          {m.member_kind === "agent" ? (
                            <AgentAvatar portrait={portraits[m.member_id]} emoji={m.emoji} size={28} soft />
                          ) : (
                            <PersonAvatar id={m.member_id} name={m.name} size={28} soft />
                          )}
                        </span>
                      </button>
                    ))}
                    {detail.members.length > 6 && (
                      <span className="flex h-7 items-center rounded-md bg-w-sand px-1.5 text-w-2xs font-semibold text-w-text2 ring-2 ring-w-paper">
                        +{detail.members.length - 6}
                      </span>
                    )}
                  </div>
                  {detail.kind === "channel" && (
                    <WSelect
                      size="sm"
                      ariaLabel="Add to channel"
                      value={adding}
                      placeholder="+ Add"
                      onChange={(v) => void addMember(v)}
                      className="w-auto whitespace-nowrap"
                      options={[
                        ...agents
                          .filter((a) => !memberKeys.has(`agent:${a.id}`) && (a.visibility === "public" || a.owner_user_id === meId))
                          .map((a) => ({ value: `agent:${a.id}`, label: a.name, hint: "agent" })),
                        ...people
                          .filter((p) => !memberKeys.has(`user:${p.id}`))
                          .map((p) => ({ value: `user:${p.id}`, label: personName(p), hint: "person" })),
                      ]}
                    />
                  )}
                </div>
              </header>
              <div className="flex min-h-0 flex-1 flex-col overflow-y-auto pb-2 pt-3 [&>:first-child]:mt-auto">
                {visibleMessages.length === 0 && (
                  <div className="mx-auto mb-6 max-w-sm px-4 text-center">
                    <span className="mx-auto mb-4 grid h-14 w-14 place-items-center rounded-2xl bg-w-aqua text-w-link">
                      {detail.kind === "dm" ? <UserCircle size={28} weight="duotone" /> : <Hash size={28} weight="bold" />}
                    </span>
                    <p className="text-w-lg font-semibold text-w-strong">{detail.kind === "dm" ? `Say hi to ${dmTitle(detail)}` : `Welcome to #${detail.name}`}</p>
                    <p className="mt-1 text-w-sm text-w-muted">
                      {detail.kind === "team" ? "The team coordinates here. Your messages reach the team lead. @mention a teammate to pull them in." : "Type @ to mention a person or pull an agent in."}
                    </p>
                  </div>
                )}
                {visibleMessages.map((m, i) => {
                  const first = startsGroup(m, visibleMessages[i - 1]);
                  const agent = m.author_agent_id ? agents.find((a) => a.id === m.author_agent_id) : undefined;
                  const mine = m.author_kind === "user" && !!meId && m.author_user_id === meId;
                  const name = m.author_name ?? "Someone";
                  return (
                    <Fragment key={m.id}>
                      <div className="px-4">
                        <DayDivider at={m.created_at} previous={visibleMessages[i - 1]?.created_at} />
                      </div>
                      <MessageRow
                        id={channelRef(m.id)}
                        first={first}
                        createdAt={m.created_at}
                        highlighted={highlighted === channelRef(m.id)}
                        editing={editing === m.id}
                        className="sm:px-5"
                        avatar={
                          m.author_kind === "agent" ? (
                            <button type="button" onClick={() => agent && selectAgent(agent.id)} aria-label={`Open ${name}`}>
                              <AgentAvatar portrait={agent ? portraits[agent.id] : null} emoji={m.author_emoji} soft />
                            </button>
                          ) : (
                            <PersonAvatar id={m.author_user_id ?? name} name={name} soft />
                          )
                        }
                        header={
                          <MessageAuthor name={name} onOpen={agent ? () => selectAgent(agent.id) : undefined} agent={m.author_kind === "agent"} you={mine} createdAt={m.created_at} editedAt={m.edited_at}>
                            {m.meta?.spoken && <SpokenMark />}
                          </MessageAuthor>
                        }
                        actions={{
                          onReply: () => openLocalThread(m),
                          copy: m.body,
                          link: channelRef(m.id),
                          onEdit: canPost && mine && !!m.body ? () => setEditing(m.id) : undefined,
                          onDelete: canPost && mine && !(m.reply_count ?? 0) ? () => removeMessage(m.channel_id, m.id) : undefined,
                        }}
                      >
                        {editing === m.id ? (
                          <MessageEditor initial={m.body} onSave={(text) => saveEdit(m.id, text)} onCancel={() => setEditing(null)} />
                        ) : (
                          <>
                            {m.meta?.quote && <QuotedBlock quote={m.meta.quote} />}
                            <div {...quoteProps(`msg:${channelRef(m.id)}`, `${name}'s message${detail.kind === "dm" ? "" : ` in #${detail.name}`}`, `channel:${m.channel_id}`)}>
                              <RunCommandContext.Provider value={runnerOf(m)}>
                                <RichText text={m.body} handles={agentsByHandle} people={peopleByHandle} onOpenAgent={selectAgent} onOpenPerson={openPerson} className="select-text text-w-text" />
                              </RunCommandContext.Provider>
                            </div>
                            {!first && m.edited_at && <span className="text-w-3xs text-w-faint">(edited)</span>}
                            {!first && m.meta?.spoken && <SpokenMark className="ml-1.5" />}
                          </>
                        )}
                        <AttachmentGrid items={m.meta?.attachments ?? []} />
                        <RepliesButton count={m.reply_count ?? 0} lastAt={m.last_reply_at} authors={m.reply_authors} agents={agents} onOpen={() => openLocalThread(m)} />
                      </MessageRow>
                    </Fragment>
                  );
                })}
                <ChannelSeen messages={messages} meId={meId} agents={agents} className="px-1" />
                <NewMessagesPill show={follow.unseen} away={follow.away} onClick={follow.jump} />
                <div ref={endRef} />
              </div>
              {!canPost ? (
                <p className="border-t-2 border-dashed border-w-line2 bg-w-sand p-3 text-w-sm text-w-muted">You have read access, so you can follow along here but not post.</p>
              ) : (
                <div className="px-3 pb-3 pt-1 sm:px-5 sm:pb-4" {...files.dropProps}>
                  {(trash || trashError) && (
                    <div role="status" className={cn("mb-2 flex items-center gap-3 rounded-lg border-2 px-3 py-1.5 text-w-sm", trashError ? "border-w-danger/40 bg-w-rose text-w-danger" : "border-w-line2 bg-w-sand text-w-text2")}>
                      <span className="min-w-0 flex-1">{trashError ?? "Message deleted."}</span>
                      {trash ? (
                        <button type="button" onClick={() => setTrash(null)} className="font-semibold text-w-link hover:underline">
                          Undo
                        </button>
                      ) : (
                        <button type="button" onClick={() => setTrashError(null)} aria-label="Dismiss" className="text-w-muted hover:text-w-text">
                          <X size={14} weight="bold" />
                        </button>
                      )}
                    </div>
                  )}
                  <TypingLine channelId={detail.id} replyTo={null} />
                  <div className="relative rounded-xl border-2 border-w-line2 bg-w-card transition-colors focus-within:border-w-teal focus-within:ring-2 focus-within:ring-w-teal/25">
                    {picker.popover}
                    {quote && <QuotePreview quote={quote} onClear={() => setQuote(null)} />}
                    <PendingStrip pending={files.pending} onRemove={files.remove} />
                    <form onSubmit={send} className="flex items-end gap-2 p-2">
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
                        onKey={(e) => {
                          if (picker.onKey(e)) return true;
                          if (e.key !== "ArrowUp" || draft || e.shiftKey || e.metaKey || e.ctrlKey || e.altKey || !canPost) return false;
                          const last = [...messages].reverse().find((m) => m.author_kind === "user" && !!meId && m.author_user_id === meId && !!m.body);
                          if (!last) return false;
                          e.preventDefault();
                          setEditing(last.id);
                          return true;
                        }}
                        placeholder={detail.kind === "dm" ? `Message ${dmTitle(detail)}` : `Message #${detail.name}`}
                        ariaLabel="Message"
                        className="[&_textarea]:border-0 [&_textarea]:bg-transparent [&_textarea]:!shadow-none [&_textarea]:!ring-0"
                      />
                      <button
                        type="submit"
                        disabled={files.uploading || (!draft.trim() && !files.ready.length)}
                        className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-base hover:bg-w-teal2 font-semibold text-w-strong")}
                      >
                        Send
                      </button>
                    </form>
                  </div>
                  <p className="mt-1.5 hidden items-center gap-1.5 px-1 text-w-2xs text-w-faint sm:flex">
                    <Keycap size="sm">Enter</Keycap> send <Keycap size="sm">Shift</Keycap><Keycap size="sm">Enter</Keycap> new line <Keycap size="sm">@</Keycap> mention <PushToTalkKeys /> talk
                  </p>
                </div>
              )}
            </>
          ) : (
            <div className="grid flex-1 place-items-center p-6">
              <EmptyState
                size="lg"
                icon={ChatsCircle}
                title="Pick a chat"
                description="Talk to an agent, a channel or a teammate."
                action={
                  <button type="button" onClick={backToChats} className={cn(BTN_SECONDARY, !compact && "sm:hidden")}>
                    Show chats
                  </button>
                }
              />
            </div>
          )}
        </section>
        {openThread !== null && current && detail && !compact && <PaneResizer pane={threadPane} edge="left" label="Resize the thread" className="hidden lg:block" />}
        {openThread !== null && current && detail && (
          <ThreadPanel
            visible={conversationVisible}
            style={threadPane.style}
            key={`${current}:${openThread}`}
            title="Thread"
            subtitle={detail.kind === "dm" ? dmTitle(detail) : `#${detail.name}`}
            draftKey={`channel:${current}:thread:${openThread}`}
            load={(intent) => navigationDetails.channelReplies(intent, current, focusChannel?.thread?.root === openThread ? focusChannel.thread.reply : openThread).then((rows) => rows?.map(threadItem))}
            refreshOn={(ev) =>
              (ev.type === "channel.message" || ev.type === "channel.edited" || ev.type === "channel.deleted") &&
              ev.payload.channel_id === current &&
              (ev.payload.reply_to === openThread || ev.payload.message_id === openThread || ev.type === "channel.edited")
            }
            quoteTarget={`thread:channel:${current}:${openThread}`}
            send={async (text, spoken, attachments, quote) => {
              await worldApi.postChannel(current, text, attachments, openThread, spoken || undefined, quote);
              if (spoken) expectSpokenReply(`channel:${current}`);
              loadCurrent();
            }}
            canPost={canPost}
            handles={agentsByHandle}
            people={peopleByHandle}
            onOpenAgent={selectAgent}
            onOpenPerson={openPerson}
            candidates={candidates}
            typing={<TypingLine channelId={current} replyTo={openThread} />}
            focus={focusChannel?.thread?.root === openThread ? { id: String(focusChannel.thread.reply), nonce: focusChannel.nonce } : null}
            onClose={closeThread}
            className={cn("absolute inset-0 z-20", !compact && "lg:static lg:shrink-0 lg:border-l-2 lg:border-dashed lg:border-w-line2", !compact && (threadPane.custom ? "lg:w-[var(--pane-w)]" : "lg:w-[26rem]"))}
          />
        )}
      </div>
    </div>
  );
}
