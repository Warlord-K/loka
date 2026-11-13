"use client";

import { focusTodo } from "./todos-ui";
import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import dynamic from "next/dynamic";
import { ArrowRight, Archive, CaretDown, ArrowLeft, CaretRight, Waveform, ChatCircleDots, ClockCounterClockwise, GitFork, GearSix, MoonStars, Scroll, Sun, SunHorizon, Clock, Sword, Ear, Lightning, Pause, Play, Plus, X, House, PawPrint, Tree, MapPin, Info, Star, User as UserIcon, UsersThree, UsersFour } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { RelativeTime } from "./relative-time";
import { pollWhileVisible } from "@/lib/visible-poll";
import { clearRefusals, installRefusalBackoff } from "@/lib/refusals";
import {
  formatUsage,
  residentFor,
  tradeFor,
  TRADES,
  OPENCODE_DISABLED_NOTE,
  cliOptions,
  worldApi,
  WorldApiError,
  answeredAsks,
  type AgentMessage,
  type BackendModels,
  type TaskBrief,
  type QuestOptions,
  type WorldAgent,
  type WorldItem,
  type WorldEvent,
  type Notifications,
  type RunnerView,
  type SetupAction,
} from "@/lib/world";
import type { Opens, Progress, RegionDetail, Region, Surface } from "@/lib/loka";
import { portraitUrl } from "./characters";
import { targetKey, LokaEngine, type Target, type TimeOfDay } from "./engine";
import { arrivalFor, buildWorld, HEARTH, type World } from "./world";
import { buildRegionWorld, lookOfNpc, mapItemOf } from "./regions";
import { areaName, districtOf, districtRegion } from "./districts";
import { campusRoom, classDoor, hallRoom, skillSlug, type CampusData } from "./campus";
import { LokaFailure, lokaApi, playerLookWith, readVisited, saveVisited, useCosmetics, useSharedProgress, useWorldEvent } from "./loka-client";
import { AcademySheet, NpcTalkSheet, OfferCard, TravelSheet, type LokaSheet } from "./loka-ui";
import { Minimap, MinimapChip, readMinimapShown, saveMinimapShown } from "./minimap";
import { MusicButton, MusicDock } from "./music-ui";
import { buildRoom } from "./interior";
import { civicRoom, houseRoom, parseOpens, type CivicContext, type CivicId, type PlaceAction, type PlaceRoom } from "./places";
import { notesName, PlaceSheetView, type PlaceSheet } from "./places-ui";
import type { MapItem } from "./types";
import { AnimatePresence, MotionConfig, motion } from "framer-motion";
import { BTN_PRIMARY, BTN_SECONDARY, BTN_SECONDARY_SM, CHUNKY, FOCUS, HUD_GROUP, HUD_SEG, HUD_SEG_ON, INPUT, ITEM_CARD, PANEL, PILL_NEUTRAL, PILL_WARN, PIXEL, SCRIM, SECTION_TITLE, TAB, TABS, TAB_COUNT, TAB_OFF, TAB_ON } from "./styles";
import { Keycap } from "./keycap";
import { WorldSplash } from "./world-splash";
import { EASE_SMOOTH, MODAL_MOTION, POPOVER_MOTION, SCRIM_MOTION, scrimMotion } from "./motion";
import { WCombobox, WSelect } from "./controls";
import { expectSpokenReply, speech, takeSpokenReply, voiceFor, VOICES } from "./speech";
import { SpeakerToggle, SpeechPill, SpokenMark, useHoldToTalk, useSpeechStatus, VoiceField } from "./voice-ui";
import { HistoryPanel, NotificationsPanel, QuestBoard, type SessionFocus } from "./panels";
import { ProSheet } from "./pro-sheet";
import { openQuickChat, QUICK_CHAT_EVENT, QuickChatWindow } from "./quick-chat";
import { ListsPanel } from "./items-ui";
import { TeamsView } from "./teams-ui";
import { StarterTeamCard } from "./starter-team-ui";
import { useTeams } from "@/lib/teams";
import { REPORTS_STALE } from "@/lib/reports";
import { OPEN_CHANGELOG, primeChangelog, unseenChangelog } from "@/lib/changelog";
import { everyoneNow, ownsAgent, ScopeToggle, useEveryone } from "./scope";
import { playSound, unlockSounds, type SoundKind } from "./sounds";
import { SETUP_EVENT, useFeature } from "./setup-hint";
import { OPEN_TERMINAL_EVENT } from "./run-command";
import { ChatPanel } from "./chat";
import { engineChoice, VoiceMode } from "./voice-mode";
import { primeLiveAudio } from "./live-voice";
import { EquipCard, OmpModelsSection } from "./omp-models";
import { ContextButton, ContextMeter, useAgentContext } from "./context-control";
import { ApprovalRow, ReviewerRow, KitSection, MainAgentRow, PowersSection, ResetContextRow, TidyHistoryRow, UsageSection, RemoveAgentButton, ArchiveAgentRow, SchedulesSection, SubscriptionsSection, FeedsSection, TagChips } from "./agent-manage";
import { AgentRunnerSelector, DesktopStatusButton } from "./desktop-devices";
import { OfflineBanner } from "./offline-banner";
import { DesktopCommandPalette, takeAgentTab, useAgentTabRequests } from "./desktop-command-palette";
import { LocalPathContext } from "./local-path";
import type { OpenWorkshopDetail } from "./workbench/bridge";
import { useRecentAgentTabs } from "./desktop-recent-tabs";
import { SearchChip, TopSearch } from "./top-search";
import { pinClippedScroll } from "@/lib/scroll";
import { escapeTaken, takeEscape } from "@/lib/escape";
import { installTextScaleKeys } from "@/lib/text-scale";
import { ShortcutSheet } from "./shortcuts";
import {
  AskChoices,
  DraftQueue,
  AttachButton,
  AttachmentGrid,
  PendingStrip,
  QuestCard,
  NewMessagesPill,
  RichText,
  RunCommandContext,
  type RunCommand,
  MessageEditor,
  MessageStamp,
  CopyButton,
  DayDivider,
  TurnControls,
  AsideControls,
  useAsideChoice,
  useAgentHandles,
  useFollowBottom,
  useRefetchOnResume,
  useAttachments,
  useTurnOptions,
} from "./chat-kit";
import { RepliesButton, ThreadPanel, type ThreadItem } from "./thread-panel";
import { MessageAuthor, MessageRow, MessageToolbar, type MessageActions } from "./message-row";
import { AgentStatus, JobList, LiveJobLine, Shimmer, STATUS_LABEL, StatusMark } from "./work-ui";
import { TYPING_EVENT } from "./typing";
import { SEEN_EVENT } from "./seen";
import { ShellRunCard } from "./shell-run";
import { useDraft } from "./drafts";
import { ChangelogPanel, CreateAgentDialog, FolderWorkshop, preloadPanels, QuestRoom, ReportsPanel, TerminalPanel, UiCard, YouPanel } from "./lazy";
import { HIGHLIGHT, LinkButton, openMessage, requestJump, resolveMessage, useMessageJump, type LinkedMessage } from "./message-links";
import { draftsFrom, MilestoneDraftEditor, MilestoneList, milestoneProgress } from "./milestones-ui";
import { AssigneeBadge, assigneeName, paceLabel, SelectionBar, StatusIcon, statusLabel, useQuestControls, type QuestViewer } from "./quest-ui";
import type { Trade } from "./types";
import { atLeast, BellButton, Hotbar, HOTBAR_ICONS, PlayerCard, Toasts, type Me, type Toast } from "./hud";
import { CLOSE_BTN, CloseButton, EmptyState, GHOST_ICON_BTN, InfoNote, SettingsGroup, WindowHeader } from "./settings-shell";
import { AgentAvatar, AgentFace, PersonAvatar, startsGroup } from "./channel-ui";
import { FindBar } from "./find-bar";
import { PaneResizer, usePaneWidth } from "./pane-resizer";
import { cliName, modelName, runtimeName } from "@/lib/model-names";

const Workbench = dynamic(() => import("./workbench/workbench").then((m) => m.Workbench), {
  ssr: false,
  loading: () => (
    <div className="fixed inset-0 z-[70] grid place-items-center bg-[#140d18] text-[#9c8a9a]">
      <p className="font-[family-name:var(--font-pixel)] text-w-lg">Opening the workshop...</p>
    </div>
  ),
});

const TIMES: { id: TimeOfDay; label: string; Icon: typeof Sun }[] = [
  { id: "clock", label: "Follow this computer's clock", Icon: Clock },
  { id: "day", label: "Day", Icon: Sun },
  { id: "dusk", label: "Dusk", Icon: SunHorizon },
  { id: "night", label: "Night", Icon: MoonStars },
];

const NOOP = () => {};
const SIDE_KEY = "world.sidechat";
const SEND_RETRY_DELAYS = [1000, 3000];

const WELCOMED_KEY = "world.welcomed";

function retryableSend(err: unknown): boolean {
  if (err instanceof TypeError) return true;
  const status = err instanceof Error ? Number.parseInt(err.message, 10) : NaN;
  return status === 408 || status === 429 || status >= 500;
}

const QUEST_APPROACHES = [
  { value: "", label: "Just do it" },
  { value: "quick", label: "Quick", hint: "small change or a question" },
  { value: "urgent", label: "Urgent", hint: "runs first, fast mode" },
  { value: "plan", label: "Plan first", hint: "you approve the plan" },
  { value: "2", label: "Best of 2", hint: "2x the cost" },
  { value: "3", label: "Best of 3", hint: "3x the cost" },
];

function approachOptions(approach: string): QuestOptions {
  if (approach === "quick" || approach === "urgent") return { pace: approach };
  if (approach === "plan") return { plan_first: true };
  if (approach === "2" || approach === "3") return { attempts: Number(approach) };
  return {};
}

const QUEST_TIME_LIMITS = [
  { value: "", label: "Standard limit" },
  { value: "30", label: "30 min" },
  { value: "120", label: "2 hours" },
  { value: "240", label: "4 hours" },
  { value: "480", label: "8 hours" },
  { value: "720", label: "12 hours" },
  { value: "1440", label: "24 hours" },
];

function LiveLine({ agent, progress }: { agent: WorldAgent; progress: string | null }) {
  const text = agent.status === "thinking" ? progress || agent.status_detail || "Thinking" : null;
  if (!text) return null;
  return (
    <div className="flex animate-job-in items-start gap-2 text-w-sm" aria-live="polite">
      <span key={text} className="line-clamp-2 animate-fade-in">
        <Shimmer>{text}</Shimmer>
      </span>
    </div>
  );
}

function bellWorthy(ev: WorldEvent, me: string | undefined): boolean {
  const p = ev.payload;
  if (p.quiet === true || p.quiet === "true") return false;
  switch (ev.type) {
    case "attention.requested":
      return !("thread_user_id" in p);
    case "message.created":
      return p.author_kind === "agent" && p.author_agent_id == null && p.ask !== true;
    case "channel.message":
      return !!me && Array.isArray(p.mentions_users) && (p.mentions_users as unknown[]).includes(me);
    case "todo.nudge":
    case "timer.done":
      return true;
    case "task.finished":
      return typeof p.error === "string" && p.error !== "";
    case "agent.budget":
    case "org.budget":
      return p.paused === true;
    case "agent.status":
      return p.status === "error";
    default:
      return false;
  }
}

function budgetPaused(agent: WorldAgent): boolean {
  return Boolean(agent.paused_reason?.startsWith("Daily budget of"));
}

function jobKinds(agent: WorldAgent): string[] {
  return (agent.jobs ?? []).map((j) => j.kind);
}

function AgentRelay({
  direction,
  other,
  message,
  handles,
  onOpenAgent,
  highlighted = false,
}: {
  direction: "from" | "to";
  other: WorldAgent | null;
  message: AgentMessage;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  highlighted?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const who = other ? other.name : "another agent";
  const preview = message.body.replace(/\s+/g, " ").trim();
  return (
    <div data-message-id={message.id} className={cn("group max-w-[min(92%,46rem)] rounded-md text-w-sm text-w-text2", highlighted && HIGHLIGHT)}>
      <div className="flex w-full min-w-0 items-center gap-1.5 rounded-md pr-1.5 transition-colors hover:bg-w-sand/70">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-1.5 py-1 pl-1.5 text-left"
        >
          <CaretRight size={11} weight="bold" className={cn("shrink-0 transition-transform duration-150", open && "rotate-90")} />
          <span className="shrink-0 font-medium text-w-link">{direction === "from" ? `From ${who}` : `To ${who}`}</span>
          {!open && <span className="min-w-0 flex-1 truncate">{preview}</span>}
        </button>
        <CopyButton text={message.body} className="shrink-0" />
        <LinkButton reference={message.id} className="shrink-0" />
        <MessageStamp createdAt={message.created_at} className="shrink-0" />
      </div>
      {open && (
        <div className="ml-4 mt-1 animate-[fade-in_150ms_ease-out] rounded-lg border-2 border-dashed border-w-line2 bg-w-card/70 px-3 py-2 text-w-base text-w-text">
          <RichText text={message.body} handles={handles} onOpenAgent={onOpenAgent} />
          {other && (
            <button type="button" onClick={() => onOpenAgent(other.id)} className="mt-1 text-w-xs text-w-link underline">
              Open {other.name}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function useEscape(onClose: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !escapeTaken(e) && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
}

function normalizeTag(value: string): string | null {
  const tag = value.trim().replace(/^#+/, "");
  return tag || null;
}

function notificationSound(e: WorldEvent, talkingTo: string | null): SoundKind | null {
  if (e.payload.quiet) return null;
  switch (e.type) {
    case "timer.done":
    case "todo.nudge":
      return null;
    case "task.finished":
      return e.payload.error ? "failed" : "done";
    case "task.merged":
      return "done";
    case "task.pr_closed":
    case "agent.status":
      return "failed";
    case "message.created":
      return e.agent_id && e.agent_id === talkingTo ? null : "notify";
    default:
      return "notify";
  }
}

function describe(e: WorldEvent, names: Map<string, string>): string | null {
  const who = (e.agent_id && names.get(e.agent_id)) || "Someone";
  const p = e.payload as Record<string, string | null | undefined>;
  switch (e.type) {
    case "task.started":
      return `${who} set off on a quest: ${p.title ?? "a task"}`;
    case "task.finished":
      if (p.error) return `${who}'s quest needs attention: ${p.error}`;
      return `${who} came back from a quest${p.pr_url ? " with a pull request" : ""}`;
    case "task.merged":
      return `${who}'s pull request was merged: ${p.title ?? "a quest"}`;
    case "task.pr_closed":
      return `${who}'s pull request was closed without merging`;
    case "task.review_feedback":
      return `${who} is working through review comments on ${p.title ?? "a quest"}`;
    case "task.ci_failed":
      return `${who} is fixing failing checks on ${p.title ?? "a quest"}`;
    case "task.plan_ready":
      return `${who} has a plan for ${p.title ?? "a quest"} waiting for your approval`;
    case "run.failed":
    case "run.timed_out":
      return `${who} ran into trouble`;
    case "agent.created":
      return `${p.name ?? who} moved into Loka`;
    case "aside.answered":
      return `${who} answered an aside`;
    case "message.created":
      return p.author_kind === "agent" && !p.onboarding ? `${p.author ?? who} said something to ${who}` : null;
    case "tick":
      return `${who} woke up for "${p.name ?? "a schedule"}"`;
    default:
      return null;
  }
}

function AgentDialog({
  agent,
  portrait,
  asides,
  onAside,
  onHistory,
  agents,
  onOpenAgent,
  onAgentUpdated,
  canInstall,
  onVoice,
  onClose,
  access,
  injected,
  onVoiceSent,
  meId,
  viewer,
  isMain,
  onMakeMain,
  embedded = false,
  compact = false,
  progress = null,
}: {
  embedded?: boolean;
  compact?: boolean;
  progress?: string | null;
  meId: string | null;
  viewer: QuestViewer | null;
  isMain: boolean;
  onMakeMain: () => Promise<void>;
  agent: WorldAgent;
  portrait: string;
  asides: Set<string>;
  onAside: (sessionId: string) => void;
  onHistory: (focus?: SessionFocus) => void;
  agents: WorldAgent[];
  onOpenAgent: (id: string) => void;
  onAgentUpdated: (agent: WorldAgent) => void;
  canInstall: boolean;
  onVoice: () => void;
  onClose: () => void;
  access: { read: boolean; use: boolean; manage: boolean };
  injected: { text: string; nonce: number; agentId: string } | null;
  onVoiceSent: (agentId: string) => void;
}) {
  const [tab, setTab] = useState<"talk" | "aside" | "work" | "settings">(() => takeAgentTab(agent.id) ?? (access.use ? "talk" : "work"));
  useAgentTabRequests(agent.id, setTab);
  const [forkArmed, setForkArmed] = useState(false);
  const [about, setAbout] = useState(false);
  const [forking, setForking] = useState(false);
  const fork = async () => {
    if (!forkArmed) {
      setForkArmed(true);
      return;
    }
    setForking(true);
    try {
      const copy = await worldApi.fork(agent.id);
      onAgentUpdated(copy);
      onOpenAgent(copy.id);
    } catch {
      setError("Could not fork them.");
    } finally {
      setForking(false);
      setForkArmed(false);
    }
  };
  const [pausing, setPausing] = useState(false);
  const togglePause = async () => {
    setPausing(true);
    try {
      onAgentUpdated(await worldApi.update(agent.id, { paused: !agent.paused_reason }));
    } catch {
      setError(agent.paused_reason ? "Could not resume them." : "Could not pause them.");
    } finally {
      setPausing(false);
    }
  };
  const [messages, setMessages] = useState<AgentMessage[]>(() => MESSAGE_CACHE.get(agent.id) ?? []);
  const [historyFor, setHistoryFor] = useState<string | null>(() => (MESSAGE_CACHE.has(agent.id) ? agent.id : null));
  const [tasks, setTasks] = useState<TaskBrief[]>(() => TASK_CACHE.get(agent.id) ?? []);
  useEffect(() => {
    if (historyFor === agent.id) MESSAGE_CACHE.set(agent.id, messages.slice(-150));
  }, [messages, historyFor, agent.id]);
  useEffect(() => {
    TASK_CACHE.set(agent.id, tasks);
  }, [tasks, agent.id]);
  const [input, setInput] = useDraft(`agent:${agent.id}:${tab === "aside" ? "aside" : "talk"}`);
  const [dictated, setDictated] = useState(false);
  const [busy, setBusy] = useState(false);
  const [answering, setAnswering] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [repo, setRepo] = useState("");
  const [steps, setSteps] = useState<string[]>([]);
  const [waitForMerge, setWaitForMerge] = useState(true);
  const [timeLimit, setTimeLimit] = useState("");
  const [approach, setApproach] = useState("");
  const [fixCi, setFixCi] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [thread, setThread] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const [outgoing, setOutgoing] = useState<{ text: string; files: number } | null>(null);
  const [waiting, setWaiting] = useState<{ after: string | undefined } | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null);
  useEscape(embedded ? NOOP : onClose);
  useEffect(() => {
    if (tab !== "talk" && tab !== "aside") return;
    if (!window.matchMedia("(pointer: fine)").matches) return;
    const focus = () => {
      const el = composerRef.current;
      if (!el) return;
      const active = document.activeElement as HTMLElement | null;
      if (active && active !== document.body && active !== el && active.tagName !== "CANVAS" && !active.closest("[cmdk-root]") && (active.tagName === "INPUT" || active.tagName === "TEXTAREA" || active.isContentEditable)) return;
      el.focus({ preventScroll: true });
    };
    const first = window.setTimeout(focus, 30);
    const again = window.setTimeout(focus, 200);
    return () => {
      window.clearTimeout(first);
      window.clearTimeout(again);
    };
  }, [agent.id, tab]);
  const lastMessageId = messages.length ? messages[messages.length - 1].id : null;
  const inTalk = tab === "talk" && access.use;
  const markSeen = useCallback(() => {
    if (document.visibilityState !== "visible") return;
    void worldApi
      .seenAgent(agent.id)
      .then(() => window.dispatchEvent(new Event("world:notes-stale")))
      .catch(() => {});
  }, [agent.id]);
  useEffect(() => {
    if (inTalk && lastMessageId) markSeen();
  }, [inTalk, lastMessageId, markSeen]);
  useEffect(() => {
    if (!inTalk) return;
    const mark = markSeen;
    const onEvent = (e: Event) => {
      const ev = (e as CustomEvent<WorldEvent>).detail;
      if (ev.type === "message.created" && ev.agent_id === agent.id) mark();
    };
    document.addEventListener("visibilitychange", mark);
    window.addEventListener("world-event", onEvent);
    return () => {
      document.removeEventListener("visibilitychange", mark);
      window.removeEventListener("world-event", onEvent);
      mark();
    };
  }, [inTalk, agent.id, markSeen]);

  const refresh = useCallback(() => {
    if (access.use)
      worldApi
        .messages(agent.id)
        .then((fresh) =>
          setMessages((cur) => {
            const oldest = fresh[0] ? Date.parse(fresh[0].created_at) : null;
            if (oldest === null) return fresh;
            const keep = cur.filter((m) => (m.agent_id === agent.id || m.author_agent_id === agent.id) && Date.parse(m.created_at) < oldest);
            return keep.length ? [...keep, ...fresh] : fresh;
          }),
        )
        .catch(() => {})
        .finally(() => setHistoryFor(agent.id));
    if (!access.read) return;
    worldApi
      .tasks()
      .then((all) => setTasks(all.filter((t) => t.assigned_agent_id === agent.id).slice(0, 12)))
      .catch(() => {});
  }, [agent.id, access.use, access.read]);

  useEffect(() => {
    refresh();
    const onEvent = (e: Event) => {
      const ev = (e as CustomEvent<WorldEvent>).detail;
      if ((ev.agent_id === agent.id && ev.type !== "agent.progress" && ev.type !== "agent.context") || (ev.type === "task.updated" && ev.payload.from_agent_id === agent.id)) refresh();
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [agent.id, refresh]);
  useRefetchOnResume(refresh);

  const lastId = messages[messages.length - 1]?.id;
  const follow = useFollowBottom(endRef, lastId, tab);

  const messagesRef = useRef(messages);
  messagesRef.current = messages;
  const loadOlder = useCallback(async () => {
    const oldest = messagesRef.current[0]?.created_at;
    if (!oldest) return false;
    const page = await worldApi.messagesBefore(agent.id, oldest);
    if (!page.length) return false;
    setMessages((cur) => {
      const seen = new Set(cur.map((m) => m.id));
      return [...page.filter((m) => !seen.has(m.id)), ...cur];
    });
    return true;
  }, [agent.id]);
  const messageIds = useMemo(() => messages.map((m) => m.id), [messages]);
  const highlighted = useMessageJump({ agentId: agent.id }, messageIds, loadOlder, (id) => {
    const m = messagesRef.current.find((x) => x.id === id);
    const aside = m && (m.session_role === "aside" || (m.session_id ? asides.has(m.session_id) : false));
    setTab(aside ? "aside" : "talk");
  });

  const saveEdit = async (message: AgentMessage, text: string) => {
    const attachments = (message.meta?.attachments ?? []).map((a) => a.id);
    await worldApi.rewind(agent.id, message.id, { body: text, attachments, options: turn.effective });
    setEditing(null);
    refresh();
  };

  const rewindTo = async (message: AgentMessage) => {
    if (!window.confirm(`Remove this message and everything after it? ${agent.name} forgets them too.`)) return;
    setError(null);
    try {
      await worldApi.rewind(agent.id, message.id);
      setInput(message.body);
      refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not rewind. Try again.");
    }
  };
  const actionsFor = (m: AgentMessage, mine: boolean): MessageActions => ({
    onReply: tab === "talk" && m.author_kind !== "system" ? () => setThread(m.id) : undefined,
    copy: m.body,
    link: m.id,
    onEdit: mine ? () => setEditing(m.id) : undefined,
    onRewind: mine ? () => void rewindTo(m) : undefined,
  });

  useEffect(() => {
    if (!injected || injected.agentId !== agent.id || !access.use) return;
    setInput((cur) => (cur ? `${cur} ${injected.text}` : injected.text));
    setDictated(true);
    setTab((t) => (t === "talk" || t === "aside" ? t : "talk"));
  }, [injected, access.use, agent.id]);

  const handles = useAgentHandles(agents);
  const files = useAttachments();
  const turn = useTurnOptions(agent);
  const asideChoice = useAsideChoice(agent);

  const deliver = async (text: string, withFiles: boolean) => {
    if (tab === "aside") {
      const res = await worldApi.aside(agent.id, text, asideChoice.effective, dictated || undefined);
      onAside(res.session_id);
      setAsking(true);
      if (dictated) onVoiceSent(agent.id);
    } else {
      await worldApi.send(agent.id, text, withFiles ? files.ready : [], turn.effective, undefined, dictated || undefined);
      if (dictated) onVoiceSent(agent.id);
    }
  };

  const answer = async (askId: string, choice: string) => {
    setError(null);
    try {
      await worldApi.send(agent.id, choice, [], turn.effective, askId);
      refresh();
      return true;
    } catch {
      setError("That did not go through. Try again.");
      return false;
    }
  };

  const send = async (e?: React.FormEvent, preset?: string) => {
    e?.preventDefault();
    const text = (preset ?? input).trim();
    if (!text && !(tab === "talk" && files.ready.length)) return;
    setBusy(true);
    setError(null);
    const talking = tab === "talk";
    const known = new Set(messagesRef.current.map((m) => m.id));
    if (talking) {
      setOutgoing({ text, files: files.ready.length });
      setWaiting({ after: lastAgentId });
      setInput("");
    }
    follow.stick();
    try {
      for (let attempt = 0; ; attempt++) {
        try {
          await deliver(text, talking);
          break;
        } catch (err) {
          if (!talking || attempt >= SEND_RETRY_DELAYS.length || !retryableSend(err)) throw err;
          await new Promise((resolve) => window.setTimeout(resolve, SEND_RETRY_DELAYS[attempt]));
          const fresh = access.use ? await worldApi.messages(agent.id).catch(() => null) : null;
          if (fresh?.some((m) => m.author_kind === "user" && !known.has(m.id) && m.body.trim() === text)) break;
        }
      }
      if (!talking) setInput((cur) => (cur.trim() === text ? "" : cur));
      setDictated(false);
      if (talking) files.clear();
      if (access.use) await worldApi.messages(agent.id).then(setMessages).catch(() => {});
      refresh();
    } catch {
      if (talking) {
        setInput((cur) => (cur.trim() ? `${text}\n${cur}` : text));
        setWaiting(null);
      }
      setError(
        tab === "aside"
          ? `${agent.name} could not be pulled aside. If you have never talked to them, say hi in Talk first.`
          : "Not sent. Your message is back in the box, press Send to try again.",
      );
    } finally {
      setOutgoing(null);
      setBusy(false);
    }
  };

  const sendNext = async (text: string) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setOutgoing({ text, files: 0 });
    setWaiting({ after: lastAgentId });
    follow.stick();
    try {
      await worldApi.send(agent.id, text, [], turn.effective);
      refresh();
    } catch {
      setWaiting(null);
      setError("That did not go through. Try again.");
    } finally {
      setOutgoing(null);
      setBusy(false);
    }
  };

  const assign = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!title.trim() || !description.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await worldApi.assign(agent.id, title.trim(), description.trim(), repo.trim() || null, draftsFrom(steps), waitForMerge && !!repo.trim(), timeLimit ? Number(timeLimit) : null, { ...approachOptions(approach), ...(fixCi && waitForMerge && repo.trim() ? { ci_feedback: true } : {}) });
      setTitle("");
      setApproach("");
      setFixCi(false);
      setDescription("");
      setRepo("");
      setSteps([]);
      refresh();
    } catch {
      setError("Could not hand over that quest.");
    } finally {
      setBusy(false);
    }
  };

  const visible = messages.filter((m) => {
    if (m.meta?.voice_request || m.meta?.hidden) return false;
    const isAside = m.session_role === "aside" || (m.session_id ? asides.has(m.session_id) : false);
    return tab === "aside" ? isAside : !isAside;
  });
  const answeredSet = answeredAsks(visible, agent.owner_user_id);
  const cardAnswers = new Map<string, string>();
  for (const m of visible) if (m.author_kind === "user" && m.author_user_id === agent.owner_user_id && m.meta?.answers && !cardAnswers.has(m.meta.answers)) cardAnswers.set(m.meta.answers, m.body);
  const ownsAgent = !!meId && agent.owner_user_id === meId;
  const runCommand = useMemo<RunCommand>(
    () => ({
      where: agent.runner_name ? `on ${agent.runner_name}` : "on this agent's device",
      who: agent.name,
      run: async (command: string, runOn?: string) => {
        try {
          await worldApi.runCommand(agent.id, command, runOn);
        } catch (err) {
          const status = err instanceof Error ? parseInt(err.message, 10) : NaN;
          if (status === 409) throw new Error(`${agent.name}'s device is offline or not set, so the command could not run. Check Devices, then try again.`);
          if (status === 403) throw new Error("Only the owner of this agent can run commands.");
          throw new Error("That did not start. Try again.");
        }
        follow.stick();
        refresh();
      },
    }),
    [agent.id, agent.name, agent.runner_name, follow, refresh],
  );
  const usage = formatUsage(agent.cost_usd, agent.tokens);
  const { context, refresh: refreshContext } = useAgentContext(agent.id, access.use, agent.status);
  const lastVisible = visible[visible.length - 1];
  const lastAgentId = [...messages].reverse().find((m) => m.agent_id === agent.id && m.author_kind === "agent")?.id;
  const asidePending = tab === "aside" && (asking || lastVisible?.author_kind === "user") && lastVisible?.author_kind !== "agent";

  useEffect(() => {
    if (lastVisible?.author_kind === "agent") setAsking(false);
  }, [lastVisible?.id, lastVisible?.author_kind]);

  useEffect(() => {
    if (agent.status === "thinking") setWaiting(null);
  }, [agent.status]);

  useEffect(() => {
    if (waiting && lastAgentId !== waiting.after) setWaiting(null);
  }, [lastAgentId, waiting]);

  useEffect(() => {
    if (!waiting) return;
    const id = window.setTimeout(() => setWaiting(null), 45000);
    return () => window.clearTimeout(id);
  }, [waiting]);

  return (
    <div
      role={embedded ? "region" : "dialog"}
      aria-modal={embedded ? undefined : true}
      aria-label={`Talk to ${agent.name}`}
      className={cn(!embedded && PANEL, "pointer-events-auto relative flex h-full w-full min-w-0 flex-col overflow-hidden @container")}
    >
      <div className={cn("shrink-0 border-b-2 border-dashed border-w-line2", embedded ? "px-3 py-3 @lg:px-5" : "px-4 py-4 @lg:px-5")}>
        <div className="flex items-center gap-3">
          {embedded && !compact && (
            <button type="button" onClick={onClose} aria-label="Back to chats" className={cn(GHOST_ICON_BTN, "sm:hidden")}>
              <ArrowLeft size={18} weight="bold" />
            </button>
          )}
          <AgentAvatar portrait={portrait} emoji={agent.avatar_emoji} size={embedded ? "2.75rem" : "3.5rem"} soft rounded="rounded-xl" />
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-center gap-2">
              <h2 className={cn("truncate font-semibold leading-tight text-w-strong", embedded ? "text-w-lg" : "text-w-xl")}>{agent.name}</h2>
              {isMain && (
                <span className={cn(PILL_WARN, "shrink-0")}>
                  <span title="Main agent" aria-label="Main agent" className="grid @md:hidden"><Star size={10} weight="fill" /></span>
                  <Star size={10} weight="fill" aria-hidden className="hidden @md:inline" />
                  <span className="hidden @md:inline">Main agent</span>
                </span>
              )}
              {agent.paused_reason && (
                <span title={`${agent.paused_reason}. Messages wait until resumed.`} className={cn(PILL_WARN, "shrink-0")}>
                  <Pause size={10} weight="fill" /> Paused
                </span>
              )}
            </div>
            <p className="mt-0.5 flex min-w-0 items-center gap-1.5 text-w-xs text-w-muted">
              <AgentStatus agent={agent} />
              <span aria-hidden className="hidden @lg:inline">·</span>
              <span className="hidden shrink-0 @lg:inline">{runtimeName(agent.cli, agent.model)}</span>
              <span aria-hidden className="hidden @2xl:inline">·</span>
              <span className="hidden max-w-40 truncate @2xl:inline" title={`Runs on ${agent.runner_name ?? "the cloud"}`}>
                {agent.runner_name ?? "Cloud"}
              </span>
              {(context || usage) && (
                <span className="ml-auto flex shrink-0 items-center gap-3">
                  <ContextMeter context={context} className="hidden @md:inline-flex" />
                  {usage && <span className="hidden tabular-nums @3xl:inline">{usage}</span>}
                </span>
              )}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-1">
            {access.use && (
              <button type="button" onClick={onVoice} aria-label="Voice chat" title="Voice chat, hands free" className={cn(CHUNKY, "grid h-10 w-10 place-items-center bg-w-teal text-w-strong hover:bg-w-teal2")}>
                <Waveform size={17} weight="bold" />
              </button>
            )}
            {!compact && (
              <button type="button" onClick={() => setAbout((v) => !v)} aria-expanded={about} aria-label="About this agent" title="About" className={cn(GHOST_ICON_BTN, about && "bg-w-sand text-w-strong")}>
                <Info size={18} weight={about ? "fill" : "regular"} />
              </button>
            )}
            {ownsAgent && !agent.archived_at && (
              <ContextButton
                agentId={agent.id}
                agentName={agent.name}
                context={context}
                onChanged={refreshContext}
              />
            )}
            {access.manage && !compact && !agent.archived_at && (
              <button
                type="button"
                disabled={pausing}
                onClick={() => void togglePause()}
                aria-label={agent.paused_reason ? `Resume ${agent.name}` : `Pause ${agent.name}`}
                title={agent.paused_reason ? `Resume ${agent.name}: work through what arrived while paused` : `Pause ${agent.name}: messages and quests wait, no tokens are spent. Work already running finishes its current step.`}
                className={cn(GHOST_ICON_BTN, "hidden @lg:grid", agent.paused_reason && "bg-w-lemon text-w-warn")}
              >
                {agent.paused_reason ? <Play size={18} weight="fill" /> : <Pause size={18} />}
              </button>
            )}
            {access.use && !compact && (
              <button
                type="button"
                disabled={forking}
                onClick={() => void fork()}
                onBlur={() => setForkArmed(false)}
                aria-label={`Fork ${agent.name}`}
                title={`Make a private copy of ${agent.name}, without keys`}
                className={cn(forkArmed ? cn(BTN_SECONDARY, "bg-w-lemon") : GHOST_ICON_BTN, "hidden @md:inline-flex @md:items-center @md:justify-center")}
              >
                <GitFork size={18} />
                {forking ? "Forking" : forkArmed ? "Fork a private copy?" : null}
              </button>
            )}
            {!embedded && <CloseButton onClose={onClose} className="ml-1" />}
          </div>
        </div>
        <AnimatePresence initial={false}>
          {about && !compact && (
            <motion.div
              key="about"
              initial={{ height: 0, opacity: 0 }}
              animate={{ height: "auto", opacity: 1, transition: { duration: 0.22, ease: EASE_SMOOTH } }}
              exit={{ height: 0, opacity: 0, transition: { duration: 0.15, ease: EASE_SMOOTH } }}
              className="overflow-hidden"
            >
              <div className={cn("space-y-2.5 pt-3", embedded ? "@lg:pl-14" : "@lg:pl-[4.25rem]")}>
                {agent.persona && <p className="max-w-3xl text-w-sm leading-relaxed text-w-text2">{agent.persona}</p>}
                <div className="flex flex-wrap items-center gap-1.5">
                  {agent.expertise && <span className="mr-1 text-w-xs text-w-muted">{agent.expertise}</span>}
                  <TagChips
                    tags={agent.tags ?? []}
                    editable={access.manage}
                    onChange={(tags) => void worldApi.update(agent.id, { tags }).then(onAgentUpdated, () => setError("Could not change tags."))}
                  />
                </div>
                <p className="flex flex-wrap items-center gap-1.5 text-w-xs @lg:hidden">
                  <span className={PILL_NEUTRAL}>{runtimeName(agent.cli, agent.model)}</span>
                  <span className={PILL_NEUTRAL}>{agent.runner_name ?? "Cloud"}</span>
                  {usage && <span className="text-w-muted">{usage}</span>}
                </p>
              </div>
            </motion.div>
          )}
        </AnimatePresence>
        {agent.status === "error" && agent.status_detail && (
          <p className="mt-3 select-text whitespace-pre-wrap break-words rounded-lg bg-w-rose px-3 py-2 text-w-xs leading-relaxed text-w-danger">{agent.status_detail}</p>
        )}
        <JobList
          agent={agent}
          trade={tradeFor(agent)}
          className={cn("mt-3", compact && "hidden", embedded ? "@lg:pl-14" : "@lg:pl-[4.25rem]")}
          onOpen={access.read ? (job) => onHistory(job.task_id ? { taskId: job.task_id } : { sessionId: job.session_id ?? null }) : undefined}
        />
      </div>

      <div className={TABS} role="tablist">
        {(
          [
            ["talk", "Talk", ChatCircleDots],
            ["aside", "Pull aside", Ear],
            ["quick", "Quick chat", Lightning],
            ["work", "Quests", Sword],
            ["history", "Session history", ClockCounterClockwise],
            ["settings", "Settings", GearSix],
          ] as const
        )
          .filter(([id]) =>
            id === "aside"
              ? access.use && tab === "aside"
              : id === "talk"
                ? access.use
                : id === "quick"
                  ? ownsAgent
                  : id === "work"
                    ? access.read
                    : id === "history"
                      ? access.use
                      : access.manage,
          )
          .map(([id, label, Icon]) =>
            id === "quick" || id === "history" ? (
              <button
                key={id}
                type="button"
                onClick={() => (id === "quick" ? openQuickChat({ agentId: agent.id }) : onHistory())}
                title={id === "quick" ? `A side chat with ${agent.name}'s memory` : `Every session and run of ${agent.name}`}
                className={cn(TAB, TAB_OFF)}
              >
                <Icon size={14} weight="bold" />
                {label}
              </button>
            ) : (
              <button key={id} type="button" role="tab" aria-selected={tab === id} onClick={() => setTab(id)} className={cn(TAB, tab === id ? TAB_ON : TAB_OFF)}>
                <Icon size={14} weight="bold" />
                {label}
              </button>
            ),
          )}
      </div>

      <div key={tab} className="min-h-[12.5rem] flex-1 overflow-y-auto px-4 pb-2 pt-4 @lg:px-5 @lg:pt-5">
        {!access.read && !access.use ? (
          <p className="text-w-md text-w-text2">You can watch {agent.name} around Loka. Ask an admin for more access to see their quests or talk to them.</p>
        ) : tab === "settings" ? (
          <div className="mx-auto max-w-3xl animate-[fade-in_180ms_ease-out] space-y-10 pb-6 motion-reduce:animate-none">
            <SettingsGroup title="Profile" description="Name, look, voice and the model it thinks with.">
              <div className="space-y-4">
                <MainAgentRow agent={agent} isMain={isMain} canMakeMain={!!meId && agent.owner_user_id === meId} onMakeMain={onMakeMain} />
                <AgentSettings key={`${agent.cli}:${agent.model}:${agent.pending_switch ? `${agent.pending_switch.cli}:${agent.pending_switch.model}` : ""}:${(agent.tags ?? []).join(",")}`} agent={agent} onUpdated={onAgentUpdated} />
              </div>
            </SettingsGroup>
            <SettingsGroup title="Access" description="What it may do without asking and who reviews its work.">
              <div className="space-y-4">
                <ApprovalRow agent={agent} canChange={access.manage} onUpdated={onAgentUpdated} />
                <ReviewerRow agent={agent} canChange={access.manage} onUpdated={onAgentUpdated} />
                <PowersSection agent={agent} onUpdated={onAgentUpdated} />
              </div>
            </SettingsGroup>
            <SettingsGroup title="Runtime" description="Where it runs and which models it can reach.">
              <div className="space-y-4">
                <AgentRunnerSelector key={`${agent.cli}:${agent.model}`} agentId={agent.id} cli={agent.cli} model={agent.model} onUpdated={onAgentUpdated} />
                <OmpModelsSection agent={agent} canChange={access.manage} />
                <KitSection agentId={agent.id} agentName={agent.name} canInstall={canInstall} />
              </div>
            </SettingsGroup>
            <SettingsGroup title="Automations" description="Schedules, events and feeds that wake it up.">
              <div className="space-y-4">
                <SchedulesSection agentId={agent.id} agentName={agent.name} />
                <SubscriptionsSection agentId={agent.id} agentName={agent.name} canChange={access.manage} />
                {access.manage && <FeedsSection agentId={agent.id} agentName={agent.name} canChange={access.manage} />}
              </div>
            </SettingsGroup>
            <SettingsGroup title="Usage" description="What it has spent and its daily limit.">
              <UsageSection agent={agent} canChange={access.manage} onUpdated={onAgentUpdated} />
            </SettingsGroup>
            <SettingsGroup title="Danger zone" description="Clean up, archive or remove this agent.">
              <div className="space-y-4">
                <TidyHistoryRow agent={agent} canTidy={access.manage} />
                <ResetContextRow agent={agent} canReset={access.manage} />
                {access.manage && <ArchiveAgentRow agent={agent} onUpdated={onAgentUpdated} />}
                <RemoveAgentButton agent={agent} onRemoved={onClose} />
              </div>
            </SettingsGroup>
          </div>
        ) : tab === "work" ? (
          <div className="animate-[fade-in_180ms_ease-out] space-y-4 motion-reduce:animate-none">
            {access.use && <form onSubmit={assign} className="space-y-3 rounded-xl border-2 border-w-line bg-w-card/70 p-4">
              <div>
                <p className="text-w-md font-semibold text-w-strong">Send {agent.name} on a quest</p>
                <p className="mt-0.5 text-w-xs text-w-muted">Say what to do and what done looks like. Add a repo to get a pull request back.</p>
              </div>
              <VoiceField placeholder="What should they do?" ariaLabel="Quest title" value={title} onChange={setTitle} />
              <VoiceField multiline placeholder="What does done look like?" ariaLabel="Quest description" value={description} onChange={setDescription} />
              <MilestoneDraftEditor steps={steps} onSteps={setSteps} waitForMerge={waitForMerge} onWaitForMerge={setWaitForMerge} hasRepo={!!repo.trim()} />
              {!!repo.trim() && waitForMerge && (
                <label className="flex items-center gap-2 text-w-xs text-w-text2">
                  <input type="checkbox" checked={fixCi} onChange={(e) => setFixCi(e.target.checked)} className="accent-w-teal" />
                  Send failing checks back to fix
                </label>
              )}
              <div className="flex flex-wrap gap-2">
                <input className={cn(INPUT, "min-w-[12rem]")} placeholder="owner/repo (optional)" value={repo} onChange={(e) => setRepo(e.target.value)} />
                <WSelect
                  ariaLabel="Time limit per run"
                  className="w-auto shrink-0 whitespace-nowrap"
                  value={timeLimit}
                  onChange={setTimeLimit}
                  options={QUEST_TIME_LIMITS}
                />
                <WSelect ariaLabel="How to approach it" className="w-auto shrink-0 whitespace-nowrap" value={approach} onChange={setApproach} options={QUEST_APPROACHES} />
                <button type="submit" disabled={busy || !title.trim() || !description.trim()} className={cn(BTN_PRIMARY, "ml-auto")}>
                  <Sword size={14} weight="bold" />
                  Hand it over
                </button>
              </div>
            </form>}
            <AgentQuestList
              tasks={tasks}
              setTasks={setTasks}
              agents={agents}
              viewer={viewer}
              canUse={access.use}
              onOpen={(id) => onHistory({ taskId: id })}
              onChanged={refresh}
            />
          </div>
        ) : visible.length || (tab === "talk" && (outgoing || waiting)) ? (
          <div className="flex flex-col" aria-live="polite">
            {visible.map((m, i) => {
              const sender = m.author_agent_id && m.author_agent_id !== agent.id ? agents.find((a) => a.id === m.author_agent_id) : null;
              const divider = <DayDivider at={m.created_at} previous={visible[i - 1]?.created_at} />;
              if (m.meta?.kind === "quest") {
                return (
                  <Fragment key={m.id}>
                  {divider}
                  <div data-message-id={m.id} className={cn("group my-2 max-w-[60rem] rounded-lg @lg:ml-12", highlighted === m.id && HIGHLIGHT)}>
                    <QuestCard
                      meta={m.meta}
                      body={m.body}
                      handles={handles}
                      onOpenAgent={onOpenAgent}
                      onOpenSession={(sessionId) => onHistory({ sessionId, taskId: m.meta?.task_id ?? null })}
                      live={(() => {
                        if (m.meta.status !== "started") return null;
                        const job = agent.jobs?.find((j) => j.task_id === m.meta?.task_id);
                        return job ? <LiveJobLine job={job} /> : null;
                      })()}
                    />
                    <div className="mt-0.5 flex items-center gap-1">
                      <MessageStamp createdAt={m.created_at} />
                      <LinkButton reference={m.id} />
                    </div>
                  </div>
                  </Fragment>
                );
              }
              if (m.meta?.kind === "shell" && m.meta.shell_run_id) {
                return (
                  <Fragment key={m.id}>
                  {divider}
                  <div data-message-id={m.id} className={cn("group my-2 max-w-[60rem] rounded-lg @lg:ml-12", highlighted === m.id && HIGHLIGHT)}>
                    <ShellRunCard meta={m.meta} body={m.body} agentName={agent.name} canRun={!!meId && agent.owner_user_id === meId} />
                    <div className="mt-0.5 flex items-center gap-1">
                      <MessageStamp createdAt={m.created_at} />
                      <LinkButton reference={m.id} />
                    </div>
                  </div>
                  </Fragment>
                );
              }
              if (m.meta?.kind === "reset") {
                return (
                  <Fragment key={m.id}>
                  {divider}
                  <div data-message-id={m.id} className={cn("my-3 flex items-center gap-2 py-1 text-w-muted", highlighted === m.id && HIGHLIGHT)}>
                    <span className="h-0 flex-1 border-t border-dashed border-w-line2" />
                    <span className="text-w-xs font-semibold">Context reset</span>
                    <MessageStamp createdAt={m.created_at} />
                    <span className="h-0 flex-1 border-t border-dashed border-w-line2" />
                  </div>
                  </Fragment>
                );
              }
              const sent = m.agent_id !== agent.id;
              if (sent || (m.author_kind === "agent" && m.author_agent_id && m.author_agent_id !== agent.id && m.meta?.kind !== "ask")) {
                const otherId = sent ? m.agent_id : m.author_agent_id;
                return (
                  <Fragment key={m.id}>
                  {divider}
                  <div className="my-1 @lg:ml-12">
                  <AgentRelay
                    direction={sent ? "to" : "from"}
                    other={agents.find((a) => a.id === otherId) ?? null}
                    message={m}
                    handles={handles}
                    onOpenAgent={onOpenAgent}
                    highlighted={highlighted === m.id}
                  />
                  </div>
                  </Fragment>
                );
              }
              const answered = answeredSet.has(m.id) || answering.has(m.id) || !ownsAgent;
              const opts = m.meta?.options;
              const mine = m.author_kind === "user" && !!meId && m.author_user_id === meId && !!m.body;
              const prev = visible[i - 1];
              const plainPrev = prev && !["quest", "shell", "reset"].includes(prev.meta?.kind ?? "") && prev.agent_id === agent.id && !(prev.author_kind === "agent" && prev.author_agent_id && prev.author_agent_id !== agent.id);
              const first = !plainPrev || startsGroup(m, prev) || m.meta?.kind === "ask";
              const speaker = m.author_kind === "agent" ? sender ?? agent : null;
              const name = speaker ? speaker.name : m.author_kind === "system" ? "Loka" : meId && m.author_user_id === meId ? "You" : "Someone";
              const pendingAsk = m.meta?.kind === "ask" && !answered;
              if (m.author_kind === "user") {
                return (
                  <Fragment key={m.id}>
                  {divider}
                  <div
                    data-message-id={m.id}
                    className={cn("group relative flex select-text flex-col items-end text-w-base leading-relaxed", first ? "mt-3" : "mt-1", highlighted === m.id && HIGHLIGHT)}
                  >
                    {first && (
                      <p className="mb-1 flex flex-wrap items-baseline justify-end gap-x-2 text-w-sm">
                        {opts && (opts.model || opts.effort || opts.fast) && (
                          <span className="font-mono text-w-2xs text-w-faint">{[opts.model, opts.effort && `${opts.effort} effort`, opts.fast && "fast"].filter(Boolean).join(" · ")}</span>
                        )}
                        <MessageStamp createdAt={m.created_at} editedAt={m.edited_at} />
                        <span className="font-semibold text-w-strong">{name}</span>
                      </p>
                    )}
                    <div className={cn("relative min-w-0 rounded-2xl rounded-tr-md border-2 border-w-line2 bg-w-teal/15 px-3.5 py-2", editing === m.id ? "w-full max-w-3xl" : "max-w-[min(85%,56rem)]")}>
                      {editing === m.id ? (
                        <MessageEditor rewind initial={m.body} onSave={(text) => saveEdit(m, text)} onCancel={() => setEditing(null)} />
                      ) : (
                        <RichText text={m.body} handles={handles} onOpenAgent={onOpenAgent} className="text-w-text" />
                      )}
                      <AttachmentGrid items={m.meta?.attachments ?? []} />
                    </div>
                    {(m.meta?.spoken || m.meta?.kind === "voice") && <SpokenMark call={m.meta?.kind === "voice"} className="mt-0.5" />}
                    {tab === "talk" && <RepliesButton count={m.reply_count ?? 0} lastAt={m.last_reply_at} authors={m.reply_authors} agents={agents} onOpen={() => setThread(m.id)} />}
                    {editing !== m.id && <MessageToolbar actions={actionsFor(m, mine)} className="right-2" />}
                  </div>
                  </Fragment>
                );
              }
              return (
                <Fragment key={m.id}>
                {divider}
                <MessageRow
                  id={m.id}
                  first={first}
                  createdAt={m.created_at}
                  highlighted={highlighted === m.id}
                  editing={editing === m.id}
                  hover={!pendingAsk}
                  className={cn(
                    "-mx-4 @lg:-mx-5 @lg:px-5",
                    pendingAsk && "bg-w-lemon/50 before:absolute before:inset-y-0 before:left-0 before:w-[3px] before:bg-w-gold",
                    m.author_kind === "system" && "text-w-muted",
                  )}
                  avatar={
                    speaker ? (
                      <button type="button" onClick={() => onOpenAgent(speaker.id)} aria-label={`Open ${speaker.name}`} className="block rounded-md">
                        <AgentFace agent={speaker} size={36} />
                      </button>
                    ) : m.author_kind === "system" ? (
                      <span className="grid h-9 w-9 place-items-center rounded-md bg-w-sand text-w-muted">
                        <Info size={18} weight="bold" />
                      </span>
                    ) : (
                      <SelfAvatar id={m.author_user_id ?? "you"} name={name} />
                    )
                  }
                  header={
                    <MessageAuthor
                      name={name}
                      onOpen={speaker ? () => onOpenAgent(speaker.id) : undefined}
                      createdAt={m.created_at}
                      editedAt={m.edited_at}
                      badges={
                        <>
                          {speaker && sender && <span className="text-w-2xs text-w-muted">to {agent.name}</span>}
                          {m.meta?.kind === "ask" && (
                            <span className={cn(answered ? PILL_NEUTRAL : PILL_WARN, "self-center")}>{!ownsAgent ? "Question for the owner" : answered ? "Asked you" : "Needs your answer"}</span>
                          )}
                        </>
                      }
                    >
                      {opts && (opts.model || opts.effort || opts.fast) && (
                        <span className="font-mono text-w-2xs text-w-faint">{[opts.model, opts.effort && `${opts.effort} effort`, opts.fast && "fast"].filter(Boolean).join(" · ")}</span>
                      )}
                    </MessageAuthor>
                  }
                  actions={actionsFor(m, mine)}
                >
                  {editing === m.id ? (
                    <MessageEditor rewind initial={m.body} onSave={(text) => saveEdit(m, text)} onCancel={() => setEditing(null)} />
                  ) : m.meta?.drafts?.length ? (
                    <RichText text={m.body.split("\n\n**Draft 1**")[0]} handles={handles} onOpenAgent={onOpenAgent} className="text-w-text" />
                  ) : (
                    <RunCommandContext.Provider value={ownsAgent && tab === "talk" && m.author_kind === "agent" ? runCommand : null}>
                      <LocalPathContext.Provider value={ownsAgent ? agent : null}>
                        <RichText text={m.body} handles={handles} onOpenAgent={onOpenAgent} className={m.author_kind === "system" ? "text-w-muted" : "text-w-text"} />
                      </LocalPathContext.Provider>
                    </RunCommandContext.Provider>
                  )}
                  <AttachmentGrid items={m.meta?.attachments ?? []} collapsed={m.meta?.media?.collapsed} />
                  {tab === "talk" && ownsAgent && m.author_kind === "agent" && m.id === lastVisible?.id && !!m.meta?.next?.length && !waiting && (
                    <div role="group" aria-label="Suggested next steps" className="mt-2 flex flex-wrap gap-1.5">
                      {m.meta.next.slice(0, 3).map((option) => (
                        <button key={option} type="button" disabled={busy} onClick={() => void sendNext(option)} className={cn(BTN_SECONDARY_SM, "h-auto min-h-8 max-w-full whitespace-normal py-1 text-left")}>
                          {option}
                        </button>
                      ))}
                    </div>
                  )}
                  {tab === "talk" && <RepliesButton count={m.reply_count ?? 0} lastAt={m.last_reply_at} authors={m.reply_authors} agents={agents} onOpen={() => setThread(m.id)} />}
                  {m.meta?.kind === "ui" && m.author_kind === "agent" && (
                    <div className="mt-2">
                      <UiCard
                        spec={m.meta.ui}
                        stateKey={`msg:${m.id}`}
                        handles={handles}
                        onOpenAgent={onOpenAgent}
                        canAnswer={ownsAgent}
                        answered={answeredSet.has(m.id)}
                        answerText={cardAnswers.get(m.id)}
                        onAnswer={(text) => answer(m.id, text)}
                      />
                    </div>
                  )}
                  {m.meta?.kind === "equip" && !!m.meta.keys?.length && access.manage && <EquipCard agentId={agent.id} agentName={agent.name} keys={m.meta.keys} />}
                  {m.meta?.kind === "ask" && !!m.meta.drafts?.length && (
                    <DraftQueue drafts={m.meta.drafts} answered={answeredSet.has(m.id) || !ownsAgent} onAnswer={(text) => answer(m.id, text)} />
                  )}
                  {m.meta?.kind === "ask" && (
                    <AskChoices
                      choices={m.meta.choices ?? []}
                      answered={answered}
                      onAnswer={(c) => {
                        setAnswering((prev) => new Set(prev).add(m.id));
                        void answer(m.id, c).then((ok) => {
                          if (!ok) setAnswering((prev) => { const next = new Set(prev); next.delete(m.id); return next; });
                        });
                      }}
                    />
                  )}
                </MessageRow>
                </Fragment>
              );
            })}
            {tab === "talk" && outgoing && (
              <div className="mt-3 flex animate-[fade-in-up_180ms_ease-out_both] flex-col items-end text-w-base leading-relaxed opacity-60 motion-reduce:animate-none">
                <p className="mb-1 flex items-baseline gap-2 text-w-sm">
                  <Shimmer className="text-w-2xs">Sending</Shimmer>
                  <span className="font-semibold text-w-strong">You</span>
                </p>
                <div className="min-w-0 max-w-[min(85%,56rem)] rounded-2xl rounded-tr-md border-2 border-w-line2 bg-w-teal/15 px-3.5 py-2">
                  <RichText text={outgoing.text || (outgoing.files ? `${outgoing.files} image${outgoing.files > 1 ? "s" : ""}` : "")} handles={handles} onOpenAgent={onOpenAgent} className="text-w-text" />
                </div>
              </div>
            )}
            {tab === "talk" && <LiveLine agent={agent} progress={progress} />}
            {tab === "talk" && waiting && agent.status !== "thinking" && (
              <LiveLine agent={{ ...agent, status: "thinking", status_detail: null }} progress={outgoing ? "Sending" : `${agent.name} got your message`} />
            )}
            {asidePending && <LiveLine agent={{ ...agent, status: "thinking", status_detail: null }} progress={`${agent.name} is thinking it over on the side`} />}
            <NewMessagesPill show={follow.unseen} away={follow.away} onClick={follow.jump} />
            <div ref={endRef} />
          </div>
        ) : tab === "aside" ? (
          <p className="text-w-md leading-relaxed text-w-text2">
            {`Ask ${agent.name} something on the side. A copy of their memory answers, and whatever they are working on keeps going.`}
          </p>
        ) : access.use && historyFor !== agent.id ? (
          <p className="text-w-md leading-relaxed text-w-text2">
            <Shimmer>Loading the conversation</Shimmer>
          </p>
        ) : null}
        {!visible.length && tab === "talk" && access.use && historyFor === agent.id && !outgoing && !waiting && (
          <div className="flex flex-wrap gap-2 motion-safe:animate-w-rise">
            {[`Hi ${agent.name}!`, "What can you help me with?", "What are you working on?"].map((starter) => (
              <button key={starter} type="button" disabled={busy} onClick={() => void send(undefined, starter)} className={cn(BTN_SECONDARY_SM, "font-medium")}>
                {starter}
              </button>
            ))}
          </div>
        )}
      </div>

      {error && <p role="alert" className="mx-4 mb-2 rounded-lg border-2 border-w-danger bg-w-rose p-2 text-w-sm text-w-danger @lg:mx-5">{error}</p>}

      {(tab === "talk" || tab === "aside") && (
        <div className="shrink-0 px-3 pb-3 pt-1 @lg:px-5 @lg:pb-4" {...(tab === "talk" ? files.dropProps : {})}>
        <div className="rounded-xl border-2 border-w-line2 bg-w-card transition-colors focus-within:border-w-teal focus-within:ring-2 focus-within:ring-w-teal/25">
        {tab === "talk" && <TurnControls agent={agent} options={turn.options} onChange={turn.update} onAgentUpdated={access.manage ? onAgentUpdated : undefined} />}
        {tab === "aside" && <AsideControls agent={agent} choice={asideChoice.choice} onChange={asideChoice.update} />}
        {tab === "talk" && <PendingStrip pending={files.pending} onRemove={files.remove} />}
        <form onSubmit={send} className="flex items-end gap-2 p-2">
          {tab === "talk" && <AttachButton onFiles={files.add} />}
          <VoiceField
            composer
            value={input}
            onChange={(v) => {
              setInput(v);
              if (!v) setDictated(false);
            }}
            onDictated={() => setDictated(true)}
            placeholder={tab === "aside" ? `Ask ${agent.name} on the side` : `Message ${agent.name}`}
            ariaLabel={tab === "aside" ? "Aside question" : "Message"}
            inputRef={composerRef}
            className="[&_textarea]:border-0 [&_textarea]:bg-transparent [&_textarea]:!shadow-none [&_textarea]:!ring-0"
          />
          <button
            type="submit"
            disabled={busy || files.uploading || (!input.trim() && !(tab === "talk" && files.ready.length))}
            className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-base hover:bg-w-teal2 font-semibold text-w-strong")}
          >
            {tab === "aside" ? "Ask" : "Send"}
          </button>
        </form>
        </div>
        </div>
      )}
      {thread && tab === "talk" && access.use && (
        <LocalPathContext.Provider value={ownsAgent ? agent : null}>
          <ThreadPanel
            key={thread}
            title="Thread"
            subtitle={`With ${agent.name}`}
            draftKey={`agent:${agent.id}:thread:${thread}`}
            waitingFor={ownsAgent ? agent.name : undefined}
            note={ownsAgent ? <InfoNote>{`Replies here come from a copy of ${agent.name}'s memory, so their main chat carries on. Nothing is kept unless you tap Remember.`}</InfoNote> : undefined}
            load={() =>
              worldApi.messageReplies(agent.id, thread).then((rows) =>
                rows.filter((m) => !(m.author_kind === "user" && m.meta?.answers && rows.some((r) => r.id === m.meta?.answers && r.meta?.memory))).map((m): ThreadItem => {
                  const speaker = m.author_kind === "agent" ? agents.find((a) => a.id === (m.author_agent_id ?? agent.id)) ?? agent : null;
                  const name = speaker ? speaker.name : m.author_kind === "system" ? "Loka" : meId && m.author_user_id === meId ? "You" : "Someone";
                  return {
                    key: m.id,
                    name,
                    agent: !!speaker,
                    avatar: speaker ? <AgentFace agent={speaker} size={36} /> : <SelfAvatar id={m.author_user_id ?? "you"} name={name} />,
                    body: m.body,
                    createdAt: m.created_at,
                    editedAt: m.edited_at,
                    attachments: m.meta?.attachments ?? [],
                    spoken: !!m.meta?.spoken,
                    call: m.meta?.kind === "voice",
                    memory: m.meta?.memory && m.meta.quick
                      ? {
                          line: m.meta.memory.line,
                          answered: m.meta.memory_answered ?? null,
                          onAnswer: async (choice) => {
                            await worldApi.quickSend(m.meta?.quick ?? "", { text: choice, answers: m.id });
                          },
                        }
                      : undefined,
                  };
                }),
              )
            }
            refreshOn={(ev) => ev.agent_id === agent.id && (ev.type === "message.created" || ev.type === "message.edited") && (ev.payload.reply_to === thread || ev.payload.message_id === thread)}
            send={async (text, spoken, attachments) => {
              await worldApi.send(agent.id, text, attachments, turn.effective, undefined, spoken || undefined, undefined, thread);
              if (spoken) onVoiceSent(agent.id);
              refresh();
            }}
            canPost={access.use}
            handles={handles}
            onOpenAgent={onOpenAgent}
            onClose={() => setThread(null)}
            className="absolute inset-0 z-20 @3xl:left-auto @3xl:w-[26rem] @3xl:border-l-2 @3xl:border-dashed @3xl:border-w-line2 @3xl:shadow-[-8px_0_24px_rgb(var(--w-ink)/0.12)]"
          />
        </LocalPathContext.Provider>
      )}
    </div>
  );
}

function AgentSettings({ agent, onUpdated }: { agent: WorldAgent; onUpdated: (agent: WorldAgent) => void }) {
  const [backends, setBackends] = useState<Record<string, BackendModels>>({});
  const [name, setName] = useState(agent.name);
  const [handle, setHandle] = useState(agent.slug);
  const [persona, setPersona] = useState(agent.persona ?? "");
  const baseCli = agent.pending_switch?.cli ?? agent.cli;
  const baseModel = agent.pending_switch?.model ?? agent.model;
  const [cli, setCli] = useState(baseCli);
  const [model, setModel] = useState(baseModel);
  const [species, setSpecies] = useState(
    typeof agent.look.species === "string" && agent.look.species !== "person" ? agent.look.species : "",
  );
  const [gear, setGear] = useState(typeof agent.gear.quest === "string" ? agent.gear.quest : "");
  const [voice, setVoice] = useState(voiceFor(agent));
  const [trade, setTrade] = useState<Trade | "">(
    TRADES.some((t) => t.id === agent.look.trade) ? (agent.look.trade as Trade) : "",
  );
  const [tags, setTags] = useState(agent.tags ?? []);
  const [tagInput, setTagInput] = useState("");
  const [expertise, setExpertise] = useState(agent.expertise ?? "");
  const [state, setState] = useState<"idle" | "saving" | "saved" | "error">("idle");
  const [saveError, setSaveError] = useState("");

  const [ompModels, setOmpModels] = useState<string[]>([]);

  useEffect(() => {
    worldApi.backends().then(setBackends).catch(() => {});
  }, []);

  useEffect(() => {
    if (cli !== "omp") return;
    worldApi.ompConfig(agent.id).then((c) => setOmpModels(c.models.map((m) => m.id))).catch(() => {});
  }, [agent.id, cli]);

  const suggestions = Array.from(new Set([...(cli === "omp" ? ompModels : []), ...(backends[cli]?.models ?? []), ...(cli === "claude" ? ["opus", "sonnet", "haiku"] : [])]));
  const switching = cli !== baseCli;

  const addTag = () => {
    const tag = normalizeTag(tagInput);
    if (!tag || tags.some((current) => current.toLowerCase() === tag.toLowerCase()) || tags.length >= 8) return;
    setTags((current) => [...current, tag]);
    setTagInput("");
    setState("idle");
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setState("saving");
    try {
      const look = { ...agent.look };
      look.species = species.trim() || "person";
      look.voice = voice;
      if (trade) look.trade = trade;
      else delete look.trade;
      const updated = await worldApi.update(agent.id, {
        name: name.trim(),
        ...(handle.trim() !== agent.slug ? { handle: handle.trim() } : {}),
        persona: persona.trim(),
        ...(cli !== baseCli || model.trim() !== baseModel ? { cli, model: model.trim() } : {}),
        look,
        gear: { ...agent.gear, quest: gear.trim() },
        tags,
        expertise: expertise.trim() || undefined,
      });
      setTags(updated.tags);
      onUpdated(updated);
      setState("saved");
    } catch (error) {
      setSaveError(error instanceof WorldApiError ? error.message : "");
      setState("error");
    }
  };

  return (
    <form onSubmit={save} className="space-y-3 text-w-base">
      <label className="block">
        <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Name</span>
        <input className={cn(INPUT, "w-full")} value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="block">
        <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Handle</span>
        <input className={cn(INPUT, "w-full")} value={handle} onChange={(e) => setHandle(e.target.value)} aria-label="Handle" />
        <span className="mt-1 block text-w-xs text-w-muted">Lowercase letters, digits and dashes. @{agent.slug} today; the old handle keeps working after a change, and one already used by another agent or person is taken.</span>
      </label>
      <label className="block">
        <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Persona</span>
        <VoiceField multiline ariaLabel="Persona" value={persona} onChange={setPersona} />
      </label>
      <label className="block">
        <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Expertise</span>
        <VoiceField ariaLabel="Expertise" placeholder="What they are good at, in a sentence" value={expertise} onChange={setExpertise} />
      </label>
      <div>
        <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Tags (agents also add their own)</span>
        {tags.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-1.5">
            {tags.map((tag) => (
              <span key={tag} className="inline-flex items-center gap-1 rounded-[0.3rem] border-[1.5px] border-w-line2 bg-w-field py-0.5 pl-2 pr-1 text-w-xs">
                #{tag}
                <button
                  type="button"
                  onClick={() => {
                    setTags((current) => current.filter((value) => value !== tag));
                    setState("idle");
                  }}
                  aria-label={`Remove ${tag} tag`}
                  className="rounded p-0.5 hover:bg-w-sand"
                >
                  <X size={11} weight="bold" />
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="flex gap-2">
          <input
            className={cn(INPUT, "min-w-0 flex-1")}
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              e.preventDefault();
              addTag();
            }}
            aria-label="New tag"
            placeholder="research"
            disabled={tags.length >= 8}
          />
          <button type="button" onClick={addTag} disabled={!normalizeTag(tagInput) || tags.length >= 8} className={BTN_SECONDARY}>Add</button>
        </div>
        <p className="mt-1 text-w-xs text-w-muted">{tags.length}/8 tags</p>
      </div>
      <div className="flex gap-2">
        <label className="block w-36">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">CLI</span>
          <WSelect
            ariaLabel="CLI"
            className="w-full"
            value={cli}
            onChange={(next) => {
              setCli(next);
              setModel(next === agent.cli ? agent.model : backends[next]?.default_model ?? backends[next]?.models[0] ?? "");
            }}
            options={cliOptions("opencode" in backends, agent.cli)}
          />
        </label>
        <label className="block flex-1">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Model</span>
          <WCombobox
            ariaLabel="Model"
            value={model}
            onChange={setModel}
            placeholder="Pick or type a model"
            options={suggestions.map((m) => ({ value: m, label: modelName(m), hint: m === backends[cli]?.default_model ? "default" : backends[cli]?.labels?.[m] }))}
          />
        </label>
      </div>
      {cli === "opencode" && !("opencode" in backends) && <p role="alert" className="text-w-xs text-w-danger">{OPENCODE_DISABLED_NOTE}</p>}
      <p className="text-w-xs text-w-muted">
        Any model name the CLI accepts works, so new models need no update here.
        {switching ? " Changing the CLI moves them to a new mind; their old one writes a handoff so they keep their memory." : ""}
        {agent.pending_switch ? ` Switching from ${cliName(agent.cli)} to ${runtimeName(agent.pending_switch.cli, agent.pending_switch.model)} once their current work finishes.` : ""}
      </p>
      <div className="flex gap-2">
        <label className="block flex-1">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Species (blank for a person)</span>
          <input className={cn(INPUT, "w-full")} placeholder="raccoon, robot, owl..." value={species} onChange={(e) => setSpecies(e.target.value)} />
        </label>
        <label className="block flex-1">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Quest gear</span>
          <VoiceField placeholder="viking helmet and sword" ariaLabel="Quest gear" value={gear} onChange={setGear} />
        </label>
      </div>
      <label className="block">
        <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Trade (how they look while working)</span>
        <WSelect
          ariaLabel="Trade"
          className="w-full"
          value={trade}
          onChange={(v) => setTrade(v as Trade | "")}
          options={[
            { value: "", label: "Pick from their work", hint: TRADES.find((t) => t.id === tradeFor({ ...agent, look: { ...agent.look, trade: undefined } }))?.label },
            ...TRADES.map((t) => ({ value: t.id, label: t.label, hint: t.verb })),
          ]}
        />
      </label>
      <div className="flex items-end gap-2">
        <label className="block flex-1">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Voice</span>
          <WSelect
            ariaLabel="Voice"
            className="w-full"
            value={voice}
            onChange={setVoice}
            options={VOICES.map((v) => ({
              value: v,
              label: v.slice(3).replace(/^./, (c) => c.toUpperCase()),
              hint: `${v.startsWith("b") ? "British" : "American"} ${v[1] === "f" ? "female" : "male"}`,
            }))}
          />
        </label>
        <button
          type="button"
          onClick={() => {
            speech().unlock();
            void speech().speak(`Hi, I'm ${name || agent.name}. This is how I sound.`, voice);
          }}
          className={cn(CHUNKY, "h-10 bg-w-card px-3 text-w-sm hover:bg-w-sand")}
        >
          Preview
        </button>
      </div>
      <div className="flex items-center gap-3">
        <button type="submit" disabled={state === "saving" || !name.trim() || !model.trim()} className={BTN_PRIMARY}>
          Save
        </button>
        {state === "saved" && <span className="text-w-ok">Saved.</span>}
        {state === "error" && <span className="text-w-danger">{saveError || "Could not save."}</span>}
      </div>
    </form>
  );
}

function AgentQuestList({
  tasks,
  setTasks,
  agents,
  viewer,
  canUse,
  onOpen,
  onChanged,
}: {
  tasks: TaskBrief[];
  setTasks: (update: (prev: TaskBrief[]) => TaskBrief[]) => void;
  agents: WorldAgent[];
  viewer: QuestViewer | null;
  canUse: boolean;
  onOpen: (taskId: string) => void;
  onChanged: () => void;
}) {
  const order = useMemo(() => tasks.map((t) => t.id), [tasks]);
  const q = useQuestControls({ tasks, setTasks, agents, viewer, order });
  return (
    <div className="relative">
      <ul className="space-y-2">
        {tasks.length === 0 && (
          <li>
            <EmptyState icon={Scroll} title="No quests yet" description="Ask any agent for something, or tell your main agent what you need." />
          </li>
        )}
        {tasks.map((t) => {
          const canEdit = q.editable(t);
          const picked = q.selected.has(t.id);
          return (
            <li key={t.id}>
              <div
                data-quest={t.id}
                role="button"
                tabIndex={0}
                onClick={(e) => {
                  if ((e.shiftKey || e.metaKey || e.ctrlKey) && canEdit) q.toggle(t.id, e.shiftKey);
                  else onOpen(t.id);
                }}
                onKeyDown={(e) => e.target === e.currentTarget && (e.key === "Enter" || e.key === " ") && onOpen(t.id)}
                onMouseMove={() => q.hover(t.id)}
                onMouseLeave={() => q.hover(null)}
                onFocus={() => q.hover(t.id)}
                onBlur={() => q.hover(null)}
                title="Open the quest's session"
                className={cn(
                  "cursor-pointer rounded-lg border-2 px-3 py-2 text-w-base transition-colors hover:border-w-ink",
                  picked ? "border-w-teal bg-w-aqua" : "border-w-line bg-w-field/60 hover:bg-w-card",
                )}
              >
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    data-quest-status={t.id}
                    disabled={!canEdit}
                    onClick={(e) => {
                      e.stopPropagation();
                      if (e.shiftKey || e.metaKey || e.ctrlKey) q.toggle(t.id, e.shiftKey);
                      else q.open("status", t.id, e.currentTarget);
                    }}
                    title={canEdit ? `${statusLabel(t.status)}. Change status (S)` : statusLabel(t.status)}
                    aria-label={`Status: ${statusLabel(t.status)}`}
                    className="flex items-center gap-1 rounded-md border border-w-line2 bg-w-field/70 px-1.5 py-0.5 text-w-2xs font-semibold text-w-text2 transition-colors enabled:hover:border-w-ink disabled:cursor-default"
                  >
                    <StatusIcon status={t.status} size={11} />
                    {statusLabel(t.status)}
                  </button>
                  <span className="min-w-0 flex-1 font-medium">{t.title}</span>
                  {t.plan_waiting ? (
                    <span className="rounded-md bg-w-warn/20 px-1.5 py-0.5 text-w-2xs font-semibold text-w-warn" title="The plan is waiting for your approval in chat">Plan waiting</span>
                  ) : t.plan_first && t.status !== "done" ? (
                    <span className="text-w-2xs text-w-muted">Plan first</span>
                  ) : null}
                  {!!t.attempts && t.attempts > 1 && <span className="text-w-2xs text-w-muted">Best of {t.attempts}</span>}
                  {t.pace && t.pace !== "normal" && <span className="text-w-2xs text-w-muted">{paceLabel(t.pace)}</span>}
                  {milestoneProgress(t.milestones) && (
                    <span className="font-mono text-w-2xs text-w-muted">{milestoneProgress(t.milestones)}</span>
                  )}
                  {t.pr_url && (
                    <a href={t.pr_url} target="_blank" rel="noopener noreferrer" onClick={(e) => e.stopPropagation()} className="text-w-xs text-w-ok underline">
                      PR
                    </a>
                  )}
                  {canEdit && (
                    <button
                      type="button"
                      data-quest-assignee={t.id}
                      onClick={(e) => {
                        e.stopPropagation();
                        q.open("assignee", t.id, e.currentTarget);
                      }}
                      title={`${assigneeName(t, q.byId, q.people)}. Change assignee (A)`}
                      aria-label={`Assignee: ${assigneeName(t, q.byId, q.people)}. Change assignee`}
                      className="flex items-center rounded p-0.5 hover:bg-w-sand"
                    >
                      <AssigneeBadge task={t} byId={q.byId} people={q.people} />
                    </button>
                  )}
                </div>
                {t.result_content && <p className="mt-1 line-clamp-2 text-w-xs text-w-text2">{t.result_content}</p>}
                {(t.milestones?.length || (canUse && !["done", "failed", "cancelled"].includes(t.status))) ? (
                  <div onClick={(e) => e.stopPropagation()}>
                    <MilestoneList
                      taskId={t.id}
                      milestones={t.milestones ?? []}
                      editable={canUse}
                      onChanged={onChanged}
                      className="mt-2 border-t border-dashed border-w-line pt-2"
                    />
                  </div>
                ) : null}
              </div>
            </li>
          );
        })}
      </ul>
      <SelectionBar
        count={q.selected.size}
        onStatus={(el) => q.open("status", null, el)}
        onAssignee={(el) => q.open("assignee", null, el)}
        onClear={q.clear}
      />
      {q.overlay}
    </div>
  );
}

function Board({
  agents: allAgents,
  archived = [],
  viewer,
  portraits,
  places,
  onVisit,
  onVisitPlace,
  onOpen,
  onOpenQuest,
  onCreate,
  canCreate,
  onClose,
  initialView = "quests",
}: {
  initialView?: "residents" | "quests" | "teams";
  agents: WorldAgent[];
  archived?: WorldAgent[];
  viewer: QuestViewer | null;
  portraits: Record<string, string>;
  places: WorldItem[];
  onVisit: (id: string) => void;
  onVisitPlace: (id: string) => boolean;
  onOpen: (id: string) => void;
  onOpenQuest: (task: TaskBrief) => void;
  onCreate: () => void;
  canCreate: boolean;
  onClose: () => void;
}) {
  useEscape(onClose);
  const [view, setView] = useState<"residents" | "quests" | "teams">(initialView);
  const everyone = useEveryone();
  const othersAgents = allAgents.filter((a) => !ownsAgent(a, viewer?.id)).length;
  const agents = everyone ? allAgents : allAgents.filter((a) => ownsAgent(a, viewer?.id));
  const myArchived = archived.filter((a) => a.owner_user_id === viewer?.id);
  const { teams } = useTeams(!!viewer);
  const ownAgents = allAgents.filter((a) => ownsAgent(a, viewer?.id)).length;
  const startingOut = ownAgents <= 1 && !teams.some((t) => t.owner_user_id === viewer?.id);
  const teamGroups = useMemo(() => {
    const ids = new Set(agents.map((a) => a.id));
    const placed = new Set<string>();
    const groups: { key: string; label: string; agents: WorldAgent[] }[] = [];
    for (const t of teams) {
      const members = t.members.filter((m) => m.member_kind === "agent" && ids.has(m.member_id) && !placed.has(m.member_id)).map((m) => m.member_id);
      if (members.length === 0) continue;
      members.forEach((id) => placed.add(id));
      groups.push({ key: t.id, label: t.name, agents: agents.filter((a) => members.includes(a.id)) });
    }
    const rest = agents.filter((a) => !placed.has(a.id));
    if (groups.length > 0 && rest.length > 0) groups.push({ key: "none", label: "Not on a team", agents: rest });
    return groups.length > 0 ? groups : [{ key: "all", label: "", agents }];
  }, [agents, teams]);
  const total = agents.reduce((s, a) => s + a.cost_usd, 0);
  const tokens = agents.reduce((sum, agent) => sum + agent.tokens, 0);
  const partialCost = agents.some((a) => !(a.cost_usd > 0));
  const usage = formatUsage(total, tokens, partialCost);
  return (
    <div role="dialog" aria-modal="true" aria-label="Quest board" className={cn(PANEL, "@container pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <WindowHeader
        title="Quest board"
        subtitle={`${agents.length} ${agents.length === 1 ? "agent" : "agents"}, ${agents.filter((a) => a.jobs?.length).length} on a quest${usage ? ` · ${usage} so far` : ""}`}
        onClose={onClose}
        actions={
          canCreate && (
            <button type="button" onClick={onCreate} className={BTN_PRIMARY}>
              <Plus size={14} weight="bold" />
              <span className="hidden sm:inline">New agent</span>
            </button>
          )
        }
      />
      <div className={TABS} role="tablist">
        {(
          [
            ["quests", "Quests", Sword, 0],
            ["teams", "Teams", UsersFour, teams.length],
            ["residents", "Residents", UsersThree, agents.length],
          ] as const
        ).map(([v, label, TabIcon, count]) => (
          <button key={v} type="button" role="tab" aria-selected={view === v} onClick={() => setView(v)} className={cn(TAB, view === v ? TAB_ON : TAB_OFF)}>
            <TabIcon size={15} weight={view === v ? "fill" : "regular"} />
            {label}
            {count > 0 && <span className={TAB_COUNT}>{count}</span>}
          </button>
        ))}
      </div>
      {view === "quests" ? (
        <QuestBoard agents={allAgents} viewer={viewer} onOpenAgent={onOpen} onOpenQuest={onOpenQuest} />
      ) : view === "teams" ? (
        <TeamsView teams={teams} agents={allAgents} meId={viewer?.id ?? null} portraits={portraits} canWrite={canCreate} />
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="w-full space-y-6 px-4 py-4 sm:px-5 sm:py-5 xl:px-8 xl:py-6">
            {canCreate && viewer && startingOut && <StarterTeamCard agentCount={allAgents.length} />}
            {places.length > 0 && (
              <section className="space-y-2">
                <div className="flex items-center justify-between gap-3">
                  <h3 className={SECTION_TITLE}>Places</h3>
                  <ScopeToggle others={othersAgents} />
                </div>
                <div className="flex flex-wrap gap-2">
                  {places.map((p) => {
                    const PlaceIcon = p.kind === "building" ? House : p.kind === "critter" ? PawPrint : Tree;
                    return (
                      <button
                        key={p.id}
                        type="button"
                        onClick={() => onVisitPlace(p.id)}
                        className={cn(ITEM_CARD, "flex items-center gap-2 px-3 py-1.5 text-w-sm text-w-text", FOCUS)}
                        title={typeof p.data.note === "string" ? p.data.note : undefined}
                        aria-label={`Visit ${p.name}`}
                      >
                        <PlaceIcon size={15} weight="duotone" className="text-w-link" />
                        {p.name}
                      </button>
                    );
                  })}
                </div>
              </section>
            )}
            <section className="space-y-2">
              <div className="flex items-center justify-between gap-3">
                <h3 className={SECTION_TITLE}>Residents</h3>
                {places.length === 0 && <ScopeToggle others={othersAgents} />}
              </div>
              {agents.length === 0 ? (
                <EmptyState icon={UsersThree} title="No residents yet" description="Make an agent and it moves into a house in Home." />
              ) : (
                <div className="space-y-6">
                {teamGroups.map((group) => (
                <div key={group.key} className="space-y-2.5">
                {group.label && (
                  <p className="flex items-center gap-2 text-w-xs font-medium text-w-muted">
                    <UsersFour size={14} weight="bold" className="shrink-0" />
                    {group.label}
                    <span className="tabular-nums text-w-faint">{group.agents.length}</span>
                  </p>
                )}
                <ul className="grid gap-3 @2xl:grid-cols-2 @5xl:grid-cols-3">
                  {group.agents.map((a) => (
                    <li key={`${group.key}:${a.id}`} className={cn(ITEM_CARD, "flex flex-col gap-3 p-3.5")}>
                      <div className="flex items-start gap-3">
                        <AgentFace agent={a} size="3.5rem" tall rounded="rounded-lg" />
                        <div className="min-w-0 flex-1">
                          <p className="flex items-center gap-2">
                            <span className="truncate text-w-md font-semibold text-w-strong">{a.name}</span>
                            <StatusMark status={a.status} />
                          </p>
                          <p className="mt-0.5 truncate text-w-xs text-w-muted">{runtimeName(a.cli, a.model)}</p>
                          <p className="mt-1 truncate text-w-sm text-w-text2">
                            {a.jobs?.length && a.status === "idle" ? (
                              <Shimmer className="truncate">
                                {TRADES.find((t) => t.id === tradeFor(a))?.verb}: {a.jobs[0].title ?? "a quest"}
                                {a.jobs.length > 1 ? ` +${a.jobs.length - 1}` : ""}
                              </Shimmer>
                            ) : (
                              <span title={a.status_detail ?? undefined}>
                                {STATUS_LABEL[a.status]}
                                {a.status_detail ? `: ${a.status_detail}` : ""}
                              </span>
                            )}
                          </p>
                        </div>
                      </div>
                      {a.tags?.length > 0 && (
                        <div className="flex flex-wrap gap-1">
                          {a.tags.slice(0, 5).map((t) => (
                            <span key={t} className={PILL_NEUTRAL}>#{t}</span>
                          ))}
                        </div>
                      )}
                      <div className="mt-auto flex gap-2">
                        <button type="button" onClick={() => onOpen(a.id)} className={cn(BTN_SECONDARY, "flex-1")}>
                          <ChatCircleDots size={15} weight="bold" />
                          Message
                        </button>
                        <button type="button" onClick={() => onVisit(a.id)} className={cn(BTN_SECONDARY, "flex-1")}>
                          <MapPin size={15} weight="bold" />
                          Visit
                        </button>
                      </div>
                    </li>
                  ))}
                </ul>
                </div>
                ))}
                </div>
              )}
              {myArchived.length > 0 && (
                <details className="group/archived rounded-lg border-2 border-dashed border-w-line2">
                  <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2.5 text-w-sm font-medium text-w-muted hover:text-w-text">
                    <Archive size={15} weight="duotone" className="shrink-0" />
                    Archived
                    <span className="tabular-nums text-w-faint">{myArchived.length}</span>
                    <CaretDown size={13} weight="bold" className="ml-auto transition-transform group-open/archived:rotate-180 motion-reduce:transition-none" />
                  </summary>
                  <ul className="divide-y divide-w-line border-t-2 border-dashed border-w-line2">
                    {myArchived.map((a) => (
                      <li key={a.id}>
                        <button type="button" onClick={() => onOpen(a.id)} className="flex w-full items-center gap-3 px-3 py-2.5 text-left transition-colors hover:bg-w-sand/40">
                          <AgentFace agent={a} size="2rem" tall rounded="rounded-md" />
                          <span className="min-w-0 flex-1 truncate text-w-sm text-w-text2">{a.name}</span>
                          <span className="shrink-0 text-w-2xs text-w-muted">
                            {a.archive_delete_at ? (
                              <>
                                Deleted <RelativeTime at={a.archive_delete_at} />
                              </>
                            ) : (
                              "Kept"
                            )}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                </details>
              )}
            </section>
          </div>
        </div>
      )}
    </div>
  );
}

function SelfAvatar({ id, name }: { id: string; name: string }) {
  if (name !== "You") return <PersonAvatar id={id} name={name} size={36} soft />;
  return (
    <span aria-hidden className="grid h-9 w-9 shrink-0 place-items-center rounded-md bg-w-teal text-w-strong">
      <UserIcon size={18} weight="bold" />
    </span>
  );
}

const MESSAGE_CACHE = new Map<string, AgentMessage[]>();
const TASK_CACHE = new Map<string, TaskBrief[]>();

const HUD_FADE = "transition-[opacity,visibility] duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none";

function PlotDialog({ hasAssistant, canCreate, onCreate, onAsk, onClose }: { hasAssistant: boolean; canCreate: boolean; onCreate: () => void; onAsk: () => void; onClose: () => void }) {
  useEscape(onClose);
  return (
    <div role="dialog" aria-modal="true" aria-label="Open plot" className={cn(PANEL, "pointer-events-auto w-full p-5 sm:p-6")}>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-w-xl font-semibold leading-tight text-w-strong">An empty plot</h2>
          <p className="mt-1 text-w-sm text-w-muted">It is waiting for a new resident.</p>
        </div>
        <button type="button" onClick={onClose} aria-label="Close" className={CLOSE_BTN}>
          <X size={16} weight="bold" />
        </button>
      </div>
      <p className="mt-3 text-w-md leading-relaxed">Create a bot directly, or ask your main agent to help shape one.</p>
      <div className="mt-5 flex flex-wrap gap-2">
        {canCreate && <button type="button" onClick={onCreate} className={BTN_PRIMARY}>Create bot</button>}
        {canCreate && <button type="button" onClick={onAsk} className={BTN_SECONDARY}>
          {hasAssistant ? "Ask your main agent" : "Summon your main agent"}
        </button>}
      </div>
    </div>
  );
}

export default function WorldGame() {
  const [gate, setGate] = useState<{ checked: boolean; me: Me | null }>({ checked: false, me: null });
  useEffect(() => installTextScaleKeys(), []);
  useEffect(() => {
    installRefusalBackoff();
    let live = true;
    fetch("/api/world/me", { cache: "no-store" })
      .then((r) => (r.ok ? (r.json() as Promise<Me>) : null))
      .catch(() => null)
      .then((me) => {
        if (live) setGate({ checked: true, me });
      });
    return () => {
      live = false;
    };
  }, []);
  if (!gate.checked) return <div className="fixed inset-0 bg-[#17361f]" />;
  return <LokaGame initialMe={gate.me} />;
}

function LokaGame({ initialMe }: { initialMe: Me | null }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const root = rootRef.current;
    return root ? pinClippedScroll(root) : undefined;
  }, []);
  const engineRef = useRef<LokaEngine | null>(null);
  const [allAgents, setAgents] = useState<WorldAgent[]>([]);
  const agents = useMemo(() => allAgents.filter((a) => !a.archived_at), [allAgents]);
  const archivedAgents = useMemo(() => allAgents.filter((a) => a.archived_at), [allAgents]);
  const [loaded, setLoaded] = useState(false);
  const [rosterOwner, setRosterOwner] = useState<string | null>(null);
  const roster = useMemo(
    () => agents.map((a) => `${a.id}:${a.name}:${JSON.stringify(a.look)}:${JSON.stringify(a.gear)}:${tradeFor(a)}`).sort().join("|"),
    [agents],
  );
  const [started, setStarted] = useState(false);
  const [callMe, setCallMe] = useState("");
  const [naming, setNaming] = useState(false);
  const [nameProblem, setNameProblem] = useState<string | null>(null);
  const [returning] = useState(() => {
    try {
      return window.localStorage.getItem(WELCOMED_KEY) === "1";
    } catch {
      return false;
    }
  });
  useEffect(() => {
    if (loaded) return preloadPanels();
  }, [loaded]);
  const [focus, setFocus] = useState<Target | null>(null);
  const [dialog, setDialog] = useState<Target | null>(null);
  const [board, setBoard] = useState<boolean | "teams">(false);
  const [creatingAgent, setCreatingAgent] = useState(false);
  const [voiceAgentId, setVoiceAgentId] = useState<string | null>(null);
  const inCallRef = useRef(false);
  inCallRef.current = voiceAgentId !== null;
  const talkPressedAt = useRef(0);
  const [time, setTime] = useState<TimeOfDay>("clock");
  const [touch, setTouch] = useState(false);
  const [feed, setFeed] = useState<{ id: number; text: string; count: number }[]>([]);
  const [asides, setAsides] = useState<Set<string>>(new Set());
  const [history, setHistory] = useState<string | null | false>(false);
  const [quick, setQuick] = useState<{ agentId?: string; sessionId?: string } | false>(false);
  const [historyFocus, setHistoryFocus] = useState<SessionFocus | null>(null);
  const [notesOpen, setNotesOpen] = useState(false);
  const [notes, setNotes] = useState<Notifications>({ events: [], unread: 0 });
  const [chatOpen, setChatOpen] = useState(false);
  const [proOpen, setProOpen] = useState(false);
  const [listsOpen, setListsOpen] = useState(false);
  const [reports, setReports] = useState<{ id: string | null } | null>(null);
  const [side, setSideState] = useState<{ id: string | null; open: boolean; auto?: boolean }>({ id: null, open: false });
  const sidePane = usePaneWidth("side-chat", 28, 20, 48);
  const setSide = useCallback((next: { id: string | null; open: boolean; auto?: boolean }) => {
    setSideState(next);
    if (next.auto) return;
    try {
      window.localStorage.setItem(SIDE_KEY, JSON.stringify({ id: next.id, open: next.open, v: 2 }));
    } catch {}
  }, []);
  useEffect(() => {
    if (reports || listsOpen || !side.auto) return;
    try {
      const saved = JSON.parse(window.localStorage.getItem(SIDE_KEY) ?? "null") as { id?: unknown; open?: unknown; v?: unknown } | null;
      setSideState({ id: typeof saved?.id === "string" ? saved.id : side.id, open: saved?.open === true && saved.v === 2 });
    } catch {
      setSideState({ id: side.id, open: false });
    }
  }, [reports, listsOpen, side.auto, side.id]);
  useEffect(() => {
    try {
      const saved = JSON.parse(window.localStorage.getItem(SIDE_KEY) ?? "null") as { id?: unknown; open?: unknown; v?: unknown } | null;
      if (saved) setSideState({ id: typeof saved.id === "string" ? saved.id : null, open: saved.open === true && saved.v === 2 });
    } catch {}
  }, []);
  const sideFrom = useRef<string | null>(null);
  const sideRef = useRef(side);
  sideRef.current = side;
  const [changelogOpen, setChangelogOpen] = useState(false);
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [folderShop, setFolderShop] = useState(false);
  const terminalFeature = useFeature("terminal");
  const [youOpen, setYouOpen] = useState(false);
  const [focusSection, setFocusSection] = useState<{ id: string; at: number } | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [workbench, setWorkbench] = useState<OpenWorkshopDetail | null>(null);
  const [controlPlane, setControlPlane] = useState<"connecting" | "online" | "offline">("connecting");
  const [chatChannel, setChatChannel] = useState<string | null>(null);
  const [chatAgent, setChatAgent] = useState<string | null>(null);
  const [chatFocus, setChatFocus] = useState<{ id: string; nonce: number } | null>(null);
  const [chatChannelFocus, setChatChannelFocus] = useState<{ id: string; nonce: number } | null>(null);
  const [chatLast, setChatLast] = useState<{ agentId: string | null; channelId: string | null }>({ agentId: null, channelId: null });
  const [progress, setProgress] = useState<Record<string, string>>({});
  const [me, setMe] = useState<Me | null>(initialMe);
  const { teams: worldTeams } = useTeams(!!me);
  const teamGroupsRef = useRef<string[][]>([]);
  useEffect(() => {
    teamGroupsRef.current = worldTeams.map((t) => t.members.filter((m) => m.member_kind === "agent").map((m) => m.member_id));
    engineRef.current?.setTeams(teamGroupsRef.current);
  }, [worldTeams]);
  const meRef = useRef<Me | null>(null);
  meRef.current = me;
  const [toasts, setToasts] = useState<Toast[]>([]);
  const openTargetsRef = useRef<Set<string>>(new Set());
  const pushToast = useCallback((toast: Toast) => {
    if (toast.target && openTargetsRef.current.has(toast.target)) return;
    setToasts((prev) => [...prev.filter((t) => !(t.target && t.target === toast.target && t.kind === toast.kind) && t.text !== toast.text).slice(-2), { ...toast, at: Date.now() }]);
  }, []);
  const dismissToasts = useCallback((gone: (toast: Toast) => boolean) => {
    setToasts((prev) => (prev.some(gone) ? prev.filter((t) => !gone(t)) : prev));
  }, []);
  const dismissToast = useCallback((id: number) => dismissToasts((t) => t.id === id), [dismissToasts]);
  const [voiceReplies, setVoiceReplies] = useState(true);
  const voiceRepliesRef = useRef(true);
  voiceRepliesRef.current = voiceReplies;
  const speechStatus = useSpeechStatus();
  useEffect(() => {
    try {
      setVoiceReplies(window.localStorage.getItem("world.voiceReplies") !== "off");
    } catch {}
  }, []);
  const toggleVoiceReplies = () => {
    setVoiceReplies((v) => {
      try {
        window.localStorage.setItem("world.voiceReplies", v ? "off" : "on");
      } catch {}
      return !v;
    });
  };
  useEffect(() => {
    fetch("/api/world/me", { cache: "no-store" })
      .then((r) => (r.ok ? (r.json() as Promise<Me>) : null))
      .then(setMe)
      .catch(() => {});
  }, []);
  const reloadMe = useCallback(async () => {
    const res = await fetch("/api/world/me", { cache: "no-store" });
    if (res.ok) setMe((await res.json()) as Me);
  }, []);
  const makeMain = useCallback(async (agentId: string) => {
    const res = await fetch("/api/world/me", {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ main_agent_id: agentId }),
    });
    if (!res.ok) throw new Error(`${res.status}`);
    setMe((await res.json()) as Me);
  }, []);
  const rename = useCallback(async (name: string) => {
    if (!name) return false;
    const res = await fetch("/api/world/me", {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name }),
    });
    if (!res.ok) return false;
    setMe((await res.json()) as Me);
    return true;
  }, []);
  const setStatus = useCallback(async (emoji: string, text: string) => {
    const res = await fetch("/api/world/me", {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ status: { emoji, text } }),
    });
    if (!res.ok) return false;
    setMe((await res.json()) as Me);
    return true;
  }, []);
  const [chatUnread, setChatUnread] = useState(0);
  const loadChatUnread = useCallback(() => {
    worldApi
      .channels()
      .then((list) => setChatUnread(list.filter((c) => c.member || c.kind === "dm").reduce((n, c) => n + c.unread, 0)))
      .catch(() => {});
  }, []);
  useEffect(() => loadChatUnread(), [loadChatUnread]);
  const chatLastKey = me?.id ? `world.chatLast.${me.id}` : null;
  useEffect(() => {
    if (!chatLastKey) return;
    try {
      const saved = JSON.parse(window.localStorage.getItem(chatLastKey) ?? "null") as { agentId?: unknown; channelId?: unknown } | null;
      setChatLast({
        agentId: typeof saved?.agentId === "string" ? saved.agentId : null,
        channelId: typeof saved?.channelId === "string" ? saved.channelId : null,
      });
    } catch {}
  }, [chatLastKey]);
  const rememberChat = useCallback(
    (selection: { agentId: string | null; channelId: string | null }) => {
      setChatLast(selection);
      if (!chatLastKey) return;
      try {
        window.localStorage.setItem(chatLastKey, JSON.stringify(selection));
      } catch {}
    },
    [chatLastKey],
  );
  const talkingTo = dialog?.kind === "resident" ? dialog.id : chatOpen ? chatLast.agentId : null;
  const talkingToRef = useRef(talkingTo);
  talkingToRef.current = talkingTo;
  const notesSeen = useRef<number | null>(null);
  const notesAsked = useRef(0);
  useEffect(() => unlockSounds(), []);
  const loadNotes = useCallback(() => {
    const asked = ++notesAsked.current;
    worldApi
      .notifications()
      .then((n) => {
        if (asked !== notesAsked.current) return;
        const prev = notesSeen.current;
        notesSeen.current = Math.max(prev ?? 0, ...n.events.map((e) => e.seq));
        if (prev !== null) {
          const unread = n.unread_seqs ? new Set(n.unread_seqs) : null;
          const kinds = new Set(
            n.events
              .filter((e) => e.seq > prev && (!unread || unread.has(e.seq)) && Date.now() - Date.parse(e.created_at) < 300_000)
              .map((e) => notificationSound(e, talkingToRef.current)),
          );
          const kind = (["failed", "done", "notify"] as const).find((k) => kinds.has(k));
          if (kind) playSound(kind);
        }
        setNotes(n);
      })
      .catch(() => {});
  }, []);
  useEffect(() => {
    loadNotes();
    window.addEventListener("world:notes-stale", loadNotes);
    return () => window.removeEventListener("world:notes-stale", loadNotes);
  }, [loadNotes]);
  const titleUnread = notes.unread + chatUnread;
  const titleAgent = dialog?.kind === "resident" ? allAgents.find((a) => a.id === dialog.id)?.name ?? null : null;
  useEffect(() => {
    const base = titleAgent ? `${titleAgent} - Loka` : "Loka";
    const wanted = titleUnread > 0 ? `(${titleUnread > 99 ? "99+" : titleUnread}) ${base}` : base;
    document.title = wanted;
    const keep = new MutationObserver(() => {
      if (document.title !== wanted) document.title = wanted;
    });
    keep.observe(document.head, { subtree: true, childList: true, characterData: true });
    return () => keep.disconnect();
  }, [titleUnread, titleAgent]);
  useEffect(() => {
    const keepTalking = () => {
      const id = sideFrom.current;
      if (id) setSide({ id, open: true, auto: !sideRef.current.open || sideRef.current.auto });
      return id;
    };
    const openLists = () => {
      keepTalking();
      setNotesOpen(false);
      setListsOpen(true);
    };
    window.addEventListener("world:open-lists", openLists);
    const onOpenReport = (e: Event) => {
      keepTalking();
      setNotesOpen(false);
      setReports({ id: (e as CustomEvent<{ id?: string | null }>).detail?.id ?? null });
    };
    window.addEventListener("world:open-report", onOpenReport);
    const params = new URLSearchParams(window.location.search);
    const linkedReport = params.get("report");
    const linkedTodo = params.get("todo");
    if (linkedReport || linkedTodo) {
      params.delete("report");
      params.delete("todo");
      const rest = params.toString();
      window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}${window.location.hash}`);
      if (linkedReport && /^[0-9a-f-]{6,36}$/i.test(linkedReport)) setReports({ id: linkedReport.toLowerCase() });
      if (linkedTodo && /^[0-9a-f-]{6,36}$/i.test(linkedTodo)) {
        setListsOpen(true);
        focusTodo(linkedTodo.toLowerCase());
      }
    }
    const onOpenChangelog = () => {
      setNotesOpen(false);
      setChangelogOpen(true);
    };
    window.addEventListener(OPEN_CHANGELOG, onOpenChangelog);
    primeChangelog();
    const fresh = unseenChangelog();
    const text = fresh.length > 0 ? `What's new: ${fresh[0].title}` : null;
    if (text) pushToast({ id: Date.now(), text, kind: "changelog", target: "changelog", onClick: () => setChangelogOpen(true) });
    return () => {
      window.removeEventListener("world:open-lists", openLists);
      window.removeEventListener("world:open-report", onOpenReport);
      window.removeEventListener(OPEN_CHANGELOG, onOpenChangelog);
    };
  }, []);
  const agentsRef = useRef<WorldAgent[]>([]);
  agentsRef.current = agents;

  const [items, setItems] = useState<WorldItem[]>([]);
  const [mapKey, setMapKey] = useState("");
  const itemsRef = useRef<WorldItem[]>([]);
  itemsRef.current = items;
  const loadItems = useCallback(async () => {
    const list = await worldApi.items().catch(() => [] as WorldItem[]);
    setItems(list);
    setMapKey(
      list
        .filter((i) => i.kind === "building" || i.kind === "decoration" || i.kind === "critter" || i.kind === "portal" || i.kind === "npc")
        .map((i) => `${i.id}:${i.name}:${JSON.stringify(i.data)}`)
        .join("|"),
    );
  }, []);
  const load = useCallback(async () => {
    try {
      const [list] = await Promise.all([worldApi.agents(), loadItems()]);
      setAgents(list);
      setRosterOwner(meRef.current?.id ?? null);
    } catch (error) {
      console.warn("world load failed", error);
    } finally {
      setLoaded(true);
    }
  }, [loadItems]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (me?.id && rosterOwner !== me.id) void load();
  }, [load, me?.id, rosterOwner]);

  const mainId = me?.main_agent_id ?? null;
  const assistant = agents.find((a) => a.id === mainId) ?? null;
  const portraits = useMemo(
    () => Object.fromEntries(agents.map((a) => [a.id, portraitUrl(residentFor(a).look, 4)])),
    [agents],
  );
  const recent = useRecentAgentTabs(me?.id, agents, Boolean(me?.id && rosterOwner === me.id));

  const [sheet, setSheet] = useState<PlaceSheet | null>(null);
  const [loka, setLoka] = useState<LokaSheet | null>(null);
  const openTargets = useMemo(() => {
    const open = new Set<string>();
    if (dialog?.kind === "resident") open.add(`agent:${dialog.id}`);
    if (chatOpen && chatAgent) open.add(`agent:${chatAgent}`);
    if (chatOpen && chatChannel) open.add(`channel:${chatChannel}`);
    if ((reports || listsOpen) && side.open && side.id) open.add(`agent:${side.id}`);
    if (reports) open.add("reports");
    if (listsOpen) open.add("lists");
    if (loka?.kind === "academy") open.add("academy");
    if (changelogOpen) open.add("changelog");
    if (typeof history === "string") open.add(`history:${history}`);
    return open;
  }, [dialog, chatOpen, chatAgent, chatChannel, reports, listsOpen, side.open, side.id, loka, changelogOpen, history]);
  openTargetsRef.current = openTargets;
  useEffect(() => {
    if (openTargets.size) dismissToasts((t) => !!t.target && openTargets.has(t.target));
  }, [openTargets, dismissToasts]);
  useEffect(() => {
    if (notesOpen) dismissToasts((t) => !!t.bell);
  }, [notesOpen, dismissToasts]);
  useEffect(() => {
    if (!notes.items) return;
    const open = new Set(notes.items.filter((item) => !item.cleared_at).map((item) => item.seq));
    const settled = Date.now() - 4000;
    dismissToasts((t) => t.kind === "attention.requested" && (t.at ?? 0) < settled && !open.has(t.id));
  }, [notes, dismissToasts]);
  const [mapOpen, setMapOpen] = useState(false);
  const [mapShown, setMapShown] = useState<boolean | null>(null);
  useEffect(() => {
    if (mapShown === null) setMapShown(readMinimapShown());
    else saveMinimapShown(mapShown);
  }, [mapShown]);
  const [musicOpen, setMusicOpen] = useState(false);
  const lokaRef = useRef<LokaSheet | null>(null);
  lokaRef.current = loka;
  const [regionId, setRegionId] = useState<string>(HEARTH);
  const regionRef = useRef<string>(HEARTH);
  regionRef.current = regionId;
  const [regions, setRegions] = useState<Region[]>([]);
  const [regionDetail, setRegionDetail] = useState<RegionDetail | null>(null);
  const regionDetailRef = useRef<RegionDetail | null>(null);
  regionDetailRef.current = regionDetail;
  const [visited, setVisited] = useState<string[]>([]);
  const cameFrom = useRef<string>(HEARTH);
  const traveling = useRef(false);
  const handoff = useRef<{ region: string; pos: { x: number; y: number } } | null>(null);
  const startedRef = useRef(false);
  startedRef.current = started;
  const ambientRef = useRef<World["ambientTime"]>(null);
  const [cosmetics] = useCosmetics(me?.id ?? null);
  const playerLookKey = cosmetics.gear.join(",");
  const decorKey = cosmetics.decor.join(",");
  const [offer, setOffer] = useState<{ id: string; challengeId: string; title: string; minutes: number } | null>(null);
  const offerRef = useRef(offer);
  offerRef.current = offer;
  const [waitingOn, setWaitingOn] = useState<string | null>(null);
  const loadRegions = useCallback(() => {
    void lokaApi.regions().then(setRegions).catch(() => {});
  }, []);
  useEffect(() => loadRegions(), [loadRegions]);
  useEffect(() => setVisited(readVisited(me?.id ?? null)), [me?.id]);
  const loadRegion = useCallback((id: string) => {
    if (id === HEARTH) return;
    void lokaApi
      .region(id)
      .then((d) => {
        if (regionRef.current === id) setRegionDetail(d);
      })
      .catch(() => {});
  }, []);
  const regionKey = regionDetail
    ? `${regionDetail.region.id}:${regionDetail.region.updated_at}:${regionDetail.items.map((i) => `${i.id}:${i.updated_at}`).join(",")}`
    : "";
  const timeFor = (t: TimeOfDay): TimeOfDay => {
    const a = ambientRef.current;
    if (t !== "clock" || !a) return t;
    return a === "day" ? "day" : a === "night" ? "night" : "dusk";
  };
  const timeForRef = useRef(timeFor);
  timeForRef.current = timeFor;
  const travel = useCallback(async (to: string, label?: string) => {
    if (traveling.current || to === regionRef.current) return;
    traveling.current = true;
    const release = window.setTimeout(() => {
      traveling.current = false;
    }, 4000);
    try {
      const detail = to === HEARTH || districtOf(to) ? null : await lokaApi.region(to);
      const go = () => {
        cameFrom.current = regionRef.current;
        setRegionDetail(detail);
        setRegionId(to);
        setVisited((v) => {
          if (to === HEARTH || v.includes(to)) return v;
          const next = [...v, to];
          saveVisited(meRef.current?.id ?? null, next);
          return next;
        });
      };
      const engine = engineRef.current;
      if (engine?.currentPlace()) engine.leaveRoom(true);
      if (engine) engine.travelOut(go);
      else go();
    } catch (err) {
      window.clearTimeout(release);
      traveling.current = false;
      const text = err instanceof LokaFailure && err.status === 404 ? `The road to ${label ?? "that region"} is closed to you.` : `Could not travel to ${label ?? "that region"} right now.`;
      pushToast({ id: Date.now(), text, kind: "travel" });
    }
  }, []);
  const travelRef = useRef(travel);
  travelRef.current = travel;
  useEffect(() => {
    const onDownloaded = (e: Event) => {
      const name = (e as CustomEvent<{ name?: string }>).detail?.name;
      const text = name ? `Saved ${name} to Downloads` : "Saved to Downloads";
      pushToast({ id: Date.now(), text, kind: "download" });
    };
    window.addEventListener("world:downloaded", onDownloaded);
    return () => window.removeEventListener("world:downloaded", onDownloaded);
  }, []);
  const [scene, setScene] = useState<{ placeId: string; title: string; room?: string; at?: { x: number; y: number } } | null>(null);
  const [campusCards, setCampusCards] = useState<Pick<CampusData, "cards" | "adventures">>({ cards: [], adventures: [] });
  const campusRef = useRef<CampusData>({ cards: [], adventures: [], mentors: [] });
  const exitRef = useRef<(to: string) => void>(() => {});
  const [kits, setKits] = useState<Record<string, { skills: { id: string; name: string }[]; servers: { id: string; name: string; enabled: boolean }[]; notes: boolean }>>({});
  const [runners, setRunners] = useState<RunnerView[]>([]);
  const [sightingAt, setSightingAt] = useState(0);
  const [budgetAlarmAt, setBudgetAlarmAt] = useState(0);
  const [engineGen, setEngineGen] = useState(0);
  const roomActions = useRef<Record<string, PlaceAction>>({});
  const sceneRoomRef = useRef<PlaceRoom | null>(null);
  const interactRef = useRef<(t: Target) => void>(() => {});
  const open = useCallback((t: Target) => interactRef.current(t), []);
  const onLeave = useCallback(() => {
    setScene(null);
    roomActions.current = {};
    sceneRoomRef.current = null;
    canvasRef.current?.focus();
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !loaded) return;
    const district = districtOf(regionId);
    const inRegion = regionId !== HEARTH && !district;
    const detail = regionDetailRef.current;
    if (inRegion && detail?.region.id !== regionId) return;
    const list = agentsRef.current;
    const isTouch = window.matchMedia("(pointer: coarse)").matches;
    const isReduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    setTouch(isTouch);
    const guide = list.find((a) => a.id === mainId);
    const residents = list.filter((a) => a.id !== mainId).map((a) => residentFor(a));
    const world = district
      ? buildRegionWorld(districtRegion(district), [], cameFrom.current, { district, decor: decorKey ? decorKey.split(",") : [] })
      : inRegion && detail
        ? buildRegionWorld(detail.region, detail.items.map((i) => mapItemOf(i, list)).filter((m): m is MapItem => m !== null), cameFrom.current)
        : buildWorld(guide ? residentFor(guide, true) : null, residents, itemsRef.current.map((i) => mapItemOf(i, list)).filter((m): m is MapItem => m !== null));
    const previous = engineRef.current;
    const prior = handoff.current;
    const sameRegion = prior?.region === world.region;
    const pos = sameRegion && startedRef.current ? prior.pos : null;
    previous?.destroy();
    ambientRef.current = world.ambientTime;
    const engine = new LokaEngine(canvas, world, {
      fadeIn: Boolean(prior) && !sameRegion,
      playerLook: playerLookWith(playerLookKey ? playerLookKey.split(",") : []),
      fontFamily: getComputedStyle(canvas).fontFamily || "monospace",
      reducedMotion: isReduced,
      touch: isTouch,
      onFocus: setFocus,
      onInteract: open,
      onStart: () => setStarted(true),
      onLeave,
      onExit: (to) => exitRef.current(to),
    });
    const roomPos = previous?.roomPosition() ?? null;
    if (pos) {
      engine.placePlayer(pos);
      engine.begin();
    } else if (prior && startedRef.current) {
      const arrival = arrivalFor(world, cameFrom.current);
      if (arrival) engine.placePlayer(arrival, arrival.dir);
      else engine.placePlayer(world.gate);
      engine.begin();
    }
    traveling.current = false;
    const inside = previous?.currentPlace() ?? null;
    if (inside && sceneRoomRef.current && engine.placeOf(inside)) engine.enterRoom(buildRoom(sceneRoomRef.current.spec), inside, { instant: true, at: roomPos ?? undefined });
    else if (inside) onLeave();
    for (const a of list) {
      engine.setAgentState(a.id, a.status, a.status_detail);
      engine.setAgentJobs(a.id, jobKinds(a), true);
    }
    engine.setBudgetPaused(list.some(budgetPaused));
    engine.setTeams(teamGroupsRef.current);
    engine.setTime(timeForRef.current(time));
    engineRef.current = engine;
    setEngineGen((n) => n + 1);
    return () => {
      handoff.current = { region: world.region, pos: engine.playerPosition() };
      engine.destroy();
      if (engineRef.current === engine) engineRef.current = null;
    };
  }, [loaded, roster, mapKey, open, onLeave, mainId, regionId, regionKey, decorKey]);

  useEffect(() => {
    engineRef.current?.setPlayerLook(playerLookWith(playerLookKey ? playerLookKey.split(",") : []));
  }, [playerLookKey, engineGen]);

  const windowOpen = Boolean(dialog) || Boolean(sheet) || Boolean(loka) || Boolean(board) || creatingAgent || Boolean(voiceAgentId) || history !== false || quick !== false || chatOpen || listsOpen || Boolean(reports) || changelogOpen || terminalOpen || youOpen || folderShop;
  const overlay = windowOpen || paletteOpen || Boolean(workbench);
  const hudHide = overlay ? "invisible opacity-0" : "visible opacity-100";
  const menu = windowOpen && (creatingAgent || Boolean(voiceAgentId) || dialog?.kind !== "plot");
  const otherMenu = Boolean(board) || creatingAgent || Boolean(voiceAgentId) || history !== false || quick !== false || chatOpen || notesOpen || listsOpen || youOpen || dialog?.kind === "plot" || paletteOpen;
  const canRead = atLeast(me, "read");
  const canWrite = atLeast(me, "write");
  const isAdmin = atLeast(me, "admin");
  useEffect(() => {
    engineRef.current?.setPaused(overlay);
  }, [overlay]);

  useEffect(() => {
    engineRef.current?.setTime(timeForRef.current(time));
    if (time !== "clock") return;
    const id = window.setInterval(() => engineRef.current?.setTime(timeForRef.current("clock")), 60_000);
    return () => window.clearInterval(id);
  }, [time]);

  const refreshTimer = useRef<number | null>(null);
  const refreshAgents = useCallback(() => {
    if (refreshTimer.current !== null) return;
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      void worldApi.agents().then(setAgents).catch(() => {});
    }, 400);
  }, []);

  useEffect(() => {
    const engine = engineRef.current;
    if (!engine) return;
    for (const a of agents) engine.setAgentJobs(a.id, jobKinds(a));
    engine.setBudgetPaused(agents.some(budgetPaused));
  }, [agents]);

  useEffect(() => {
    let source: EventSource | null = null;
    let notesTimer: number | null = null;
    let retryTimer: number | null = null;
    let retryDelay = 3_000;
    const connect = () => {
      const next = new EventSource("/api/world/stream");
      next.onopen = () => {
        retryDelay = 3_000;
        setControlPlane("online");
        void load();
        loadNotes();
        loadChatUnread();
        clearRefusals();
        window.dispatchEvent(new Event("world-reconnected"));
      };
      next.onerror = () => {
        setControlPlane(next.readyState === EventSource.CONNECTING ? "connecting" : "offline");
        if (next.readyState !== EventSource.CLOSED || retryTimer !== null) return;
        retryTimer = window.setTimeout(() => {
          retryTimer = null;
          retryDelay = Math.min(retryDelay * 2, 30_000);
          revive();
        }, retryDelay);
      };
      next.addEventListener("world", onWorld as EventListener);
      source = next;
    };
    const revive = () => {
      if (document.visibilityState !== "visible" || source?.readyState !== EventSource.CLOSED) return;
      setControlPlane("connecting");
      connect();
    };
    const onWorld = (msg: MessageEvent<string>) => {
      let ev: WorldEvent;
      try {
        ev = JSON.parse(msg.data) as WorldEvent;
      } catch {
        return;
      }
      if (ev.type === "agent.typing") {
        window.dispatchEvent(new CustomEvent(TYPING_EVENT, { detail: ev }));
        return;
      }
      if (ev.type === "agent.seen") {
        window.dispatchEvent(new CustomEvent(SEEN_EVENT, { detail: ev }));
        return;
      }
      window.dispatchEvent(new CustomEvent("world-event", { detail: ev }));
      if (ev.type === "agent.progress" && ev.agent_id) {
        const agentId = ev.agent_id;
        const text = String(ev.payload.text ?? "");
        setProgress((cur) => ({ ...cur, [agentId]: text }));
        return;
      }
      if (ev.agent_id && (ev.type === "agent.status" || (ev.type === "message.created" && ev.payload.author_kind === "agent"))) {
        const agentId = ev.agent_id;
        setProgress((cur) => {
          if (!(agentId in cur)) return cur;
          const next = { ...cur };
          delete next[agentId];
          return next;
        });
      }
      if (ev.type === "agent.status" && ev.agent_id) {
        const status = (ev.payload.status as WorldAgent["status"]) ?? "idle";
        const detail = (ev.payload.detail as string | null) ?? null;
        engineRef.current?.setAgentState(ev.agent_id, status, detail);
        setAgents((prev) => prev.map((a) => (a.id === ev.agent_id ? { ...a, status, status_detail: detail } : a)));
        if (status === "idle") void worldApi.agents().then(setAgents).catch(() => {});
      }
      const loka = engineRef.current;
      if (loka) {
        const author = ev.payload.author_agent_id;
        if (ev.type === "message.created" && ev.agent_id && ev.payload.author_kind === "agent" && typeof author === "string") loka.agentMessage(author, ev.agent_id, String(ev.payload.body ?? ""));
        if (ev.type === "attention.requested" && ev.agent_id) loka.agentAsks(ev.agent_id);
        if (ev.type === "task.merged" || (ev.type === "task.finished" && !ev.payload.error)) loka.celebrate();
        if (ev.type === "task.finished" && ev.payload.error && ev.agent_id) loka.questFailed(ev.agent_id);
        if (ev.type === "agent.budget") loka.budgetWarning();
      }
      if (
        (ev.type === "task.merged" || (ev.type === "task.finished" && !ev.payload.error)) &&
        !ev.payload.quiet &&
        ownsAgent(agentsRef.current.find((a) => a.id === ev.agent_id), meRef.current?.id)
      )
        playSound("done");
      if (ev.type === "agent.budget") setBudgetAlarmAt(Date.now());
      if (ev.type === "feed.item") setSightingAt(Date.now());
      if (ev.type === "agent.created" || ev.type === "agent.updated" || ev.type === "agent.removed" || ev.type === "agent.transferred" || ev.type === "agent.archived" || ev.type === "agent.unarchived") void load();
      if (ev.type === "agent.main_changed") {
        void load();
        if (ev.payload.user_id === meRef.current?.id) void reloadMe();
      }
      if (ev.type.startsWith("item.")) void loadItems();
      if (ev.type.startsWith("todo.")) window.dispatchEvent(new Event("world:todos-stale"));
      if (ev.type.startsWith("report.")) window.dispatchEvent(new CustomEvent(REPORTS_STALE, { detail: ev.payload }));
      if (ev.type === "report.created" && ev.agent_id && !ev.payload.quiet && ev.payload.owner_user_id === meRef.current?.id) {
        const id = String(ev.payload.report_id ?? "");
        pushToast({ id: ev.seq, kind: "report.created", target: "reports", bell: true, text: `New report: ${String(ev.payload.title ?? "")}`, onClick: () => setReports({ id }) });
      }
      if (ev.type === "todo.nudge" && ev.payload.owner_user_id === meRef.current?.id) {
        playSound("timer");
        pushToast({ id: ev.seq, kind: ev.payload.timer ? "timer.done" : "todo.nudge", target: "lists", bell: true, text: `${ev.payload.timer ? "Time's up" : "Still open"}: ${String(ev.payload.text ?? "")}`, onClick: () => setListsOpen(true) });
      }
      if (ev.type === "timer.done") {
        void loadItems();
        const mine = meRef.current?.id;
        if (!ev.payload.then && (!ev.payload.owner_user_id || ev.payload.owner_user_id === mine)) {
          playSound("timer");
          pushToast({ id: ev.seq, kind: "timer.done", target: "lists", bell: true, text: `Timer done: ${String(ev.payload.label ?? "")}`, onClick: () => setListsOpen(true) });
        }
      }
      if (bellWorthy(ev, meRef.current?.id)) {
        if (notesTimer === null) notesTimer = window.setTimeout(() => {
          notesTimer = null;
          loadNotes();
        }, 1200);
      }
      if (ev.type === "channel.message") loadChatUnread();
      const spokenFrom =
        ev.type === "message.created" && ev.agent_id && ev.payload.author_kind === "agent" && !ev.payload.author_agent_id && !ev.payload.quiet
          ? takeSpokenReply(`agent:${ev.agent_id}`) && ev.agent_id
          : ev.type === "channel.message" && ev.payload.author_kind === "agent" && typeof ev.payload.author_agent_id === "string"
            ? takeSpokenReply(`channel:${String(ev.payload.channel_id)}`) && ev.payload.author_agent_id
            : null;
      if (spokenFrom) {
        const speaker = agentsRef.current.find((a) => a.id === spokenFrom);
        if (speaker && voiceRepliesRef.current && !inCallRef.current) void speech().speak(String(ev.payload.body ?? ""), voiceFor(speaker));
      }
      if (
        ev.type === "message.created" &&
        ev.agent_id &&
        ev.agent_id === talkingToRef.current &&
        ev.payload.author_kind === "agent" &&
        !ev.payload.author_agent_id &&
        !ev.payload.quiet &&
        (document.hidden || !document.hasFocus())
      )
        playSound("reply");
      const mine = meRef.current?.id;
      const mentioned = Array.isArray(ev.payload.mentions_users) && mine && (ev.payload.mentions_users as string[]).includes(mine);
      if (ev.type === "channel.message" && mentioned) {
        const channelId = ev.payload.channel_id as string;
        pushToast({
          id: ev.seq,
          kind: "channel.message",
          target: `channel:${channelId}`,
          bell: true,
          text: `${ev.payload.author as string} mentioned you in ${ev.payload.channel as string}: ${(ev.payload.body as string).slice(0, 120)}`,
          onClick: () => {
            setChatChannel(channelId);
            setChatOpen(true);
          },
        });
      }
      const askingAgent = ev.agent_id ? agentsRef.current.find((a) => a.id === ev.agent_id) : undefined;
      if (ev.type === "attention.requested" && askingAgent?.owner_user_id === mine) {
        const agentId = ev.agent_id;
        pushToast({
          id: ev.seq,
          kind: "attention.requested",
          target: agentId ? `agent:${agentId}` : undefined,
          bell: true,
          text: `${(ev.payload.agent as string) ?? "An agent"} needs your decision: ${(ev.payload.text as string).slice(0, 140)}`,
          onClick: () => agentId && setDialog({ kind: "resident", id: agentId }),
        });
      }
      if (ev.type === "message.created" && ev.agent_id && ev.payload.author_kind === "user" && askingAgent?.owner_user_id === mine) {
        const target = `agent:${ev.agent_id}`;
        dismissToasts((t) => t.kind === "attention.requested" && t.target === target);
      }
      if (ev.type === "run.activity" && ev.agent_id) {
        const runId = ev.run_id ?? (ev.payload.run_id as string | undefined);
        const text = String(ev.payload.text ?? "");
        setAgents((prev) =>
          prev.map((a) =>
            a.id === ev.agent_id && a.jobs?.some((j) => j.run_id === runId)
              ? { ...a, jobs: a.jobs.map((j) => (j.run_id === runId ? { ...j, activity: text, status: "running" } : j)) }
              : a,
          ),
        );
        if (!agentsRef.current.find((a) => a.id === ev.agent_id)?.jobs?.some((j) => j.run_id === runId)) refreshAgents();
        return;
      }
      if (["run.completed", "run.failed", "run.timed_out", "task.started", "task.finished", "session.replied", "review.finished"].includes(ev.type)) refreshAgents();
      if (ev.type === "aside.answered") {
        const sid = ev.payload.aside_session_id as string | undefined;
        if (sid) setAsides((prev) => new Set(prev).add(sid));
      }
      const names = new Map(agentsRef.current.map((a) => [a.id, a.name]));
      const text = describe(ev, names);
      if (ev.payload.quiet) return;
      const fromOthers = !!ev.agent_id && !ownsAgent(agentsRef.current.find((a) => a.id === ev.agent_id), meRef.current?.id);
      if (text && (!fromOthers || everyoneNow()))
        setFeed((prev) => {
          const same = prev.find((f) => f.text === text);
          if (same) return [...prev.filter((f) => f !== same), { id: ev.seq, text, count: same.count + 1 }];
          return [...prev.slice(-3), { id: ev.seq, text, count: 1 }];
        });
    };
    connect();
    window.addEventListener("focus", revive);
    window.addEventListener("online", revive);
    document.addEventListener("visibilitychange", revive);
    return () => {
      window.removeEventListener("focus", revive);
      window.removeEventListener("online", revive);
      document.removeEventListener("visibilitychange", revive);
      if (notesTimer !== null) window.clearTimeout(notesTimer);
      if (retryTimer !== null) window.clearTimeout(retryTimer);
      source?.close();
    };
  }, [load, loadItems, loadNotes, loadChatUnread, reloadMe]);

  const showOffer = useCallback((next: { id: string; challengeId: string; title: string; minutes: number }) => {
    if (lokaRef.current?.kind === "academy") return;
    setOffer(next);
    const main = meRef.current?.main_agent_id;
    if (main && regionRef.current === HEARTH) engineRef.current?.approach(main, "Puzzle while you wait?");
  }, []);
  const dismissOffer = useCallback(() => {
    setOffer(null);
    const main = meRef.current?.main_agent_id;
    if (main) engineRef.current?.hush(main);
  }, []);

  useEffect(() => {
    const onLoka = (e: Event) => {
      const ev = (e as CustomEvent<WorldEvent>).detail;
      const mine = meRef.current?.id;
      const p = ev.payload;
      if (ev.type.startsWith("region.")) {
        loadRegions();
        if (p.region_id === regionRef.current) {
          if (ev.type === "region.archived") void travelRef.current(HEARTH);
          else loadRegion(regionRef.current);
        }
      }
      if (ev.type.startsWith("item.") && p.region_id && p.region_id === regionRef.current) loadRegion(regionRef.current);
      if (ev.type === "npc.replied" && typeof p.reply === "string" && lokaRef.current?.kind !== "npc") engineRef.current?.folkSay(String(p.npc_id ?? ""), p.reply);
      if (typeof p.owner_user_id === "string" && p.owner_user_id !== mine) return;
      if (ev.type === "academy.offer_ready" && typeof p.challenge_id === "string") {
        showOffer({ id: String(p.offer_id ?? ""), challengeId: p.challenge_id, title: String(p.title ?? "A challenge"), minutes: Number(p.minutes) || 10 });
      }
      if (ev.type === "challenge.graded" && lokaRef.current?.kind !== "academy") {
        const challengeId = String(p.challenge_id ?? "");
        const xp = Number(p.xp_awarded) || 0;
        pushToast({
          id: ev.seq,
          kind: p.status === "passed" ? "challenge.passed" : "challenge.graded",
          target: "academy",
          text: `${String(p.title ?? "Your answer")} was graded: ${p.status === "passed" ? "passed" : "not yet"}${xp ? `, +${xp} xp` : ""}${p.level_up ? `. Level ${String(p.level)}!` : ""}`,
          onClick: () => setLoka({ kind: "academy", challengeId }),
        });
      }
      if (ev.type === "academy.unlocked" && p.unlock && typeof p.unlock === "object" && lokaRef.current?.kind !== "academy") {
        const name = String((p.unlock as { name?: unknown }).name ?? "something new");
        pushToast({ id: ev.seq, kind: "academy.unlocked", target: "academy", text: `Unlocked ${name}. Equip it in the Academy.`, onClick: () => setLoka({ kind: "academy" }) });
      }
      if ((ev.type === "task.finished" || ev.type === "task.merged") && lokaRef.current?.kind === "academy" && ev.agent_id) {
        const agent = agentsRef.current.find((a) => a.id === ev.agent_id);
        if (!ownsAgent(agent, mine)) return;
        const agentId = ev.agent_id;
        const taskId = ev.task_id ?? (typeof p.task_id === "string" ? p.task_id : null);
        pushToast({
          id: ev.seq,
          kind: "task.finished",
          target: `history:${agentId}`,
          bell: true,
          text: `${agent?.name ?? "Your agent"} finished the quest${p.title ? `: ${String(p.title)}` : ""}. Your answer is saved. Tap to see it.`,
          onClick: () => {
            setHistory(agentId);
            setHistoryFocus(taskId ? { taskId } : null);
          },
        });
      }
    };
    window.addEventListener("world-event", onLoka);
    return () => window.removeEventListener("world-event", onLoka);
  }, [loadRegions, loadRegion, showOffer]);

  const [record, setRecord] = useState<Progress | null>(null);
  const academyOn = useRef(false);
  academyOn.current = record?.academy_on === true;
  const refreshProgress = useCallback(() => {
    lokaApi.progress().then(setRecord).catch(() => {});
  }, []);
  useEffect(refreshProgress, [refreshProgress, me?.id]);
  useSharedProgress(setRecord);
  useWorldEvent(
    useCallback(
      (ev: WorldEvent) => {
        if (ev.type === "challenge.graded" || ev.type === "academy.unlocked") refreshProgress();
      },
      [refreshProgress],
    ),
  );

  const lastInput = useRef(Date.now());
  const offerAsked = useRef(false);
  const offerQuietUntil = useRef(0);
  useEffect(() => {
    const bump = () => {
      lastInput.current = Date.now();
      offerAsked.current = false;
    };
    const events = ["keydown", "pointerdown", "pointermove", "wheel", "touchstart"] as const;
    for (const name of events) window.addEventListener(name, bump, { passive: true });
    const tick = window.setInterval(() => {
      if (offerAsked.current || document.hidden || Date.now() - lastInput.current < 60_000 || Date.now() < offerQuietUntil.current) return;
      if (!academyOn.current || offerRef.current || lokaRef.current?.kind === "academy") return;
      const me = meRef.current;
      if (!me || !atLeast(me, "write")) return;
      const busy = agentsRef.current.filter((a) => ownsAgent(a, me.id) && a.jobs?.some((j) => j.kind !== "publish"));
      if (!busy.length) return;
      offerAsked.current = true;
      const job = busy.flatMap((a) => a.jobs ?? []).find((j) => j.task_id && j.kind !== "publish");
      setWaitingOn(job?.title ?? null);
      lokaApi
        .offer(job?.task_id ?? null)
        .then(async ({ offer: o }) => {
          if (o.status !== "ready" || !o.challenge_id) return;
          const d = await lokaApi.challenge(o.challenge_id);
          if (d.card.state === "passed") return;
          showOffer({ id: o.id, challengeId: o.challenge_id, title: d.card.title, minutes: d.card.minutes });
        })
        .catch((err) => {
          if (err instanceof LokaFailure && err.status === 429) offerQuietUntil.current = Date.now() + 20 * 60_000;
          if (err instanceof LokaFailure && err.code === "academy_off") academyOn.current = false;
        });
    }, 10_000);
    return () => {
      for (const name of events) window.removeEventListener(name, bump);
      window.clearInterval(tick);
    };
  }, [showOffer]);

  const closePanels = useCallback(() => {
    setDialog(null);
    setBoard(false);
    setCreatingAgent(false);
    setVoiceAgentId(null);
    setHistory(false);
    setHistoryFocus(null);
    setNotesOpen(false);
    setChatOpen(false);
    setChatChannel(null);
    setChatAgent(null);
    setListsOpen(false);
    setReports(null);
    setChangelogOpen(false);
    setFolderShop(false);
    setYouOpen(false);
    setPaletteOpen(false);
    setSheet(null);
    setLoka(null);
  }, []);

  const openAgent = useCallback((id: string) => {
    closePanels();
    setDialog({ kind: "resident", id });
  }, [closePanels]);

  const linkedAgent = useRef<string | null | undefined>(undefined);
  useEffect(() => {
    if (linkedAgent.current === undefined) linkedAgent.current = new URLSearchParams(window.location.search).get("agent");
    const wanted = linkedAgent.current;
    if (!wanted || !loaded || !agents.length) return;
    linkedAgent.current = null;
    const found = allAgents.find((a) => a.slug === wanted || a.id === wanted);
    if (!found) return;
    engineRef.current?.begin();
    setDialog({ kind: "resident", id: found.id });
  }, [loaded, agents]);
  const openSlug = dialog?.kind === "resident" ? allAgents.find((a) => a.id === dialog.id)?.slug ?? null : null;
  useEffect(() => {
    if (linkedAgent.current) return;
    const params = new URLSearchParams(window.location.search);
    if (params.get("agent") === openSlug) return;
    if (openSlug) params.set("agent", openSlug);
    else params.delete("agent");
    const rest = params.toString();
    window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}${window.location.hash}`);
  }, [openSlug]);

  const openSection = useCallback((id: string) => {
    closePanels();
    setYouOpen(true);
    setFocusSection({ id, at: Date.now() });
  }, [closePanels]);
  const openDevices = useCallback(() => openSection("world-devices"), [openSection]);

  useEffect(() => {
    const onSetup = (event: Event) => {
      const action = (event as CustomEvent<SetupAction | string>).detail;
      if (typeof action === "string" && action.startsWith("page:")) openSection(action);
      else if (action === "github") openSection("page:github");
      else openSection(action === "keys" ? "world-environment" : "world-devices");
    };
    const onTerminal = () => {
      closePanels();
      setTerminalOpen(true);
    };
    window.addEventListener(SETUP_EVENT, onSetup);
    window.addEventListener(OPEN_TERMINAL_EVENT, onTerminal);
    return () => {
      window.removeEventListener(SETUP_EVENT, onSetup);
      window.removeEventListener(OPEN_TERMINAL_EVENT, onTerminal);
    };
  }, [openSection, closePanels]);

  useEffect(() => {
    const onQuick = (event: Event) => {
      const detail = (event as CustomEvent<{ agentId?: string; sessionId?: string }>).detail ?? {};
      closePanels();
      setQuick({ agentId: detail.agentId, sessionId: detail.sessionId });
    };
    window.addEventListener(QUICK_CHAT_EVENT, onQuick);
    return () => window.removeEventListener(QUICK_CHAT_EVENT, onQuick);
  }, [closePanels]);

  useEffect(() => {
    if (dialog?.kind === "resident") recent.open(dialog.id);
  }, [dialog, recent.open]);

  const linkState = useRef({ dialog, chatOpen });
  linkState.current = { dialog, chatOpen };
  useEffect(() => {
    const onOpenMessage = (event: Event) => {
      const link = (event as CustomEvent<LinkedMessage>).detail;
      if (!link) return;
      const { dialog: open, chatOpen: chatting } = linkState.current;
      if (link.kind === "agent" && link.agent_id) {
        const agentId = link.agent_id;
        if (chatting) setChatFocus({ id: agentId, nonce: Date.now() });
        else if (!(open?.kind === "resident" && open.id === agentId)) {
          closePanels();
          setDialog({ kind: "resident", id: agentId });
        }
      } else if (link.kind === "channel" && link.channel_id) {
        if (chatting) setChatChannelFocus({ id: link.channel_id, nonce: Date.now() });
        else {
          closePanels();
          setChatChannel(link.channel_id);
          setChatOpen(true);
        }
      } else return;
      requestJump(link);
    };
    const onOpenQuest = (event: Event) => {
      const detail = (event as CustomEvent<{ taskId: string; agentId: string | null }>).detail;
      if (!detail) return;
      closePanels();
      setHistory(detail.agentId ?? null);
      setHistoryFocus({ taskId: detail.taskId });
    };
    const onOpenChannel = (event: Event) => {
      const id = (event as CustomEvent<{ id: string }>).detail?.id;
      if (!id) return;
      if (linkState.current.chatOpen) setChatChannelFocus({ id, nonce: Date.now() });
      else {
        closePanels();
        setChatChannel(id);
        setChatOpen(true);
      }
    };
    const onTodoFocus = () => {
      const id = sideFrom.current;
      if (id) {
        setSide({ id, open: true, auto: !sideRef.current.open || sideRef.current.auto });
        setNotesOpen(false);
        setReports(null);
      } else {
        closePanels();
      }
      setListsOpen(true);
    };
    window.addEventListener("world:todo-focus", onTodoFocus);
    window.addEventListener("world:open-quest", onOpenQuest);
    window.addEventListener("world:open-channel", onOpenChannel);
    window.addEventListener("world:open-message", onOpenMessage);
    const params = new URLSearchParams(window.location.search);
    const quest = params.get("quest");
    if (quest && /^[0-9a-f-]{36}$/i.test(quest)) {
      params.delete("quest");
      window.history.replaceState(null, "", `${window.location.pathname}${params.toString() ? `?${params.toString()}` : ""}${window.location.hash}`);
      onOpenQuest(new CustomEvent("world:open-quest", { detail: { taskId: quest, agentId: null } }));
    }
    const linked = params.get("msg");
    if (linked) {
      params.delete("msg");
      const rest = params.toString();
      window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}${window.location.hash}`);
      void resolveMessage(linked).then((found) => found && openMessage(found));
    }
    return () => {
      window.removeEventListener("world:open-message", onOpenMessage);
      window.removeEventListener("world:open-quest", onOpenQuest);
      window.removeEventListener("world:todo-focus", onTodoFocus);
      window.removeEventListener("world:open-channel", onOpenChannel);
    };
  }, [closePanels]);

  useEffect(() => {
    const onOpen = (event: Event) => {
      const detail = (event as CustomEvent<OpenWorkshopDetail>).detail;
      if (!detail) return;
      closePanels();
      setWorkbench(detail);
    };
    window.addEventListener("world:open-workbench", onOpen);
    const params = new URLSearchParams(window.location.search);
    const linked = params.get("workbench");
    if (linked && /^[0-9a-f-]{36}$/i.test(linked)) {
      params.delete("workbench");
      const rest = params.toString();
      window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}${window.location.hash}`);
      setWorkbench(linked);
    }
    return () => window.removeEventListener("world:open-workbench", onOpen);
  }, [closePanels]);

  useEffect(() => {
    if (!paletteOpen) return;
    const closePalette = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      event.stopImmediatePropagation();
      setPaletteOpen(false);
    };
    window.addEventListener("keydown", closePalette, true);
    return () => window.removeEventListener("keydown", closePalette, true);
  }, [paletteOpen]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const modifier = event.metaKey || event.ctrlKey;
      if (modifier && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPaletteQuery("");
        setPaletteOpen((value) => !value);
        return;
      }
      if (!modifier || event.altKey) return;
      const index = Number(event.key) - 1;
      if (index >= 0 && index < recent.ids.length) {
        event.preventDefault();
        openAgent(recent.ids[index]);
      }
    };
    const onDesktopCommand = (event: Event) => {
      const detail = (event as CustomEvent<string | { command?: string }>).detail;
      const command = typeof detail === "string" ? detail : detail?.command;
      if (command === "palette") {
        setPaletteQuery("");
        setPaletteOpen(true);
      }
      else if (command === "settings") {
        setFocusSection(null);
        setYouOpen(true);
      }
      else if (command === "next-tab" || command === "previous-tab") {
        if (!recent.ids.length) return;
        const current = dialog?.kind === "resident" ? recent.ids.indexOf(dialog.id) : -1;
        const direction = command === "next-tab" ? 1 : -1;
        const next = (current + direction + recent.ids.length) % recent.ids.length;
        openAgent(recent.ids[next]);
      } else if (command === "close-tab" && dialog?.kind === "resident") {
        const index = recent.ids.indexOf(dialog.id);
        const remaining = recent.ids.filter((id) => id !== dialog.id);
        recent.close(dialog.id);
        if (remaining.length) openAgent(remaining[Math.min(Math.max(index - 1, 0), remaining.length - 1)]);
        else setDialog(null);
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("world:desktop-command", onDesktopCommand);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("world:desktop-command", onDesktopCommand);
    };
  }, [dialog, openAgent, recent.ids, recent.close]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable)) return;
      if (e.metaKey || e.ctrlKey || e.altKey || !started) return;
      if (e.key === "?" && !paletteOpen && !workbench) {
        e.preventDefault();
        setShortcutsOpen(true);
        return;
      }
      if (overlay) return;
      if (e.key === "Escape" && engineRef.current?.currentPlace()) {
        e.preventDefault();
        engineRef.current.leaveRoom();
        return;
      }
      const k = e.key.toLowerCase();
      if (k === "q" && canRead) setBoard(true);
      else if (k === "c" && canRead) setChatOpen(true);
      else if (k === "h" && canWrite) setHistory(null);
      else if (k === "k" && canWrite) setQuick({});
      else if (k === "n") openNotesRef.current();
      else if (k === "p" && canWrite) void summonRef.current();
      else if (k === "l" && canWrite) setListsOpen(true);
      else if (k === "r" && canRead) setReports({ id: null });
      else if (k === "t" && canWrite && terminalFeature?.state !== "unavailable") setTerminalOpen(true);
      else if (k === "o" && canWrite) setFolderShop(true);
      else if (k === "y") {
        setFocusSection(null);
        setYouOpen(true);
      }
      else if (k === "m") setMapShown((o) => !o);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [overlay, started, canRead, canWrite, isAdmin, terminalFeature?.state, paletteOpen, workbench]);

  const openNotes = () => setNotesOpen((o) => !o);

  useEffect(() => {
    if (!feed.length) return;
    const id = window.setTimeout(() => setFeed((prev) => prev.slice(1)), 9000);
    return () => window.clearTimeout(id);
  }, [feed]);

  const dialogAgent = dialog?.kind === "resident" ? allAgents.find((a) => a.id === dialog.id) ?? null : null;

  const closeDialog = useCallback(() => {
    setDialog(null);
    setInjected(null);
    canvasRef.current?.focus();
  }, []);
  const closeBoard = useCallback(() => {
    setBoard(false);
    canvasRef.current?.focus();
  }, []);
  const openCreateAgent = useCallback(() => {
    setBoard(false);
    setDialog(null);
    setCreatingAgent(true);
  }, []);
  const storeAgent = useCallback((agent: WorldAgent) => {
    setAgents((current) => {
      const found = current.some((item) => item.id === agent.id);
      return found ? current.map((item) => (item.id === agent.id ? agent : item)) : [...current, agent];
    });
  }, []);

  const onboardingStarted = useRef(false);
  useEffect(() => {
    if (!started || !me?.onboarding || !canWrite || onboardingStarted.current) return;
    onboardingStarted.current = true;
    void worldApi
      .startOnboarding()
      .then(async ({ pending, agent_id }) => {
        if (!pending || !agent_id) return;
        await Promise.all([load(), reloadMe()]);
        setBoard(false);
        setDialog({ kind: "resident", id: agent_id });
      })
      .catch(() => {
        onboardingStarted.current = false;
      });
  }, [started, me?.onboarding, canWrite, load, reloadMe]);

  const summonAssistant = async () => {
    const a = assistant ?? (await worldApi.ensureAssistant());
    if (!assistant) await Promise.all([load(), reloadMe()]);
    setBoard(false);
    setDialog({ kind: "resident", id: a.id });
  };

  const talkTarget = (): WorldAgent | null => {
    const open = dialog?.kind === "resident" ? allAgents.find((a) => a.id === dialog.id) : null;
    const near = focus?.kind === "resident" ? agents.find((a) => a.id === focus.id) : null;
    return open ?? near ?? assistant;
  };
  const [injected, setInjected] = useState<{ text: string; nonce: number; agentId: string } | null>(null);
  const talk = useHoldToTalk((text) => {
    const target = talkTarget();
    if (!target) {
      pushToast({ id: Date.now(), text: "Walk up to someone, or summon your main agent, before talking." });
      return;
    }
    speech().warm("tts");
    setDialog({ kind: "resident", id: target.id });
    setInjected({ text, nonce: Date.now(), agentId: target.id });
  });
  const voiceAgent = voiceAgentId ? agents.find((a) => a.id === voiceAgentId) ?? null : null;
  const openVoice = (id?: string) => {
    const target = id ? agents.find((a) => a.id === id) ?? null : talkTarget();
    if (!target) {
      pushToast({ id: Date.now(), text: "Walk up to someone, or summon your main agent, before starting a voice chat." });
      return;
    }
    if (engineChoice() === "live") primeLiveAudio();
    setDialog(null);
    setVoiceAgentId(target.id);
  };
  const talkRef = useRef(talk);
  talkRef.current = talk;
  useEffect(() => {
    if (talk.error) pushToast({ id: Date.now(), text: talk.error ?? "" });
  }, [talk.error]);

  useEffect(() => {
    const typing = (el: EventTarget | null) => {
      const t = el as HTMLElement | null;
      return Boolean(t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable));
    };
    const down = (e: KeyboardEvent) => {
      if (e.key === "Escape" && talkRef.current.state !== "idle" && !escapeTaken(e)) {
        takeEscape(e);
        talkRef.current.cancel();
        return;
      }
      if (e.key.toLowerCase() !== "v" || e.altKey || e.metaKey || e.ctrlKey || e.repeat || typing(e.target) || !started || !canWrite || otherMenu) return;
      e.preventDefault();
      talkRef.current.start();
    };
    const up = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() !== "v") return;
      void talkRef.current.stop();
    };
    window.addEventListener("keydown", down, true);
    window.addEventListener("keyup", up);
    return () => {
      window.removeEventListener("keydown", down, true);
      window.removeEventListener("keyup", up);
    };
  }, [started, canWrite, otherMenu]);

  const openNotesRef = useRef(openNotes);
  openNotesRef.current = openNotes;
  const summonRef = useRef(summonAssistant);
  summonRef.current = summonAssistant;

  const begin = () => {
    try {
      window.localStorage.setItem(WELCOMED_KEY, "1");
    } catch {}
    engineRef.current?.begin();
    canvasRef.current?.focus();
  };

  const startNamed = async (event: React.FormEvent) => {
    event.preventDefault();
    const name = callMe.trim();
    if (!name) return;
    setNaming(true);
    const ok = await rename(name);
    setNaming(false);
    if (!ok) {
      setNameProblem("Use letters or numbers, up to 40 characters.");
      return;
    }
    begin();
  };

  const focusAgent = focus?.kind === "resident" ? agents.find((a) => a.id === focus.id) : null;

  const renderAgentDialog = (
    agent: WorldAgent,
    opts: { onClose: () => void; key: string; onOpenAgent: (id: string) => void; embedded?: boolean; compact?: boolean },
  ) => (
    <AgentDialog
      key={opts.key}
      embedded={opts.embedded}
      compact={opts.compact}
      progress={progress[agent.id] ?? null}
      meId={me?.id ?? null}
      viewer={me ? { id: me.id, role: me.role } : null}
      isMain={agent.id === mainId}
      onMakeMain={async () => {
        await makeMain(agent.id);
        void load();
      }}
      agent={agent}
      portrait={portraitUrl(residentFor(agent).look, 6)}
      asides={asides}
      onAside={(sid) => setAsides((prev) => new Set(prev).add(sid))}
      agents={agents}
      onOpenAgent={opts.onOpenAgent}
      canInstall={isAdmin}
      onVoice={() => openVoice(agent.id)}
      onAgentUpdated={(updated) => {
        storeAgent(updated);
        void load();
      }}
      onHistory={(focus) => {
        closePanels();
        setHistory(agent.id);
        setHistoryFocus(focus ?? null);
      }}
      injected={injected}
      onVoiceSent={(id) => expectSpokenReply(`agent:${id}`)}
      access={agentAccess(agent)}
      onClose={opts.onClose}
    />
  );
  const agentAccess = (agent: WorldAgent) => ({
    read: canRead,
    use: canWrite && (isAdmin || agent.visibility === "public" || agent.owner_user_id === me?.id),
    manage: isAdmin || (canWrite && agent.owner_user_id === me?.id),
  });
  const chatAgents = agents
    .filter((a) => agentAccess(a).use)
    .sort((a, b) => {
      const rank = (x: WorldAgent) => (x.id === mainId ? 0 : x.status === "waiting" || x.status === "error" ? 1 : x.owner_user_id === me?.id ? 2 : 3);
      return rank(a) - rank(b) || a.name.localeCompare(b.name);
    });
  sideFrom.current = reports || listsOpen ? null : dialog?.kind === "resident" ? dialog.id : chatOpen && !chatChannel ? chatLast.agentId : null;
  const sideAgent = side.open ? chatAgents.find((a) => a.id === side.id) ?? chatAgents.find((a) => a.id === mainId) ?? chatAgents[0] ?? null : null;
  const sideToggle = chatAgents.length ? (
    <button
      type="button"
      onClick={() => setSide({ id: sideAgent?.id ?? side.id, open: !side.open })}
      aria-pressed={side.open}
      aria-label={side.open ? "Hide agent chat" : "Talk to an agent here"}
      title={side.open ? "Hide agent chat" : "Talk to an agent while you read"}
      className={cn(CHUNKY, "hidden h-10 items-center gap-1.5 px-3.5 text-w-sm font-semibold lg:inline-flex", side.open ? "bg-w-lemon" : "bg-w-card hover:bg-w-sand")}
    >
      <ChatCircleDots size={14} weight="bold" /> Chat
    </button>
  ) : null;
  const withSide = (panel: React.ReactNode) => (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-h-0 min-w-0 flex-1">{panel}</div>
      {sideAgent && <PaneResizer pane={sidePane} edge="left" label="Resize the agent chat" className="mx-1.5 hidden lg:block" />}
      {sideAgent && (
        <aside
          style={sidePane.style}
          aria-label={`Talk to ${sideAgent.name}`}
          onKeyDown={(e) => {
            const el = e.target as HTMLElement;
            if (e.key === "Escape" && el.matches("input, textarea, select, [contenteditable='true']")) {
              takeEscape(e);
              el.blur();
            }
          }}
          className={cn(PANEL, "pointer-events-auto hidden shrink-0 animate-w-slide-left flex-col overflow-hidden motion-reduce:animate-none lg:flex", sidePane.custom ? "w-[var(--pane-w)]" : "w-[24rem] xl:w-[28rem] 2xl:w-[32rem]")}
        >
          <div className="flex items-center gap-2 border-b-2 border-dashed border-w-line2 bg-w-sand px-3 py-2">
            <AgentFace agent={sideAgent} size={24} />
            <WSelect
              size="sm"
              ariaLabel="Agent to talk to"
              value={sideAgent.id}
              onChange={(id) => setSide({ id, open: true, auto: side.auto })}
              className="min-w-0 flex-1 font-semibold"
              options={chatAgents.map((a) => ({ value: a.id, label: a.name, hint: a.id === mainId ? "main" : undefined }))}
            />
            <button type="button" onClick={() => setSide({ id: sideAgent.id, open: false })} aria-label="Hide agent chat" title="Hide agent chat" className={cn(CHUNKY, "bg-w-card p-1.5 hover:bg-w-paper")}>
              <X size={14} weight="bold" />
            </button>
          </div>
          <div className="min-h-0 flex-1">
            {renderAgentDialog(sideAgent, {
              key: `side:${sideAgent.id}`,
              embedded: true,
              compact: true,
              onClose: () => setSide({ id: sideAgent.id, open: false }),
              onOpenAgent: openAgent,
            })}
          </div>
        </aside>
      )}
    </div>
  );

  const workshopFeature = useFeature("workshop");
  useEffect(() => {
    if (!canWrite) return;
    let stopped = false;
    const loadRunners = () => {
      void worldApi.runners().then((list) => !stopped && setRunners(list)).catch(() => {});
    };
    loadRunners();
    const stop = pollWhileVisible(loadRunners, 60_000);
    window.addEventListener("world-reconnected", loadRunners);
    return () => {
      stopped = true;
      stop();
      window.removeEventListener("world-reconnected", loadRunners);
    };
  }, [canWrite]);
  const myAgents = useMemo(() => agents.filter((a) => a.owner_user_id === me?.id).sort((a, b) => Number(b.id === mainId) - Number(a.id === mainId)), [agents, me?.id, mainId]);
  const devices = useMemo(
    () =>
      runners.map((r) => ({
        id: r.id,
        name: r.name,
        state: (r.status !== "online" ? "offline" : agents.some((a) => a.runner_id === r.id && a.jobs?.length) ? "busy" : "online") as "busy" | "online" | "offline",
      })),
    [runners, agents],
  );
  const activeTimers = items.filter((i) => i.kind === "timer" && i.status === "active" && i.owner_user_id === me?.id);
  const nextTimer = activeTimers.reduce<{ label: string; at: number } | null>((best, i) => {
    const at = i.fires_at ? Date.parse(i.fires_at) : NaN;
    return Number.isFinite(at) && (!best || at < best.at) ? { label: i.name, at } : best;
  }, null);
  const runCount = agents.reduce((n, a) => n + (a.jobs?.length ?? 0), 0);
  const reading = agents.some((a) => a.jobs?.some((j) => j.kind === "review"));
  const signsKey = JSON.stringify([nextTimer, notes.unread + chatUnread, runCount, devices, sightingAt, reading]);
  useEffect(() => {
    engineRef.current?.setSigns({ nextTimer, unread: notes.unread + chatUnread, runs: runCount, devices, sightingAt, reading });
  }, [signsKey, engineGen]);
  const civicCtx: CivicContext = {
    agents: myAgents.map((a) => ({ id: a.id, name: a.name })),
    devices,
    unread: notes.unread,
    chatUnread,
    timers: activeTimers.length,
    budgetAlarm: agents.some(budgetPaused) || Date.now() - budgetAlarmAt < 600_000,
    runs: runCount,
  };
  const roomFor = (placeId: string): PlaceRoom | null => {
    const place = engineRef.current?.placeOf(placeId);
    if (!place) return null;
    if (place.kind === "civic") return civicRoom(place.ref as CivicId, civicCtx);
    if (place.kind === "campus") return campusRoom(scene?.placeId === placeId ? scene.room : undefined, campusRef.current);
    if (place.kind !== "house") return null;
    const agent = agents.find((a) => a.id === place.ref);
    if (!agent) return null;
    const kit = kits[agent.id];
    return houseRoom({
      agentId: agent.id,
      name: agent.name,
      skills: kit?.skills ?? [],
      servers: kit?.servers ?? [],
      hasNotes: kit?.notes ?? false,
      workshops: canWrite && agentAccess(agent).use,
      busy: Boolean(agent.jobs?.length),
    });
  };
  const mentors = useMemo(
    () =>
      [...items, ...(regionDetail?.items ?? [])]
        .filter((i) => i.kind === "npc" && (typeof i.data.skill === "string" || typeof i.data.classroom === "string"))
        .map((i) => ({ id: i.id, name: i.name, skill: skillSlug(String(i.data.classroom ?? i.data.skill)), look: lookOfNpc(i, agents) })),
    [items, regionDetail, agents],
  );
  campusRef.current = { ...campusCards, mentors };
  const inCampus = scene?.placeId === "campus";
  const loadCampus = useCallback(() => {
    void Promise.all([lokaApi.challenges(), lokaApi.adventures()])
      .then(([cards, adventures]) => setCampusCards({ cards, adventures }))
      .catch(() => {});
  }, []);
  useEffect(() => {
    if (!inCampus) return;
    loadCampus();
    const on = (e: Event) => {
      const type = (e as CustomEvent<WorldEvent>).detail.type;
      if (type.startsWith("challenge.") || type.startsWith("adventure.")) loadCampus();
    };
    window.addEventListener("world-event", on);
    return () => window.removeEventListener("world-event", on);
  }, [inCampus, loadCampus, loka]);
  exitRef.current = (to: string) => {
    setScene((s) => {
      if (!s) return s;
      const from = s.room?.startsWith("class:") ? s.room.slice(6) : null;
      const door = from && to === "hall" ? buildRoom(hallRoom(campusRef.current).spec).things.find((th) => th.id === classDoor(from)) : null;
      return { ...s, room: to, at: door ? { x: door.x, y: door.y + 4 } : undefined };
    });
  };
  const sceneRoom = scene ? roomFor(scene.placeId) : null;
  sceneRoomRef.current = sceneRoom;
  const sceneKey = sceneRoom ? JSON.stringify(sceneRoom.spec) : "";
  useEffect(() => {
    const room = sceneRoomRef.current;
    const engine = engineRef.current;
    if (!room || !scene || !engine) return;
    roomActions.current = room.actions;
    engine.enterRoom(buildRoom(room.spec), scene.placeId, { at: scene.at });
  }, [sceneKey, scene, engineGen]);

  const loadKit = (agent: WorldAgent) => {
    void worldApi
      .kit(agent.id)
      .then((k) =>
        setKits((prev) => ({
          ...prev,
          [agent.id]: {
            skills: k.skills.filter((x) => x.name !== notesName(agent)).map((x) => ({ id: x.id, name: x.name })),
            servers: k.mcp_servers.map((m) => ({ id: m.id, name: m.name, enabled: m.enabled })),
            notes: k.skills.some((x) => x.name === notesName(agent)),
          },
        })),
      )
      .catch(() => {});
  };
  const openWorkshopFor = (agentId: string | null) => {
    if (workshopFeature?.state === "unavailable") {
      setSheet({ kind: "setup", feature: "workshop", title: "Workshop" });
      return;
    }
    if (!agentId) {
      setWorkbench({});
      return;
    }
    const agent = agentsRef.current.find((a) => a.id === agentId);
    const sessions = [...new Set((agent?.jobs ?? []).filter((j) => j.kind !== "review" && j.session_id).map((j) => j.session_id as string))];
    setWorkbench(sessions.length === 1 ? sessions[0] : { agentId });
  };
  const openSurface = (surface: Surface) => {
    if (surface === "timers") setListsOpen(true);
    else if (surface === "schedules" || surface === "usage" || surface === "feeds") setSheet({ kind: "agents", surface });
    else if (surface === "sessions") setHistory(null);
    else if (surface === "reports") setReports({ id: null });
    else if (surface === "workshop") openWorkshopFor(null);
    else if (surface === "terminal") {
      if (terminalFeature?.state === "unavailable") setSheet({ kind: "setup", feature: "terminal", title: "Terminal" });
      else setTerminalOpen(true);
    } else if (surface === "devices") openDevices();
    else if (!notesOpen) openNotesRef.current();
  };
  const followOpens = (opens: Opens, name: string) => {
    if (opens.kind === "channel") window.dispatchEvent(new CustomEvent("world:open-channel", { detail: { id: opens.ref } }));
    else if (opens.kind === "quest") window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: opens.ref, agentId: null } }));
    else if (opens.kind === "list") setListsOpen(true);
    else if (opens.kind === "url") window.open(opens.ref, "_blank", "noopener,noreferrer");
    else if (opens.kind === "surface") openSurface(opens.ref);
    else if (opens.kind === "challenge") setLoka({ kind: "academy", challengeId: opens.ref });
    else void travel(opens.ref, name);
  };
  const runAction = (a: PlaceAction) => {
    if (a.kind === "room") {
      setScene((s) => (s ? { ...s, room: a.room, at: undefined } : s));
      return;
    }
    if (a.kind === "academy") {
      setLoka({ kind: "academy", challengeId: a.challengeId ?? null });
      return;
    }
    if (a.kind === "npc") {
      const item = itemsRef.current.find((i) => i.id === a.id) ?? regionDetailRef.current?.items.find((i) => i.id === a.id);
      if (item) setLoka({ kind: "npc", id: item.id, name: item.name, data: item.data });
      return;
    }
    if (a.kind === "surface") openSurface(a.surface);
    else if (a.kind === "agent") setDialog({ kind: "resident", id: a.agentId });
    else if (a.kind === "skills" || a.kind === "notes" || a.kind === "kit") setSheet({ kind: a.kind, agentId: a.agentId });
    else if (a.kind === "notes-all") setSheet({ kind: "notes-all" });
    else if (a.kind === "workbench") openWorkshopFor(a.agentId);
    else if (a.kind === "history") setHistory(a.agentId);
    else if (a.kind === "chat") setChatOpen(true);
    else if (a.kind === "slack") openSection("world-slack");
    else {
      const target = assistant ?? myAgents[0];
      if (target) setDialog({ kind: "resident", id: target.id });
      else setChatOpen(true);
    }
  };
  interactRef.current = (t: Target) => {
    if (t.kind === "board") {
      setBoard(true);
      return;
    }
    if (t.kind === "thing") {
      const action = roomActions.current[t.id];
      if (action) runAction(action);
      return;
    }
    if (t.kind === "place") {
      const place = engineRef.current?.placeOf(t.id);
      if (!place) return;
      if (place.kind === "portal" || place.kind === "exit") {
        void travel(place.to ?? HEARTH, place.name);
        return;
      }
      if (place.kind === "academy") {
        setLoka({ kind: "academy" });
        return;
      }
      if (place.kind === "campus") {
        setScene({ placeId: t.id, title: place.name, room: "hall" });
        return;
      }
      if (place.kind === "building") {
        const item = itemsRef.current.find((i) => i.id === place.ref) ?? regionDetailRef.current?.items.find((i) => i.id === place.ref);
        const opens = parseOpens(item?.data.opens);
        if (item && opens) followOpens(opens, item.name);
        return;
      }
      if (!roomFor(t.id)) return;
      const agent = place.kind === "house" ? agentsRef.current.find((a) => a.id === place.ref) : null;
      if (agent) loadKit(agent);
      setScene({ placeId: t.id, title: place.name });
      return;
    }
    if (t.kind === "folk") {
      const item = itemsRef.current.find((i) => i.id === t.id) ?? regionDetailRef.current?.items.find((i) => i.id === t.id);
      if (item) setLoka({ kind: "npc", id: item.id, name: item.name, data: item.data });
      return;
    }
    setDialog(t);
  };

  const fixRefusal = (fix: "budget" | "keys") => {
    setLoka(null);
    if (fix === "budget") setSheet({ kind: "agents", surface: "usage" });
    else openSection("world-environment");
  };

  const windowKey = voiceAgent
    ? "voice"
    : creatingAgent
      ? "create"
      : youOpen
        ? "you"
        : terminalOpen
          ? "terminal"
          : folderShop
            ? "workshop"
            : changelogOpen
              ? "changelog"
              : reports
                ? "reports"
                : listsOpen
                  ? "lists"
                  : chatOpen
                      ? "chat"
                      : quick !== false
                        ? "quick"
                      : history !== false
                        ? "history"
                        : board
                          ? "board"
                          : loka && !dialogAgent
                            ? `loka:${loka.kind}`
                            : sheet && !dialogAgent
                              ? "sheet"
                              : dialogAgent
                                ? `agent:${dialogAgent.id}`
                                : dialog?.kind ?? "none";

  return (
    <MotionConfig reducedMotion="user">
    <div ref={rootRef} className="fixed inset-0 overflow-clip bg-[#17361f] text-w-text select-none">
      <canvas
        ref={canvasRef}
        tabIndex={0}
        aria-label="Loka, where your agents live and work. Open Chat or the quest board to reach everyone without walking."
        className={cn(PIXEL, "absolute inset-0 h-full w-full touch-none outline-none")}
      />

      <div className="pointer-events-none absolute inset-x-0 top-20 z-30 flex justify-center px-4 sm:top-24">
        <OfflineBanner controlPlane={controlPlane} />
      </div>

      <div className="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-start justify-between gap-3 p-3 sm:p-4">
        <PlayerCard
          me={me}
          progress={record}
          onOpen={() => openSection("page:profile")}
          onProgress={() => setLoka({ kind: "academy", tab: "progress" })}
        />
        <div className="pointer-events-auto flex min-w-0 flex-wrap items-center justify-end gap-2">
          {canWrite && <DesktopStatusButton key={me?.id ?? "guest"} controlPlane={controlPlane} onOpen={openDevices} />}
          <div className={HUD_GROUP} role="radiogroup" aria-label="Time of day">
            {TIMES.map(({ id, label, Icon }) => (
              <button
                key={id}
                type="button"
                role="radio"
                aria-checked={time === id}
                aria-label={label}
                title={label}
                onClick={() => setTime(id)}
                className={time === id ? HUD_SEG_ON : HUD_SEG}
              >
                <Icon size={17} weight={time === id ? "fill" : "bold"} />
              </button>
            ))}
          </div>
          {!workbench && (
            <SearchChip
              onSearch={() => {
                setPaletteQuery("");
                setPaletteOpen(true);
              }}
            />
          )}
          <div className={HUD_GROUP} role="group" aria-label="Sound and map">
            <MusicButton grouped open={musicOpen} onToggle={() => setMusicOpen((v) => !v)} />
            {canWrite && <SpeakerToggle grouped on={voiceReplies} onToggle={toggleVoiceReplies} />}
            <MinimapChip grouped shown={Boolean(mapShown)} onToggle={() => setMapShown((o) => !o)} />
          </div>
          <BellButton unread={notes.unread} onClick={openNotes} />
        </div>
      </div>


      <AnimatePresence>
      {notesOpen && (
        <Fragment key="notes">
          <div className="absolute inset-0 z-[45]" onPointerDown={() => setNotesOpen(false)} />
          <motion.div {...POPOVER_MOTION} className="absolute right-3 top-16 z-50 flex max-h-[min(72vh,42rem)] w-[min(27rem,calc(100vw-1.5rem))] origin-top-right sm:right-4 sm:top-[4.25rem]">
            <NotificationsPanel
              notes={notes}
              agents={agents}
              onChanged={loadNotes}
              onClose={() => setNotesOpen(false)}
              onOpenAgent={(id) => {
                setNotesOpen(false);
                if (chatOpen && chatAgents.some((a) => a.id === id)) {
                  setChatChannel(null);
                  setChatFocus({ id, nonce: Date.now() });
                } else {
                  closePanels();
                  setDialog({ kind: "resident", id });
                }
              }}
            />
          </motion.div>
        </Fragment>
      )}
      </AnimatePresence>

      {!workbench && (
        <TopSearch
          onSearch={() => {
            setPaletteQuery("");
            setPaletteOpen(true);
          }}
        />
      )}

      {canRead && (
        <div aria-hidden={overlay || undefined} className={cn("pointer-events-none absolute inset-x-0 bottom-3 z-20 flex justify-center px-3 sm:bottom-4", HUD_FADE, hudHide)}>
          <Hotbar
            slots={[
              { key: "Q", label: "Quests", group: 0, icon: HOTBAR_ICONS.Quests, badge: agents.reduce((n, a) => n + (a.jobs?.length ?? 0), 0), onClick: () => setBoard(true) },
              { key: "C", label: "Chat", group: 0, icon: HOTBAR_ICONS.Chat, badge: chatUnread, onClick: () => setChatOpen(true) },
              ...(canWrite
                ? [{ key: "L", label: "Lists", group: 0, icon: HOTBAR_ICONS.Lists, badge: items.filter((i) => i.kind === "timer" && i.status === "active").length, onClick: () => setListsOpen(true) }]
                : []),
              { key: "R", label: "Reports", group: 0, icon: HOTBAR_ICONS.Reports, onClick: () => setReports({ id: null }) },
              ...(canWrite ? [{ key: "P", label: assistant ? assistant.name : "Main agent", hint: assistant ? `${assistant.name}, your main agent` : "Summon your main agent", group: 1, icon: HOTBAR_ICONS.Main, image: assistant ? portraits[assistant.id] : undefined, tone: "gold" as const, onClick: () => void summonAssistant() }] : []),
              ...(canWrite ? [{ key: "K", label: "Quick chat", hint: "A side chat with an agent's memory", group: 1, icon: HOTBAR_ICONS.Quick, onClick: () => setQuick({}) }] : []),
              ...(canWrite ? [{ key: "O", label: "Workshop", group: 2, icon: HOTBAR_ICONS.Workshop, onClick: () => setFolderShop(true) }] : []),
              ...(canWrite && terminalFeature?.state !== "unavailable" ? [{ key: "T", label: "Terminal", group: 2, icon: HOTBAR_ICONS.Terminal, onClick: () => setTerminalOpen(true) }] : []),
              ...(canWrite
                ? [
                    {
                      key: "V",
                      label: talk.state === "listening" ? "Listening" : "Talk",
                      hint: "Hold to talk, tap for voice chat",
                      group: 3,
                      icon: HOTBAR_ICONS.Talk,
                      tone: talk.state === "listening" ? ("live" as const) : ("plain" as const),
                      onPress: () => {
                        talkPressedAt.current = Date.now();
                        talk.start();
                      },
                      onRelease: () => {
                        const tap = Date.now() - talkPressedAt.current < 300;
                        void talk.stop();
                        if (tap) openVoice();
                      },
                    },
                  ]
                : []),
            ]}
          />
        </div>
      )}

      <SpeechPill status={speechStatus} talk={talk.state} onCancel={talk.cancel} />

      <AnimatePresence>{engineGen === 0 && <WorldSplash key="splash" exit />}</AnimatePresence>

      <Toasts toasts={toasts} dismiss={dismissToast} />

      <AnimatePresence>
        {!started && (
          <motion.div
            key="welcome"
            initial={false}
            exit={{ opacity: 0, transition: { duration: 0.25, ease: EASE_SMOOTH } }}
            aria-hidden={overlay || undefined}
            className={cn(HUD_FADE, hudHide, "absolute inset-0 z-10 flex items-end bg-w-scrim/30 p-3 sm:items-center sm:p-10 lg:p-16")}
          >
            <motion.section
              aria-labelledby="welcome-title"
              initial={{ opacity: 0, y: 12, scale: 0.98 }}
              animate={{ opacity: 1, y: 0, scale: 1, transition: { duration: 0.35, ease: EASE_SMOOTH, delay: 0.15 } }}
              exit={{ opacity: 0, y: 8, scale: 0.98, transition: { duration: 0.2, ease: EASE_SMOOTH } }}
              className={cn(PANEL, "w-full max-w-md p-5 sm:p-7")}
            >
              <h1 id="welcome-title" className={cn(PIXEL, "text-balance text-[1.75rem] leading-tight text-w-strong sm:text-[2rem]")}>
                {me?.name ? (returning && !me.onboarding ? `Welcome back, ${me.name.split(" ")[0]}` : `Welcome to Loka, ${me.name.split(" ")[0]}`) : "Welcome to Loka"}
              </h1>
              <p className="mt-3 text-pretty text-w-base leading-relaxed text-w-text2">
                {me?.onboarding
                  ? "Your AI teammates live here. Give them a quest and they write the code, open the pull request and tell you how it went. Your main agent will help you set things up."
                  : agents.length
                    ? (() => {
                        const busy = agents.filter((a) => a.jobs?.length).length;
                        const crew = `${agents.length} ${agents.length === 1 ? "agent lives" : "agents live"} here`;
                        return `${crew}${busy ? `, and ${busy} ${busy === 1 ? "is" : "are"} out on a quest` : ""}. Walk up to anyone to chat, or hand out work from the quest board.`;
                      })()
                    : "Nobody lives here yet. Summon your main agent and ask it to bring in a few residents."}
              </p>
              {me && !me.name ? (
                <form onSubmit={(e) => void startNamed(e)} className="mt-6">
                  <label htmlFor="call-me" className="block text-w-sm font-semibold text-w-strong">
                    What should your agents call you?
                  </label>
                  <div className="mt-2 flex flex-wrap gap-2">
                    <input
                      id="call-me"
                      autoFocus
                      value={callMe}
                      maxLength={40}
                      onChange={(e) => {
                        setCallMe(e.target.value);
                        setNameProblem(null);
                      }}
                      placeholder="Your name"
                      className={cn(INPUT, "h-10 py-0")}
                    />
                    <button type="submit" disabled={!callMe.trim() || naming} className={BTN_PRIMARY}>
                      Start exploring
                      <ArrowRight size={15} weight="bold" />
                    </button>
                  </div>
                  {nameProblem && (
                    <p role="alert" className="mt-2 text-w-sm text-w-danger">
                      {nameProblem}
                    </p>
                  )}
                </form>
              ) : (
                <button type="button" onClick={begin} autoFocus className={cn(BTN_PRIMARY, "mt-6")}>
                  Start exploring
                  <ArrowRight size={15} weight="bold" />
                </button>
              )}
              <div className="mt-6 border-t-2 border-dashed border-w-line pt-4">
                {touch ? (
                  <p className="text-w-sm text-w-text2">Tap to walk. Tap someone to talk.</p>
                ) : (
                  <ul className="flex flex-wrap items-center gap-x-4 gap-y-2 text-w-xs text-w-text2" aria-label="Controls">
                    <li className="flex items-center gap-1.5">
                      <span className="flex gap-1">
                        {["W", "A", "S", "D"].map((k) => (
                          <Keycap key={k} size="sm">
                            {k}
                          </Keycap>
                        ))}
                      </span>
                      <span className="text-w-muted">or</span>
                      <span className="flex gap-1" aria-label="arrow keys">
                        {["↑", "←", "↓", "→"].map((k) => (
                          <Keycap key={k} size="sm">
                            {k}
                          </Keycap>
                        ))}
                      </span>
                      <span>walk</span>
                    </li>
                    <li className="flex items-center gap-1.5">
                      <Keycap size="sm">Shift</Keycap>
                      <span>run</span>
                    </li>
                    <li className="flex items-center gap-1.5">
                      <Keycap size="sm">E</Keycap>
                      <span>talk</span>
                    </li>
                  </ul>
                )}
              </div>
            </motion.section>
          </motion.div>
        )}
      </AnimatePresence>

      {started && (
        <div aria-hidden={overlay || undefined} className={cn("pointer-events-none absolute inset-x-0 flex p-3 sm:p-4", HUD_FADE, hudHide, touch && focus ? "bottom-24 justify-center sm:bottom-28" : "bottom-0 justify-start")}>
          {touch && focus ? (
            <button
              type="button"
              onClick={() => focus && engineRef.current?.interact(focus)}
              className={cn(PIXEL, "pointer-events-auto animate-w-rise rounded-xl border-[3px] border-w-ink bg-w-paper px-6 py-3 text-w-lg shadow-[0_4px_0_rgb(var(--w-ink))] transition-[transform,box-shadow] active:translate-y-[4px] active:shadow-none motion-reduce:animate-none")}
            >
              {focusAgent ? `Talk to ${focusAgent.name}` : engineRef.current?.labelOf(focus) ?? "Take a look"}
            </button>
          ) : scene ? null : (
            touch ? (
              <p className={cn(PIXEL, "rounded-lg bg-w-scrim/70 px-3 py-1.5 text-w-xs text-w-onscrim/85")}>Tap to walk. Tap someone to talk.</p>
            ) : (
              <p className="hidden items-center gap-3 rounded-lg bg-w-scrim/70 px-2.5 xl:flex py-1.5 text-w-xs text-w-onscrim/85 backdrop-blur-[2px]">
                <span className="flex items-center gap-1.5">
                  <span className="flex gap-0.5">{["W", "A", "S", "D"].map((k) => <Keycap key={k} tone="scrim" size="sm">{k}</Keycap>)}</span>
                  walk
                </span>
                <span className="flex items-center gap-1.5"><Keycap tone="scrim" size="sm">E</Keycap>talk</span>
                <span className="flex items-center gap-1.5"><Keycap tone="scrim" size="sm">?</Keycap>shortcuts</span>
              </p>
            )
          )}
        </div>
      )}

      <div className="pointer-events-none absolute right-3 top-[4.25rem] z-40 flex flex-col items-end gap-2 sm:right-4 sm:top-[4.5rem]">
        <MusicDock open={musicOpen && started} meId={me?.id ?? null} onClose={() => setMusicOpen(false)} />
        {mapShown && started && !scene && (
          <div aria-hidden={overlay || undefined} className={cn(HUD_FADE, hudHide)}>
          <Minimap
            engine={engineRef}
            area={regionId === HEARTH || districtOf(regionId) ? areaName(regionId) : regionDetail?.region.name ?? "Loka"}
            open={mapOpen}
            onToggle={() => setMapOpen((o) => !o)}
            onTravel={() => {
              setMapOpen(false);
              setLoka({ kind: "travel" });
            }}
          />
          </div>
        )}
      </div>

      {offer && (
        <div aria-hidden={overlay || undefined} className={cn("pointer-events-none absolute inset-x-0 bottom-16 z-30 flex justify-center px-3 sm:bottom-20", HUD_FADE, hudHide)}>
          <OfferCard
            title={offer.title}
            minutes={offer.minutes}
            from={assistant?.name ?? "A mentor"}
            onDismiss={dismissOffer}
            onAccept={() => {
              const challengeId = offer.challengeId;
              dismissOffer();
              closePanels();
              setLoka({ kind: "academy", challengeId });
            }}
          />
        </div>
      )}

      {started && scene && (
        <div aria-hidden={overlay || undefined} className={cn("pointer-events-none absolute inset-x-0 top-[4.25rem] z-20 flex justify-center px-3 sm:top-[4.5rem]", HUD_FADE, hudHide)}>
          <div className="pointer-events-auto flex items-center gap-2 rounded-lg border-2 border-w-ink bg-w-paper/95 py-1 pl-3 pr-1 text-w-sm shadow-[0_3px_0_rgb(var(--w-ink))]">
            <span className={cn(PIXEL, "max-w-[12rem] truncate")}>{sceneRoom?.spec.title ?? scene.title}</span>
            {!touch && <span className="hidden text-w-muted sm:inline">Esc or the door to leave</span>}
            <button type="button" onClick={() => engineRef.current?.leaveRoom()} className={BTN_SECONDARY}>
              Leave
            </button>
          </div>
        </div>
      )}

      {feed.length > 0 && (
        <ul aria-hidden={overlay || undefined} className={cn("pointer-events-none absolute bottom-14 left-3 z-20 space-y-1.5 sm:bottom-16 sm:left-4", HUD_FADE, hudHide)}>
          {feed.map((f) => (
            <li key={f.id} className="flex items-center gap-2 rounded-lg border-2 border-w-ink bg-w-paper/95 px-3 py-1.5 text-w-sm shadow-[0_3px_0_rgb(var(--w-ink))]">
              <Scroll size={14} weight="bold" className="shrink-0 text-w-warn" />
              {f.text}
              {f.count > 1 && <span className="font-semibold text-w-warn">x{f.count}</span>}
            </li>
          ))}
        </ul>
      )}

      <AnimatePresence>
      {windowOpen && (
        <motion.div
          key="window"
          {...scrimMotion(menu ? 2 : 0)}
          className={cn(
            "absolute inset-0 z-30 flex justify-center p-3 sm:p-6",
            menu ? "items-stretch bg-w-scrim/55 backdrop-blur-[2px]" : "items-end bg-w-scrim/30",
          )}
          onPointerDown={(e) => {
            if (e.target === e.currentTarget) {
              if (board) closeBoard();
              else if (quick !== false) setQuick(false);
              else if (history !== false) setHistory(false);
              else if (chatOpen) setChatOpen(false);
              else if (listsOpen) setListsOpen(false);
              else if (reports) setReports(null);
              else if (changelogOpen) setChangelogOpen(false);
              else if (terminalOpen) setTerminalOpen(false);
              else if (folderShop) setFolderShop(false);
              else if (youOpen) setYouOpen(false);
              else if (creatingAgent) setCreatingAgent(false);
              else if (voiceAgentId) setVoiceAgentId(null);
              else if (loka) setLoka(null);
              else if (sheet) setSheet(null);
              else closeDialog();
            }
          }}
        >
          <AnimatePresence mode="popLayout" initial={false}>
          <motion.div
            key={windowKey}
            {...MODAL_MOTION}
            data-find-scope
            className={cn("w-full origin-center", menu ? "mx-auto flex h-full flex-col" : "max-w-3xl 2xl:max-w-4xl")}
          >
            {voiceAgent ? (
              <VoiceMode
                agent={voiceAgent}
                portrait={portraitUrl(residentFor(voiceAgent).look, 6)}
                onClose={() => {
                  setVoiceAgentId(null);
                  canvasRef.current?.focus();
                }}
                onOpenChat={() => {
                  setVoiceAgentId(null);
                  setDialog({ kind: "resident", id: voiceAgent.id });
                }}
              />
            ) : creatingAgent ? (
              <CreateAgentDialog
                onCreated={(agent) => {
                  storeAgent(agent);
                  setCreatingAgent(false);
                  setDialog({ kind: "resident", id: agent.id });
                  void load();
                }}
                onClose={() => setCreatingAgent(false)}
              />
            ) : youOpen ? (
              <YouPanel
                key={focusSection?.at ?? 0}
                me={me}
                assistant={assistant}
                voiceReplies={voiceReplies}
                onToggleVoice={toggleVoiceReplies}
                onRename={rename}
                onStatus={setStatus}
                onOpenAssistant={() => {
                  setYouOpen(false);
                  void summonAssistant();
                }}
                onOpenPro={() => setProOpen(true)}
                controlPlane={controlPlane}
                focusSection={focusSection}
                onClose={() => setYouOpen(false)}
              />
            ) : terminalOpen ? (
              <TerminalPanel meId={me?.id ?? null} onClose={() => setTerminalOpen(false)} />
            ) : folderShop ? (
              <FolderWorkshop
                agents={agents}
                meId={me?.id ?? null}
                onClose={() => setFolderShop(false)}
                onHistory={() => {
                  setFolderShop(false);
                  setHistory(null);
                }}
              />
            ) : changelogOpen ? (
              <ChangelogPanel onClose={() => setChangelogOpen(false)} />
            ) : reports ? (
              withSide(
                <ReportsPanel
                  agents={agents}
                  meId={me?.id ?? null}
                  initialId={reports.id}
                  sideToggle={sideToggle}
                  onClose={() => setReports(null)}
                  onOpenAgent={(id) => {
                    setReports(null);
                    setDialog({ kind: "resident", id });
                  }}
                />,
              )
            ) : listsOpen ? (
              withSide(<ListsPanel items={items} agents={agents} meId={me?.id ?? null} sideToggle={sideToggle} onClose={() => setListsOpen(false)} onChanged={() => void loadItems()} />)
            ) : chatOpen ? (
              <ChatPanel
                meId={me?.id ?? null}
                agents={agents}
                canPost={canWrite}
                initialChannel={chatChannel ?? (chatAgent ? null : chatLast.channelId)}
                initialAgent={
                  chatChannel
                    ? null
                    : chatAgent ?? (chatLast.agentId && chatAgents.some((a) => a.id === chatLast.agentId) ? chatLast.agentId : chatLast.channelId ? null : mainId)
                }
                onSelect={rememberChat}
                talkable={chatAgents}
                mainId={mainId}
                renderAgent={(agent, nav) =>
                  renderAgentDialog(agent, { key: `chat:${agent.id}`, embedded: true, onClose: nav.back, onOpenAgent: nav.open })
                }
                onClose={() => {
                  setChatOpen(false);
                  setChatChannel(null);
                  setChatAgent(null);
                }}
                onOpenAgent={(id) => {
                  setChatOpen(false);
                  setChatChannel(null);
                  setDialog({ kind: "resident", id });
                }}
                onUnreadChange={() => {
                  loadChatUnread();
                  loadNotes();
                }}
                agentUnread={notes.by_agent}
                focusAgent={chatFocus}
                focusChannel={chatChannelFocus}
              />
            ) : quick !== false ? (
              <QuickChatWindow
                key={`${quick.agentId ?? ""}:${quick.sessionId ?? ""}`}
                agents={myAgents}
                mainAgentId={assistant?.id ?? null}
                meId={me?.id ?? null}
                initialAgentId={quick.agentId ?? null}
                initialSessionId={quick.sessionId ?? null}
                onOpenAgent={(id) => {
                  setQuick(false);
                  setDialog({ kind: "resident", id });
                }}
                onClose={() => setQuick(false)}
              />
            ) : history !== false && historyFocus?.taskId && !historyFocus.sessionId ? (
              <QuestRoom
                key={historyFocus.taskId}
                taskId={historyFocus.taskId}
                agents={agents}
                meId={me?.id ?? null}
                onOpenAgent={(id) => {
                  setHistory(false);
                  setHistoryFocus(null);
                  setDialog({ kind: "resident", id });
                }}
                onOpenSession={(sessionId, agentId) => {
                  setHistory(agentId);
                  setHistoryFocus({ sessionId });
                }}
                onClose={() => {
                  setHistory(false);
                  setHistoryFocus(null);
                }}
              />
            ) : history !== false ? (
              <HistoryPanel
                key={`${history}:${historyFocus?.sessionId ?? historyFocus?.taskId ?? ""}`}
                agents={agents}
                initialAgentId={history}
                focus={historyFocus}
                onClose={() => {
                  setHistory(false);
                  setHistoryFocus(null);
                }}
              />

            ) : board ? (
              <Board
                key={String(board)}
                initialView={board === "teams" ? "teams" : "quests"}
                agents={agents}
                archived={archivedAgents}
                viewer={me ? { id: me.id, role: me.role } : null}
                portraits={portraits}
                places={items.filter((i) => i.kind === "building" || i.kind === "decoration" || i.kind === "critter")}
                onVisitPlace={(id) => {
                  setBoard(false);
                  const ok = engineRef.current?.walkToPlacement(id) ?? false;
                  canvasRef.current?.focus();
                  if (!ok) pushToast({ id: Date.now(), text: "That spot could not fit on the map yet." });
                  return ok;
                }}
                onClose={closeBoard}
                canCreate={canWrite}
                onCreate={openCreateAgent}
                onOpen={(id) => {
                  setBoard(false);
                  setDialog({ kind: "resident", id });
                }}
                onOpenQuest={(task) => {
                  setBoard(false);
                  setHistory(task.assigned_agent_id);
                  setHistoryFocus({ taskId: task.id });
                }}
                onVisit={(id) => {
                  setBoard(false);
                  engineRef.current?.walkTo({ kind: "resident", id });
                  canvasRef.current?.focus();
                }}
              />
            ) : loka && !dialogAgent ? (
              <div className="mx-auto flex max-h-full w-full max-w-3xl">
                {loka.kind === "npc" ? (
                  <NpcTalkSheet
                    key={loka.id}
                    npc={loka}
                    meId={me?.id ?? null}
                    agents={agents}
                    onFix={fixRefusal}
                    onSay={(text) => engineRef.current?.folkSay(loka.id, text)}
                    onClose={() => {
                      setLoka(null);
                      canvasRef.current?.focus();
                    }}
                  />
                ) : loka.kind === "academy" ? (
                  <AcademySheet
                    meId={me?.id ?? null}
                    initial={loka.challengeId ?? null}
                    initialTab={loka.tab}
                    waitingOn={waitingOn}
                    onFix={fixRefusal}
                    onClose={() => {
                      setLoka(null);
                      setWaitingOn(null);
                      canvasRef.current?.focus();
                    }}
                  />
                ) : (
                  <TravelSheet
                    regions={regions}
                    current={regionId}
                    visited={visited}
                    onTravel={(id) => {
                      setLoka(null);
                      void travel(id);
                    }}
                    onClose={() => setLoka(null)}
                  />
                )}
              </div>
            ) : sheet && !dialogAgent ? (
              <div className="mx-auto flex max-h-full w-full max-w-3xl">
                <PlaceSheetView
                  sheet={sheet}
                  agents={agents}
                  meId={me?.id ?? null}
                  isAdmin={isAdmin}
                  canInstall={isAdmin}
                  onSheet={setSheet}
                  onClose={() => {
                    setSheet(null);
                    canvasRef.current?.focus();
                  }}
                  onOpenAgent={(id) => {
                    setSheet(null);
                    setDialog({ kind: "resident", id });
                  }}
                  onAgentUpdated={storeAgent}
                />
              </div>
            ) : dialogAgent ? (
              renderAgentDialog(dialogAgent, { onClose: closeDialog, key: targetKey(dialog), onOpenAgent: (id) => setDialog({ kind: "resident", id }) })
            ) : dialog?.kind === "plot" ? (
              <PlotDialog hasAssistant={Boolean(assistant)} canCreate={canWrite} onCreate={openCreateAgent} onAsk={() => void summonAssistant()} onClose={closeDialog} />
            ) : null}
          </motion.div>
          </AnimatePresence>
        </motion.div>
      )}
      </AnimatePresence>

      {workbench && (
        <Workbench
          request={workbench}
          agents={agents}
          renderAgent={(agent) =>
            renderAgentDialog(agent, {
              key: `workbench:${agent.id}`,
              embedded: true,
              onClose: () => {},
              onOpenAgent: (id) => {
                setWorkbench(null);
                setDialog({ kind: "resident", id });
              },
            })
          }
          onOpenAgent={(id) => {
            setWorkbench(null);
            setDialog({ kind: "resident", id });
          }}
          onClose={() => {
            setWorkbench(null);
            canvasRef.current?.focus();
          }}
        />
      )}

      <DesktopCommandPalette
        open={paletteOpen}
        initialSearch={paletteQuery}
        canWorkshop={canWrite}
        onOpenWorkshop={() => {
          const target = dialog?.kind === "resident" ? dialog.id : focus?.kind === "resident" ? focus.id : scene?.placeId.startsWith("house:") ? scene.placeId.slice(6) : null;
          closePanels();
          setWorkbench(target ? { agentId: target } : {});
        }}
        agents={agents}
        canRead={canRead}
        canWrite={canWrite}
        isAdmin={isAdmin}
        onOpenChange={setPaletteOpen}
        onOpenAgent={openAgent}
        onOpenQuests={() => { closePanels(); setBoard(true); }}
        onOpenChat={() => { closePanels(); setChatOpen(true); }}
        onOpenHistory={() => { closePanels(); setHistory(null); }}
        onOpenFolderWorkshop={() => { closePanels(); setFolderShop(true); }}
        onOpenAssistant={() => { closePanels(); void summonAssistant(); }}
        onOpenYou={() => { closePanels(); setFocusSection(null); setYouOpen(true); }}
        onOpenDevices={openDevices}
        onOpenPro={() => { closePanels(); setProOpen(true); }}
        onOpenLists={() => { closePanels(); setListsOpen(true); }}
        onOpenTerminal={() => { closePanels(); setTerminalOpen(true); }}
        canTerminal={canWrite && terminalFeature?.state !== "unavailable"}
        onOpenShortcuts={() => setShortcutsOpen(true)}
        mainAgent={assistant}
        onJump={(to) => {
          if (to.kind === "rules") return openSection("world-rules");
          if (to.kind === "you") {
            try {
              window.localStorage.setItem("world.you.page", to.page);
            } catch {}
            return openSection(to.focus ?? `page:${to.page}`);
          }
          if (to.kind === "map") return setMapShown((o) => !o);
          if (to.kind === "music") return setMusicOpen(true);
          if (to.kind === "notifications") return void (notesOpen || openNotesRef.current());
          closePanels();
          if (to.kind === "schedules" || to.kind === "feeds") setSheet({ kind: "agents", surface: to.kind, agentId: to.agentId });
          else if (to.kind === "kit") setSheet({ kind: "kit", agentId: to.agentId });
          else if (to.kind === "teams") setBoard("teams");
          else if (to.kind === "surface") setSheet({ kind: "agents", surface: to.surface });
          else if (to.kind === "travel") setLoka({ kind: "travel" });
          else if (to.kind === "academy") setLoka({ kind: "academy", tab: to.tab });
          else if (to.kind === "new-agent") setCreatingAgent(true);
          else if (to.kind === "region") void travel(to.id, to.name);
          else if (to.kind === "place") {
            const { place } = to;
            const opens = place.kind === "building" ? parseOpens(place.data.opens) : null;
            if (opens) return followOpens(opens, place.name);
            void travel(place.region_id ?? HEARTH, place.region_name ?? "Home");
            if (place.kind === "npc") setLoka({ kind: "npc", id: place.id, name: place.name, data: place.data });
          }
        }}
      />
      <AnimatePresence>{shortcutsOpen && <ShortcutSheet key="shortcuts" onClose={() => setShortcutsOpen(false)} />}</AnimatePresence>
      <FindBar
        onNoScope={() => {
          setPaletteQuery("");
          setPaletteOpen(true);
        }}
      />
      <AnimatePresence>
      {proOpen && (
        <motion.div
          key="pro"
          {...SCRIM_MOTION}
          className={cn(SCRIM, "z-50 p-4")}
          onPointerDown={(e) => {
            if (e.target === e.currentTarget) setProOpen(false);
          }}
        >
          <motion.div {...MODAL_MOTION} className="flex w-full max-w-md justify-center">
            <ProSheet onClose={() => setProOpen(false)} />
          </motion.div>
        </motion.div>
      )}
      </AnimatePresence>
    </div>
    </MotionConfig>
  );
}
