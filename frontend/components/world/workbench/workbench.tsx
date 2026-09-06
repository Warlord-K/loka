"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { GitBranch, Hammer, MagnifyingGlass, Plus, Robot, SidebarSimple, TerminalWindow, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type SessionBrief, type WorldAgent, type WorldEvent } from "@/lib/world";
import { SessionDetail, sessionTitle, useWorldRefresh } from "../panels";
import { PIXEL } from "../styles";
import { Shimmer } from "../work-ui";
import { sessionKeyFor, setOpenWorkshop, wb, type OpenWorkshopDetail, type WbChange, type WbMatch, type WbOpen } from "./bridge";
import { EditorPane, type EditorHandle, type Tab, type TabMode } from "./editor-pane";
import { baseName, FileTree, STATUS_COLOR } from "./file-tree";
import { TerminalPane } from "./terminal-pane";
import { scrollWithin } from "@/lib/scroll";

interface Layout {
  tabs: Tab[];
  active: string | null;
  expanded: string[];
  left: number;
  right: number;
  bottom: number;
  terminal: boolean;
  rightOpen: boolean;
  split: boolean;
  pane: string;
  extra: string[];
}

const DEFAULT_LAYOUT: Layout = {
  tabs: [],
  active: null,
  expanded: [],
  left: 260,
  right: 380,
  bottom: 240,
  terminal: true,
  rightOpen: true,
  split: true,
  pane: "executor",
  extra: [],
};

function storageKey(sessionId: string) {
  return `world.workbench.v1:${sessionId}`;
}

const OLD_DEFAULT_RIGHT = 380;

function defaultRight(): number {
  return Math.round(Math.max(340, Math.min(900, window.innerWidth * 0.36)));
}

function maxRight(): number {
  return Math.max(400, Math.round(window.innerWidth * 0.6));
}

function readLayout(sessionId: string): Layout {
  const base = { ...DEFAULT_LAYOUT, right: defaultRight() };
  try {
    const raw = window.localStorage.getItem(storageKey(sessionId));
    if (raw) {
      const stored = JSON.parse(raw) as Partial<Layout>;
      if (stored.right === OLD_DEFAULT_RIGHT) delete stored.right;
      return { ...base, ...stored };
    }
  } catch {}
  return base;
}

const COMPACT_QUERY = "(max-width: 900px)";

function useCompact(): boolean {
  const [compact, setCompact] = useState(() => typeof window !== "undefined" && window.matchMedia(COMPACT_QUERY).matches);
  useEffect(() => {
    const query = window.matchMedia(COMPACT_QUERY);
    const update = () => setCompact(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return compact;
}

function typingIn(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable || !!el.closest(".cm-editor, .xterm");
}

function score(path: string, query: string): number {
  if (!query) return 1;
  const hay = path.toLowerCase();
  const name = baseName(hay);
  const q = query.toLowerCase();
  if (name.startsWith(q)) return 1000 - path.length;
  if (name.includes(q)) return 800 - path.length;
  if (hay.includes(q)) return 600 - path.length;
  let i = 0;
  let gaps = 0;
  let last = -1;
  for (let j = 0; j < hay.length && i < q.length; j++) {
    if (hay[j] === q[i]) {
      if (last >= 0) gaps += j - last - 1;
      last = j;
      i++;
    }
  }
  return i === q.length ? 300 - gaps - path.length * 0.1 : -1;
}

function Resizer({ axis, onDrag }: { axis: "x" | "y"; onDrag: (delta: number) => void }) {
  const start = (event: React.PointerEvent) => {
    event.preventDefault();
    let last = axis === "x" ? event.clientX : event.clientY;
    const move = (e: PointerEvent) => {
      const now = axis === "x" ? e.clientX : e.clientY;
      onDrag(now - last);
      last = now;
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.style.cursor = "";
    };
    document.body.style.cursor = axis === "x" ? "col-resize" : "row-resize";
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  return (
    <div
      role="separator"
      aria-orientation={axis === "x" ? "vertical" : "horizontal"}
      onPointerDown={start}
      className={cn(
        "relative z-10 shrink-0 touch-none bg-[#2a1f2f] transition-colors before:absolute before:content-[''] hover:bg-[#e8b64a]/60",
        axis === "x" ? "w-1 cursor-col-resize before:-inset-x-2 before:inset-y-0" : "h-1 cursor-row-resize before:-inset-y-2 before:inset-x-0",
      )}
    />
  );
}

function QuickOpen({
  sessionKey,
  mode,
  onMode,
  onPick,
  onClose,
}: {
  sessionKey: string;
  mode: "files" | "text";
  onMode: (mode: "files" | "text") => void;
  onPick: (path: string, line?: number, column?: number) => void;
  onClose: () => void;
}) {
  const [files, setFiles] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [matches, setMatches] = useState<WbMatch[]>([]);
  const [searching, setSearching] = useState(false);
  const [index, setIndex] = useState(0);
  const list = useRef<HTMLUListElement>(null);

  useEffect(() => {
    void wb.files(sessionKey).then(setFiles).catch(() => {});
  }, [sessionKey]);

  useEffect(() => {
    if (mode !== "text") return;
    if (query.trim().length < 2) {
      setMatches([]);
      return;
    }
    setSearching(true);
    const id = window.setTimeout(() => {
      wb.find(sessionKey, query)
        .then(setMatches)
        .catch(() => setMatches([]))
        .finally(() => setSearching(false));
    }, 220);
    return () => window.clearTimeout(id);
  }, [mode, query, sessionKey]);

  const ranked = useMemo(
    () =>
      files
        .map((path) => ({ path, s: score(path, query.trim()) }))
        .filter((item) => item.s >= 0)
        .sort((a, b) => b.s - a.s)
        .slice(0, 60),
    [files, query],
  );
  const count = mode === "files" ? ranked.length : matches.length;
  useEffect(() => setIndex(0), [query, mode]);
  useEffect(() => {
    scrollWithin(list.current?.querySelector(`[data-index="${index}"]`), { block: "nearest" });
  }, [index]);

  const pick = (i: number) => {
    if (mode === "files") {
      const item = ranked[i];
      if (item) onPick(item.path);
    } else {
      const m = matches[i];
      if (m) onPick(m.path, m.line, m.column);
    }
  };

  return (
    <div className="absolute inset-0 z-30 bg-[#0b070d]/50" onPointerDown={onClose}>
      <div onPointerDown={(e) => e.stopPropagation()} className="mx-auto mt-[8vh] w-[min(40rem,calc(100%-2rem))] overflow-hidden rounded-lg border-2 border-[#3a2a3d] bg-[#1d1422] shadow-[0_6px_0_#0b070d]">
        <div className="flex items-center gap-2 border-b border-[#3a2a3d] px-3">
          <MagnifyingGlass size={15} className="text-[#9c8a9a]" />
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setIndex((i) => Math.min(count - 1, i + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setIndex((i) => Math.max(0, i - 1));
              } else if (e.key === "Enter") {
                e.preventDefault();
                pick(index);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                onClose();
              } else if (e.key === "Tab") {
                e.preventDefault();
                onMode(mode === "files" ? "text" : "files");
              }
            }}
            placeholder={mode === "files" ? "Go to file" : "Search text in every file"}
            className="h-10 min-w-0 flex-1 bg-transparent text-w-base text-[#e9dcc6] outline-none placeholder:text-[#7d6b7b]"
          />
          <span className="flex overflow-hidden rounded-md border border-[#3a2a3d] text-w-2xs text-[#9c8a9a]">
            {(["files", "text"] as const).map((m) => (
              <button key={m} type="button" onClick={() => onMode(m)} className={cn("px-2 py-0.5", mode === m && "bg-[#33243a] text-white")}>
                {m === "files" ? "Files" : "Text"}
              </button>
            ))}
          </span>
        </div>
        <ul ref={list} className="max-h-[50vh] overflow-y-auto py-1 text-w-xs">
          {mode === "files"
            ? ranked.map((item, i) => (
                <li key={item.path}>
                  <button
                    type="button"
                    data-index={i}
                    onMouseEnter={() => setIndex(i)}
                    onClick={() => pick(i)}
                    className={cn("flex w-full items-baseline gap-2 px-3 py-1 text-left", i === index ? "bg-[#33243a] text-white" : "text-[#d8c9b2]")}
                  >
                    <span className="shrink-0">{baseName(item.path)}</span>
                    <span className="min-w-0 truncate text-w-2xs text-[#7d6b7b]">{item.path}</span>
                  </button>
                </li>
              ))
            : matches.map((m, i) => (
                <li key={`${m.path}:${m.line}:${i}`}>
                  <button
                    type="button"
                    data-index={i}
                    onMouseEnter={() => setIndex(i)}
                    onClick={() => pick(i)}
                    className={cn("block w-full px-3 py-1 text-left", i === index ? "bg-[#33243a] text-white" : "text-[#d8c9b2]")}
                  >
                    <span className="block truncate font-mono text-w-xs">{m.text.trim()}</span>
                    <span className="block truncate text-w-2xs text-[#7d6b7b]">
                      {m.path}:{m.line}
                    </span>
                  </button>
                </li>
              ))}
          {count === 0 && (
            <li className="px-3 py-4 text-center text-w-xs text-[#7d6b7b]">
              {mode === "text" ? (searching ? "Searching..." : query.trim().length < 2 ? "Type at least two characters." : "No matches.") : "No files match."}
            </li>
          )}
        </ul>
        <p className="border-t border-[#3a2a3d] px-3 py-1.5 text-w-2xs text-[#7d6b7b]">Enter opens, Tab switches between files and text, Esc closes.</p>
      </div>
    </div>
  );
}

function Picker({ sessions, agentName, onPick, onClose }: { sessions: SessionBrief[]; agentName: string; onPick: (s: SessionBrief) => void; onClose: () => void }) {
  return (
    <div className="grid h-full place-items-center p-4">
      <div className="w-[min(30rem,100%)] rounded-lg border-2 border-[#3a2a3d] bg-[#1d1422] p-4 text-[#e9dcc6]">
        <p className={cn(PIXEL, "text-w-lg")}>Which workshop?</p>
        <p className="mt-1 text-w-xs text-[#9c8a9a]">{agentName} has more than one quest going. Pick the one to open.</p>
        <ul className="mt-3 space-y-1.5">
          {sessions.map((s) => (
            <li key={s.id}>
              <button type="button" onClick={() => onPick(s)} className="flex w-full items-center gap-2 rounded-md border border-[#3a2a3d] px-3 py-2 text-left text-w-sm hover:border-[#e8b64a] hover:bg-[#241a29]">
                {s.running && <span className="h-2 w-2 shrink-0 rounded-full bg-[#7be06b]" />}
                <span className="min-w-0 flex-1 truncate">{sessionTitle(s, agentName)}</span>
              </button>
            </li>
          ))}
        </ul>
        <button type="button" onClick={onClose} className="mt-3 text-w-xs text-[#9c8a9a] hover:text-white">
          Never mind
        </button>
      </div>
    </div>
  );
}

export function Workbench({
  request,
  agents,
  renderAgent,
  onOpenAgent,
  onClose,
}: {
  request: OpenWorkshopDetail;
  agents: WorldAgent[];
  renderAgent: (agent: WorldAgent) => React.ReactNode;
  onOpenAgent: (id: string) => void;
  onClose: () => void;
}) {
  const [session, setSession] = useState<SessionBrief | null>(null);
  const [choices, setChoices] = useState<SessionBrief[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [info, setInfo] = useState<WbOpen | null>(null);
  const [layout, setLayout] = useState<Layout>(DEFAULT_LAYOUT);
  const [changes, setChanges] = useState<WbChange[]>([]);
  const [refresh, setRefresh] = useState<{ nonce: number; paths: string[] }>({ nonce: 0, paths: [] });
  const [dirty, setDirty] = useState<string[]>([]);
  const [cursor, setCursor] = useState<{ line: number; column: number; language: string } | null>(null);
  const [quick, setQuick] = useState<"files" | "text" | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const compact = useCompact();
  const editor = useRef<EditorHandle>(null);
  const restored = useRef<string | null>(null);

  const say = useCallback((text: string) => setToast(text), []);
  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(() => setToast(null), 4200);
    return () => window.clearTimeout(id);
  }, [toast]);

  const agentId = session?.agent_id ?? (typeof request === "object" ? request.agentId ?? null : null);
  const agent = agents.find((a) => a.id === agentId) ?? null;

  const reloadSession = useCallback(async () => {
    const wantId = typeof request === "string" ? request : request.sessionId ?? null;
    const wantTask = typeof request === "object" ? request.taskId ?? null : null;
    const wantAgent = typeof request === "object" ? request.agentId ?? null : null;
    const list = await worldApi.sessions(wantAgent ?? undefined);
    if (wantId) return list.find((s) => s.id === wantId) ?? null;
    if (wantTask) return list.find((s) => s.task_id === wantTask && s.role === "task") ?? list.find((s) => s.task_id === wantTask) ?? null;
    const tasks = list.filter((s) => s.role === "task");
    const live = tasks.filter((s) => s.running);
    if (live.length > 1) {
      setChoices(live);
      return undefined;
    }
    return live[0] ?? tasks[0] ?? null;
  }, [request]);

  useEffect(() => {
    let gone = false;
    reloadSession()
      .then((found) => {
        if (gone) return;
        if (found) setSession(found);
        else if (found === null) setProblem((p) => p ?? "That quest has no session yet.");
      })
      .catch(() => !gone && setProblem("Could not load the session."));
    return () => {
      gone = true;
    };
  }, [reloadSession]);

  const key = session ? sessionKeyFor(session) : null;

  useEffect(() => {
    if (!session || !key) {
      if (session && !key) setProblem("This session has no workspace.");
      return;
    }
    let gone = false;
    setChoices(null);
    wb.open(key, session.base_branch ?? null)
      .then((opened) => {
        if (gone) return;
        setInfo(opened);
        if (restored.current !== session.id) {
          restored.current = session.id;
          const stored = readLayout(session.id);
          setLayout(window.matchMedia(COMPACT_QUERY).matches ? { ...stored, left: 0, rightOpen: false } : stored);
        }
      })
      .catch((error: Error) => !gone && setProblem(error.message));
    return () => {
      gone = true;
    };
  }, [session?.id, key]);

  useEffect(() => {
    if (!session || restored.current !== session.id) return;
    try {
      window.localStorage.setItem(storageKey(session.id), JSON.stringify(layout));
    } catch {}
  }, [layout, session]);

  const wkey = info?.key ?? null;
  const loadChanges = useCallback(async () => {
    if (!wkey) return;
    try {
      const result = await wb.changes(wkey, info?.base ?? null);
      setChanges(result.files);
    } catch {}
  }, [wkey, info?.base]);

  useEffect(() => {
    void loadChanges();
  }, [loadChanges]);

  useEffect(() => {
    if (!wkey) return;
    let timer: number | null = null;
    void wb
      .watch(wkey, (paths) => {
        setRefresh((prev) => ({ nonce: prev.nonce + 1, paths }));
        if (timer !== null) window.clearTimeout(timer);
        timer = window.setTimeout(() => void loadChanges(), 250);
      })
      .catch(() => {});
    return () => {
      if (timer !== null) window.clearTimeout(timer);
      void wb.unwatch(wkey).catch(() => {});
    };
  }, [wkey, loadChanges]);

  useEffect(() => {
    const onEvent = (event: Event) => {
      const detail = (event as CustomEvent<{ type: string; agent_id?: string | null }>).detail;
      if (!session || detail.agent_id !== session.agent_id) return;
      if (/^(run|session|review|task)\./.test(detail.type)) {
        void reloadSession().then((found) => found && found.id === session.id && setSession(found));
        void loadChanges();
      }
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [session, reloadSession, loadChanges]);

  const patch = useCallback((next: Partial<Layout> | ((l: Layout) => Partial<Layout>)) => {
    setLayout((l) => ({ ...l, ...(typeof next === "function" ? next(l) : next) }));
  }, []);

  const [extras, setExtras] = useState<SessionBrief[]>([]);
  const [adding, setAdding] = useState(false);
  const [addingFor, setAddingFor] = useState<string | null>(null);
  const folderId = session?.role === "direct" && session.working_dir?.startsWith("folders/") ? session.working_dir.slice("folders/".length) : null;
  const extraIds = layout.extra.join(",");
  const loadExtras = useCallback(async () => {
    const ids = extraIds ? extraIds.split(",") : [];
    if (!ids.length) {
      setExtras([]);
      return;
    }
    try {
      const list = await worldApi.sessions();
      const found = ids.map((id) => list.find((s) => s.id === id)).filter((s): s is SessionBrief => !!s && s.status !== "archived");
      setExtras(found);
      if (found.length !== ids.length) patch((l) => ({ extra: l.extra.filter((id) => found.some((s) => s.id === id)) }));
    } catch {}
  }, [extraIds, patch]);
  useEffect(() => {
    void loadExtras();
  }, [loadExtras]);
  const extraAgents = useMemo(() => new Set(extras.map((s) => s.agent_id)), [extras]);
  const extraEvent = useCallback(
    (e: WorldEvent) => !!e.agent_id && extraAgents.has(e.agent_id) && ["run.started", "run.completed", "run.failed", "run.timed_out"].includes(e.type),
    [extraAgents],
  );
  const reloadExtras = useCallback(() => void loadExtras(), [loadExtras]);
  useWorldRefresh(reloadExtras, extraEvent);
  const helpers = useMemo(
    () => (agent?.owner_user_id ? agents.filter((a) => a.owner_user_id === agent.owner_user_id) : []),
    [agents, agent],
  );
  const addTab = async (helper: WorldAgent) => {
    if (!folderId || addingFor) return;
    setAddingFor(helper.id);
    try {
      const made = await worldApi.startDirect(helper.id, { folder_id: folderId, folder_name: session?.title ?? undefined });
      const list = await worldApi.sessions(helper.id);
      const brief = list.find((s) => s.id === made.session_id);
      if (brief) setExtras((prev) => [...prev, brief]);
      patch((l) => ({ extra: [...l.extra, made.session_id], pane: made.session_id, rightOpen: true }));
      setAdding(false);
    } catch (error) {
      say(error instanceof Error ? error.message : "Could not add that agent.");
    } finally {
      setAddingFor(null);
    }
  };
  const closeTab = (id: string) => patch((l) => ({ extra: l.extra.filter((x) => x !== id), pane: l.pane === id ? "executor" : l.pane }));
  const tabLabel = (s: SessionBrief) => {
    const name = agents.find((a) => a.id === s.agent_id)?.name ?? "Agent";
    const same = [session, ...extras].filter((x): x is SessionBrief => !!x && x.agent_id === s.agent_id);
    return same.length > 1 ? `${name} ${same.findIndex((x) => x.id === s.id) + 1}` : name;
  };

  const openFile = useCallback(
    (path: string, mode: TabMode = "code") => {
      patch((l) => {
        const exists = l.tabs.find((t) => t.path === path);
        const tabs = exists ? l.tabs.map((t) => (t.path === path ? { ...t, mode } : t)) : [...l.tabs, { path, mode }];
        return { tabs, active: path, ...(compact ? { left: 0, rightOpen: false } : {}) };
      });
    },
    [patch, compact],
  );

  const openChange = useCallback((change: WbChange) => openFile(change.path, "diff"), [openFile]);

  useEffect(() => {
    if (!session || !info) return;
    setOpenWorkshop({ sessionId: session.id, agentId: session.agent_id, runnerId: agent?.runner_id ?? null, root: info.root });
    return () => setOpenWorkshop(null);
  }, [session, info, agent?.runner_id]);

  useEffect(() => {
    if (!info || typeof request !== "object" || !request.file) return;
    const wanted = typeof request === "object" ? request.sessionId : null;
    if (wanted && session && wanted !== session.id) return;
    const file = request.file;
    const line = request.line ?? null;
    openFile(file);
    if (line) window.setTimeout(() => editor.current?.reveal(file, line, 1), 120);
  }, [request, info, session, openFile]);

  const step = useCallback(
    (direction: 1 | -1) => {
      if (!changes.length) return;
      const at = changes.findIndex((c) => c.path === layout.active);
      const next = at < 0 ? (direction === 1 ? 0 : changes.length - 1) : (at + direction + changes.length) % changes.length;
      openChange(changes[next]);
    },
    [changes, layout.active, openChange],
  );

  const attemptClose = useCallback(() => {
    const unsaved = editor.current?.dirtyPaths() ?? [];
    if (unsaved.length && !window.confirm(`${unsaved.length} file${unsaved.length === 1 ? " has" : "s have"} unsaved changes. Leave the workshop anyway?`)) return;
    onClose();
  }, [onClose]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey;
      const k = event.key.toLowerCase();
      if (mod && !event.shiftKey && k === "p") {
        event.preventDefault();
        event.stopPropagation();
        if (wkey) setQuick("files");
        return;
      }
      if (mod && event.shiftKey && k === "f") {
        event.preventDefault();
        event.stopPropagation();
        if (wkey) setQuick("text");
        return;
      }
      if (mod && k === "s") {
        if ((event.target as HTMLElement | null)?.closest?.(".cm-editor")) return;
        event.preventDefault();
        editor.current?.saveActive();
        return;
      }
      if ((mod && k === "j") || (event.ctrlKey && event.key === "`")) {
        event.preventDefault();
        event.stopPropagation();
        patch((l) => ({ terminal: !l.terminal }));
        return;
      }
      if (mod && k === "b") {
        event.preventDefault();
        event.stopPropagation();
        patch((l) => ({ left: l.left > 0 ? 0 : 260 }));
        return;
      }
      if (mod && /^[1-9]$/.test(event.key)) {
        event.stopPropagation();
        return;
      }
      if (quick) return;
      const bracket = event.code === "BracketRight" || event.key === "]" ? 1 : event.code === "BracketLeft" || event.key === "[" ? -1 : 0;
      if (bracket && !mod && (event.altKey || (!event.shiftKey && !typingIn(event.target)))) {
        event.preventDefault();
        step(bracket);
        return;
      }
      if (event.key === "Escape" && !typingIn(event.target)) {
        event.preventDefault();
        event.stopPropagation();
        attemptClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [wkey, quick, step, attemptClose, patch]);

  const liveJobs = (agent?.jobs ?? []).filter((job) => job.session_id === session?.id);
  const live = !!session?.running || liveJobs.length > 0;
  const dirtyCount = dirty.length;

  const header = (
    <div className="flex h-10 shrink-0 items-center gap-3 border-b border-[#3a2a3d] bg-[#1d1422] px-3 text-[#e9dcc6]">
      <Hammer size={16} weight="bold" className="text-[#e8b64a]" />
      <span className={cn(PIXEL, "hidden text-w-md sm:inline")}>Workshop</span>
      {session && (
        <span className="min-w-0 truncate text-w-sm text-[#9c8a9a]">
          {agent ? `${agent.avatar_emoji} ${agent.name} · ` : ""}
          {sessionTitle(session, agent?.name)}
        </span>
      )}
      <span className="flex-1" />
      {wkey && (
        <button type="button" onClick={() => setQuick("files")} className="hidden items-center gap-1.5 rounded-md border border-[#3a2a3d] px-2 py-0.5 text-w-xs text-[#9c8a9a] hover:text-white md:flex">
          <MagnifyingGlass size={12} /> Go to file <kbd className="font-mono text-w-3xs">⌘P</kbd>
        </button>
      )}
      <button type="button" title="Explorer" aria-label="Toggle explorer" onClick={() => patch((l) => ({ left: l.left > 0 ? 0 : 260, ...(compact ? { rightOpen: false } : {}) }))} className="text-[#9c8a9a] hover:text-white [@media(pointer:coarse)]:p-1.5">
        <SidebarSimple size={16} />
      </button>
      <button type="button" title="Terminal" aria-label="Toggle terminal" aria-pressed={layout.terminal} onClick={() => patch((l) => ({ terminal: !l.terminal }))} className={cn("hover:text-white [@media(pointer:coarse)]:p-1.5", layout.terminal ? "text-[#e9dcc6]" : "text-[#9c8a9a]")}>
        <TerminalWindow size={16} />
      </button>
      <button type="button" title="Agent panel" aria-label="Toggle agent panel" aria-pressed={layout.rightOpen} onClick={() => patch((l) => ({ rightOpen: !l.rightOpen, ...(compact ? { left: 0 } : {}) }))} className={cn("hover:text-white [@media(pointer:coarse)]:p-1.5", layout.rightOpen ? "text-[#e9dcc6]" : "text-[#9c8a9a]")}>
        <Robot size={16} />
      </button>
      <button type="button" aria-label="Close workshop" onClick={attemptClose} className="grid h-6 w-6 place-items-center [@media(pointer:coarse)]:h-8 [@media(pointer:coarse)]:w-8 rounded-md border border-[#3a2a3d] text-[#9c8a9a] hover:border-[#e07a5f] hover:text-white">
        <X size={13} weight="bold" />
      </button>
    </div>
  );

  if (choices && !session) {
    return (
      <div role="dialog" aria-modal="true" aria-label="Workshop" className="fixed inset-0 z-[70] flex flex-col bg-[#140d18]">
        {header}
        <Picker
          sessions={choices}
          agentName={agent?.name ?? "This agent"}
          onPick={(s) => {
            setChoices(null);
            setSession(s);
          }}
          onClose={onClose}
        />
      </div>
    );
  }

  if (problem || !info || !wkey || !session) {
    return (
      <div role="dialog" aria-modal="true" aria-label="Workshop" className="fixed inset-0 z-[70] flex flex-col bg-[#140d18]">
        {header}
        <div className="grid flex-1 place-items-center p-6 text-center">
          <div className="max-w-md text-[#9c8a9a]">
            <p className={cn(PIXEL, "text-w-lg text-[#e9dcc6]")}>{problem ? "The workshop is closed" : "Opening the workshop..."}</p>
            {problem && <p className="mt-2 text-w-sm">{problem}</p>}
            {problem && /not on this Mac/i.test(problem) && (
              <p className="mt-2 text-w-xs">The workshop opens the files on this Mac. Quests that ran in the cloud or on another device are not here.</p>
            )}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div role="dialog" aria-modal="true" aria-label="Workshop" className="fixed inset-0 z-[70] flex flex-col bg-[#140d18] text-[#e9dcc6]">
      {header}
      {live && (
        <div className="flex shrink-0 items-center gap-2 border-b border-[#4a3a1a] bg-[#2c2312] px-3 py-1 text-w-xs text-[#f2d79a]">
          <span className="min-w-0 flex-1 truncate">
            {agent?.name ?? "The agent"} is working here right now{liveJobs[0]?.activity ? `: ${liveJobs[0].activity}` : ""}. Saves only check the file you opened, so watch for changes on disk.
          </span>
        </div>
      )}
      <div className="relative flex min-h-0 flex-1">
        {compact && (layout.left > 0 || layout.rightOpen) && (
          <button type="button" aria-label="Close panel" onClick={() => patch({ left: 0, rightOpen: false })} className="absolute inset-0 z-20 bg-black/40" />
        )}
        {layout.left > 0 && (
          <>
            <div style={compact ? undefined : { width: layout.left }} className={cn(compact ? "absolute inset-y-0 left-0 z-30 w-[min(20rem,85vw)] shadow-[4px_0_0_#0b070d]" : "min-w-[10rem] shrink-0")}>
              <FileTree
                sessionKey={wkey}
                changes={changes}
                active={layout.active}
                refresh={refresh}
                expanded={layout.expanded}
                onExpandedChange={(expanded) => patch({ expanded })}
                onOpen={(path) => openFile(path)}
                onOpenChange={openChange}
                onRenamed={(from, to) =>
                  patch((l) => ({
                    tabs: l.tabs.map((t) => (t.path === from || t.path.startsWith(`${from}/`) ? { ...t, path: to + t.path.slice(from.length) } : t)),
                    active: l.active && (l.active === from || l.active.startsWith(`${from}/`)) ? to + l.active.slice(from.length) : l.active,
                  }))
                }
                onDeleted={(path) =>
                  patch((l) => {
                    const tabs = l.tabs.filter((t) => t.path !== path && !t.path.startsWith(`${path}/`));
                    return { tabs, active: tabs.some((t) => t.path === l.active) ? l.active : tabs[tabs.length - 1]?.path ?? null };
                  })
                }
                onError={say}
              />
            </div>
            {!compact && <Resizer axis="x" onDrag={(d) => patch((l) => ({ left: Math.max(160, Math.min(560, l.left + d)) }))} />}
          </>
        )}
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="min-h-0 flex-1">
            <EditorPane
              ref={editor}
              sessionKey={wkey}
              base={info.base}
              tabs={layout.tabs}
              active={layout.active}
              changes={changes}
              refresh={refresh}
              split={layout.split}
              onSplitChange={(split) => patch({ split })}
              onActivate={(active) => patch({ active })}
              onClose={(path) =>
                patch((l) => {
                  const at = l.tabs.findIndex((t) => t.path === path);
                  const tabs = l.tabs.filter((t) => t.path !== path);
                  return { tabs, active: l.active === path ? tabs[Math.min(at, tabs.length - 1)]?.path ?? null : l.active };
                })
              }
              onModeChange={(path, mode) => openFile(path, mode)}
              onCursor={setCursor}
              onDirtyChange={setDirty}
              onSaved={() => {
                say("Saved.");
                void loadChanges();
              }}
              onError={say}
            />
          </div>
          {layout.terminal && (
            <>
              <Resizer axis="y" onDrag={(d) => patch((l) => ({ bottom: Math.max(120, Math.min(window.innerHeight - 220, l.bottom - d)) }))} />
              <div style={{ height: layout.bottom }} className="shrink-0">
                <TerminalPane sessionKey={wkey} onHide={() => patch({ terminal: false })} onError={say} />
              </div>
            </>
          )}
        </div>
        {layout.rightOpen && (
          <>
            {!compact && <Resizer axis="x" onDrag={(d) => patch((l) => ({ right: Math.max(300, Math.min(maxRight(), l.right - d)) }))} />}
            <div
              style={compact ? undefined : { width: layout.right }}
              className={cn(
                "flex flex-col border-l border-[#3a2a3d] bg-[#1d1422]",
                compact ? "absolute inset-y-0 right-0 z-30 w-[min(26rem,92vw)] shadow-[-4px_0_0_#0b070d]" : "min-w-[18rem] shrink-0",
              )}
            >
              <div className="relative flex shrink-0 items-center gap-1 border-b border-[#3a2a3d] px-2 pt-1.5">
                <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
                  {["executor", ...extras.map((s) => s.id), "agent"].map((pane) => {
                    const extra = extras.find((s) => s.id === pane);
                    const label = extra
                      ? tabLabel(extra)
                      : pane === "executor"
                        ? session.role === "direct"
                          ? extras.length
                            ? tabLabel(session)
                            : "Chat"
                          : "Executor"
                        : session.role === "direct" && extras.length
                          ? "Main mind"
                          : agent?.name ?? "Agent";
                    return (
                      <span
                        key={pane}
                        className={cn("flex shrink-0 items-center rounded-t text-w-xs", layout.pane === pane ? "bg-[#fbf3e0] text-[#261822]" : "text-[#9c8a9a] hover:text-white")}
                      >
                        <button type="button" aria-pressed={layout.pane === pane} onClick={() => patch({ pane })} className={cn("py-1", extra ? "pl-3 pr-1" : "px-3")}>
                          {extra?.running ? <Shimmer>{label}</Shimmer> : label}
                        </button>
                        {extra && (
                          <button type="button" aria-label={`Close ${label}`} onClick={() => closeTab(extra.id)} className="mr-1 rounded-md p-0.5 opacity-60 hover:opacity-100">
                            <X size={10} weight="bold" />
                          </button>
                        )}
                      </span>
                    );
                  })}
                </div>
                {folderId && (
                  <button
                    type="button"
                    aria-label="Add an agent to this folder"
                    title="Add an agent, or another session of one, to this folder"
                    aria-expanded={adding}
                    onClick={() => setAdding((a) => !a)}
                    className={cn("mb-1 grid h-6 w-6 shrink-0 place-items-center rounded-md border border-[#3a2a3d] hover:text-white", adding ? "text-white" : "text-[#9c8a9a]")}
                  >
                    <Plus size={12} weight="bold" />
                  </button>
                )}
                {adding && folderId && (
                  <div className="absolute right-2 top-full z-40 mt-1 w-64 rounded-md border border-[#3a2a3d] bg-[#1d1422] p-1.5 text-[#e9dcc6] shadow-[0_4px_0_#0b070d]">
                    <p className="px-2 pb-1 pt-0.5 text-w-2xs text-[#9c8a9a]">Each pick opens a new session in {session.title ?? "this folder"}.</p>
                    {helpers.length === 0 ? (
                      <p className="px-2 py-1.5 text-w-xs text-[#9c8a9a]">No agents to add yet.</p>
                    ) : (
                      <ul>
                        {helpers.map((a) => (
                          <li key={a.id}>
                            <button
                              type="button"
                              disabled={!!addingFor}
                              onClick={() => void addTab(a)}
                              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-w-xs hover:bg-[#241a29] disabled:opacity-60"
                            >
                              <span aria-hidden>{a.avatar_emoji}</span>
                              <span className="min-w-0 flex-1 truncate">{a.name}</span>
                              {addingFor === a.id ? <Shimmer className="text-w-2xs">opening</Shimmer> : <span className="truncate text-w-2xs text-[#7d6b7b]">{a.id === agent?.id || extraAgents.has(a.id) ? "new session" : a.model}</span>}
                            </button>
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>
                )}
              </div>
              <button
                type="button"
                onClick={() => changes[0] && openChange(changes[0])}
                disabled={!changes.length}
                className="flex shrink-0 items-center gap-2 border-b border-[#3a2a3d] px-3 py-1.5 text-left text-w-xs hover:bg-[#241a29] disabled:cursor-default disabled:hover:bg-transparent"
              >
                <span className="font-semibold">
                  {changes.length} {changes.length === 1 ? "file" : "files"} changed
                </span>
                <span className="flex min-w-0 flex-1 gap-1 overflow-hidden">
                  {(["M", "A", "U", "D", "R"] as const).map((s) => {
                    const n = changes.filter((c) => c.status === s).length;
                    return n ? (
                      <span key={s} className={cn("font-mono text-w-2xs", STATUS_COLOR[s])}>
                        {n}
                        {s}
                      </span>
                    ) : null;
                  })}
                </span>
                {changes.length > 0 && <span className="rounded-md bg-[#e8b64a] px-1.5 py-px text-w-2xs font-semibold text-[#261822]">Review</span>}
              </button>
              <div className="min-h-0 flex-1 overflow-hidden bg-[#fbf3e0] text-[#3b2a1a]">
                {layout.pane === "executor" ? (
                  <SessionDetail
                    key={session.id}
                    session={session}
                    agents={agents}
                    onSessionRefresh={() => void reloadSession().then((found) => found && setSession(found))}
                    onOpenAgent={onOpenAgent}
                    workshop={false}
                  />
                ) : extras.some((s) => s.id === layout.pane) ? (
                  <SessionDetail
                    key={layout.pane}
                    session={extras.find((s) => s.id === layout.pane)!}
                    agents={agents}
                    onSessionRefresh={reloadExtras}
                    onOpenAgent={onOpenAgent}
                    workshop={false}
                  />
                ) : layout.pane !== "agent" ? (
                  <p className="p-4 text-w-sm text-[#8a705a]">This session is closed.</p>
                ) : agent ? (
                  <div className="h-full overflow-hidden">{renderAgent(agent)}</div>
                ) : (
                  <p className="p-4 text-w-sm text-[#8a705a]">This agent is not visible to you.</p>
                )}
              </div>
            </div>
          </>
        )}
      </div>
      <div className="flex h-6 shrink-0 items-center gap-4 overflow-hidden whitespace-nowrap border-t border-[#3a2a3d] bg-[#1d1422] px-3 text-w-2xs text-[#9c8a9a]">
        {info.branch && (
          <span className="flex items-center gap-1 text-[#e9dcc6]">
            <GitBranch size={11} weight="bold" />
            {info.branch}
            {info.base && <span className="text-[#7d6b7b]">→ {info.base}</span>}
          </span>
        )}
        <span className="hidden sm:inline">{info.name}</span>
        {dirtyCount > 0 && <span className="text-[#e8b64a]">{dirtyCount} unsaved</span>}
        <span className="flex-1" />
        {toast && <span className="text-[#e9dcc6]">{toast}</span>}
        {cursor && (
          <>
            <span>
              Ln {cursor.line}, Col {cursor.column}
            </span>
            <span>UTF-8</span>
            <span>{cursor.language}</span>
          </>
        )}
      </div>
      {quick && (
        <QuickOpen
          sessionKey={wkey}
          mode={quick}
          onMode={setQuick}
          onClose={() => setQuick(null)}
          onPick={(path, line, column) => {
            setQuick(null);
            openFile(path);
            if (line) editor.current?.reveal(path, line, column ?? 1);
          }}
        />
      )}
    </div>
  );
}
