"use client";

import { createContext, memo, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ComponentType, type ReactNode } from "react";
import { createPortal } from "react-dom";
import dynamic from "next/dynamic";
import { useStoreApi } from "@xyflow/react";
import { ArrowSquareOut, Browser, Flag, Monitor, Pause, TerminalWindow, Trash, X, type Icon as PhosphorIcon } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type QuestRoomPage, type RunnerView, type TaskBrief, type WorldEvent } from "@/lib/world";
import { desktopApi, isDesktopApp } from "@/lib/desktop";
import type { CanvasItem } from "@/lib/reports";
import {
  EMBEDS,
  PLACEABLE_EMBEDS,
  bodyMode,
  currentStep,
  embedSlots,
  screenScale,
  stagePlan,
  startsShellBySelf,
  embedLive,
  nextRun,
  parseTerminalTarget,
  pushTail,
  runProgress,
  readEmbed,
  terminalTarget,
  type CanvasEmbed,
  type EmbedDrop,
  type EmbedKind,
  type EmbedRun,
  type EmbedSlots,
} from "@/lib/canvas-embeds";
import { QUEST_STAGE_LABEL, questEventTaskId, questStage } from "@/lib/quest-status";
import { runIsLive } from "./panels";
import { eventLine, QUIET_EVENTS } from "./quest-room";
import { StatusIcon } from "./quest-ui";
import { milestoneProgress } from "./milestones-ui";
import { Shimmer } from "./shimmer";
import { EASE, FOCUS } from "./styles";
import { useLiveQuests } from "./live-quests";

const TerminalPanel = dynamic(() => import("./terminal").then((m) => m.TerminalPanel), {
  ssr: false,
  loading: () => <Skeleton dark label="Opening the terminal" />,
});

export type EmbedHost = {
  reportId: string;
  ownerId: string;
  meId: string | null;
  canEdit: boolean;
  remove: (item: CanvasItem) => void;
};

type ViewProps = { item: CanvasItem; embed: CanvasEmbed; host: EmbedHost; fontScale: number };

type EmbedView = {
  icon: PhosphorIcon;
  dark?: boolean;
  bodyTitled?: boolean;
  openLabel?: string;
  open?: (embed: CanvasEmbed) => void;
  ownerOnly?: string;
  releaseGone?: (reportId: string, present: ReadonlySet<string>) => void;
  Body: ComponentType<ViewProps>;
};

function Skeleton({ label, dark = false }: { label: string; dark?: boolean }) {
  return (
    <div className={cn("grid h-full w-full place-items-center p-4 text-center text-w-sm", dark ? "bg-w-term-bg text-w-term-dim" : "bg-w-card text-w-muted")}>
      <Shimmer>{label}</Shimmer>
    </div>
  );
}

function Note({ children, dark = false }: { children: ReactNode; dark?: boolean }) {
  return <div className={cn("grid h-full w-full place-items-center p-4 text-center text-w-sm text-balance", dark ? "bg-w-term-bg text-w-term-dim" : "bg-w-card text-w-muted")}>{children}</div>;
}

const embedKey = (host: EmbedHost, item: CanvasItem) => `${host.reportId}:${item.id}`;

const TerminalBody = memo(function TerminalBody({ item, embed, host, fontScale }: ViewProps) {
  const target = parseTerminalTarget(embed.target);
  const key = embedKey(host, item);
  const device = target?.device ?? null;
  const shell = target?.shell ?? null;
  const startsShell = startsShellBySelf(item.by);
  const source = useMemo(() => (device ? { kind: "embed" as const, key, device, shell, startsShell } : null), [key, device, shell, startsShell]);
  const [fresh, setFresh] = useState(0);
  if (!source) return <Note dark>This terminal points at a device Loka cannot read.</Note>;
  return (
    <div className="nowheel nodrag h-full w-full cursor-auto">
      <TerminalPanel key={fresh} meId={host.meId} onClose={() => setFresh((n) => n + 1)} source={source} fontScale={fontScale} />
    </div>
  );
});

function openQuest(id: string, agentId: string | null) {
  window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: id, agentId } }));
}

type QuestState = { page: QuestRoomPage | null; problem: string | null };

function QuestBody({ embed }: ViewProps) {
  const id = embed.target;
  const [state, setState] = useState<QuestState>({ page: null, problem: null });
  const [tail, setTail] = useState<string[]>([]);
  const loading = useRef(false);
  const again = useRef(false);
  const timer = useRef<number | null>(null);
  const gone = useRef(false);

  const load = useCallback(() => {
    if (loading.current) {
      again.current = true;
      return;
    }
    loading.current = true;
    worldApi
      .questRoom(id)
      .then((page) => {
        if (gone.current) return;
        setState({ page, problem: null });
        const who = "The agent";
        const lines = page.room.events.filter((e) => !QUIET_EVENTS.test(e.type)).slice(-3).map((e) => eventLine(e, who));
        setTail((prev) => (prev.length ? prev : lines.reduce<string[]>((acc, line) => pushTail(acc, line), [])));
      })
      .catch((error: unknown) => {
        if (gone.current) return;
        const msg = error instanceof Error ? error.message : "";
        const problem = msg.startsWith("403") ? "You cannot open this quest." : msg.startsWith("404") ? "This quest was deleted." : "Could not load this quest. It tries again on the next update.";
        setState((prev) => ({ page: msg.startsWith("403") || msg.startsWith("404") ? null : prev.page, problem }));
      })
      .finally(() => {
        loading.current = false;
        if (again.current && !gone.current) {
          again.current = false;
          load();
        }
      });
  }, [id]);

  useEffect(() => {
    gone.current = false;
    load();
    return () => {
      gone.current = true;
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, [load]);

  const page = state.page;
  const liveRun = useMemo(() => (page ? [...page.room.runs].reverse().find(runIsLive) ?? null : null), [page]);

  useEffect(() => {
    const soon = () => {
      if (timer.current !== null) return;
      timer.current = window.setTimeout(() => {
        timer.current = null;
        load();
      }, 700);
    };
    const onEvent = (event: Event) => {
      const e = (event as CustomEvent<WorldEvent>).detail;
      if (questEventTaskId(e) === id) {
        if (e.type !== "task.changed" && e.type !== "task.deleted" && !QUIET_EVENTS.test(e.type)) setTail((prev) => pushTail(prev, eventLine(e, "The agent")));
        soon();
        return;
      }
      const line = runProgress(e, liveRun?.id ?? null);
      if (line) setTail((prev) => pushTail(prev, line));
    };
    window.addEventListener("world-event", onEvent);
    window.addEventListener("world-reconnected", load);
    return () => {
      window.removeEventListener("world-event", onEvent);
      window.removeEventListener("world-reconnected", load);
    };
  }, [id, load, liveRun]);

  if (!page) return state.problem ? <Note>{state.problem}</Note> : <Skeleton label="Loading the quest" />;
  const task = page.task;
  const stage = questStage(task);
  const step = currentStep(task.milestones);
  const progress = milestoneProgress(task.milestones);
  const steps = task.milestones.filter((m) => m.status !== "skipped");
  return (
    <div className="nowheel flex h-full w-full flex-col gap-2 overflow-hidden bg-w-card p-3 text-w-sm">
      <div className="flex min-w-0 items-start gap-2">
        <StatusIcon status={task.status} size={16} />
        <p className="line-clamp-2 min-w-0 flex-1 font-semibold leading-snug text-w-strong">{task.title}</p>
      </div>
      <p className="flex items-center gap-2 text-w-xs font-semibold">
        <span className={cn(stage === "needs_you" ? "text-w-alert" : stage === "stuck" ? "text-w-danger" : "text-w-text2")}>
          {stage === "working" ? <Shimmer>{QUEST_STAGE_LABEL.working}</Shimmer> : QUEST_STAGE_LABEL[stage]}
        </span>
        {progress && <span className="font-medium text-w-muted">{progress} steps</span>}
        {state.problem && <span className="font-medium text-w-muted">{state.problem}</span>}
      </p>
      {steps.length > 0 && (
        <ul className="min-h-0 shrink space-y-0.5 overflow-hidden text-w-xs" aria-label="Checklist">
          {steps.slice(0, 6).map((m) => (
            <li key={m.id} className={cn("flex items-center gap-1.5 truncate", m.status === "done" ? "text-w-muted line-through" : m.id === step?.id ? "font-semibold text-w-strong" : "text-w-text2")}>
              <StatusIcon status={m.status === "done" ? "done" : m.status === "failed" ? "failed" : m.status === "active" ? "in_progress" : "todo"} size={11} />
              <span className="truncate">{m.title}</span>
            </li>
          ))}
        </ul>
      )}
      {tail.length > 0 && (
        <div className="mt-auto min-h-0 space-y-0.5 overflow-hidden border-t-2 border-dashed border-w-line pt-1.5 font-mono text-w-2xs leading-snug text-w-text2" aria-live="polite">
          {tail.map((line, n) => (
            <p key={`${n}:${line}`} className={cn("truncate", n < tail.length - 1 && "text-w-muted")}>
              {line}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}

function BrowserBody() {
  return <Note>Watching an agent&apos;s browser in a workspace is not available yet.</Note>;
}

export const EMBED_VIEWS: Record<EmbedKind, EmbedView> = {
  terminal: {
    icon: TerminalWindow,
    dark: true,
    openLabel: "Open the terminal panel",
    open: () => window.dispatchEvent(new CustomEvent("world:open-terminal")),
    ownerOnly: "This terminal runs on a device of the workspace owner, so only they can watch it.",
    releaseGone: (reportId, present) => void import("./terminal").then((m) => m.releaseEmbedTerminals(reportId, present)),
    Body: TerminalBody,
  },
  quest: {
    icon: Flag,
    bodyTitled: true,
    openLabel: "Open the quest",
    open: (embed) => openQuest(embed.target, null),
    Body: QuestBody,
  },
  browser: { icon: Browser, Body: BrowserBody },
};

function usePageVisible(): boolean {
  const [visible, setVisible] = useState(() => typeof document === "undefined" || document.visibilityState !== "hidden");
  useEffect(() => {
    const on = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", on);
    return () => document.removeEventListener("visibilitychange", on);
  }, []);
  return visible;
}

function useOnScreen<T extends Element>(): [(node: T | null) => void, boolean] {
  const [node, setNode] = useState<T | null>(null);
  const [seen, setSeen] = useState(true);
  useEffect(() => {
    if (!node || typeof IntersectionObserver === "undefined") {
      setSeen(!!node);
      return;
    }
    const watch = new IntersectionObserver((entries) => setSeen(entries.some((e) => e.isIntersecting)), { threshold: 0 });
    watch.observe(node);
    return () => watch.disconnect();
  }, [node]);
  return [setNode, seen];
}

type Stage = { start: (key: string) => void; slots: EmbedSlots<HTMLDivElement> };

const StageCtx = createContext<Stage | null>(null);

function useSettledScale(ms: number): number {
  const store = useStoreApi();
  const [scale, setScale] = useState(() => screenScale(store.getState().transform[2]));
  useEffect(() => {
    let zoom = store.getState().transform[2];
    let timer = 0;
    const unsubscribe = store.subscribe((state) => {
      const next = state.transform[2];
      if (next === zoom) return;
      zoom = next;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setScale(screenScale(zoom)), ms);
    });
    return () => {
      unsubscribe();
      window.clearTimeout(timer);
    };
  }, [store, ms]);
  return scale;
}

export function EmbedStage({ items, host, children }: { items: CanvasItem[]; host: EmbedHost; children: ReactNode }) {
  const holder = useRef<HTMLDivElement>(null);
  const [slots] = useState(() =>
    embedSlots<HTMLDivElement>(
      () => {
        const made = document.createElement("div");
        made.className = "absolute inset-0";
        return made;
      },
      (made) => holder.current?.appendChild(made),
    ),
  );
  const [started, setStarted] = useState<ReadonlySet<string>>(() => new Set());
  const scale = useSettledScale(160);
  const owner = host.meId !== null && host.meId === host.ownerId;
  const entries = useMemo(
    () =>
      items.flatMap((item) => {
        const embed = readEmbed(item.embed);
        return item.kind === "embed" && embed ? [{ key: embedKey(host, item), kind: embed.kind, item, embed }] : [];
      }),
    [items, host],
  );
  const plan = stagePlan(started, entries);
  const gone = plan.gone.join("\n");
  useEffect(() => {
    if (!gone) return;
    const drop = new Set(gone.split("\n"));
    for (const key of drop) slots.drop(key);
    setStarted((prev) => new Set([...prev].filter((key) => !drop.has(key))));
  }, [gone, slots]);
  const present = entries.map((e) => e.item.id).join("\n");
  useEffect(() => {
    if (!owner) return;
    const ids = new Set(present ? present.split("\n") : []);
    for (const view of Object.values(EMBED_VIEWS)) view.releaseGone?.(host.reportId, ids);
  }, [owner, host.reportId, present]);
  useEffect(() => () => slots.clear(), [slots]);
  const start = useCallback((key: string) => setStarted((prev) => (prev.has(key) ? prev : new Set(prev).add(key))), []);
  const stage = useMemo<Stage>(() => ({ start, slots }), [start, slots]);
  const keepKeys = plan.keep.join("\n");
  const kept = useMemo(
    () =>
      (keepKeys ? keepKeys.split("\n") : []).map((key) => {
        const entry = entries.find((e) => e.key === key);
        if (!entry || (EMBED_VIEWS[entry.kind].ownerOnly && !owner)) return null;
        const View = EMBED_VIEWS[entry.kind].Body;
        return createPortal(<View item={entry.item} embed={entry.embed} host={host} fontScale={scale} />, slots.node(key), key);
      }),
    [keepKeys, entries, host, scale, owner, slots],
  );
  return (
    <StageCtx.Provider value={stage}>
      {children}
      <div ref={holder} hidden />
      {kept}
    </StageCtx.Provider>
  );
}

function EmbedSlot({ slotKey, live, dark }: { slotKey: string; live: boolean; dark: boolean }) {
  const stage = useContext(StageCtx);
  const slot = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (live) stage?.start(slotKey);
  }, [live, stage, slotKey]);
  const slots = stage?.slots;
  useLayoutEffect(() => {
    const here = slot.current;
    if (!slots || !here) return;
    return slots.attach(slotKey, here);
  }, [slots, slotKey]);
  if (!stage) return <Note dark={dark}>Reload the workspace to watch this.</Note>;
  return <div ref={slot} className={cn("relative h-full w-full overflow-hidden", dark ? "bg-w-term-bg" : "bg-w-card")} />;
}

export function EmbedItem({ item, host }: { item: CanvasItem; host: EmbedHost }) {
  const embed = readEmbed(item.embed);
  const [ref, onScreen] = useOnScreen<HTMLDivElement>();
  const pageVisible = usePageVisible();
  const live = embedLive({ mounted: true, onScreen, pageVisible });
  const [run, setRun] = useState<EmbedRun>(() => ({ live, round: live ? 1 : 0 }));
  useEffect(() => setRun((prev) => nextRun(prev, live)), [live]);
  const view = embed ? EMBED_VIEWS[embed.kind] : null;
  const mode = embed ? bodyMode(embed.kind, run.live) : "paused";
  const Icon = view?.icon ?? Monitor;
  const label = embed ? EMBEDS[embed.kind].label : "Embed";
  const title = (!view?.bodyTitled && item.caption) || label;
  const blocked = view?.ownerOnly && host.meId !== host.ownerId ? view.ownerOnly : null;
  const remove = () => host.remove(item);
  return (
    <div ref={ref} className={cn("flex h-full w-full flex-col overflow-hidden rounded-lg border-2 border-w-ink shadow-[0_3px_0_rgb(var(--w-ink))]", view?.dark ? "bg-w-term-bg" : "bg-w-card")}>
      <div className="flex h-8 shrink-0 items-center gap-1.5 border-b-2 border-w-ink bg-w-paper px-2">
        <Icon size={14} weight="duotone" className="shrink-0 text-w-text2" aria-hidden />
        <span className="min-w-0 flex-1 truncate text-w-xs font-semibold text-w-strong" title={title}>
          {title}
        </span>
        {!run.live && embed && <Pause size={12} weight="bold" className="shrink-0 text-w-faint" aria-label="Paused while out of view" />}
        {embed && view?.open && !blocked && (
          <button type="button" onClick={() => view.open?.(embed)} aria-label={view.openLabel} title={view.openLabel} className={cn("nodrag grid h-6 w-6 place-items-center rounded text-w-muted hover:bg-w-sand hover:text-w-strong", EASE, FOCUS)}>
            <ArrowSquareOut size={13} weight="bold" />
          </button>
        )}
        {host.canEdit && (
          <button type="button" onClick={remove} aria-label="Remove from the workspace" title="Remove from the workspace" className={cn("nodrag grid h-6 w-6 place-items-center rounded text-w-muted hover:bg-w-sand hover:text-w-danger", EASE, FOCUS)}>
            <Trash size={13} weight="bold" />
          </button>
        )}
      </div>
      <div className="relative min-h-0 flex-1">
        {!embed || !view ? (
          <Note>This embed is from a newer Loka. Reload to see it.</Note>
        ) : blocked ? (
          <Note dark={view.dark}>{blocked}</Note>
        ) : mode === "kept" ? (
          <EmbedSlot slotKey={embedKey(host, item)} live={run.live} dark={!!view.dark} />
        ) : mode === "shown" ? (
          <view.Body key={run.round} item={item} embed={embed} host={host} fontScale={1} />
        ) : (
          <Note dark={view.dark}>Paused while out of view</Note>
        )}
      </div>
    </div>
  );
}

type Device = { id: string; name: string; here: boolean };

function useMyDevices(meId: string | null): Device[] | null {
  const [runners, setRunners] = useState<RunnerView[] | null>(null);
  const [here, setHere] = useState<string | null>(null);
  useEffect(() => {
    let gone = false;
    worldApi
      .runners()
      .then((list) => !gone && setRunners(list))
      .catch(() => !gone && setRunners([]));
    if (isDesktopApp())
      desktopApi
        .status()
        .then((status) => !gone && setHere(status.features?.includes("home_terminal") ? status.runner_id : null))
        .catch(() => {});
    return () => {
      gone = true;
    };
  }, []);
  if (!runners) return null;
  return runners
    .filter((r) => r.status === "online" && (!meId || r.owner_user_id === meId))
    .map((r) => ({ id: r.id, name: r.name, here: r.id === here }))
    .sort((a, b) => Number(b.here) - Number(a.here));
}

function questRank(q: TaskBrief): number {
  return q.status === "in_progress" ? 0 : q.status === "review" ? 1 : q.status === "todo" || q.status === "planning" ? 2 : 3;
}

const ROW = cn("flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left hover:bg-w-sand", EASE, FOCUS);

export function EmbedPicker({ meId, onPick, onClose }: { meId: string | null; onPick: (drop: EmbedDrop) => void; onClose: () => void }) {
  const [kind, setKind] = useState<EmbedKind>("terminal");
  const [query, setQuery] = useState("");
  const devices = useMyDevices(meId);
  const quests = useLiveQuests(kind === "quest");
  const words = query.trim().toLowerCase();
  const shownQuests = (quests ?? [])
    .filter((q) => !words || q.title.toLowerCase().includes(words) || q.id.startsWith(words))
    .sort((a, b) => questRank(a) - questRank(b))
    .slice(0, 12);
  return (
    <div className="flex max-h-[min(26rem,60vh)] flex-col overflow-hidden rounded-lg border-2 border-w-ink bg-w-paper shadow-[0_3px_0_rgb(var(--w-ink))]">
      <div className="flex items-center gap-1 border-b-2 border-w-line p-2">
        <div role="tablist" aria-label="What to place" className="flex flex-1 items-center gap-1">
          {PLACEABLE_EMBEDS.map((k) => (
            <button
              key={k}
              type="button"
              role="tab"
              aria-selected={kind === k}
              onClick={() => setKind(k)}
              className={cn("h-8 rounded-md px-2.5 text-w-sm font-semibold", EASE, FOCUS, kind === k ? "bg-w-invert text-w-oninvert" : "text-w-text2 hover:bg-w-sand")}
            >
              {EMBEDS[k].label}
            </button>
          ))}
        </div>
        <button type="button" onClick={onClose} aria-label="Close" className="grid h-8 w-8 shrink-0 place-items-center rounded-md text-w-muted hover:bg-w-sand">
          <X size={14} weight="bold" />
        </button>
      </div>
      {kind === "quest" && (
        <div className="border-b-2 border-w-line p-2">
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                onClose();
              }
              if (e.key === "Enter" && shownQuests[0]) {
                e.preventDefault();
                onPick({ embed: { kind: "quest", target: shownQuests[0].id }, caption: shownQuests[0].title });
              }
            }}
            aria-label="Find a quest"
            placeholder="Find a quest by name"
            className="h-9 w-full rounded-md border-2 border-w-ink bg-w-field px-2 text-w-sm outline-none placeholder:text-w-faint focus:ring-2 focus:ring-w-teal"
          />
        </div>
      )}
      <ul className="nowheel min-h-0 flex-1 overflow-y-auto p-1" aria-label={kind === "terminal" ? "Your devices" : "Quests"}>
        {kind === "terminal" &&
          (devices === null ? (
            <li className="px-2 py-3 text-w-sm text-w-muted">
              <Shimmer>Looking for devices</Shimmer>
            </li>
          ) : devices.length === 0 ? (
            <li className="px-2 py-3 text-w-sm text-w-muted">No devices are online. Pair or wake one in Devices.</li>
          ) : (
            devices.map((d) => (
              <li key={d.id}>
                <button type="button" onClick={() => onPick({ embed: { kind: "terminal", target: terminalTarget(d.id) }, caption: `Terminal on ${d.here ? "this Mac" : d.name}` })} className={ROW}>
                  <Monitor size={16} weight="duotone" className="mt-0.5 shrink-0 text-w-text2" />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-w-sm text-w-text">{d.name}</span>
                    <span className="block text-w-xs text-w-muted">{d.here ? "This Mac, a new shell" : "A new shell on this device"}</span>
                  </span>
                </button>
              </li>
            ))
          ))}
        {kind === "quest" &&
          (quests === undefined ? (
            <li className="px-2 py-3 text-w-sm text-w-muted">
              <Shimmer>Loading quests</Shimmer>
            </li>
          ) : shownQuests.length === 0 ? (
            <li className="px-2 py-3 text-w-sm text-w-muted">{words ? "No quest matches." : "No quests yet."}</li>
          ) : (
            shownQuests.map((q) => (
              <li key={q.id}>
                <button type="button" onClick={() => onPick({ embed: { kind: "quest", target: q.id }, caption: q.title })} className={ROW}>
                  <span className="mt-0.5 shrink-0">
                    <StatusIcon status={q.status} size={14} />
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-w-sm text-w-text">{q.title}</span>
                    <span className="block text-w-xs text-w-muted">{QUEST_STAGE_LABEL[questStage(q)]}</span>
                  </span>
                </button>
              </li>
            ))
          ))}
      </ul>
    </div>
  );
}
