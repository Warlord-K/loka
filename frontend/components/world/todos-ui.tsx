"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Alarm, BellRinging, CalendarBlank, CheckSquare, Clock, Copy, LinkSimple, Plus, Quotes, Square, X, Check } from "@phosphor-icons/react";
import { MAIN_TARGET, sendQuote } from "./quote";
import { AnimatePresence, motion } from "framer-motion";
import { MODAL_MOTION, scrimMotion } from "./motion";
import { cn } from "@/lib/utils";
import { useNow } from "@/lib/clock";
import { ProposalPanel, ProposalStrip } from "./todo-actions";
import { matchesQuery, worldApi, type Todo, type WorldAgent } from "@/lib/world";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, CHUNKY, ITEM_CARD, MODAL, PILL_INFO, BTN_DANGER_GHOST, BTN_SECONDARY, PIXEL } from "./styles";
import { BoardColumn } from "./board-kit";
import { MOVE_FAILED, moveFailureNote, placeOpenTodo } from "@/lib/todo-board";
import { latestOnly } from "@/lib/latest";
import { AgentFace } from "./channel-ui";
import { Shimmer } from "./work-ui";
import { CLOSE_BTN } from "./settings-shell";
import { MeaningIcon } from "@/lib/meaning-icons";

const PRIORITY = [
  { label: "Urgent", dot: "bg-w-danger", tone: "text-w-danger", bars: 3 },
  { label: "High", dot: "bg-w-alert", tone: "text-w-alert", bars: 2 },
  { label: "Normal", dot: "bg-w-muted", tone: "text-w-muted", bars: 1 },
  { label: "Low", dot: "bg-w-line2", tone: "text-w-faint", bars: 0 },
];

function PriorityMark({ level }: { level: number }) {
  const p = PRIORITY[Math.min(3, Math.max(0, level))];
  return (
    <svg viewBox="0 0 12 12" width={12} height={12} aria-hidden className={cn("shrink-0", p.tone)}>
      {[0, 1, 2].map((i) => (
        <rect key={i} x={1 + i * 4} y={8 - i * 3} width={2.4} height={3 + i * 3} rx={0.8} fill="currentColor" opacity={i < p.bars ? 1 : 0.25} />
      ))}
    </svg>
  );
}
const UNDO_MS = 4000;
const FAILED_MS = 6000;
type MoveStep = "up" | "down" | "left" | "right";
const MOVE_KEYS: Record<string, MoveStep> = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" };
const SAVE_FAILED = "Could not save that change, so it is back as it was.";
const todoCache: { list: Todo[] | null } = { list: null };
const COLUMN_DOT: Record<string, string> = { todo: "bg-w-muted", progress: "bg-w-teal", done: "bg-w-ok", cancelled: "bg-w-faint" };

export function ageLabel(ms: number): string {
  const m = Math.max(0, Math.floor(ms / 60000));
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

export function tomorrowMorning(now = new Date()): Date {
  const d = new Date(now);
  d.setDate(d.getDate() + 1);
  d.setHours(9, 0, 0, 0);
  return d;
}

function inHour(): string {
  return new Date(Date.now() + 3600_000).toISOString();
}

function shortTime(iso: string): string {
  const d = new Date(iso);
  const sameDay = d.toDateString() === new Date().toDateString();
  return sameDay ? d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }) : d.toLocaleDateString([], { weekday: "short", hour: "numeric", minute: "2-digit" });
}

function ageTone(ms: number): string {
  const days = ms / 86_400_000;
  if (days >= 7) return "text-w-danger";
  if (days >= 3) return "text-w-warn";
  return "text-w-muted";
}

export function isTimerTodo(todo: Todo): boolean {
  return todo.status === "open" && Boolean(todo.nudge_at) && !todo.repeat_nudge;
}

export function countdown(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}` : `${m}:${String(sec).padStart(2, "0")}`;
}

export function parseQuickTimer(input: string): { text: string; minutes: number | null } {
  const match = input.match(/\s+(?:in\s+)?(?:(\d+(?:\.\d+)?)\s*(?:h|hr|hrs|hours?))?\s*(?:(\d+)\s*(?:m|min|mins|minutes?))?\s*$/i);
  if (!match || (!match[1] && !match[2])) return { text: input.trim(), minutes: null };
  const minutes = Math.round(Number(match[1] ?? 0) * 60 + Number(match[2] ?? 0));
  const text = input.slice(0, match.index).trim();
  if (!text || minutes <= 0) return { text: input.trim(), minutes: null };
  return { text, minutes };
}

const TIMER_PRESETS = [5, 15, 30, 60, 180];

type Column = "todo" | "progress" | "done" | "cancelled";

const COLUMNS: { id: Column; label: string }[] = [
  { id: "todo", label: "To do" },
  { id: "progress", label: "In progress" },
  { id: "done", label: "Done" },
  { id: "cancelled", label: "Cancelled" },
];

function columnOf(todo: Todo): Column {
  if (todo.status === "done") return "done";
  if (todo.status === "dismissed") return "cancelled";
  return todo.started_at ? "progress" : "todo";
}

function columnPatch(column: Column): Parameters<typeof worldApi.updateTodo>[1] {
  switch (column) {
    case "todo":
      return { status: "open", started: false };
    case "progress":
      return { status: "open", started: true };
    case "done":
      return { status: "done" };
    case "cancelled":
      return { status: "dismissed" };
  }
}

function columnChange(column: Column, todo: Todo): Partial<Todo> {
  const stamp = new Date().toISOString();
  switch (column) {
    case "todo":
      return { status: "open", started_at: null, done_at: null };
    case "progress":
      return { status: "open", started_at: todo.started_at ?? stamp, done_at: null };
    case "done":
      return { status: "done", done_at: stamp, nudge_at: null };
    case "cancelled":
      return { status: "dismissed", done_at: stamp, nudge_at: null };
  }
}

function TimerChip({ todo, now, onClick }: { todo: Todo; now: number; onClick?: () => void }) {
  const timer = isTimerTodo(todo);
  const firesAt = timer && todo.nudge_at ? new Date(todo.nudge_at).getTime() : null;
  const tick = useNow(1000, firesAt !== null && firesAt - Date.now() < 3_600_000);
  if (firesAt === null) {
    if (!todo.nudge_at || todo.status !== "open") return null;
    return (
      <span className="flex items-center gap-1" title="Next reminder">
        <BellRinging size={11} weight="bold" />
        {shortTime(todo.nudge_at)}
      </span>
    );
  }
  const left = firesAt - Math.max(tick, now);
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick?.();
      }}
      title={`Pings you at ${shortTime(todo.nudge_at as string)}`}
      className={cn("flex items-center gap-1 rounded px-1 py-0.5 font-semibold tabular-nums", left <= 0 ? "bg-w-rose text-w-danger" : "bg-w-sand text-w-danger")}
    >
      <Alarm size={11} weight="bold" />
      {left <= 0 ? "time's up" : left < 3_600_000 ? countdown(left) : shortTime(todo.nudge_at as string)}
    </button>
  );
}

function TodoCard({
  todo,
  pending,
  agent,
  now,
  dragging,
  over,
  onOpen,
  onDragStart,
  onDragEnd,
  onDragOverCard,
  onDropCard,
  onMoveKey,
  onStrike,
  onPriority,
  agents,
  onDecided,
}: {
  agents: WorldAgent[];
  onDecided: () => void;
  todo: Todo;
  pending: boolean;
  agent: WorldAgent | undefined;
  now: number;
  dragging: boolean;
  over: boolean;
  onOpen: () => void;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDragOverCard: () => void;
  onDropCard: () => void;
  onMoveKey: (step: MoveStep) => void;
  onStrike: () => void;
  onPriority: () => void;
}) {
  const age = now - new Date(todo.added_at).getTime();
  const overdue = todo.status === "open" && todo.due_at ? new Date(todo.due_at).getTime() <= now : false;
  const p = PRIORITY[Math.min(3, Math.max(0, todo.priority))];
  const closed = todo.status !== "open";
  const acceptDrop = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    onDragOverCard();
  };
  return (
    <li
      draggable
      tabIndex={0}
      data-todo-id={todo.id}
      aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown Alt+ArrowLeft Alt+ArrowRight"
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        const step = e.altKey && !e.metaKey && !e.ctrlKey ? MOVE_KEYS[e.key] : undefined;
        if (step) {
          e.preventDefault();
          onMoveKey(step);
        } else if (e.key === "Enter") onOpen();
      }}
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", todo.id);
        onDragStart();
      }}
      onDragEnd={onDragEnd}
      onDragEnter={acceptDrop}
      onDragOver={acceptDrop}
      onDrop={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onDropCard();
      }}
      className={cn(
        ITEM_CARD,
        "group relative flex cursor-pointer items-start gap-2.5 px-3 py-2.5 text-w-sm outline-none focus-visible:ring-2 focus-visible:ring-w-teal",
        dragging && "opacity-40",
        (pending || closed) && "opacity-65",
        over && "before:absolute before:-top-1.5 before:inset-x-1 before:h-0.5 before:rounded-md before:bg-w-teal",
      )}
    >
      {closed ? (
        <CheckSquare size={17} weight="fill" className={cn("mt-px shrink-0", todo.status === "done" ? "text-w-ok" : "text-w-faint")} />
      ) : (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onStrike();
          }}
          aria-label={pending ? `Undo ${todo.text}` : `Done: ${todo.text}`}
          className="mt-px shrink-0 rounded transition-colors"
        >
          {pending ? <CheckSquare size={17} weight="fill" className="text-w-ok" /> : <Square size={17} weight="regular" className="text-w-muted hover:text-w-ok" />}
        </button>
      )}
      <span className="min-w-0 flex-1">
        <span className={cn("line-clamp-3 break-words font-medium leading-snug text-w-strong", (pending || closed) && "font-normal text-w-muted line-through")}>{todo.text}</span>
        <span className="mt-2 flex flex-wrap items-center gap-x-2.5 gap-y-1 text-w-2xs text-w-muted">
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onPriority();
            }}
            title={`${p.label} priority, click to change`}
            aria-label={`${p.label} priority, change it`}
            className={cn("-mx-1 flex items-center gap-1 rounded px-1 py-0.5 font-medium transition-colors hover:bg-w-sand", p.tone)}
          >
            <PriorityMark level={todo.priority} />
            {todo.priority < 2 && p.label}
          </button>
          <span className={cn("flex items-center gap-1 tabular-nums", ageTone(age))} title={`Added ${new Date(todo.added_at).toLocaleString()}`}>
            <Clock size={11} weight="bold" />
            {ageLabel(age)}
          </span>
          {todo.due_at && todo.status === "open" && (
            <span className={cn("flex items-center gap-1", overdue && "font-semibold text-w-danger")}>
              <CalendarBlank size={11} weight="bold" />
              {overdue ? "overdue" : shortTime(todo.due_at)}
            </span>
          )}
          <TimerChip todo={todo} now={now} onClick={onOpen} />
          {todo.source?.url && <LinkSimple size={11} weight="bold" aria-label="Has a link" />}
          <span className="ml-auto flex min-w-0 items-center gap-1.5">
            {todo.source?.suggested && (
              <span className={cn(PILL_INFO, "py-[3px]")} title="Suggested. Nothing starts until you approve.">
                <MeaningIcon.suggestion size={10} weight="fill" />
                Suggested
              </span>
            )}
            {agent ? (
              <span className="flex min-w-0 items-center gap-1" title={`Added by ${agent.name}`}>
                <AgentFace agent={agent} size={16} />
                <span className="truncate">{agent.name}</span>
              </span>
            ) : (
              <span className="truncate">You</span>
            )}
          </span>
        </span>
        <ProposalStrip todo={todo} agents={agents} onDecided={onDecided} onOpen={onOpen} />
      </span>
    </li>
  );
}

function ColumnAdd({ column, onAdd }: { column: Column; onAdd: (text: string, column: Column) => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState("");
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (open) input.current?.focus();
  }, [open]);
  if (!open)
    return (
      <button type="button" onClick={() => setOpen(true)} className="flex w-full items-center gap-1.5 rounded-lg px-2.5 py-2 text-w-sm text-w-muted transition-colors hover:bg-w-paper/70 hover:text-w-text">
        <Plus size={13} weight="bold" />
        Add a to-do
      </button>
    );
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        const text = draft.trim();
        if (!text) return;
        setDraft("");
        void onAdd(text, column).catch(() => setDraft(text));
      }}
    >
      <input
        ref={input}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== "Escape") return;
          e.stopPropagation();
          setOpen(false);
        }}
        onBlur={() => !draft.trim() && setOpen(false)}
        placeholder="New to-do, add 10m for a timer"
        aria-label="New to-do"
        className="w-full rounded-lg border-2 border-w-line2 bg-w-field px-3 py-2 text-w-sm outline-none placeholder:text-w-faint focus:border-w-teal focus:ring-2 focus:ring-w-teal/30"
      />
    </form>
  );
}

function localInput(iso: string | null): string {
  const d = iso ? new Date(iso) : new Date(Date.now() + 3_600_000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function TodoDetail({
  active,
  todo,
  agent,
  now,
  onClose,
  onMove,
  onPatch,
  agents,
  onDecided,
}: {
  active: boolean;
  agents: WorldAgent[];
  onDecided: () => void;
  todo: Todo;
  agent: WorldAgent | undefined;
  now: number;
  onClose: () => void;
  onMove: (column: Column) => void;
  onPatch: (change: Partial<Todo>, send: Parameters<typeof worldApi.updateTodo>[1]) => void;
}) {
  const [text, setText] = useState(todo.text);
  const [custom, setCustom] = useState("");
  const [at, setAt] = useState(() => localInput(todo.nudge_at));
  const [copied, setCopied] = useState(false);
  const [timerError, setTimerError] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => setText(todo.text), [todo.id, todo.text]);
  const column = columnOf(todo);
  const dirty = text.trim() !== todo.text && text.trim().length > 0;
  const save = () => dirty && onPatch({ text: text.trim() }, { text: text.trim() });
  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!active || e.key !== "Escape") return;
      e.stopPropagation();
      saveRef.current();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [active, onClose]);
  useEffect(() => box.current?.focus(), []);
  const setTimer = (when: Date) => {
    if (when.getTime() <= Date.now()) {
      setTimerError("Pick a time in the future.");
      return;
    }
    setTimerError(null);
    const iso = when.toISOString();
    onPatch({ nudge_at: iso, repeat_nudge: false }, { nudge_at: iso, repeat_nudge: false });
  };
  const fromMinutes = (m: number) => setTimer(new Date(Date.now() + m * 60_000));
  const timer = isTimerTodo(todo);
  const age = now - new Date(todo.added_at).getTime();
  const seg = "flex-1 rounded-md px-2 py-1 text-w-xs font-semibold transition-colors";
  return (
    <motion.div {...scrimMotion(1)} className="absolute inset-0 z-40 grid place-items-center bg-w-scrim/40 p-3" onMouseDown={(e) => {
        if (e.target !== e.currentTarget) return;
        save();
        onClose();
      }}>
      <motion.div
        {...MODAL_MOTION}
        ref={box}
        tabIndex={-1}
        role="dialog"
        aria-label="To-do"
        className={cn(MODAL, "flex max-h-full max-w-lg flex-col gap-3 p-4 outline-none")}
      >
        <div className="flex items-center justify-between gap-2">
          <p className={cn(PIXEL, "text-w-xl leading-none text-w-strong")}>To-do</p>
          <div className="flex items-center gap-1.5">
            <button
              type="button"
              onClick={() => {
                void navigator.clipboard.writeText(text).then(() => {
                  setCopied(true);
                  window.setTimeout(() => setCopied(false), 1500);
                });
              }}
              className={BTN_SECONDARY}
            >
              {copied ? <Check size={15} weight="bold" className="text-w-ok" /> : <Copy size={15} weight="bold" />}
              {copied ? "Copied" : "Copy"}
            </button>
            <button
              type="button"
              title="Quote this to-do in a message to your main agent"
              onClick={() => {
                save();
                onClose();
                sendQuote(MAIN_TARGET, { text: (text.trim() || todo.text).slice(0, 2000), label: "To-do", source: `todo:${todo.id}` });
              }}
              className={BTN_SECONDARY}
            >
              <Quotes size={15} weight="bold" />
              Ask about it
            </button>
            <button
              type="button"
              onClick={() => {
                save();
                onClose();
              }}
              aria-label="Close"
              className={CLOSE_BTN}
            >
              <X size={16} weight="bold" />
            </button>
          </div>
        </div>
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          onBlur={save}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              save();
            }
          }}
          aria-label="To-do text"
          rows={Math.min(10, Math.max(3, Math.ceil(text.length / 60)))}
          className="w-full shrink-0 resize-y rounded-lg border-2 border-w-line bg-w-field px-2.5 py-2 text-w-base leading-relaxed outline-none focus:border-w-ink"
        />
        <ProposalPanel todo={todo} agents={agents} onDecided={onDecided} />
        <div>
          <p className="mb-1 text-w-xs font-semibold text-w-muted">Status</p>
          <div className="flex gap-1 rounded-lg border-2 border-w-line bg-w-field p-1">
            {COLUMNS.map((c) => (
              <button key={c.id} type="button" aria-pressed={column === c.id} onClick={() => onMove(c.id)} className={cn(seg, column === c.id ? "bg-w-invert text-w-oninvert" : "text-w-text2 hover:bg-w-sand")}>
                {c.label}
              </button>
            ))}
          </div>
        </div>
        <div>
          <p className="mb-1 text-w-xs font-semibold text-w-muted">Priority</p>
          <div className="flex gap-1 rounded-lg border-2 border-w-line bg-w-field p-1">
            {PRIORITY.map((p, i) => (
              <button
                key={p.label}
                type="button"
                aria-pressed={todo.priority === i}
                onClick={() => onPatch({ priority: i }, { priority: i })}
                className={cn(seg, "flex items-center justify-center gap-1.5", todo.priority === i ? "bg-w-invert text-w-oninvert" : "text-w-text2 hover:bg-w-sand")}
              >
                <span className={cn("h-2 w-2 rounded-full", p.dot)} />
                {p.label}
              </button>
            ))}
          </div>
        </div>
        {todo.status === "open" && (
          <div>
            <div className="mb-1 flex items-baseline justify-between gap-2">
              <p className="text-w-xs font-semibold text-w-muted">Timer</p>
              <span className="text-w-xs text-w-text2">
                {todo.nudge_at ? (
                  <span className="flex items-center gap-1">
                    {timer ? "Pings you once" : "Next reminder"} <TimerChip todo={todo} now={now} />
                  </span>
                ) : (
                  "No timer"
                )}
              </span>
            </div>
            <div className="flex flex-wrap gap-1">
              {TIMER_PRESETS.map((m) => (
                <button key={m} type="button" onClick={() => fromMinutes(m)} className={BTN_SECONDARY_SM}>
                  {m < 60 ? `${m}m` : `${m / 60}h`}
                </button>
              ))}
              <button type="button" onClick={() => setTimer(tomorrowMorning())} className={BTN_SECONDARY_SM}>
                Tomorrow 9am
              </button>
              {todo.nudge_at && (
                <button
                  type="button"
                  onClick={() => onPatch({ nudge_at: null, repeat_nudge: true }, { nudge_at: null, repeat_nudge: true })}
                  className={cn(BTN_DANGER_GHOST, "h-8 text-w-xs")}
                >
                  Clear
                </button>
              )}
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <form
                className="flex items-center gap-1.5"
                onSubmit={(e) => {
                  e.preventDefault();
                  const parsed = parseQuickTimer(`x ${custom.trim()}`);
                  const minutes = parsed.minutes ?? (Number(custom) > 0 ? Math.round(Number(custom)) : null);
                  if (!minutes) {
                    setTimerError("Try 45m, 2h or 1h30m.");
                    return;
                  }
                  setCustom("");
                  fromMinutes(minutes);
                }}
              >
                <input
                  value={custom}
                  onChange={(e) => setCustom(e.target.value)}
                  placeholder="In 45m, 2h, 1h30m"
                  aria-label="Custom timer length"
                  className="h-8 w-40 rounded-lg border-2 border-w-line bg-w-field px-2.5 text-w-sm outline-none transition-colors focus:border-w-ink"
                />
                <button type="submit" className={BTN_SECONDARY_SM}>
                  Set
                </button>
              </form>
              <form
                className="flex items-center gap-1.5"
                onSubmit={(e) => {
                  e.preventDefault();
                  const when = new Date(at);
                  if (Number.isNaN(when.getTime())) {
                    setTimerError("Pick a date and time.");
                    return;
                  }
                  setTimer(when);
                }}
              >
                <input
                  type="datetime-local"
                  value={at}
                  onChange={(e) => setAt(e.target.value)}
                  aria-label="Timer date and time"
                  className="h-8 rounded-lg border-2 border-w-line bg-w-field px-2.5 text-w-sm outline-none transition-colors focus:border-w-ink"
                />
                <button type="submit" className={BTN_SECONDARY_SM}>
                  Set
                </button>
              </form>
            </div>
            {timerError && <p className="mt-1 text-w-xs text-w-danger">{timerError}</p>}
          </div>
        )}
        <div className="flex flex-wrap gap-x-3 gap-y-1 border-t-2 border-dashed border-w-line pt-2 text-w-xs text-w-muted">
          <span>
            {todo.source?.suggested ? "Suggested" : "Added"} {ageLabel(age)} ago by {agent ? agent.name : "you"}
          </span>
          {todo.started_at && <span>Started {shortTime(todo.started_at)}</span>}
          {todo.due_at && <span>Due {shortTime(todo.due_at)}</span>}
          {todo.nudge_count > 0 && <span>Pinged {todo.nudge_count}x</span>}
          {todo.source?.url && (
            <a href={todo.source.url} target="_blank" rel="noopener noreferrer" className="flex items-center gap-0.5 text-w-link underline-offset-2 hover:underline">
              <LinkSimple size={11} weight="bold" />
              {todo.source.label || "Source"}
            </a>
          )}
        </div>
      </motion.div>
    </motion.div>
  );
}

function sortOpen(list: Todo[]): Todo[] {
  return [...list].sort((a, b) => a.priority - b.priority || (a.position ?? Number.MAX_SAFE_INTEGER) - (b.position ?? Number.MAX_SAFE_INTEGER) || a.added_at.localeCompare(b.added_at));
}

export function focusTodo(id: string) {
  window.dispatchEvent(new CustomEvent("world:todo-focus", { detail: { id } }));
}

export function TodoBoard({ active = true, agents, filter = "", focus, onNavigate }: { active?: boolean; agents: WorldAgent[]; filter?: string; focus?: { todo: Todo; nonce: number } | null; onNavigate?: (todo: Todo) => void }) {
  const [todos, setTodos] = useState<Todo[]>(() => todoCache.list ?? []);
  const [loaded, setLoaded] = useState(() => todoCache.list !== null);
  useEffect(() => {
    if (loaded) todoCache.list = todos;
  }, [todos, loaded]);
  const [pending, setPending] = useState<Record<string, number>>({});
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<{ column: Column; before: string | null } | null>(null);
  const [opened, setOpened] = useState<string | null>(null);
  const [outsideBoard, setOutsideBoard] = useState<Todo | null>(null);
  const todoSelection = useRef(latestOnly()).current;
  const [failed, setFailed] = useState<string | null>(null);
  const [refocus, setRefocus] = useState<string | null>(null);
  const boardRef = useRef<HTMLElement>(null);
  const [now, setNow] = useState(() => Date.now());
  const pendingRef = useRef(pending);
  pendingRef.current = pending;

  const load = useCallback(() => {
    worldApi
      .todos()
      .then((t) => {
        setTodos(t);
        setLoaded(true);
      })
      .catch(() => setLoaded(true));
  }, []);

  const focusedNonce = useRef(0);
  useEffect(() => {
    if (!focus || focus.nonce === focusedNonce.current) return;
    focusedNonce.current = focus.nonce;
    todoSelection.begin();
    const local = todos.find((todo) => todo.id === focus.todo.id);
    setOutsideBoard(local ? null : focus.todo);
    setOpened(focus.todo.id);
  }, [focus, todos, todoSelection]);
  const openCard = (id: string | null) => {
    const todo = id ? todos.find((item) => item.id === id) ?? (outsideBoard?.id === id ? outsideBoard : null) : null;
    if (todo && onNavigate) {
      onNavigate(todo);
      return;
    }
    todoSelection.begin();
    setOutsideBoard(null);
    setOpened(id);
  };

  useEffect(() => {
    load();
    window.addEventListener("world:todos-stale", load);
    const tick = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => {
      window.removeEventListener("world:todos-stale", load);
      window.clearInterval(tick);
    };
  }, [load]);

  useEffect(() => {
    if (!failed) return;
    const timer = window.setTimeout(() => setFailed(null), FAILED_MS);
    return () => window.clearTimeout(timer);
  }, [failed]);

  const commit = useCallback(
    (id: string) => {
      window.clearTimeout(pendingRef.current[id]);
      setPending(({ [id]: _, ...rest }) => rest);
      void worldApi.updateTodo(id, { status: "done" }).then(load, load);
    },
    [load],
  );

  useEffect(
    () => () => {
      for (const [id, timer] of Object.entries(pendingRef.current)) {
        window.clearTimeout(timer);
        void worldApi.updateTodo(id, { status: "done" }).catch(() => {});
      }
    },
    [],
  );

  const strike = (id: string) => {
    if (pending[id]) {
      window.clearTimeout(pending[id]);
      setPending(({ [id]: _, ...rest }) => rest);
      return;
    }
    const timer = window.setTimeout(() => commit(id), UNDO_MS);
    setPending((p) => ({ ...p, [id]: timer }));
  };

  const settle = (work: Promise<unknown>, message: string) => {
    void work.then(load, () => {
      setFailed(message);
      load();
    });
  };

  const patch = (id: string, change: Partial<Todo>, send: Parameters<typeof worldApi.updateTodo>[1], message = SAVE_FAILED) => {
    setTodos((prev) => prev.map((t) => (t.id === id ? { ...t, ...change } : t)));
    settle(worldApi.updateTodo(id, send).then((saved) => {
      setOutsideBoard((current) => current?.id === saved.id ? saved : current);
    }), message);
  };

  const byColumn = useMemo(() => {
    const groups: Record<Column, Todo[]> = { todo: [], progress: [], done: [], cancelled: [] };
    for (const t of todos) groups[columnOf(t)].push(t);
    groups.todo = sortOpen(groups.todo);
    groups.progress = sortOpen(groups.progress);
    const recent = (a: Todo, b: Todo) => (b.done_at ?? "").localeCompare(a.done_at ?? "");
    groups.done.sort(recent);
    groups.cancelled.sort(recent);
    return groups;
  }, [todos]);
  const by = (id: string | null) => (id ? agents.find((a) => a.id === id) : undefined);

  const move = (id: string, column: Column, before: string | null) => {
    const moved = todos.find((t) => t.id === id);
    if (!moved) {
      if (outsideBoard?.id === id && columnOf(outsideBoard) !== column) patch(id, columnChange(column, outsideBoard), columnPatch(column), MOVE_FAILED);
      return;
    }
    const from = columnOf(moved);
    if (pending[id]) {
      window.clearTimeout(pending[id]);
      setPending(({ [id]: _, ...rest }) => rest);
    }
    if (column === "done" || column === "cancelled") {
      if (from !== column) patch(id, columnChange(column, moved), columnPatch(column), MOVE_FAILED);
      return;
    }
    if (before === id) return;
    const placed = { ...moved, ...columnChange(column, moved) };
    const { priority, order } = placeOpenTodo({ todo: byColumn.todo, progress: byColumn.progress }, moved, column, before);
    const positioned = new Map(order.map((todoId, i) => [todoId, i + 1]));
    setTodos((prev) =>
      prev.map((t) => {
        const next = t.id === id ? { ...placed, priority } : t;
        const position = positioned.get(t.id);
        return position ? { ...next, position } : next;
      }),
    );
    const saved = { column: false, priority: false };
    const label = COLUMNS.find((c) => c.id === column)?.label ?? column;
    const save = async () => {
      if (from !== column) {
        await worldApi.updateTodo(id, columnPatch(column));
        saved.column = true;
      }
      if (priority !== moved.priority) {
        await worldApi.updateTodo(id, { priority });
        saved.priority = true;
      }
      await worldApi.reorderTodos(order);
    };
    void save().then(load, () => {
      setFailed(moveFailureNote(saved, label));
      load();
    });
  };

  const add = async (input: string, column: Column) => {
    const { text, minutes } = parseQuickTimer(input);
    const made = await worldApi.addTodo(minutes ? { text, nudge_at: new Date(Date.now() + minutes * 60_000).toISOString(), repeat_nudge: false } : { text });
    if (column !== "todo") await worldApi.updateTodo(made.id, columnPatch(column));
    load();
  };

  const drop = (column: Column, before: string | null) => {
    const id = dragging;
    setDragging(null);
    setOver(null);
    if (id) move(id, column, before);
  };

  const moveByKey = (id: string, column: Column, items: Todo[], step: MoveStep) => {
    const closed = column === "done" || column === "cancelled";
    if (step === "left" || step === "right") {
      const next = COLUMNS[COLUMNS.findIndex((c) => c.id === column) + (step === "left" ? -1 : 1)];
      if (!next) return;
      setRefocus(id);
      move(id, next.id, null);
      return;
    }
    const at = items.findIndex((t) => t.id === id);
    if (closed || at < 0) return;
    const before = step === "up" ? (at > 0 ? items[at - 1].id : undefined) : at < items.length - 1 ? (items[at + 2]?.id ?? null) : undefined;
    if (before === undefined) return;
    setRefocus(id);
    move(id, column, before);
  };

  useEffect(() => {
    if (!refocus) return;
    boardRef.current?.querySelector<HTMLElement>(`[data-todo-id="${refocus}"]`)?.focus();
    setRefocus(null);
  }, [refocus, todos]);

  const struck = Object.keys(pending).length;
  const openCount = byColumn.todo.length + byColumn.progress.length - struck;
  const detail = opened ? todos.find((t) => t.id === opened) ?? (outsideBoard?.id === opened ? outsideBoard : undefined) : undefined;
  const dragProps = (column: Column) => {
    const accept = (e: React.DragEvent) => {
      if (!dragging) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      if (over?.column !== column || over.before !== null) setOver({ column, before: null });
    };
    return {
      onDragEnter: accept,
      onDragOver: accept,
      onDragLeave: (e: React.DragEvent) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node)) setOver((o) => (o?.column === column ? null : o));
      },
      onDrop: (e: React.DragEvent) => {
        e.preventDefault();
        drop(column, null);
      },
    };
  };

  return (
    <section ref={boardRef} className="relative flex min-h-0 flex-1 flex-col">
      <div className="flex min-h-0 flex-1 snap-x snap-mandatory gap-3 overflow-x-auto pb-1 sm:snap-none">
        {COLUMNS.map((col) => {
          const items = filter ? byColumn[col.id].filter((t) => matchesQuery(filter, t.text, t.source?.label, by(t.added_by_agent)?.name)) : byColumn[col.id];
          const closed = col.id === "done" || col.id === "cancelled";
          return (
            <BoardColumn
              key={col.id}
              {...dragProps(col.id)}
              label={col.label}
              count={items.length}
              icon={<span className={cn("h-2 w-2 shrink-0 rounded-full", COLUMN_DOT[col.id])} />}
              over={over?.column === col.id}
              compact={col.id === "cancelled" && items.length === 0 && over?.column !== col.id}
              empty={closed ? (col.id === "done" ? "Tick things off or drop them here" : "Drop things you will not do here") : null}
              className="min-h-[14rem]"
            >
                {items.map((t) => (
                  <TodoCard
                    key={t.id}
                    todo={t}
                    pending={Boolean(pending[t.id])}
                    agent={by(t.added_by_agent)}
                    now={now}
                    dragging={dragging === t.id}
                    over={over?.before === t.id}
                    onOpen={() => openCard(t.id)}
                    onDragStart={() => setDragging(t.id)}
                    onDragEnd={() => {
                      setDragging(null);
                      setOver(null);
                    }}
                    onDragOverCard={() => {
                      if (dragging && over?.before !== t.id) setOver({ column: col.id, before: closed ? null : t.id });
                    }}
                    onDropCard={() => drop(col.id, closed ? null : t.id)}
                    onMoveKey={(step) => moveByKey(t.id, col.id, items, step)}
                    onStrike={() => strike(t.id)}
                    agents={agents}
                    onDecided={load}
                    onPriority={() => {
                      const priority = (t.priority + 3) % 4;
                      patch(t.id, { priority }, { priority });
                    }}
                  />
                ))}
                {!closed && (
                  <li>
                    <ColumnAdd column={col.id} onAdd={add} />
                  </li>
                )}
            </BoardColumn>
          );
        })}
      </div>
      <p className="mt-2 hidden text-w-2xs text-w-muted sm:block">{loaded ? `${openCount} open` : <Shimmer>Loading</Shimmer>} · Click a card to read or edit it, drag it or press Alt (Option) with an arrow key to move it</p>
      {failed && (
        <p role="alert" className="mt-2 rounded-lg bg-w-rose px-3 py-1.5 text-w-sm text-w-danger animate-[fade-in-up_180ms_ease-out_both] motion-reduce:animate-none">
          {failed}
        </p>
      )}
      {struck > 0 && (
        <div role="status" className="mt-2 flex items-center justify-between gap-2 rounded-lg bg-w-invert px-3 py-1.5 text-w-sm text-w-oninvert animate-[fade-in-up_180ms_ease-out_both] motion-reduce:animate-none">
          <span>{struck === 1 ? "Struck off" : `${struck} struck off`}</span>
          <button
            type="button"
            onClick={() => {
              Object.values(pending).forEach((t) => window.clearTimeout(t));
              setPending({});
            }}
            className="font-semibold text-w-honey hover:underline"
          >
            Undo
          </button>
        </div>
      )}
      <AnimatePresence>
      {detail && (
        <TodoDetail
          key={detail.id}
          active={active}
          todo={detail}
          agent={by(detail.added_by_agent)}
          now={now}
          onClose={() => openCard(null)}
          onMove={(column) => move(detail.id, column, null)}
          onPatch={(change, send) => patch(detail.id, change, send)}
          agents={agents}
          onDecided={load}
        />
      )}
      </AnimatePresence>
    </section>
  );
}

export function TodoNudgeActions({ todoId, onOpen }: { todoId: string; onOpen: () => void }) {
  const [state, setState] = useState<string | null>(null);
  const act = (label: string, patch: Parameters<typeof worldApi.updateTodo>[1]) => {
    setState(label);
    void worldApi
      .updateTodo(todoId, patch)
      .then(() => window.dispatchEvent(new Event("world:todos-stale")))
      .catch(() => setState("Could not save"));
  };
  if (state) return <span className="shrink-0 text-w-xs text-w-muted">{state}</span>;
  const btn = cn(BTN_SECONDARY_SM, "shrink-0");
  return (
    <span className="flex shrink-0 flex-wrap gap-1.5">
      <button type="button" onClick={() => act("Done", { status: "done" })} className={cn(BTN_PRIMARY_SM, "shrink-0")}>
        Done
      </button>
      <button type="button" onClick={() => act("Snoozed 1h", { nudge_at: inHour() })} className={btn}>
        1h
      </button>
      <button type="button" onClick={() => act("Snoozed to tomorrow", { nudge_at: tomorrowMorning().toISOString() })} className={btn}>
        Tmrw
      </button>
      <button type="button" onClick={onOpen} className={btn}>
        Open
      </button>
    </span>
  );
}
