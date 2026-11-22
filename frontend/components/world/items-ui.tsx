"use client";

import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Alarm, ArrowLeft, ArrowsOutLineHorizontal, CaretDown, Check, CheckSquare, CheckSquareOffset, Copy, DotsSixVertical, LinkSimple, Notepad, PaintBucket, Plus, Robot, Square, Trash } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { useNow } from "@/lib/clock";
import { checklistMarkdown, useCopy } from "@/lib/copy";
import { matchesQuery, worldApi, type WorldAgent, type WorldItem } from "@/lib/world";
import { BTN_PRIMARY, BTN_SECONDARY, CHUNKY, INPUT, PANEL, TAB, TABS, TAB_COUNT, TAB_OFF, TAB_ON, TAG_X } from "./styles";
import { FilterField } from "./filter-field";
import { TodoBoard } from "./todos-ui";
import { ownsItem, ScopeToggle, useEveryone } from "./scope";
import { EmptyState, WindowHeader } from "./settings-shell";
import { escapeTaken } from "@/lib/escape";
import { WorldLinkButton, worldUrl } from "./message-links";
import { quoteProps } from "./quote";

function left(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}` : `${m}:${String(sec).padStart(2, "0")}`;
}

type Entry = { text: string; done: boolean };

export function isAgentTimer(t: WorldItem): boolean {
  return Boolean(t.created_by_agent && typeof t.data.then === "string" && t.data.then.trim());
}

type TimerGroup = {
  key: string;
  name: string;
  active: boolean;
  ids: string[];
  count: number;
  firesAt: number | null;
  agentId: string | null;
};

function groupTimers(timers: WorldItem[]): TimerGroup[] {
  const groups = new Map<string, TimerGroup>();
  for (const t of timers) {
    const active = t.status === "active" && Boolean(t.fires_at);
    const key = `${active ? "a" : "d"}|${t.created_by_agent ?? ""}|${t.name.trim().toLowerCase()}`;
    const at = active ? new Date(t.fires_at as string).getTime() : null;
    const count = active ? 1 : t.done_count ?? 1;
    const g = groups.get(key);
    if (!g)
      groups.set(key, {
        key,
        name: t.name,
        active,
        ids: [t.id],
        count,
        firesAt: at,
        agentId: t.created_by_agent,
      });
    else {
      g.ids.push(t.id);
      g.count += count;
      if (at !== null && (g.firesAt === null || at < g.firesAt)) g.firesAt = at;
    }
  }
  return [...groups.values()].sort((a, b) => Number(b.active) - Number(a.active) || (a.firesAt ?? 0) - (b.firesAt ?? 0));
}

function ReminderStack({ groups, now, by, canWrite, onArchive }: { groups: TimerGroup[]; now: number; by: (id: string | null) => WorldAgent | undefined; canWrite: boolean; onArchive: (...ids: string[]) => void }) {
  const [open, setOpen] = useState(false);
  const [showDone, setShowDone] = useState(false);
  const active = groups.filter((g) => g.active);
  const finished = groups.filter((g) => !g.active);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest("[data-reminder-stack]")) setOpen(false);
    };
    window.addEventListener("mousedown", away);
    return () => window.removeEventListener("mousedown", away);
  }, [open]);
  if (!groups.length) return null;
  const shown = showDone ? [...active, ...finished] : active;
  return (
    <div data-reminder-stack className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        title="Agents' own reminders. They never ping you."
        className={cn(CHUNKY, "relative flex h-10 items-center gap-1.5 bg-w-card px-3 text-w-sm font-medium hover:bg-w-sand")}
      >
        <span className="relative grid h-4 w-4 place-items-center">
          {active.length > 1 && <span className="absolute left-0.5 top-0.5 h-4 w-4 rounded-md border border-w-ink bg-w-line" />}
          <Robot size={14} weight="bold" className="relative" />
        </span>
        {active.length} agent {active.length === 1 ? "reminder" : "reminders"}
      </button>
      {open && (
        <div className="absolute right-0 top-full z-30 mt-2 max-h-[60vh] w-80 overflow-y-auto rounded-xl border-2 border-w-ink bg-w-paper p-2 shadow-[0_4px_0_rgb(var(--w-ink))]">
          <p className="px-1 pb-2 text-w-xs text-w-muted">Agents set these to check back on things themselves. They never ping you.</p>
          {shown.length === 0 && <EmptyState icon={Alarm} title="Nothing ticking right now" description="When an agent sets a reminder to check back, it waits here." className="border-0 px-1" />}
          <ul className="space-y-1.5">
            {shown.map((g, i) => (
              <li
                key={g.key}
                style={{ zIndex: shown.length - i }}
                className={cn("relative flex items-center gap-2 rounded-lg border-2 border-w-line bg-w-field/80 px-2.5 py-1.5 text-w-sm", !g.active && "opacity-60")}
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate">{g.name}</span>
                  {by(g.agentId) && (
                    <span className="block truncate text-w-2xs text-w-muted">
                      {by(g.agentId)?.name}
                    </span>
                  )}
                </span>
                {g.count > 1 && <span className={TAG_X}>x{g.count}</span>}
                <span className="text-w-sm font-semibold tabular-nums text-w-strong">{g.active && g.firesAt !== null ? left(g.firesAt - now) : "done"}</span>
                {canWrite && (
                  <button type="button" onClick={() => onArchive(...g.ids)} aria-label={`Remove ${g.name}`} className="rounded-md p-1 text-w-muted hover:bg-w-sand hover:text-w-danger">
                    <Trash size={12} weight="bold" />
                  </button>
                )}
              </li>
            ))}
          </ul>
          {finished.length > 0 && (
            <button type="button" onClick={() => setShowDone((v) => !v)} className="mt-2 px-1 text-w-xs text-w-muted hover:text-w-text">
              {showDone ? "Hide finished" : `Show ${finished.length} finished`}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function Fold({ title, count, open, onToggle, children }: { title: string; count: number; open: boolean; onToggle: () => void; children: React.ReactNode }) {
  return (
    <section className="rounded-xl border-2 border-w-line bg-w-field/50 p-2.5">
      <button type="button" onClick={onToggle} aria-expanded={open} className="flex w-full items-center gap-1.5 text-left text-w-sm font-semibold text-w-strong">
        <CaretDown size={12} weight="bold" className={cn("transition-transform", !open && "-rotate-90")} />
        {title}
        <span className="text-w-muted">{count}</span>
      </button>
      {open && <div className="mt-2">{children}</div>}
    </section>
  );
}

const NOTE_COLORS: Record<string, string> = {
  lemon: "bg-w-lemon",
  mint: "bg-w-mint",
  rose: "bg-w-rose",
  aqua: "bg-w-aqua",
  honey: "bg-w-honey/60",
  seafoam: "bg-w-seafoam/50",
  coral: "bg-w-coral/40",
  sand: "bg-w-sand",
};
const NOTE_COLOR_NAMES = Object.keys(NOTE_COLORS);
const NOTE_SIZES = ["small", "medium", "large"] as const;

interface NoteStyle {
  color?: string;
  size?: (typeof NOTE_SIZES)[number];
  order?: number;
}

function noteOf(list: WorldItem): NoteStyle {
  const note = list.data.note;
  return note && typeof note === "object" ? (note as NoteStyle) : {};
}

function StickyNote({
  list,
  index,
  maker,
  meId,
  canWrite,
  dragging,
  onDragStart,
  onDragEnd,
  onDrop,
  onToggle,
  onRestyle,
  onArchive,
}: {
  list: WorldItem;
  index: number;
  maker?: string;
  meId: string | null;
  canWrite: boolean;
  dragging: string | null;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDrop: () => void;
  onToggle: (i: number) => void;
  onRestyle: (change: Partial<NoteStyle>) => void;
  onArchive: () => void;
}) {
  const [over, setOver] = useState(false);
  const [copied, copy] = useCopy();
  const canReference = meId !== null && list.owner_user_id === meId;
  const note = noteOf(list);
  const color = note.color && NOTE_COLORS[note.color] ? note.color : NOTE_COLOR_NAMES[index % 4];
  const size = note.size && NOTE_SIZES.includes(note.size) ? note.size : "medium";
  const entries = (list.data.items as Entry[] | undefined) ?? [];
  const done = entries.filter((e) => e.done).length;
  const next = <T,>(all: readonly T[], current: T) => all[(all.indexOf(current) + 1) % all.length];
  const acceptDrop = (e: React.DragEvent) => {
    if (!dragging || dragging === list.id) return;
    e.preventDefault();
    setOver(true);
  };
  return (
    <div
      {...(canReference ? quoteProps(`list:${list.id}`, `List "${list.name}"`) : {})}
      onDragEnter={acceptDrop}
      onDragOver={acceptDrop}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        onDrop();
      }}
      className={cn(
        "group flex select-text flex-col rounded-md border-2 border-w-ink p-3 shadow-[0_3px_0_rgb(var(--w-ink))]",
        NOTE_COLORS[color],
        size === "large" && "sm:col-span-2",
        index % 3 === 0 ? "-rotate-[0.6deg]" : index % 3 === 1 ? "rotate-[0.5deg]" : "",
        dragging === list.id && "opacity-50",
        over && "ring-4 ring-w-teal ring-offset-2 ring-offset-w-paper",
      )}
    >
      <div className="mb-2 flex items-start gap-1.5">
        {canWrite && (
          <span
            draggable
            onDragStart={(e) => {
              e.dataTransfer.effectAllowed = "move";
              e.dataTransfer.setData("text/plain", list.id);
              onDragStart();
            }}
            onDragEnd={onDragEnd}
            className="mt-0.5 shrink-0 cursor-grab text-w-muted"
            aria-hidden
          >
            <DotsSixVertical size={16} weight="bold" />
          </span>
        )}
        <div className="min-w-0 flex-1">
          <p className="text-w-md font-semibold leading-snug [overflow-wrap:anywhere]">{list.name}</p>
          <p className="text-w-xs text-w-muted">
            {done}/{entries.length} done
            {maker ? ` · made by ${maker}` : ""}
          </p>
        </div>
        <div className="flex shrink-0 items-center opacity-60 group-hover:opacity-100 group-focus-within:opacity-100">
          {canReference && <WorldLinkButton kind="list" id={list.id} label={list.name} className="p-1" />}
          {canWrite && (
            <>
              <button type="button" onClick={() => onRestyle({ color: next(NOTE_COLOR_NAMES, color) })} aria-label={`Change colour of ${list.name}`} title="Change colour" className="rounded-md p-1 text-w-muted hover:bg-w-field/60 hover:text-w-text">
                <PaintBucket size={14} weight="bold" />
              </button>
              <button type="button" onClick={() => onRestyle({ size: next(NOTE_SIZES, size) })} aria-label={`Change size of ${list.name}, now ${size}`} title={`Size: ${size}`} className="rounded-md p-1 text-w-muted hover:bg-w-field/60 hover:text-w-text">
                <ArrowsOutLineHorizontal size={14} weight="bold" />
              </button>
            </>
          )}
          <button
            type="button"
            onClick={() => void copy(checklistMarkdown(list.name, entries))}
            aria-label={copied ? "Copied" : `Copy ${list.name} as a Markdown checklist`}
            title={copied ? "Copied" : "Copy as a checklist"}
            className={cn("rounded-md p-1 hover:bg-w-field/60", copied ? "text-w-ok" : "text-w-muted hover:text-w-text")}
          >
            {copied ? <Check size={14} weight="bold" /> : <Copy size={14} weight="bold" />}
          </button>
          {canWrite && (
            <button type="button" onClick={onArchive} aria-label={`Remove ${list.name}`} className="rounded-md p-1 text-w-muted hover:bg-w-field/60 hover:text-w-danger">
              <Trash size={14} weight="bold" />
            </button>
          )}
        </div>
      </div>
      <ul className={cn("space-y-1", size === "small" && "max-h-56 overflow-y-auto pr-1", size === "large" && "sm:columns-2 sm:gap-4")}>
        {entries.map((e, i) => (
          <li key={i} className="break-inside-avoid">
            {canWrite ? (
              <button type="button" onClick={() => onToggle(i)} className="flex w-full items-start gap-2 rounded-md px-1 py-0.5 text-left text-w-base hover:bg-w-field/50">
                {e.done ? <CheckSquare size={17} weight="fill" className="mt-0.5 shrink-0 text-w-ok" /> : <Square size={17} weight="bold" className="mt-0.5 shrink-0 text-w-muted" />}
                <span className={cn("min-w-0 [overflow-wrap:anywhere]", e.done && "text-w-muted line-through")}>{e.text}</span>
              </button>
            ) : (
              <div className="flex w-full items-start gap-2 px-1 py-0.5 text-w-base">
                {e.done ? <CheckSquare size={17} weight="fill" className="mt-0.5 shrink-0 text-w-ok" /> : <Square size={17} weight="bold" className="mt-0.5 shrink-0 text-w-muted" />}
                <span className={cn("min-w-0 [overflow-wrap:anywhere]", e.done && "text-w-muted line-through")}>{e.text}</span>
              </div>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function NewListForm({ onCancel, onCreated }: { onCancel: () => void; onCreated: (item: WorldItem) => void }) {
  const [title, setTitle] = useState("");
  const [items, setItems] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!title.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      const item = await worldApi.createList(title, items.split("\n"));
      if (live.current) onCreated(item);
    } catch {
      if (!live.current) return;
      setError("Could not make the list.");
      setBusy(false);
    }
  };
  return (
    <form onSubmit={submit} className="mx-auto w-full max-w-xl space-y-3 rounded-xl border-2 border-w-line bg-w-card p-4">
      <div>
        <h2 className="text-w-md font-semibold text-w-strong">New list</h2>
        <p className="text-w-xs text-w-muted">Add one item per line.</p>
      </div>
      <input autoFocus className={cn(INPUT, "w-full")} value={title} onChange={(event) => setTitle(event.target.value)} placeholder="List name" aria-label="List name" />
      <textarea className={cn(INPUT, "min-h-[8rem] w-full")} value={items} onChange={(event) => setItems(event.target.value)} placeholder={"Milk\nBread\nCoffee"} aria-label="List items, one per line" />
      {error && <p role="alert" className="text-w-sm text-w-danger">{error}</p>}
      <div className="flex justify-end gap-2">
        <button type="button" onClick={onCancel} className={BTN_SECONDARY}>Cancel</button>
        <button type="submit" disabled={busy || !title.trim()} className={BTN_PRIMARY}>{busy ? "Making" : "Make list"}</button>
      </div>
    </form>
  );
}

export function ListsPanel({
  items: allItems,
  agents,
  meId,
  canWrite,
  sideToggle,
  focusId,
  onFocusChange,
  onClose,
  onChanged,
}: {
  items: WorldItem[];
  agents: WorldAgent[];
  meId: string | null;
  canWrite: boolean;
  sideToggle?: ReactNode;
  focusId?: string | null;
  onFocusChange?: (id: string | null) => void;
  onClose: () => void;
  onChanged: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !escapeTaken(e) && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const [filter, setFilter] = useState("");
  const [creating, setCreating] = useState(false);
  const [focused, setFocused] = useState<WorldItem | null | undefined>(undefined);
  const focusIdRef = useRef(focusId);
  focusIdRef.current = focusId;
  const [linkCopied, copyLink] = useCopy();
  useEffect(() => {
    if (!focusId) {
      setFocused(undefined);
      return;
    }
    const local = allItems.find((item) => item.id === focusId);
    setFocused(local);
    let live = true;
    void worldApi.item(focusId).then((item) => live && setFocused(item)).catch(() => live && setFocused(null));
    return () => {
      live = false;
    };
  }, [focusId, allItems]);
  const everyone = useEveryone();
  const mineOnly = allItems.filter((i) => ownsItem(i, meId));
  const items = everyone ? allItems : mineOnly;
  const othersCount = allItems.filter((i) => (i.kind === "list" || i.kind === "timer") && !ownsItem(i, meId)).reduce((n, i) => n + (i.done_count ?? 1), 0);
  const [dragging, setDragging] = useState<string | null>(null);
  const lists = items
    .filter((i) => i.kind === "list" && i.status !== "archived")
    .filter((l) => matchesQuery(filter, l.name, ...((l.data.items as Entry[] | undefined) ?? []).map((e) => e.text)))
    .sort((a, b) => (noteOf(a).order ?? 1000) - (noteOf(b).order ?? 1000) || a.created_at.localeCompare(b.created_at));
  const timers = items.filter((i) => i.kind === "timer" && i.status !== "archived" && matchesQuery(filter, i.name));
  const focusedTimerActive = focused?.kind === "timer" && focused.id === focusId && focused.status === "active";
  const now = useNow(1000, focusedTimerActive || timers.some((t) => t.status === "active"));
  const by = (id: string | null) => agents.find((a) => a.id === id);
  const yours = groupTimers(timers.filter((t) => !isAgentTimer(t)));
  const reminders = groupTimers(timers.filter(isAgentTimer));
  const [timersOpen, setTimersOpen] = useState(true);
  const [view, setView] = useState<"todos" | "lists">("todos");
  useEffect(() => {
    const showTodos = () => {
      setView("todos");
      setCreating(false);
    };
    window.addEventListener("world:todo-focus", showTodos);
    return () => window.removeEventListener("world:todo-focus", showTodos);
  }, []);
  const shown = creating || focusId ? "lists" : view;

  const toggle = async (list: WorldItem, index: number) => {
    const entries = ((list.data.items as Entry[] | undefined) ?? []).map((e, i) => (i === index ? { ...e, done: !e.done } : e));
    const updated = await worldApi.updateItem(list.id, { data: { ...list.data, items: entries } }).catch(() => null);
    if (updated) setFocused((current) => (current?.id === updated.id ? updated : current));
    onChanged();
  };
  const restyle = async (list: WorldItem, change: Partial<NoteStyle>) => {
    const updated = await worldApi.updateItem(list.id, { data: { ...list.data, note: { ...noteOf(list), ...change } } }).catch(() => null);
    if (updated) setFocused((current) => (current?.id === updated.id ? updated : current));
    onChanged();
  };
  const moveBefore = async (id: string, target: string) => {
    if (id === target) return;
    const ids = lists.map((l) => l.id).filter((x) => x !== id);
    ids.splice(ids.indexOf(target), 0, id);
    await Promise.all(
      ids.flatMap((x, i) => {
        const list = lists.find((l) => l.id === x);
        return list && noteOf(list).order !== i + 1 ? [worldApi.updateItem(x, { data: { ...list.data, note: { ...noteOf(list), order: i + 1 } } }).catch(() => {})] : [];
      }),
    );
    onChanged();
  };
  const archive = async (...ids: string[]) => {
    await Promise.all(ids.map((id) => worldApi.updateItem(id, { status: "archived", group: true }).catch(() => {})));
    if (focusIdRef.current && ids.includes(focusIdRef.current)) onFocusChange?.(null);
    onChanged();
  };
  const timerList = (groups: TimerGroup[], empty: string, showAgent: boolean) => (
    <ul className="space-y-2">
      {groups.length === 0 && <li className="text-w-base text-w-muted">{empty}</li>}
      {groups.map((g) => (
        <li key={g.key} className="flex items-center gap-2 rounded-lg border-2 border-w-line bg-w-field/60 px-3 py-2 text-w-base">
          <Alarm size={16} weight="bold" className={g.active ? "text-w-alert" : "text-w-muted"} />
          <span className="min-w-0 flex-1">
            <span className="block truncate">{g.name}</span>
            {showAgent && by(g.agentId) && (
              <span className="block truncate text-w-xs text-w-muted">
                {by(g.agentId)?.name}
              </span>
            )}
          </span>
          {g.count > 1 && <span className={TAG_X}>x{g.count}</span>}
          <span className="text-w-base font-semibold tabular-nums text-w-strong">{g.active && g.firesAt !== null ? left(g.firesAt - now) : "done"}</span>
          {canWrite && (
            <button type="button" onClick={() => void archive(...g.ids)} aria-label={`Remove ${g.name}`} className="rounded-md p-1 text-w-muted hover:bg-w-sand hover:text-w-danger">
              <Trash size={14} weight="bold" />
            </button>
          )}
        </li>
      ))}
    </ul>
  );

  return (
    <div role="dialog" aria-modal="true" aria-label="Your lists" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden @container")}>
      <WindowHeader
        title={focused?.name ?? "Lists"}
        subtitle={focusId ? "A checklist from your Lists board." : 'To-dos and timers. Type "stretch 20m" to add one with a timer.'}
        onClose={onClose}
        actions={
          <>
            {focusId && (
              <button type="button" onClick={() => { setView("lists"); onFocusChange?.(null); }} className={BTN_SECONDARY}>
                <ArrowLeft size={14} weight="bold" />
                All lists
              </button>
            )}
            {focusId && focused?.kind === "timer" && (
              <button type="button" onClick={() => void copyLink(worldUrl("list", focused.id))} className={BTN_SECONDARY} aria-label={linkCopied ? "Link copied" : `Copy link to ${focused.name}`}>
                {linkCopied ? <Check size={14} weight="bold" /> : <LinkSimple size={14} weight="bold" />}
                {linkCopied ? "Copied" : "Copy link"}
              </button>
            )}
            {!focusId && canWrite && (
              <button
                type="button"
                onClick={() => {
                  setCreating(true);
                  setView("lists");
                }}
                className={BTN_PRIMARY}
              >
                <Plus size={14} weight="bold" />
                New list
              </button>
            )}
            <ScopeToggle others={othersCount} className="hidden @xl:inline-flex" />
            <FilterField value={filter} onChange={setFilter} placeholder="Filter lists" className="hidden w-56 @xl:flex" />
            <ReminderStack groups={reminders} now={now} by={by} canWrite={canWrite} onArchive={(...ids) => void archive(...ids)} />
            {sideToggle}
          </>
        }
      />
      {!focusId && (
        <div className={TABS} role="tablist">
          {(
            [
              ["todos", "To-dos", CheckSquareOffset, 0],
              ["lists", "Lists", Notepad, lists.length + (yours.length > 0 ? 1 : 0)],
            ] as const
          ).map(([id, label, TabIcon, count]) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={shown === id}
              onClick={() => {
                setCreating(false);
                setView(id);
              }}
              className={cn(
                TAB,
                shown === id ? TAB_ON : TAB_OFF,
              )}
            >
              <TabIcon size={15} weight={shown === id ? "fill" : "regular"} />
              {label}
              {count > 0 && <span className={TAB_COUNT}>{count}</span>}
            </button>
          ))}
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto p-4 @xl:p-5 @4xl:overflow-hidden">
        {!focusId && !creating && <div className="mb-2 flex items-center gap-2 @xl:hidden">
          <FilterField value={filter} onChange={setFilter} placeholder="Filter your lists" className="flex-1" />
          <ScopeToggle others={othersCount} />
        </div>}
        {creating && !focusId ? (
          <NewListForm
            onCancel={() => setCreating(false)}
            onCreated={(item) => {
              setCreating(false);
              setFocused(item);
              onChanged();
              onFocusChange?.(item.id);
            }}
          />
        ) : focusId ? (
          focused === undefined ? (
            <p className="text-w-sm text-w-muted">Opening list...</p>
          ) : focused === null || (focused.kind !== "list" && focused.kind !== "timer") ? (
            <EmptyState icon={Notepad} title="List unavailable" description="It may have been removed or shared with someone else." />
          ) : focused.kind === "list" ? (
            <div className="mx-auto w-full max-w-2xl">
              <StickyNote
                list={focused}
                index={0}
                maker={by(focused.created_by_agent)?.name}
                meId={meId}
                canWrite={canWrite}
                dragging={dragging}
                onDragStart={() => setDragging(focused.id)}
                onDragEnd={() => setDragging(null)}
                onDrop={() => setDragging(null)}
                onToggle={(index) => void toggle(focused, index)}
                onRestyle={(change) => void restyle(focused, change)}
                onArchive={() => void archive(focused.id)}
              />
            </div>
          ) : (
            <div {...quoteProps(`list:${focused.id}`, `Timer "${focused.name}"`)} className="mx-auto flex w-full max-w-xl items-center gap-3 rounded-xl border-2 border-w-line bg-w-card p-4 select-text">
              <Alarm size={20} weight="bold" className={focused.status === "active" ? "text-w-alert" : "text-w-muted"} />
              <div className="min-w-0 flex-1">
                <p className="font-semibold text-w-strong">{focused.name}</p>
                <p className="text-w-sm text-w-muted">{focused.status === "active" && focused.fires_at ? left(new Date(focused.fires_at).getTime() - now) : "Finished"}</p>
              </div>
              {canWrite && (
                <button type="button" onClick={() => void archive(focused.id)} aria-label={`Remove ${focused.name}`} className="rounded-md p-1 text-w-muted hover:bg-w-sand hover:text-w-danger">
                  <Trash size={16} weight="bold" />
                </button>
              )}
            </div>
          )
        ) : shown === "todos" ? (
          <div className="flex min-h-[24rem] min-w-0 flex-1 flex-col @4xl:min-h-0">
            <TodoBoard agents={agents} filter={filter} />
          </div>
        ) : (
          <div className="min-h-0 flex-1 @4xl:overflow-y-auto">
            <div className="grid grid-flow-row-dense grid-cols-[repeat(auto-fill,minmax(15rem,1fr))] items-start gap-4">
              {lists.map((l, n) => (
                <StickyNote
                  key={l.id}
                  list={l}
                  index={n}
                  maker={by(l.created_by_agent)?.name}
                  meId={meId}
                  canWrite={canWrite}
                  dragging={dragging}
                  onDragStart={() => setDragging(l.id)}
                  onDragEnd={() => setDragging(null)}
                  onDrop={() => {
                    if (dragging) void moveBefore(dragging, l.id);
                    setDragging(null);
                  }}
                  onToggle={(i) => void toggle(l, i)}
                  onRestyle={(change) => void restyle(l, change)}
                  onArchive={() => void archive(l.id)}
                />
              ))}
              {yours.length > 0 && (
                <div>
                  <Fold title="Older timers" count={yours.length} open={timersOpen} onToggle={() => setTimersOpen((v) => !v)}>
                    {timerList(yours, "No timers.", false)}
                  </Fold>
                </div>
              )}
              {lists.length === 0 && yours.length === 0 && <p className="text-w-base text-w-muted">No lists match.</p>}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
