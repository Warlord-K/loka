"use client";

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { useStoreApi } from "@xyflow/react";
import { cn } from "@/lib/utils";
import { canvasItemKind, canvasItemLabel, reportsApi, type CanvasItem } from "@/lib/reports";
import { SIGNAL_EVENT, type WorldAgent, type WorldEvent } from "@/lib/world";
import {
  activityLabel,
  cursorActivity,
  cursorTone,
  dropCursor,
  dropGone,
  gliding,
  itemName,
  layoutLabels,
  moveCursor,
  nextPhaseIn,
  onScreen,
  PENDING_MS,
  PERSON_GLIDE_MS,
  personPresence,
  presenceDue,
  phaseAt,
  positionAt,
  readyToPlace,
  targetPoint,
  TRAIL_MAX,
  type Cursor,
  type PersonPresence,
  type SentPresence,
  type CursorActivity,
} from "@/lib/canvas-cursors";
import { AgentFace, PersonAvatar } from "./channel-ui";

const TONE = ["", "text-[rgb(var(--w-chart-1))]", "text-[rgb(var(--w-chart-2))]", "text-[rgb(var(--w-chart-3))]", "text-[rgb(var(--w-chart-4))]", "text-[rgb(var(--w-chart-5))]"];

interface Shown {
  actor: string;
  agent: WorldAgent | null;
  person: { id: string; name: string } | null;
  label: string;
}

interface Waiting {
  activity: CursorActivity;
  at: number;
  version: number;
}

const stillMotion = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

export function CanvasCursors({ reportId, items, agents, meId }: { reportId: string; items: CanvasItem[]; agents: WorldAgent[]; meId: string | null }) {
  const store = useStoreApi();
  const [shown, setShown] = useState<Shown[]>([]);
  const cursors = useRef(new Map<string, Cursor>());
  const incoming = useRef(new Map<string, Waiting>());
  const boxes = useRef(new Map<string, CanvasItem>());
  const version = useRef(0);
  const people = useRef(new Map<string, WorldAgent>());
  const viewers = useRef(new Map<string, { id: string; name: string }>());
  const arrivals = useRef(new Map<string, PersonPresence>());
  const nodes = useRef(new Map<string, HTMLDivElement>());
  const size = useRef({ w: 0, h: 0, rem: 16 });
  const widths = useRef(new Map<string, number>());
  const labels = useRef(new Map<string, HTMLElement>());
  const observer = useRef<ResizeObserver | null>(null);
  const frame = useRef<number | null>(null);
  const timer = useRef<number | null>(null);
  const schedule = useRef<() => void>(() => {});

  people.current = new Map(agents.map((a) => [a.id, a]));
  const meRef = useRef(meId);
  meRef.current = meId;

  useEffect(() => {
    const live = new Map(items.map((i) => [i.id, i]));
    boxes.current.forEach((box, id) => {
      if (!live.has(id)) live.set(id, box);
    });
    if (live.size > items.length + 200) boxes.current = new Map(items.map((i) => [i.id, i]));
    else boxes.current = live;
    version.current += 1;
    if (incoming.current.size) schedule.current();
  }, [items]);

  useEffect(() => {
    size.current.rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    const watch = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const actor = (entry.target as HTMLElement).dataset.actor;
        const box = entry.borderBoxSize[0];
        if (!actor || !box) continue;
        widths.current.set(actor, box.inlineSize);
        size.current.h = Math.max(size.current.h, box.blockSize);
      }
      size.current.w = Math.max(0, ...widths.current.values());
      schedule.current();
    });
    observer.current = watch;
    labels.current.forEach((el) => watch.observe(el));
    return () => {
      watch.disconnect();
      observer.current = null;
    };
  }, []);

  const register = useCallback((actor: string, el: HTMLDivElement | null) => {
    const old = nodes.current.get(actor);
    const oldLabel = labels.current.get(actor);
    if (oldLabel && oldLabel !== el?.querySelector("[data-cursor-label]")) {
      observer.current?.unobserve(oldLabel);
      labels.current.delete(actor);
      widths.current.delete(actor);
    }
    if (!el) {
      if (old) nodes.current.delete(actor);
      return;
    }
    nodes.current.set(actor, el);
    const tag = el.querySelector<HTMLElement>("[data-cursor-label]");
    if (tag && !labels.current.has(actor)) {
      labels.current.set(actor, tag);
      observer.current?.observe(tag);
    }
    schedule.current();
  }, []);

  useEffect(() => {
    const nameOf = (id: string) => {
      const item = boxes.current.get(id);
      return item ? itemName(item, canvasItemKind(item)) : null;
    };

    const take = (now: number, still: boolean) => {
      let changed = false;
      incoming.current.forEach((waiting, actor) => {
        if (!readyToPlace(waiting.activity.verb, waiting.version, version.current, now - waiting.at)) return;
        const point = targetPoint(waiting.activity, (id) => boxes.current.get(id));
        if (!point) {
          if (now - waiting.at > PENDING_MS) incoming.current.delete(actor);
          return;
        }
        incoming.current.delete(actor);
        cursors.current = moveCursor(cursors.current, actor, point, activityLabel(waiting.activity, nameOf), now, still);
        changed = true;
      });
      arrivals.current.forEach((arrival, actor) => {
        arrivals.current.delete(actor);
        if (!arrival.point) {
          const left = dropCursor(cursors.current, actor);
          if (left) cursors.current = left;
          viewers.current.delete(actor);
          changed = changed || Boolean(left);
          return;
        }
        const known = viewers.current.get(actor);
        viewers.current.set(actor, { id: arrival.userId, name: arrival.name });
        cursors.current = moveCursor(cursors.current, actor, arrival.point, "", now, still, PERSON_GLIDE_MS);
        changed = changed || !known || known.name !== arrival.name;
      });
      const pruned = dropGone(cursors.current, now);
      if (pruned) {
        cursors.current = pruned;
        changed = true;
      }
      return changed;
    };

    const render = () => {
      frame.current = null;
      const now = performance.now();
      const still = stillMotion();
      if (take(now, still)) {
        setShown(
          [...cursors.current.values()]
            .map((c) => ({ actor: c.actor, agent: people.current.get(c.actor) ?? null, person: viewers.current.get(c.actor) ?? null, label: c.label }))
            .filter((s) => Boolean(s.agent || s.person)),
        );
      }
      const { transform, width, height } = store.getState();
      const [tx, ty, zoom] = transform;
      const placed = [...cursors.current.values()].map((c) => {
        const p = positionAt(c, now, still);
        return { id: c.actor, x: p.x * zoom + tx, y: p.y * zoom + ty };
      });
      const label = size.current;
      const spots = layoutLabels(placed, width, { w: label.w || label.rem * 12, h: (label.h || label.rem * 1.75) + label.rem * 0.25 });
      let moving = false;
      for (const p of placed) {
        const el = nodes.current.get(p.id);
        const cursor = cursors.current.get(p.id);
        if (!el || !cursor) continue;
        if (gliding(cursor, now, still)) moving = true;
        const visible = onScreen(p.x, p.y, width, height);
        el.style.visibility = visible ? "visible" : "hidden";
        if (!visible) continue;
        el.style.transform = `translate3d(${p.x}px, ${p.y}px, 0)`;
        el.style.opacity = phaseAt(cursor, now) === "idle" ? "0.4" : "1";
        const tag = el.querySelector<HTMLElement>("[data-cursor-label]");
        const spot = spots.get(p.id);
        if (tag && spot) {
          const across = spot.flip ? -((widths.current.get(p.id) ?? label.w) + label.rem * 0.25) : label.rem * 0.9;
          tag.style.transform = `translate3d(${across}px, ${label.rem * 1.1 + spot.stack * (label.h + label.rem * 0.25)}px, 0)`;
          tag.dataset.idle = phaseAt(cursor, now) === "idle" ? "true" : "false";
        }
        const dots = el.querySelectorAll<HTMLElement>("[data-cursor-trail]");
        dots.forEach((dot, i) => {
          const point = still ? undefined : cursor.trail[cursor.trail.length - dots.length + i];
          if (!point) {
            dot.style.opacity = "0";
            return;
          }
          dot.style.opacity = String(0.18 + i * 0.14);
          dot.style.transform = `translate3d(${point.x * zoom + tx - p.x}px, ${point.y * zoom + ty - p.y}px, 0)`;
        });
      }
      if (moving) schedule.current();
      if (timer.current !== null) window.clearTimeout(timer.current);
      timer.current = null;
      const waits = [...cursors.current.values()].map((c) => nextPhaseIn(c, now)).filter((n): n is number => n !== null);
      incoming.current.forEach((waiting) => waits.push(Math.max(0, PENDING_MS - (now - waiting.at))));
      if (waits.length) timer.current = window.setTimeout(() => schedule.current(), Math.min(...waits) + 20);
    };

    schedule.current = () => {
      if (frame.current === null) frame.current = window.requestAnimationFrame(render);
    };

    const onEvent = (e: Event) => {
      const detail = (e as CustomEvent<WorldEvent>).detail;
      const viewer = personPresence(detail, reportId, meRef.current);
      if (viewer) {
        arrivals.current.set(viewer.actor, viewer);
        schedule.current();
        return;
      }
      const hit = cursorActivity(detail, reportId);
      if (!hit || !people.current.has(hit.actor)) return;
      incoming.current.set(hit.actor, { activity: hit.activity, at: performance.now(), version: version.current });
      schedule.current();
    };
    const unsubscribe = store.subscribe((state, prev) => {
      if (state.transform !== prev.transform || state.width !== prev.width || state.height !== prev.height) {
        if (cursors.current.size) schedule.current();
      }
    });
    window.addEventListener("world-event", onEvent);
    window.addEventListener(SIGNAL_EVENT, onEvent);
    return () => {
      window.removeEventListener("world-event", onEvent);
      window.removeEventListener(SIGNAL_EVENT, onEvent);
      unsubscribe();
      if (frame.current !== null) window.cancelAnimationFrame(frame.current);
      if (timer.current !== null) window.clearTimeout(timer.current);
      frame.current = null;
      timer.current = null;
      schedule.current = () => {};
    };
  }, [reportId, store]);

  useEffect(() => {
    cursors.current = new Map();
    incoming.current = new Map();
    arrivals.current = new Map();
    viewers.current = new Map();
    setShown([]);
  }, [reportId]);

  useEffect(() => {
    const pane = store.getState().domNode;
    if (!pane || !meId) return;
    let last: SentPresence | null = null;
    let pending: { x: number; y: number; item: string | null } | null = null;
    let raf: number | null = null;
    const flush = () => {
      raf = null;
      const next = pending;
      const now = performance.now();
      if (!next || !presenceDue(last, next, now)) return;
      last = { ...next, at: now };
      void reportsApi.presence(reportId, next);
    };
    const onMove = (e: PointerEvent) => {
      if (document.visibilityState !== "visible") return;
      const box = pane.getBoundingClientRect();
      const [tx, ty, zoom] = store.getState().transform;
      const node = (e.target as Element | null)?.closest?.(".react-flow__node[data-id]");
      const item = node?.getAttribute("data-id") ?? null;
      pending = { x: (e.clientX - box.left - tx) / zoom, y: (e.clientY - box.top - ty) / zoom, item: item && /^[A-Za-z0-9_-]{1,40}$/.test(item) ? item : null };
      if (raf === null) raf = window.requestAnimationFrame(flush);
    };
    const leave = (keepalive = false) => {
      pending = null;
      if (!last) return;
      last = null;
      void reportsApi.presence(reportId, { leaving: true }, { keepalive });
    };
    const onLeave = () => leave();
    const onHidden = () => {
      if (document.visibilityState !== "visible") leave(true);
    };
    pane.addEventListener("pointermove", onMove);
    pane.addEventListener("pointerleave", onLeave);
    document.addEventListener("visibilitychange", onHidden);
    return () => {
      pane.removeEventListener("pointermove", onMove);
      pane.removeEventListener("pointerleave", onLeave);
      document.removeEventListener("visibilitychange", onHidden);
      if (raf !== null) window.cancelAnimationFrame(raf);
      leave(true);
    };
  }, [store, reportId, meId]);

  if (!shown.length) return null;
  return (
    <div aria-hidden className="pointer-events-none absolute inset-0 z-[6] overflow-hidden">
      {shown.map(({ actor, agent, person, label }) => (
        <CursorRow key={actor} actor={actor} agent={agent} person={person} label={label} register={register} />
      ))}
    </div>
  );
}

const CursorRow = memo(function CursorRow({ actor, agent, person, label, register }: { actor: string; agent: WorldAgent | null; person: { id: string; name: string } | null; label: string; register: (actor: string, el: HTMLDivElement | null) => void }) {
  const ref = useCallback((el: HTMLDivElement | null) => register(actor, el), [actor, register]);
  return (
    <div ref={ref} className={cn("absolute left-0 top-0 will-change-transform transition-opacity duration-500 motion-reduce:transition-none", agent ? TONE[cursorTone(actor)] : "text-w-ink")} style={{ visibility: "hidden" }}>
      {Array.from({ length: TRAIL_MAX }, (_, i) => (
        <span key={i} data-cursor-trail className="absolute -left-1 -top-1 size-2 rounded-full bg-current opacity-0 motion-reduce:hidden" />
      ))}
      <svg viewBox="0 0 20 24" className="relative size-5 drop-shadow-[0_2px_0_rgb(var(--w-ink))]">
        <path d="M1 1 L1 19 L6 14.5 L9.5 22 L12.5 20.6 L9 13.2 L16 13.2 Z" fill="currentColor" stroke="rgb(var(--w-ink))" strokeWidth={1.6} strokeLinejoin="round" />
      </svg>
      <div data-cursor-label data-actor={actor} className="group absolute left-0 top-0 flex max-w-[18rem] items-center gap-1.5 whitespace-nowrap rounded-lg border-2 border-current bg-w-card py-0.5 pl-0.5 pr-2 shadow-[0_2px_0_rgb(var(--w-ink))]">
        {agent ? <AgentFace agent={agent} size={20} rounded="rounded-md" /> : person && <PersonAvatar id={person.id} name={person.name} size={20} />}
        <span className="text-w-xs font-bold text-w-strong">{agent?.name ?? person?.name}</span>
        {label && <span className="truncate text-w-xs text-w-text2 group-data-[idle=true]:hidden">{label}</span>}
      </div>
    </div>
  );
});
