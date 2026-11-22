"use client";

import { SOLO } from "@/lib/edition";
import { AgentFace } from "./channel-ui";

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Command } from "cmdk";
import Glide from "./glide";
import { AppleLogo, ArrowsClockwise, Crown, Lightning, ArrowCounterClockwise, Bell, BookOpenText, Buildings, CalendarDots, ChatCircleDots, CheckSquare, CircleHalf, ClockCounterClockwise, Code, Coins, Compass, Star, Student, Desktop, DeviceMobile, DownloadSimple, FileText, FlowArrow, Gavel, GearSix, GraduationCap, Hash, Hammer, House, IdentificationBadge, IdentificationCard, Key, Keyboard, ListChecks, MagnifyingGlass, MapTrifold, Microphone, Monitor, Moon, MusicNotes, Pause, PaperPlaneRight, Play, Plugs, Plus, PuzzlePiece, Rss, Scroll, ShareNetwork, SignOut, SkipForward, SlackLogo, Smiley, SpeakerHigh, SpeakerSlash, Square, Sun, Sword, TerminalWindow, TextAa, Timer, Trophy, UploadSimple, User, UserCircle, UserSwitch, UsersThree, X } from "@phosphor-icons/react";
import { openQuickChat } from "./quick-chat";
import { cn } from "@/lib/utils";
import { worldApi, type SearchResults, type WorldAgent } from "@/lib/world";
import { openMessage } from "./message-links";
import { StatusIcon } from "./quest-ui";
import { CHUNKY } from "./styles";
import { focusTodo } from "./todos-ui";
import { CANVAS_KIND_LABEL, openReport, type CanvasKind } from "@/lib/reports";
import { openPractice, startPractice, subjectLabel } from "@/lib/practice";
import { CHANGELOG, changelogDate, openChangelog } from "@/lib/changelog";
import { desktopApi, isDesktopApp } from "@/lib/desktop";
import { stepTextScale } from "@/lib/text-scale";
import { Keys, hintKeys, isMac } from "./keycap";
import { useThemeMode } from "./theme";
import { useSoundPrefs } from "./sounds";
import { setMusicOn, skipMusic, toggleMusicPause, useMusic } from "./music";
import { MeaningIcon } from "@/lib/meaning-icons";

function words(text: string): string[] {
  return text.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
}

function subsequence(hay: string, needle: string): boolean {
  let i = 0;
  for (const ch of hay) if (ch === needle[i]) i += 1;
  return i === needle.length;
}

export const TELL = "tell:main";

export type PaletteJump =
  | { kind: "schedules" | "feeds"; agentId: string; id: string }
  | { kind: "kit"; agentId: string; id: string; itemKind: "skill" | "mcp" }
  | { kind: "surface"; surface: "usage" | "schedules" | "feeds" }
  | { kind: "you"; page: string; focus?: string }
  | { kind: "academy"; tab?: "progress"; challengeId?: string | null; courseId?: string | null }
  | { kind: "map" | "travel" | "notifications" | "music" | "new-agent" }
  | { kind: "teams"; id?: string }
  | { kind: "list"; id: string }
  | { kind: "rules" | "device" | "person"; id: string }
  | { kind: "region"; id: string; name: string }
  | { kind: "place"; id: string };

const TALK = /^(talk( to)?|chat( with)?|message|dm|open)\s+/;

export function paletteScore(value: string, search: string, keywords?: string[]): number {
  const typed = search.trim().toLowerCase();
  const query = value.startsWith("agent:") ? typed.replace(TALK, "") || typed : typed;
  if (!query) return 1;
  if (value === TELL) return 0.01;
  if (value.startsWith("hit:")) return 0.3;
  if (value.startsWith("give:")) return 0.55;
  if (value.startsWith("doc:")) return 0.28;
  if (value.startsWith("log:")) return 0.2;
  const isAgent = value.startsWith("agent:");
  const primary = (keywords?.[0] ?? "").toLowerCase();
  const all = words(`${keywords?.join(" ") ?? ""} ${isAgent ? "" : value.slice(value.indexOf(":") + 1)}`);
  const terms = words(query);
  if (!terms.length) return 0;
  const everyPrefix = terms.every((t) => all.some((w) => w.startsWith(t)));
  if (isAgent) {
    if (primary.startsWith(query)) return 1;
    if (words(primary).some((w) => w.startsWith(query))) return 0.95;
    if (everyPrefix) return 0.8;
    if (all.join(" ").includes(query)) return 0.4;
    return subsequence(primary.replace(/\s+/g, ""), query.replace(/\s+/g, "")) ? 0.2 : 0;
  }
  return everyPrefix ? 0.6 : 0;
}

const AGENT_TAB = "world:agent-tab";
let pendingTab: string | null = null;

export function takeAgentTab(agentId: string): "work" | null {
  if (pendingTab !== agentId) return null;
  pendingTab = null;
  return "work";
}

export function useAgentTabRequests(agentId: string, onTab: (tab: "work") => void) {
  const latest = useRef(onTab);
  latest.current = onTab;
  useEffect(() => {
    const onRequest = (e: Event) => {
      if ((e as CustomEvent<string>).detail !== agentId) return;
      const tab = takeAgentTab(agentId);
      if (tab) latest.current(tab);
    };
    window.addEventListener(AGENT_TAB, onRequest);
    return () => window.removeEventListener(AGENT_TAB, onRequest);
  }, [agentId]);
}

function requestQuestTab(agentId: string) {
  pendingTab = agentId;
  window.dispatchEvent(new CustomEvent(AGENT_TAB, { detail: agentId }));
}

const RECENT_KEY = "world.palette.recent";
const RECENT_MAX = 5;

function loadRecent(): string[] {
  try {
    const saved = JSON.parse(window.localStorage.getItem(RECENT_KEY) ?? "[]") as unknown;
    return Array.isArray(saved) ? saved.filter((v): v is string => typeof v === "string").slice(0, RECENT_MAX) : [];
  } catch {
    return [];
  }
}

function rememberRecent(value: string) {
  if (!value.startsWith("action:") && !value.startsWith("agent:")) return;
  try {
    const next = [value, ...loadRecent().filter((v) => v !== value)].slice(0, RECENT_MAX);
    window.localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {}
}

type DocEntry = { slug: string; title: string; summary: string; group: string; head: string[]; body: Set<string> };

const NO_DOCS: DocEntry[] = [];

function useDocs(open: boolean): DocEntry[] {
  return open ? NO_DOCS : NO_DOCS;
}

function plainText(text: string): string {
  return text.replace(/\*\*|`/g, "");
}

const CHANGES = CHANGELOG.map((entry, index) => ({
  index,
  entry,
  head: words(entry.title),
  items: entry.items.map((item) => ({ text: plainText(item), words: words(item) })),
}));

function matchesAll(terms: string[], hay: string[] | Set<string>): boolean {
  const list = Array.isArray(hay) ? hay : [...hay];
  return terms.every((t) => list.some((w) => w.startsWith(t)));
}

const GROUP = "[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1 [&_[cmdk-group-heading]]:text-w-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-w-muted";
const ITEM = "relative flex cursor-pointer items-center gap-2 rounded-lg px-2.5 py-2 text-w-base outline-none";

function shortDate(iso: string | null): string {
  if (!iso) return "";
  const date = new Date(iso);
  const sameDay = date.toDateString() === new Date().toDateString();
  return sameDay ? date.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" }) : date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function useGlobalSearch(open: boolean, search: string) {
  const [results, setResults] = useState<SearchResults | null>(null);
  const [pending, setPending] = useState(false);
  const query = search.trim();
  useEffect(() => {
    if (!open || query.length < 2) {
      setResults(null);
      setPending(false);
      return;
    }
    const controller = new AbortController();
    setPending(true);
    const timer = window.setTimeout(() => {
      worldApi
        .search(query, controller.signal)
        .then((found) => {
          setResults(found);
          setPending(false);
        })
        .catch(() => {
          if (!controller.signal.aborted) setPending(false);
        });
    }, 220);
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [open, query]);
  return { results, pending };
}

function cronLabel(cron: string | null, next: string | null): string {
  if (next) return `next ${shortDate(next)}`;
  return cron ?? "";
}

function HitRow({ value, icon, title, detail, aside, onSelect }: { value: string; icon: ReactNode; title: string; detail?: string | null; aside?: string | null; onSelect: () => void }) {
  return (
    <Command.Item value={value} onSelect={onSelect} className={cn(ITEM, detail && "items-start")}>
      {icon}
      <span className="min-w-0 flex-1">
        <span className="block truncate">{title}</span>
        {detail && <span className="block truncate text-w-xs text-w-muted">{detail}</span>}
      </span>
      {aside && <span className="shrink-0 pt-0.5 text-w-2xs text-w-muted">{aside}</span>}
    </Command.Item>
  );
}

function HitGroup({ heading, children }: { heading: string; children: ReactNode }) {
  return (
    <Command.Group heading={heading} className={GROUP}>
      {children}
    </Command.Group>
  );
}

type ActionGroup = "Go to" | "Create" | "Settings" | "Preferences" | "Account" | "Help";

const ACTION_GROUPS: ActionGroup[] = ["Go to", "Create", "Settings", "Preferences", "Account", "Help"];

interface PaletteAction {
  id: string;
  label: string;
  hint: string;
  icon: typeof Scroll;
  available: boolean;
  run: () => void;
  group?: ActionGroup;
  keywords?: string;
  core?: boolean;
}

export function DesktopCommandPalette({
  open,
  agents,
  canRead,
  canWrite,
  isAdmin,
  canWorkshop,
  onOpenWorkshop,
  onOpenChange,
  canOpenRecord,
  onOpenAgent,
  onOpenQuests,
  onOpenChat,
  onOpenHistory,
  onOpenFolderWorkshop,
  onOpenAssistant,
  onOpenYou,
  onOpenDevices,
  onOpenPro,
  onOpenLists,
  onOpenTerminal,
  onTerminalChat,
  canTerminal,
  onOpenShortcuts,
  mainAgent,
  onJump,
  initialSearch = "",
}: {
  mainAgent: WorldAgent | null;
  onJump: (jump: PaletteJump) => void;
  initialSearch?: string;
  open: boolean;
  agents: WorldAgent[];
  canRead: boolean;
  canWrite: boolean;
  isAdmin: boolean;
  canWorkshop: boolean;
  onOpenWorkshop: () => void;
  onOpenChange: (open: boolean) => void;
  canOpenRecord: () => boolean;
  onOpenAgent: (id: string) => void;
  onOpenQuests: () => void;
  onOpenChat: () => void;
  onOpenHistory: () => void;
  onOpenFolderWorkshop: () => void;
  onOpenAssistant: () => void;
  onOpenYou: () => void;
  onOpenDevices: () => void;
  onOpenPro: () => void;
  onOpenLists: () => void;
  onOpenTerminal: () => void;
  onTerminalChat: () => void;
  canTerminal: boolean;
  onOpenShortcuts: () => void;
}) {
  const previousFocus = useRef<HTMLElement | null>(null);
  const [questControls, setQuestControls] = useState(0);
  const [search, setSearch] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!open) return setSearch("");
    setSearch(initialSearch);
    const frame = window.requestAnimationFrame(() => {
      const input = inputRef.current;
      if (!input) return;
      input.focus();
      input.setSelectionRange(input.value.length, input.value.length);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [open, initialSearch]);
  const { results, pending } = useGlobalSearch(open, search);
  const [selected, setSelected] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!results) return;
    const frame = window.requestAnimationFrame(() => {
      const list = listRef.current;
      const current = list?.querySelector('[cmdk-item][data-selected="true"]');
      if (!list || (current && current.getAttribute("data-value") !== TELL)) return;
      const first = list.querySelector(`[cmdk-item]:not([data-value="${TELL}"])`) ?? list.querySelector("[cmdk-item]");
      const value = first?.getAttribute("data-value");
      if (value) setSelected(value);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [results]);
  useEffect(() => {
    const onControls = (e: Event) => setQuestControls((n) => Math.max(0, n + ((e as CustomEvent<boolean>).detail ? 1 : -1)));
    window.addEventListener("world:quest-controls", onControls);
    return () => window.removeEventListener("world:quest-controls", onControls);
  }, []);
  const questCommand = (kind: "status" | "assignee") => () => window.dispatchEvent(new CustomEvent("world:quest-command", { detail: kind }));
  useEffect(() => {
    if (open) previousFocus.current = document.activeElement as HTMLElement | null;
    else previousFocus.current?.focus();
  }, [open]);
  const [themeMode, setThemeMode] = useThemeMode();
  const [sounds, setSounds] = useSoundPrefs();
  const music = useMusic();
  const docs = useDocs(open);
  const [recent, setRecent] = useState<string[]>([]);
  useEffect(() => {
    if (open) setRecent(loadRecent());
  }, [open]);
  const mod = isMac() ? "⌘" : "Ctrl";
  const go = (to: PaletteJump) => () => onJump(to);
  const you = (page: string, focus?: string) => go({ kind: "you", page, focus });
  const openTab = (href: string) => () => window.open(href, "_blank", "noopener,noreferrer");
  const newQuest = () => {
    if (!mainAgent) return onOpenQuests();
    requestQuestTab(mainAgent.id);
    onOpenAgent(mainAgent.id);
  };
  const suggestSetup = () => {
    void fetch("/api/world/setup-scan", { method: "POST" }).finally(onOpenAssistant);
  };
  const actions: PaletteAction[] = [
    { id: "quest-status", label: "Change quest status", hint: "S", icon: CircleHalf, available: canWrite && questControls > 0, run: questCommand("status"), core: true },
    { id: "quest-assign", label: "Assign quest to", hint: "A", icon: UserSwitch, available: canWrite && questControls > 0, run: questCommand("assignee"), core: true },
    { id: "quests", label: "Open quests", hint: "Q", icon: Scroll, available: canRead, run: onOpenQuests, keywords: "board tasks work", core: true },
    { id: "lists", label: "Open your lists (to-dos, lists, timers)", hint: "L", icon: CheckSquare, available: canWrite, run: onOpenLists, keywords: "todos reminders checklist", core: true },
    { id: "reports", label: "Open reports", hint: "R", icon: FileText, available: canRead, run: () => openReport(), keywords: "library wiki notes docs", core: true },
    { id: "chat", label: "Open chat", hint: "C", icon: ChatCircleDots, available: canRead, run: onOpenChat, keywords: "channels messages dm post office", core: true },
    { id: "quick", label: "Quick chat with an agent", hint: "K", icon: Lightning, available: canWrite, run: () => openQuickChat(), keywords: "aside side question ask memory throwaway research", core: true },
    { id: "history", label: "Open session history", hint: "H", icon: ClockCounterClockwise, available: canWrite, run: onOpenHistory, keywords: "sessions runs rewind library", core: true },
    { id: "folder-workshop", label: "Work in a folder with an agent", hint: "O", icon: Hammer, available: canWrite, run: onOpenFolderWorkshop, keywords: "workshop mac directory", core: true },
    { id: "terminal", label: "Open terminal", hint: "T", icon: TerminalWindow, available: canTerminal, run: onOpenTerminal, keywords: "shell forge console", core: true },
    { id: "terminal-chat", label: "Ask an agent about the terminal", hint: "", icon: ChatCircleDots, available: canTerminal && canWrite, run: onTerminalChat, keywords: "terminal shell output error side chat quote explain" },
    { id: "workshop", label: "Open workshop", hint: "", icon: Hammer, available: canWorkshop, run: onOpenWorkshop, keywords: "workbench files editor code", core: true },
    { id: "assistant", label: "Open your main agent", hint: "P", icon: Star, available: canWrite, run: onOpenAssistant, keywords: "assistant main", core: true },
    { id: "notifications", label: "Open notifications", hint: "N", icon: Bell, available: true, run: go({ kind: "notifications" }), keywords: "bell inbox alerts unread" },
    { id: "teams", label: "Open teams", hint: "", icon: UsersThree, available: canRead, run: go({ kind: "teams" }), keywords: "groups builders squad" },
    { id: "map", label: "Show or hide the map", hint: "M", icon: MapTrifold, available: true, run: go({ kind: "map" }), keywords: "minimap" },
    { id: "travel", label: "Travel around Loka", hint: "", icon: Compass, available: true, run: go({ kind: "travel" }), keywords: "regions districts map go university workshop hilltop home" },
    { id: "academy", label: "Open the Academy", hint: "", icon: GraduationCap, available: canWrite, run: go({ kind: "academy" }), keywords: "challenges learn university classes" },
    { id: "progress", label: "Academy progress and XP", hint: "", icon: Trophy, available: canWrite, run: go({ kind: "academy", tab: "progress" }), keywords: "level xp unlocks cosmetics" },
    { id: "usage", label: "Usage and daily budgets", hint: "", icon: Coins, available: canWrite, run: go({ kind: "surface", surface: "usage" }), keywords: "cost spend money tokens budget limits market" },
    { id: "schedules", label: "Schedules and routines", hint: "", icon: CalendarDots, available: canWrite, run: go({ kind: "surface", surface: "schedules" }), keywords: "cron heartbeat timers clock tower" },
    { id: "feeds", label: "Feeds and webhooks", hint: "", icon: Rss, available: canWrite, run: go({ kind: "surface", surface: "feeds" }), keywords: "rss observatory" },
    { id: "devices", label: "Manage execution devices", hint: "", icon: DeviceMobile, available: canWrite, run: onOpenDevices, keywords: "runners mac computers machines" },
    { id: "you", label: "Open settings", hint: "Y", icon: GearSix, available: true, run: onOpenYou, keywords: "settings preferences profile account you", core: true },
    { id: "new-quest", label: mainAgent ? `New quest for ${mainAgent.name}` : "New quest", hint: "", icon: Sword, available: canWrite, run: newQuest, group: "Create", keywords: "create task assign work start" },
    { id: "new-practice", label: "New practice with a coach", hint: "", icon: Student, available: canWrite, run: startPractice, group: "Create", keywords: "practise practice japanese kanji kana drawing handwriting sketch coach pencil pen canvas" },
    { id: "suggest-setup", label: "Suggest a setup from how I already work", hint: "", icon: PuzzlePiece, available: canWrite && !!mainAgent, run: suggestSetup, group: "Create", keywords: "onboarding scan claude code codex cursor omp skills rules mcp team import" },
    { id: "new-agent", label: "New agent", hint: "", icon: Plus, available: canWrite, run: go({ kind: "new-agent" }), group: "Create", keywords: "create add hire resident" },
    { id: "set-profile", label: "Profile settings", hint: "", icon: UserCircle, available: true, run: you("profile"), group: "Settings", keywords: "name handle email status" },
    { id: "set-main-agent", label: "Main agent settings", hint: "", icon: Star, available: canWrite, run: you("main-agent"), group: "Settings", keywords: "assistant crown" },
    { id: "set-rules", label: "Rules", hint: "", icon: Scroll, available: true, run: you("rules", "world-rules"), group: "Settings", keywords: "instructions talking working" },
    { id: "set-setup", label: "Setup status", hint: "", icon: ListChecks, available: canWrite, run: you("setup"), group: "Settings", keywords: "ready readiness checklist" },
    { id: "set-keys", label: "Keys and environment", hint: "", icon: Key, available: canWrite, run: you("keys", "world-environment"), group: "Settings", keywords: "api provider secrets openai anthropic values" },
    { id: "set-tokens", label: SOLO ? "Connect other tools" : "API tokens", hint: "", icon: Code, available: true, run: you("tokens", "world-tokens"), group: "Settings", keywords: "mcp claude cursor external" },
    { id: "set-appearance", label: "Appearance settings", hint: "", icon: CircleHalf, available: true, run: you("appearance"), group: "Settings", keywords: "theme dark light" },
    { id: "set-sounds", label: "Sound settings", hint: "", icon: SpeakerHigh, available: true, run: you("sounds"), group: "Settings", keywords: "volume audio effects" },
    { id: "set-voice", label: "Voice settings", hint: "", icon: Microphone, available: canWrite, run: you("voice"), group: "Settings", keywords: "speech dictate speak" },
    { id: "theme-dark", label: "Use dark theme", hint: "", icon: Moon, available: themeMode !== "dark", run: () => setThemeMode("dark"), group: "Preferences", keywords: "appearance night mode" },
    { id: "theme-light", label: "Use light theme", hint: "", icon: Sun, available: themeMode !== "light", run: () => setThemeMode("light"), group: "Preferences", keywords: "appearance day mode" },
    { id: "theme-system", label: "Match the system theme", hint: "", icon: Monitor, available: themeMode !== "system", run: () => setThemeMode("system"), group: "Preferences", keywords: "appearance automatic mode" },
    { id: "sounds-toggle", label: sounds.on ? "Turn sound effects off" : "Turn sound effects on", hint: "", icon: sounds.on ? SpeakerSlash : SpeakerHigh, available: true, run: () => setSounds({ ...sounds, on: !sounds.on }), group: "Preferences", keywords: "mute audio volume" },
    { id: "music-open", label: "Open the music player", hint: "", icon: MusicNotes, available: true, run: go({ kind: "music" }), group: "Preferences", keywords: "songs youtube playlist" },
    { id: "music-play", label: music.prefs.on && music.playing ? "Pause music" : "Play music", hint: "", icon: music.prefs.on && music.playing ? Pause : Play, available: true, run: toggleMusicPause, group: "Preferences", keywords: "songs background resume" },
    { id: "music-skip", label: "Skip to the next song", hint: "", icon: SkipForward, available: music.prefs.on, run: skipMusic, group: "Preferences", keywords: "music next track" },
    { id: "music-off", label: "Turn music off", hint: "", icon: SpeakerSlash, available: music.prefs.on, run: () => setMusicOn(false), group: "Preferences", keywords: "stop songs mute" },
    { id: "text-bigger", label: "Make text bigger", hint: `${mod} +`, icon: TextAa, available: true, run: () => stepTextScale(1), group: "Preferences", keywords: "text size zoom in larger font" },
    { id: "text-smaller", label: "Make text smaller", hint: `${mod} -`, icon: TextAa, available: true, run: () => stepTextScale(-1), group: "Preferences", keywords: "text size zoom out font" },
    { id: "text-reset", label: "Reset text size", hint: `${mod} 0`, icon: ArrowCounterClockwise, available: true, run: () => stepTextScale(0), group: "Preferences", keywords: "text size zoom default font" },
    { id: "upgrade-pro", label: "Upgrade to Pro", hint: "", icon: Crown, available: isDesktopApp(), run: onOpenPro, group: "Help", keywords: "loka pro team organizations devices move" },
    { id: "pro", label: "Loka Pro", hint: "", icon: Crown, available: !isDesktopApp(), run: onOpenPro, group: "Help", keywords: "team organizations devices waitlist" },
    { id: "check-updates", label: "Check for updates", hint: "", icon: ArrowsClockwise, available: isDesktopApp(), run: () => void desktopApi.checkForUpdates().catch(() => {}), group: "Help", keywords: "update upgrade version new release" },
    { id: "changelog", label: "What's new", hint: "", icon: MeaningIcon.whatsNew, available: true, run: openChangelog, group: "Help", keywords: "changelog updates release notes" },
    { id: "shortcuts", label: "Keyboard shortcuts", hint: "?", icon: Keyboard, available: true, run: onOpenShortcuts, group: "Help", keywords: "keys hotkeys" },
    { id: "docs", label: "Read the docs", hint: "", icon: BookOpenText, available: true, run: openTab("/docs"), group: "Help", keywords: "help guide documentation manual" },
  ];
  const shown = actions.filter((action) => action.available);
  const select = (run: () => void, value?: string) => {
    if (value) rememberRecent(value);
    setSearch("");
    onOpenChange(false);
    run();
  };
  const selectRecord = (run: () => void, value?: string) => select(() => {
    if (!canOpenRecord()) return;
    if (value) rememberRecent(value);
    run();
  });
  const tellTarget = canWrite && mainAgent ? mainAgent : null;
  const tellText = search.trim();
  const [sending, setSending] = useState(false);
  const [tellError, setTellError] = useState<string | null>(null);
  useEffect(() => setTellError(null), [search, open]);
  const tell = async () => {
    if (!tellTarget || !tellText || sending) return;
    setSending(true);
    setTellError(null);
    try {
      await worldApi.send(tellTarget.id, tellText);
      select(() => onOpenAgent(tellTarget.id));
    } catch {
      setTellError(`Could not send that to ${tellTarget.name}. Try again.`);
    } finally {
      setSending(false);
    }
  };
  const agentName = (id: string | null) => (id ? agents.find((a) => a.id === id)?.name ?? null : null);
  const jump = (to: PaletteJump) => () => {
    const run = () => onJump(to);
    if (to.kind === "teams" || to.kind === "list" || to.kind === "region" || to.kind === "place") select(run);
    else selectRecord(run);
  };
  const academyHits = (results?.places ?? []).filter((p) => p.kind === "challenge" || p.kind === "adventure");
  const mapPlaces = (results?.places ?? []).filter((p) => p.kind === "building" || p.kind === "npc");
  const giveQuest = (agentId: string) => {
    requestQuestTab(agentId);
    onOpenAgent(agentId);
  };
  const query = search.trim().toLowerCase();
  const terms = useMemo(() => words(query), [query]);
  const giveTargets = useMemo(() => {
    if (!canWrite) return [];
    const verbs = terms.filter((t) => t.length >= 3 && ["give", "quest"].some((w) => w.startsWith(t)));
    if (!verbs.length) return [];
    const rest = terms.filter((t) => !verbs.includes(t) && t !== "a" && t !== "to");
    return agents.filter((agent) => matchesAll(rest, words(`${agent.name} ${agent.slug}`))).slice(0, 8);
  }, [agents, canWrite, terms]);
  const docHits = useMemo(() => {
    if (query.length < 2 || !terms.length) return [];
    const inHead = docs.filter((doc) => matchesAll(terms, doc.head));
    const inBody = query.length < 3 ? [] : docs.filter((doc) => !inHead.includes(doc) && matchesAll(terms, doc.body));
    return [...inHead, ...inBody].slice(0, 5);
  }, [docs, query, terms]);
  const changeHits = useMemo(() => {
    if (query.length < 3 || !terms.length) return [];
    const found: { index: number; title: string; detail: string; date: string; rank: number }[] = [];
    for (const change of CHANGES) {
      const titled = matchesAll(terms, change.head);
      const item = change.items.find((i) => matchesAll(terms, i.words));
      if (!titled && !item) continue;
      found.push({ index: change.index, title: change.entry.title, detail: (item ?? change.items[0])?.text ?? "", date: changelogDate(change.entry.date), rank: titled ? 0 : 1 });
    }
    return found.sort((x, y) => x.rank - y.rank || x.index - y.index).slice(0, 4);
  }, [query, terms]);
  const recentItems = recent.flatMap((value): ({ kind: "agent"; agent: WorldAgent } | { kind: "action"; action: PaletteAction })[] => {
    if (value.startsWith("agent:")) {
      const agent = agents.find((a) => a.id === value.slice("agent:".length));
      return agent ? [{ kind: "agent", agent }] : [];
    }
    const action = shown.find((a) => `action:${a.id}` === value);
    return action ? [{ kind: "action", action }] : [];
  });
  const agentItem = (agent: WorldAgent) => (
    <Command.Item key={agent.id} value={`agent:${agent.id}`} keywords={[agent.name, agent.slug, agent.expertise ?? "", ...(agent.tags ?? [])]} onSelect={() => selectRecord(() => onOpenAgent(agent.id), `agent:${agent.id}`)} className={ITEM}>
      <AgentFace agent={agent} size={20} />
      <span className="min-w-0 flex-1 truncate font-medium">{agent.name}</span>
      <span className="text-w-xs text-w-muted">@{agent.slug}</span>
    </Command.Item>
  );
  const actionItem = (action: PaletteAction) => (
    <Command.Item key={action.id} value={`action:${action.id}`} keywords={[action.label, action.keywords ?? ""]} onSelect={() => select(action.run, `action:${action.id}`)} className={ITEM}>
      <action.icon size={17} weight="bold" className="text-w-link" />
      <span className="flex-1">{action.label}</span>
      {action.hint && <Keys keys={hintKeys(action.hint)} size="sm" />}
    </Command.Item>
  );

  return (
    <Command.Dialog
      open={open}
      onOpenChange={onOpenChange}
      label="Loka commands"
      loop
      filter={paletteScore}
      value={selected}
      onValueChange={setSelected}
      onKeyDown={(e) => {
        if (e.key !== "Enter" || !tellTarget || !tellText || e.nativeEvent.isComposing) return;
        if (e.metaKey || e.ctrlKey) {
          e.preventDefault();
          void tell();
        } else if (selected === TELL && pending) e.preventDefault();
      }}
      overlayClassName="fixed inset-0 z-[80] bg-w-scrim/55 backdrop-blur-[2px] data-[state=open]:animate-w-scrim-in data-[state=closed]:animate-w-scrim-out motion-reduce:!animate-none"
      contentClassName="fixed inset-x-0 top-[12vh] z-[81] mx-auto w-[calc(100%-1.5rem)] max-w-xl origin-center data-[state=open]:animate-w-modal-in data-[state=closed]:animate-w-modal-out motion-reduce:!animate-none overflow-hidden rounded-xl border-[3px] border-w-ink bg-w-paper text-w-text shadow-[0_6px_0_rgb(var(--w-ink))] motion-reduce:animate-none"
    >
        <div className="flex items-center gap-2 border-b-2 border-dashed border-w-line2 px-3">
          <MagnifyingGlass size={18} weight="bold" className="text-w-muted" />
          <Command.Input ref={inputRef} autoFocus value={search} onValueChange={setSearch} placeholder={tellTarget ? `Search everything, or type a message for ${tellTarget.name}` : "Search everything in Loka"} className="h-12 min-w-0 flex-1 bg-transparent text-w-md outline-none placeholder:text-w-faint" />
          <button type="button" onClick={() => onOpenChange(false)} aria-label="Close command palette" className={cn(CHUNKY, "bg-w-card p-1 hover:bg-w-sand")}>
            <X size={14} weight="bold" />
          </button>
        </div>
        <Command.List ref={listRef} className="max-h-[min(60vh,28rem)] overflow-y-auto p-2 [&_[cmdk-list-sizer]]:relative">
          <Glide target={"[cmdk-item][data-selected=\"true\"]"} className="rounded-lg bg-w-sand" />
          <Command.Empty className={cn("px-3 py-8 text-center text-w-sm text-w-muted", pending && "hidden")}>Nothing matches.</Command.Empty>
          {search.trim() ? (
            <>
          {agents.length > 0 && (
            <Command.Group heading="Conversations" className={GROUP}>
              {agents.map(agentItem)}
            </Command.Group>
          )}
          {giveTargets.length > 0 && (
            <Command.Group heading="Give a quest" className={GROUP}>
              {giveTargets.map((agent) => (
                <Command.Item key={agent.id} value={`give:${agent.id}`} onSelect={() => select(() => giveQuest(agent.id))} className={ITEM}>
                  <Sword size={17} weight="duotone" className="text-w-link" />
                  <span className="min-w-0 flex-1 truncate">Give {agent.name} a quest</span>
                  <AgentFace agent={agent} size={18} />
                </Command.Item>
              ))}
            </Command.Group>
          )}
          {ACTION_GROUPS.map((group) => {
            const items = shown.filter((action) => (action.group ?? "Go to") === group);
            return items.length > 0 && (
              <Command.Group key={group} heading={group} className={GROUP}>
                {items.map(actionItem)}
              </Command.Group>
            );
          })}
          {results && (
            <>
              {(results.reports?.length ?? 0) > 0 && (
                <HitGroup heading="Reports">
                  {results.reports?.map((report) => [
                    <Command.Item key={report.id} value={`hit:report:${report.id}`} onSelect={() => selectRecord(() => (report.practice ? openPractice(report.id) : openReport(report.id)))} className={cn(ITEM, "items-start")}>
                      {report.practice ? <Student size={16} weight="duotone" className="mt-0.5 shrink-0 text-w-link" /> : report.kind === "canvas" ? <FlowArrow size={16} weight="duotone" className="mt-0.5 shrink-0 text-w-link" /> : <FileText size={16} weight="duotone" className="mt-0.5 shrink-0 text-w-link" />}
                      <span className="min-w-0 flex-1">
                        <span className="flex items-baseline gap-1.5 text-w-xs text-w-muted">
                          <span className="truncate font-medium text-w-text">{report.title}</span>
                          {report.practice ? <span className="shrink-0">Practice · {subjectLabel(report.practice.subject)}</span> : report.kind === "canvas" && <span className="shrink-0">Canvas</span>}
                          {report.folder && <span className="truncate">{report.folder}</span>}
                        </span>
                        {report.excerpt && !report.items?.length && <span className="line-clamp-2 text-w-sm leading-snug">{report.excerpt}</span>}
                      </span>
                      <span className="shrink-0 pt-0.5 text-w-2xs text-w-muted">{shortDate(report.updated_at)}</span>
                    </Command.Item>,
                    ...(report.items ?? []).map((item) => (
                      <Command.Item
                        key={`${report.id}#${item.id}`}
                        value={`hit:canvas-item:${report.id}#${item.id}`}
                        onSelect={() => selectRecord(() => openReport(report.id, item.id))}
                        className={cn(ITEM, "items-start pl-9")}
                      >
                        <span className="mt-0.5 w-12 shrink-0 text-w-2xs font-semibold text-w-muted">{CANVAS_KIND_LABEL[item.kind as CanvasKind] ?? "Item"}</span>
                        <span className="line-clamp-2 min-w-0 flex-1 text-w-sm leading-snug">{item.label.replace(/[#*_`>]+/g, " ").replace(/\s+/g, " ").trim()}</span>
                      </Command.Item>
                    )),
                  ])}
                </HitGroup>
              )}
              {results.messages.length > 0 && (
                <HitGroup heading="Messages">
                  {results.messages.map((hit) => (
                    <Command.Item key={hit.ref} value={`hit:message:${hit.ref}`} onSelect={() => selectRecord(() => openMessage(hit))} className={cn(ITEM, "items-start")}>
                      <span aria-hidden="true" className="shrink-0 pt-0.5">{(() => {
                        const agent = hit.kind === "agent" ? agents.find((a) => a.id === hit.agent_id) : null;
                        if (agent) return <AgentFace agent={agent} size={16} />;
                        return hit.channel_kind === "dm" ? <ChatCircleDots size={16} weight="duotone" className="text-w-link" /> : <Hash size={16} weight="duotone" className="text-w-link" />;
                      })()}</span>
                      <span className="min-w-0 flex-1">
                        <span className="flex items-baseline gap-1.5 text-w-xs text-w-muted">
                          <span className="truncate font-medium text-w-text">{hit.kind === "agent" ? hit.agent_name : hit.channel_kind === "dm" ? "Direct message" : `#${hit.channel_name}`}</span>
                          <span className="truncate">{hit.author}</span>
                        </span>
                        <span className="line-clamp-2 text-w-sm leading-snug">{hit.excerpt}</span>
                      </span>
                      <span className="shrink-0 pt-0.5 text-w-2xs text-w-muted">{shortDate(hit.created_at)}</span>
                    </Command.Item>
                  ))}
                </HitGroup>
              )}
              {results.quests.length > 0 && (
                <HitGroup heading="Quests">
                  {results.quests.map((quest) => {
                    const agent = agents.find((a) => a.id === quest.assigned_agent_id);
                    return (
                      <Command.Item
                        key={quest.id}
                        value={`hit:quest:${quest.id}`}
                        onSelect={() => selectRecord(() => window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: quest.id, agentId: quest.assigned_agent_id } })))}
                        className={ITEM}
                      >
                        <StatusIcon status={quest.status} />
                        <span className="min-w-0 flex-1 truncate">{quest.title}</span>
                        {agent && <span className="flex shrink-0 items-center gap-1.5 text-w-xs text-w-muted"><AgentFace agent={agent} size={16} />{agent.name}</span>}
                      </Command.Item>
                    );
                  })}
                </HitGroup>
              )}
              {results.todos.length > 0 && (
                <HitGroup heading="To-dos">
                  {results.todos.map((todo) => (
                    <Command.Item key={todo.id} value={`hit:todo:${todo.id}`} onSelect={() => selectRecord(() => focusTodo(todo.id))} className={ITEM}>
                      {todo.status === "done" ? <CheckSquare size={16} weight="duotone" className="text-w-ok" /> : <Square size={16} weight="duotone" className="text-w-muted" />}
                      <span className={cn("min-w-0 flex-1 truncate", todo.status !== "open" && "text-w-muted line-through")}>{todo.text}</span>
                      <span className="shrink-0 text-w-2xs text-w-muted">{shortDate(todo.added_at)}</span>
                    </Command.Item>
                  ))}
                </HitGroup>
              )}
              {results.lists.length > 0 && (
                <HitGroup heading="Lists and timers">
                  {results.lists.map((list) => (
                    <Command.Item key={list.id} value={`hit:list:${list.id}`} onSelect={jump({ kind: "list", id: list.id })} className={ITEM}>
                      {list.kind === "timer" ? <Timer size={16} weight="duotone" className="text-w-link" /> : <ListChecks size={16} weight="duotone" className="text-w-link" />}
                      <span className="min-w-0 flex-1 truncate">{list.name}</span>
                    </Command.Item>
                  ))}
                </HitGroup>
              )}
              {results.channels.length > 0 && (
                <HitGroup heading="Channels">
                  {results.channels.map((channel) => (
                    <Command.Item
                      key={channel.id}
                      value={`hit:channel:${channel.id}`}
                      onSelect={() => selectRecord(() => window.dispatchEvent(new CustomEvent("world:open-channel", { detail: { id: channel.id } })))}
                      className={ITEM}
                    >
                      <Hash size={16} weight="duotone" className="text-w-link" />
                      <span className="shrink-0 font-medium">{channel.name}</span>
                      {channel.topic && <span className="min-w-0 flex-1 truncate text-w-xs text-w-muted">{channel.topic}</span>}
                    </Command.Item>
                  ))}
                </HitGroup>
              )}
              {(results.schedules?.length ?? 0) > 0 && (
                <HitGroup heading="Schedules and routines">
                  {results.schedules?.map((s) => (
                    <HitRow
                      key={s.id}
                      value={`hit:schedule:${s.id}`}
                      icon={<CalendarDots size={16} weight="duotone" className={cn("shrink-0", s.enabled ? "text-w-link" : "text-w-muted")} />}
                      title={s.name}
                      detail={[agentName(s.agent_id), s.kind === "heartbeat" ? "heartbeat" : "routine", s.enabled ? null : "off"].filter(Boolean).join(" · ")}
                      aside={s.enabled ? cronLabel(s.cron_expression, s.next_run_at) : null}
                      onSelect={jump({ kind: "schedules", agentId: s.agent_id, id: s.id })}
                    />
                  ))}
                </HitGroup>
              )}
              {(results.teams?.length ?? 0) > 0 && (
                <HitGroup heading="Teams">
                  {results.teams?.map((t) => (
                    <HitRow
                      key={t.id}
                      value={`hit:team:${t.id}`}
                      icon={<UsersThree size={16} weight="duotone" className="shrink-0 text-w-link" />}
                      title={t.name}
                      detail={t.purpose || (t.lead_agent_id ? `Led by ${agentName(t.lead_agent_id) ?? "an agent"}` : null)}
                      onSelect={jump({ kind: "teams", id: t.id })}
                    />
                  ))}
                </HitGroup>
              )}
              {academyHits.length > 0 && (
                <HitGroup heading="Academy">
                  {academyHits.map((p) => {
                    const report = p.kind === "challenge" && typeof p.data.report_id === "string" ? p.data.report_id : null;
                    return (
                      <HitRow
                        key={p.id}
                        value={`hit:academy:${p.id}`}
                        icon={p.kind === "adventure" ? <Compass size={16} weight="duotone" className="shrink-0 text-w-warn" /> : <GraduationCap size={16} weight="duotone" className="shrink-0 text-w-ok" />}
                        title={p.name}
                        detail={p.kind === "adventure" ? `Course${typeof p.data.skill === "string" ? ` in ${p.data.skill}` : ""}` : `${report ? "Canvas lesson" : "Lesson"}${typeof p.data.skill === "string" ? ` in ${p.data.skill}` : ""}`}
                        aside="Open"
                        onSelect={report ? () => select(() => openPractice(report)) : jump({ kind: "academy", challengeId: p.kind === "challenge" ? p.id : null, courseId: p.kind === "adventure" ? p.id : null })}
                      />
                    );
                  })}
                </HitGroup>
              )}
              {((results.regions?.length ?? 0) > 0 || mapPlaces.length > 0) && (
                <HitGroup heading="Places in Loka">
                  {results.regions?.map((r) => (
                    <HitRow
                      key={r.id}
                      value={`hit:region:${r.id}`}
                      icon={<MapTrifold size={16} weight="duotone" className="shrink-0 text-w-link" />}
                      title={r.name}
                      detail={`${r.mine ? "Your" : "Shared"} ${r.theme} region`}
                      aside="Travel"
                      onSelect={jump({ kind: "region", id: r.id, name: r.name })}
                    />
                  ))}
                  {mapPlaces.map((p) => (
                    <HitRow
                      key={p.id}
                      value={`hit:place:${p.id}`}
                      icon={p.kind === "npc" ? <Smiley size={16} weight="duotone" className="shrink-0 text-w-link" /> : <House size={16} weight="duotone" className="shrink-0 text-w-link" />}
                      title={p.name}
                      detail={`${p.kind === "npc" ? "Character" : "Building"} in ${p.region_name ?? "Home"}`}
                      aside={p.kind === "npc" ? "Talk" : "Visit"}
                      onSelect={jump({ kind: "place", id: p.id })}
                    />
                  ))}
                </HitGroup>
              )}
              {(results.feeds?.length ?? 0) > 0 && (
                <HitGroup heading="Feeds">
                  {results.feeds?.map((f) => (
                    <HitRow
                      key={f.id}
                      value={`hit:feed:${f.id}`}
                      icon={<Rss size={16} weight="duotone" className={cn("shrink-0", f.paused ? "text-w-muted" : "text-w-link")} />}
                      title={f.name}
                      detail={[agentName(f.agent_id), f.kind === "rss" ? "RSS" : "webhook", f.paused ? "paused" : null].filter(Boolean).join(" · ")}
                      onSelect={jump({ kind: "feeds", agentId: f.agent_id, id: f.id })}
                    />
                  ))}
                </HitGroup>
              )}
              {(results.rules?.length ?? 0) > 0 && (
                <HitGroup heading="Rules">
                  {results.rules?.map((r) => (
                    <HitRow
                      key={r.id}
                      value={`hit:rule:${r.id}`}
                      icon={<Gavel size={16} weight="duotone" className={cn("shrink-0", r.enabled ? "text-w-link" : "text-w-muted")} />}
                      title={r.excerpt}
                      detail={[r.global ? "For everyone" : "Yours", r.scope === "talk" ? "when talking" : r.scope === "work" ? "when working" : "always", r.enabled ? null : "off"].filter(Boolean).join(" · ")}
                      onSelect={jump({ kind: "rules", id: r.id })}
                    />
                  ))}
                </HitGroup>
              )}
              {(results.kit?.length ?? 0) > 0 && (
                <HitGroup heading="Skills and MCP servers">
                  {results.kit?.map((k) => (
                    <HitRow
                      key={`${k.kind}:${k.id}`}
                      value={`hit:kit:${k.kind}:${k.id}`}
                      icon={k.kind === "skill" ? <PuzzlePiece size={16} weight="duotone" className="shrink-0 text-w-link" /> : <Plugs size={16} weight="duotone" className="shrink-0 text-w-link" />}
                      title={k.name}
                      detail={[k.kind === "skill" ? "Skill" : "MCP server", k.description].filter(Boolean).join(" · ")}
                      aside={agentName(k.agent_id) ? `${agentName(k.agent_id)}'s kit` : null}
                      onSelect={jump({ kind: "kit", agentId: k.agent_id, id: k.id, itemKind: k.kind })}
                    />
                  ))}
                </HitGroup>
              )}
              {(results.devices?.length ?? 0) > 0 && (
                <HitGroup heading="Devices">
                  {results.devices?.map((d) => (
                    <HitRow
                      key={d.id}
                      value={`hit:device:${d.id}`}
                      icon={<Desktop size={16} weight="duotone" className={cn("shrink-0", d.status === "online" ? "text-w-ok" : "text-w-muted")} />}
                      title={d.name}
                      detail={`${d.platform === "macos" ? "Mac" : d.platform} · ${d.status}`}
                      onSelect={jump({ kind: "device", id: d.id })}
                    />
                  ))}
                </HitGroup>
              )}
            </>
          )}
          {docHits.length > 0 && (
            <HitGroup heading="Docs">
              {docHits.map((doc) => (
                <HitRow
                  key={doc.slug}
                  value={`doc:${doc.slug}`}
                  icon={<BookOpenText size={16} weight="duotone" className="shrink-0 text-w-link" />}
                  title={doc.title}
                  detail={doc.summary}
                  aside={doc.group}
                  onSelect={() => select(() => window.open(`/docs/${doc.slug}`, "_blank", "noopener,noreferrer"))}
                />
              ))}
            </HitGroup>
          )}
          {changeHits.length > 0 && (
            <HitGroup heading="What's new">
              {changeHits.map((hit) => (
                <HitRow
                  key={hit.index}
                  value={`log:${hit.index}`}
                  icon={<MeaningIcon.whatsNew size={16} weight="duotone" className="shrink-0 text-w-link" />}
                  title={hit.title}
                  detail={hit.detail}
                  aside={hit.date}
                  onSelect={() => select(openChangelog)}
                />
              ))}
            </HitGroup>
          )}
          {pending && <Command.Loading className="px-3 py-2 text-w-xs text-w-muted">Searching everything...</Command.Loading>}
          {tellTarget && tellText && (
            <Command.Group heading={`Ask ${tellTarget.name}`} className={GROUP}>
              <Command.Item value={TELL} onSelect={() => void tell()} disabled={sending} className={cn(ITEM, "items-start")}>
                <PaperPlaneRight size={16} weight="bold" className="mt-0.5 shrink-0 text-w-link" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate">
                    <span className="font-medium text-w-strong">Tell {tellTarget.name}:</span> {tellText}
                  </span>
                  {tellError ? <span className="block text-w-xs text-w-alert">{tellError}</span> : <span className="block text-w-xs text-w-muted">{sending ? "Sending..." : "Sends this as a message and opens the chat"}</span>}
                </span>
                <Keys keys={["⌘", "↵"]} size="sm" className="shrink-0" />
              </Command.Item>
            </Command.Group>
          )}
            </>
          ) : (
            <>
          {recentItems.length > 0 && (
            <Command.Group heading="Recent" className={GROUP}>
              {recentItems.map((item) => (item.kind === "agent" ? agentItem(item.agent) : actionItem(item.action)))}
            </Command.Group>
          )}
          <Command.Group heading="Go to" className={GROUP}>
            {shown.filter((action) => action.core && !recent.includes(`action:${action.id}`)).map(actionItem)}
          </Command.Group>
          <Command.Group heading="Create" className={GROUP}>
            {shown.filter((action) => action.group === "Create" && !recent.includes(`action:${action.id}`)).map(actionItem)}
          </Command.Group>
          <Command.Separator className="my-2 border-t-2 border-dashed border-w-line" />
          <Command.Group heading="Conversations" className={GROUP}>
            {agents.filter((agent) => !recent.includes(`agent:${agent.id}`)).map(agentItem)}
          </Command.Group>
          <p className="px-2 pb-1 pt-2 text-w-2xs text-w-muted">Type to search settings, preferences, docs and everything in Loka.</p>
            </>
          )}
        </Command.List>
        <p className="border-t-2 border-dashed border-w-line2 px-3 py-2 text-w-2xs text-w-muted">
          Use ↑↓ to move, Enter to open, Esc to close.{tellTarget ? ` ⌘Enter tells ${tellTarget.name}.` : ""}
        </p>
    </Command.Dialog>
  );
}
