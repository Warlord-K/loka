"use client";

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Command } from "cmdk";
import { MagnifyingGlass, User, UserCircleDashed, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type Person, type QuestPatch, type QuestTiming, type TaskBrief, type WorldAgent } from "@/lib/world";
import type { Role } from "./hud";
import { BTN, BTN_SECONDARY_SM } from "./styles";
import { Keycap } from "./keycap";
import { AgentFace, PersonAvatar } from "./channel-ui";

export interface QuestViewer {
  id: string | null;
  role: Role;
}

export const QUEST_STATUSES: { id: string; label: string; match: string[] }[] = [
  { id: "todo", label: "To do", match: ["todo", "planning", "plan_review"] },
  { id: "in_progress", label: "On a quest", match: ["in_progress"] },
  { id: "review", label: "Needs you", match: ["review"] },
  { id: "done", label: "Done", match: ["done"] },
  { id: "failed", label: "Stuck", match: ["failed"] },
  { id: "cancelled", label: "Cancelled", match: ["cancelled"] },
];

export function statusGroup(status: string): string {
  return QUEST_STATUSES.find((s) => s.match.includes(status))?.id ?? "todo";
}

export function statusLabel(status: string): string {
  return QUEST_STATUSES.find((s) => s.id === statusGroup(status))?.label ?? status;
}

export function paceLabel(pace: string | null | undefined): string {
  return pace === "quick" ? "Quick" : pace === "urgent" ? "Urgent" : "Normal";
}

function secs(value: unknown): string {
  const n = typeof value === "number" ? Math.max(0, Math.round(value)) : 0;
  if (n < 60) return `${n}s`;
  if (n < 3600) return `${Math.floor(n / 60)}m${n % 60 ? ` ${n % 60}s` : ""}`;
  return `${Math.floor(n / 3600)}h ${Math.round((n % 3600) / 60)}m`;
}

export function formatTiming(timing: QuestTiming | null | undefined): string {
  if (!timing || typeof timing !== "object") return "";
  const parts = [
    ["waited", timing.queued_secs],
    ["worked", timing.work_secs],
    ["review", timing.review_secs],
    ["publish", timing.publish_secs],
  ]
    .filter(([, v]) => typeof v === "number" && v > 0)
    .map(([label, v]) => `${label} ${secs(v)}`);
  if (typeof timing.total_secs !== "number" || timing.total_secs <= 0) return parts.join(", ");
  return `${secs(timing.total_secs)} in all${parts.length ? ` (${parts.join(", ")})` : ""}`;
}

export function StatusIcon({ status, size = 14 }: { status: string; size?: number }) {
  const group = statusGroup(status);
  const ring = { todo: "#8a705a", in_progress: "#0fa3a3", review: "#d99a1e", done: "#0e8f63", failed: "#e0663d", cancelled: "#a08870" }[group] ?? "#8a705a";
  return (
    <svg viewBox="0 0 14 14" width={size} height={size} aria-hidden className="shrink-0">
      {group === "todo" && <circle cx="7" cy="7" r="5.5" style={{ fill: "rgb(var(--w-card))" }} stroke={ring} strokeWidth="1.8" strokeDasharray={status === "todo" ? undefined : "2 1.6"} />}
      {(group === "in_progress" || group === "review") && (
        <>
          <circle cx="7" cy="7" r="5.5" style={{ fill: "rgb(var(--w-card))" }} stroke={ring} strokeWidth="1.8" />
          <path d={group === "in_progress" ? "M7 3.2 A3.8 3.8 0 0 1 7 10.8 Z" : "M7 3.2 A3.8 3.8 0 1 1 3.2 7 L7 7 Z"} fill={ring} />
        </>
      )}
      {(group === "done" || group === "failed" || group === "cancelled") && <circle cx="7" cy="7" r="6.2" fill={ring} />}
      {group === "done" && <path d="M4.2 7.2 L6.2 9.1 L9.9 5.1" fill="none" stroke="#fff8ea" strokeWidth="1.8" strokeLinecap="square" />}
      {group === "failed" && <path d="M7 3.6 V7.8 M7 9.4 V10.4" stroke="#fff8ea" strokeWidth="1.8" />}
      {group === "cancelled" && <path d="M4.8 4.8 L9.2 9.2 M9.2 4.8 L4.8 9.2" stroke="#fff8ea" strokeWidth="1.7" />}
    </svg>
  );
}

function usable(viewer: QuestViewer | null, agent: WorldAgent): boolean {
  if (!viewer) return false;
  if (viewer.role === "admin") return true;
  return viewer.role === "write" && agent.owner_user_id === viewer.id;
}

export function canEditQuest(viewer: QuestViewer | null, task: TaskBrief, byId: Map<string, WorldAgent>): boolean {
  if (!viewer) return false;
  if (viewer.role === "admin") return true;
  if (viewer.role !== "write") return false;
  if (!task.assigned_agent_id) return true;
  const agent = byId.get(task.assigned_agent_id);
  return agent ? usable(viewer, agent) : false;
}

function personName(p: Person): string {
  return p.name || p.email?.split("@")[0] || "Someone";
}

export function AssigneeBadge({ task, byId, people }: { task: TaskBrief; byId: Map<string, WorldAgent>; people: Person[] }) {
  const agent = task.assigned_agent_id ? byId.get(task.assigned_agent_id) : undefined;
  if (agent) return <AgentFace agent={agent} size={20} />;
  const person = task.assignee_user_id ? people.find((p) => p.id === task.assignee_user_id) : undefined;
  if (person) return <PersonAvatar id={person.id} name={personName(person)} size={20} soft />;
  return <UserCircleDashed size={18} weight="bold" className="text-w-faint" aria-hidden />;
}

export function assigneeName(task: TaskBrief, byId: Map<string, WorldAgent>, people: Person[]): string {
  const agent = task.assigned_agent_id ? byId.get(task.assigned_agent_id) : undefined;
  if (agent) return agent.name;
  const person = task.assignee_user_id ? people.find((p) => p.id === task.assignee_user_id) : undefined;
  return person ? personName(person) : "Unassigned";
}

interface Option {
  id: string;
  label: string;
  icon: ReactNode;
  keywords: string;
  patch: QuestPatch;
  hint?: string;
  group?: string;
}

function Popover({ anchor, onClose, children }: { anchor: DOMRect; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  useEffect(() => {
    const el = ref.current;
    const h = el?.offsetHeight ?? 300;
    const w = el?.offsetWidth ?? 260;
    const below = anchor.bottom + 6;
    const top = below + h > window.innerHeight - 8 ? Math.max(8, anchor.top - h - 6) : below;
    const left = Math.min(Math.max(8, anchor.left), window.innerWidth - w - 8);
    setPos({ top, left });
  }, [anchor]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onDown, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onDown, true);
    };
  }, [onClose]);
  return createPortal(
    <div
      ref={ref}
      style={{ top: pos?.top ?? anchor.bottom + 6, left: pos?.left ?? anchor.left, opacity: pos ? undefined : 0 }}
      className="pointer-events-auto fixed z-[95] w-64 overflow-hidden rounded-xl border-[3px] border-w-ink bg-w-paper text-w-text shadow-[0_5px_0_rgb(var(--w-ink))] animate-[pop-in_120ms_ease-out_both] motion-reduce:animate-none"
    >
      {children}
    </div>,
    document.body,
  );
}

function Picker({ title, options, current, anchor, onPick, onClose }: { title: string; options: Option[]; current: string | null; anchor: DOMRect; onPick: (o: Option) => void; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const id = window.requestAnimationFrame(() => input.current?.focus());
    return () => window.cancelAnimationFrame(id);
  }, []);
  const groups = useMemo(() => {
    const out: { name: string; items: Option[] }[] = [];
    for (const o of options) {
      const name = o.group ?? "";
      const last = out[out.length - 1];
      if (last && last.name === name) last.items.push(o);
      else out.push({ name, items: [o] });
    }
    return out;
  }, [options]);
  return (
    <Popover anchor={anchor} onClose={onClose}>
      <Command label={title} loop>
        <div className="flex items-center gap-2 border-b-2 border-dashed border-w-line2 px-2.5">
          <MagnifyingGlass size={14} weight="bold" className="text-w-muted" />
          <Command.Input
            ref={input}
            autoFocus
            value={query}
            onValueChange={setQuery}
            placeholder={title}
            onKeyDown={(e) => {
              if (query || e.metaKey || e.ctrlKey || e.altKey || !/^[0-9]$/.test(e.key)) return;
              const hit = options.find((o) => o.hint === e.key);
              if (!hit) return;
              e.preventDefault();
              onPick(hit);
            }}
            className="h-9 min-w-0 flex-1 bg-transparent text-w-sm outline-none placeholder:text-w-faint"
          />
        </div>
        <Command.List className="max-h-72 overflow-y-auto p-1.5">
          <Command.Empty className="px-2 py-4 text-center text-w-sm text-w-muted">Nothing matches.</Command.Empty>
          {groups.map((g) => (
            <Command.Group
              key={g.name || "main"}
              heading={g.name || undefined}
              className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:pb-0.5 [&_[cmdk-group-heading]]:pt-1.5 [&_[cmdk-group-heading]]:text-w-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-w-muted"
            >
              {g.items.map((o) => (
                <Command.Item
                  key={o.id}
                  value={`${o.id} ${o.label} ${o.keywords}`}
                  onSelect={() => onPick(o)}
                  className="flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 text-w-sm outline-none data-[selected=true]:bg-w-sand"
                >
                  <span className="flex w-5 justify-center">{o.icon}</span>
                  <span className="min-w-0 flex-1 truncate">{o.label}</span>
                  {current === o.id && <span className="text-w-2xs text-w-link">current</span>}
                  {o.hint && <kbd className="rounded-md border border-w-line2 bg-w-field px-1 font-mono text-w-3xs text-w-muted">{o.hint}</kbd>}
                </Command.Item>
              ))}
            </Command.Group>
          ))}
        </Command.List>
      </Command>
    </Popover>
  );
}

function optimistic(task: TaskBrief, patch: QuestPatch): TaskBrief {
  const next = { ...task };
  if (patch.status) next.status = patch.status;
  if ("assigned_agent_id" in patch || "assignee_user_id" in patch) {
    next.assigned_agent_id = patch.assigned_agent_id ?? null;
    next.assignee_user_id = patch.assigned_agent_id ? null : patch.assignee_user_id ?? null;
    if (!patch.status && next.assigned_agent_id && next.assigned_agent_id !== task.assigned_agent_id && ["done", "cancelled", "failed"].includes(task.status)) {
      next.status = "todo";
    }
  }
  return next;
}

function isTyping(el: EventTarget | null): boolean {
  const t = el as HTMLElement | null;
  return Boolean(t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable));
}

type PickerState = { kind: "status" | "assignee"; ids: string[]; anchor: DOMRect } | null;

export function useQuestControls({
  active = true,
  tasks,
  setTasks,
  agents,
  viewer,
  order,
}: {
  active?: boolean;
  tasks: TaskBrief[];
  setTasks: (update: (prev: TaskBrief[]) => TaskBrief[]) => void;
  agents: WorldAgent[];
  viewer: QuestViewer | null;
  order: string[];
}) {
  const byId = useMemo(() => new Map(agents.map((a) => [a.id, a])), [agents]);
  const [people, setPeople] = useState<Person[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [picker, setPicker] = useState<PickerState>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const hovered = useRef<string | null>(null);
  const lastHovered = useRef<string | null>(null);
  const anchorId = useRef<string | null>(null);
  const tasksRef = useRef(tasks);
  tasksRef.current = tasks;

  useEffect(() => {
    if (!viewer || (viewer.role !== "write" && viewer.role !== "admin")) return;
    worldApi.people().then(setPeople).catch(() => {});
  }, [viewer]);

  useEffect(() => {
    if (!notice) return;
    const id = window.setTimeout(() => setNotice(null), 5000);
    return () => window.clearTimeout(id);
  }, [notice]);

  useEffect(() => {
    const live = new Set(tasks.map((t) => t.id));
    setSelected((prev) => {
      const next = new Set([...prev].filter((id) => live.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [tasks]);

  const editable = useCallback((t: TaskBrief) => canEditQuest(viewer, t, byId), [viewer, byId]);

  const apply = useCallback(
    async (ids: string[], patch: QuestPatch) => {
      const targets = tasksRef.current.filter((t) => ids.includes(t.id) && editable(t));
      if (!targets.length) return;
      const before = new Map(targets.map((t) => [t.id, t]));
      setTasks((prev) => prev.map((t) => (before.has(t.id) ? optimistic(t, patch) : t)));
      const results = await Promise.allSettled(targets.map((t) => worldApi.updateQuest(t.id, patch)));
      const failed = targets.filter((_, i) => results[i].status === "rejected");
      if (!failed.length) return;
      setTasks((prev) => prev.map((t) => before.get(t.id) && failed.some((f) => f.id === t.id) ? (before.get(t.id) as TaskBrief) : t));
      setNotice(failed.length === 1 ? `Could not update "${failed[0].title}".` : `Could not update ${failed.length} quests.`);
    },
    [editable, setTasks],
  );

  const targetsFor = useCallback(
    (id: string | null): string[] => {
      if (selected.size && (!id || selected.has(id))) return [...selected];
      return id ? [id] : [];
    },
    [selected],
  );

  const open = useCallback(
    (kind: "status" | "assignee", id: string | null, el?: Element | null) => {
      if (!active) return;
      const ids = targetsFor(id).filter((x) => {
        const t = tasksRef.current.find((q) => q.id === x);
        return t && editable(t);
      });
      if (!ids.length) return;
      const lead = id && ids.includes(id) ? id : ids[0];
      const node = el ?? document.querySelector(`[data-quest-${kind}="${lead}"]`) ?? document.querySelector(`[data-quest="${lead}"]`);
      const anchor = node?.getBoundingClientRect() ?? new DOMRect(window.innerWidth / 2 - 128, window.innerHeight / 3, 0, 0);
      setPicker({ kind, ids, anchor });
    },
    [active, targetsFor, editable],
  );

  const toggle = useCallback(
    (id: string, range = false) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (range && anchorId.current && order.includes(anchorId.current)) {
          const a = order.indexOf(anchorId.current);
          const b = order.indexOf(id);
          for (const x of order.slice(Math.min(a, b), Math.max(a, b) + 1)) next.add(x);
        } else if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      });
      anchorId.current = id;
    },
    [order],
  );

  const clear = useCallback(() => setSelected(new Set()), []);

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (picker || isTyping(e.target) || e.metaKey || e.ctrlKey || e.altKey) return;
      const k = e.key.toLowerCase();
      const id = hovered.current;
      if (k === "s" || k === "a") {
        if (!id && !selected.size) return;
        e.preventDefault();
        e.stopImmediatePropagation();
        open(k === "s" ? "status" : "assignee", id);
      } else if (k === "x" && id) {
        e.preventDefault();
        e.stopImmediatePropagation();
        toggle(id, e.shiftKey);
      } else if (k === "escape" && selected.size) {
        e.preventDefault();
        e.stopImmediatePropagation();
        clear();
      }
    };
    const onCommand = (e: Event) => {
      const kind = (e as CustomEvent<string>).detail;
      if (kind === "status" || kind === "assignee") window.setTimeout(() => open(kind, hovered.current ?? lastHovered.current), 0);
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("world:quest-command", onCommand);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("world:quest-command", onCommand);
    };
  }, [active, picker, selected, open, toggle, clear]);

  useEffect(() => {
    if (!active) return;
    window.dispatchEvent(new CustomEvent("world:quest-controls", { detail: true }));
    return () => {
      window.dispatchEvent(new CustomEvent("world:quest-controls", { detail: false }));
    };
  }, [active]);

  const pickerTasks = picker ? tasks.filter((t) => picker.ids.includes(t.id)) : [];
  const single = pickerTasks.length === 1 ? pickerTasks[0] : null;

  const statusOptions: Option[] = QUEST_STATUSES.map((s, i) => ({
    id: s.id,
    label: s.label,
    icon: <StatusIcon status={s.id} />,
    keywords: s.match.join(" "),
    patch: { status: s.id },
    hint: String(i + 1),
  }));

  const assigneeOptions: Option[] = useMemo(() => {
    const out: Option[] = [{ id: "none", label: "Unassigned", icon: <UserCircleDashed size={16} weight="bold" className="text-w-faint" />, keywords: "nobody none", patch: { assigned_agent_id: null, assignee_user_id: null }, hint: "0" }];
    const usableAgents = agents.filter((a) => usable(viewer, a)).sort((a, b) => a.name.localeCompare(b.name));
    usableAgents.forEach((a, i) =>
      out.push({
        id: `agent:${a.id}`,
        label: a.name,
        icon: <AgentFace agent={a} size={20} />,
        keywords: `${a.slug} ${(a.tags ?? []).join(" ")} ${a.expertise ?? ""}`,
        patch: { assigned_agent_id: a.id },
        hint: i < 9 ? String(i + 1) : undefined,
        group: "Agents",
      }),
    );
    const me = people.find((p) => p.id === viewer?.id);
    const rest = people.filter((p) => p.id !== viewer?.id).sort((a, b) => personName(a).localeCompare(personName(b)));
    for (const p of me ? [me, ...rest] : rest) {
      out.push({
        id: `person:${p.id}`,
        label: p.id === viewer?.id ? `${personName(p)} (you)` : personName(p),
        icon: <User size={15} weight="bold" className="text-w-warn" />,
        keywords: p.email ?? "",
        patch: { assignee_user_id: p.id },
        group: "People",
      });
    }
    return out;
  }, [agents, people, viewer]);

  const currentAssignee = single
    ? single.assigned_agent_id
      ? `agent:${single.assigned_agent_id}`
      : single.assignee_user_id
        ? `person:${single.assignee_user_id}`
        : "none"
    : null;

  const overlay = (
    <>
      {active && picker && (
        <Picker
          key={`${picker.kind}:${picker.ids.join(",")}`}
          title={picker.kind === "status" ? (picker.ids.length > 1 ? `Status for ${picker.ids.length} quests` : "Change status") : picker.ids.length > 1 ? `Assign ${picker.ids.length} quests` : "Assign to"}
          options={picker.kind === "status" ? statusOptions : assigneeOptions}
          current={picker.kind === "status" ? (single ? statusGroup(single.status) : null) : currentAssignee}
          anchor={picker.anchor}
          onPick={(o) => {
            const ids = picker.ids;
            setPicker(null);
            void apply(ids, o.patch);
          }}
          onClose={() => setPicker(null)}
        />
      )}
      {active && notice && (
        <div role="status" className="pointer-events-none fixed inset-x-0 bottom-24 z-[94] flex justify-center px-3">
          <p className="pointer-events-auto rounded-lg border-2 border-w-ink bg-w-lemon px-3 py-1.5 text-w-sm shadow-[0_3px_0_rgb(var(--w-ink))]">{notice}</p>
        </div>
      )}
    </>
  );

  return {
    byId,
    people,
    selected,
    editable,
    apply,
    open,
    toggle,
    clear,
    hover: (id: string | null) => {
      hovered.current = id;
      if (id) lastHovered.current = id;
    },
    overlay,
  };
}

export function SelectionBar({ count, onStatus, onAssignee, onClear }: { count: number; onStatus: (el: Element) => void; onAssignee: (el: Element) => void; onClear: () => void }) {
  if (!count) return null;
  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-3 z-10 flex justify-center px-3">
      <div className={cn("pointer-events-auto flex items-center gap-2 rounded-xl border-[3px] border-w-ink bg-w-paper px-3 py-2 shadow-[0_4px_0_rgb(var(--w-ink))] animate-[pop-in_140ms_ease-out_both] motion-reduce:animate-none")}>
        <span className="pl-1 pr-1 text-w-sm font-semibold tabular-nums text-w-strong">{count} selected</span>
        <button type="button" onClick={(e) => onStatus(e.currentTarget)} className={BTN_SECONDARY_SM}>
          Status <Keycap size="sm">S</Keycap>
        </button>
        <button type="button" onClick={(e) => onAssignee(e.currentTarget)} className={BTN_SECONDARY_SM}>
          Assignee <Keycap size="sm">A</Keycap>
        </button>
        <button type="button" onClick={onClear} aria-label="Clear selection" className={cn(BTN, "h-8 w-8 bg-w-card hover:bg-w-sand")}>
          <X size={13} weight="bold" />
        </button>
      </div>
    </div>
  );
}
