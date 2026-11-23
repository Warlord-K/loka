export type CursorVerb = "add" | "edit" | "move" | "resize" | "remove" | "connect" | "disconnect" | "arrange" | "resolve";

export interface CursorActivity {
  verb: CursorVerb;
  items: string[];
}

export interface CursorBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Point {
  x: number;
  y: number;
}

export interface Cursor {
  actor: string;
  from: Point;
  to: Point;
  startedAt: number;
  lastAt: number;
  trail: Point[];
  label: string;
  glide?: number;
}

export interface CursorEventLike {
  type: string;
  agent_id: string | null;
  payload: Record<string, unknown>;
  replayed?: boolean;
}

export const GLIDE_MS = 420;
export const IDLE_MS = 8000;
export const DROP_MS = 30000;
export const TRAIL_MAX = 3;
export const PENDING_MS = 5000;
export const NAME_MAX = 28;
export const CURSOR_TONES = 5;
export const PRESENCE_EVENT = "presence.canvas";
export const PERSON_GLIDE_MS = 160;
export const PRESENCE_GAP_MS = 150;
export const PRESENCE_MOVE_MIN = 24;

const VERBS: readonly CursorVerb[] = ["add", "edit", "move", "resize", "remove", "connect", "disconnect", "arrange", "resolve"];

export const RELAYOUT_VERBS: readonly CursorVerb[] = ["add", "move", "resize", "arrange"];

export function readActivity(raw: unknown): CursorActivity | null {
  if (!raw || typeof raw !== "object") return null;
  const { verb, items } = raw as { verb?: unknown; items?: unknown };
  if (typeof verb !== "string" || !VERBS.includes(verb as CursorVerb) || !Array.isArray(items)) return null;
  const ids = items.filter((i): i is string => typeof i === "string" && i.length > 0 && i.length <= 40).slice(0, 8);
  return ids.length ? { verb: verb as CursorVerb, items: ids } : null;
}

export function cursorActivity(event: CursorEventLike, reportId: string): { actor: string; activity: CursorActivity } | null {
  if (event.replayed || !event.agent_id || event.payload.report_id !== reportId) return null;
  if (event.type === "report.updated") {
    const activity = readActivity(event.payload.activity);
    return activity ? { actor: event.agent_id, activity } : null;
  }
  if (event.type === "canvas.mark_resolved" && typeof event.payload.item_id === "string" && event.payload.item_id) {
    return { actor: event.agent_id, activity: { verb: "resolve", items: [event.payload.item_id] } };
  }
  return null;
}

const centre = (b: CursorBox): Point => ({ x: b.x + b.w / 2, y: b.y + b.h / 2 });

export function readyToPlace(verb: CursorVerb, seenAt: number, current: number, age: number): boolean {
  return !RELAYOUT_VERBS.includes(verb) || current > seenAt || age >= PENDING_MS;
}

export function targetPoint(activity: CursorActivity, boxOf: (id: string) => CursorBox | undefined): Point | null {
  const boxes = activity.items.map(boxOf).filter((b): b is CursorBox => Boolean(b));
  if (!boxes.length) return null;
  if ((activity.verb === "connect" || activity.verb === "disconnect") && boxes.length > 1) {
    const [a, b] = [centre(boxes[0]), centre(boxes[1])];
    return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  }
  const b = boxes[0];
  return { x: b.x + b.w * 0.62, y: b.y + Math.min(b.h * 0.4, 120) };
}

export interface NamedItem {
  kind: string;
  caption?: string;
  text?: string;
  command?: string;
}

export function itemName(item: NamedItem, kindLabel: string): string {
  if (item.caption?.trim()) return item.caption.trim();
  if (item.kind === "ref" || item.kind === "embed" || item.kind === "ui") return kindLabel;
  const line = (item.text ?? item.command ?? "")
    .split("\n")
    .map((l) => l.replace(/[*_`#>~]|\[\[|\]\]/g, "").replace(/\[([^\]]*)\]\([^)]*\)/g, "$1").trim())
    .find(Boolean);
  return line || kindLabel;
}

export function shortName(name: string): string {
  const flat = name.replace(/\s+/g, " ").trim();
  return flat.length > NAME_MAX ? `${flat.slice(0, NAME_MAX - 1).trimEnd()}…` : flat;
}

export function activityLabel(activity: CursorActivity, nameOf: (id: string) => string | null): string {
  const names = activity.items.map((id) => nameOf(id)).map((n) => (n ? shortName(n) : null));
  const one = names[0] ?? "an item";
  const count = activity.items.length;
  const many = count > 1 ? `${count} items` : one;
  switch (activity.verb) {
    case "add":
      return `adding ${many}`;
    case "edit":
      return `editing ${many}`;
    case "move":
      return `moving ${many}`;
    case "resize":
      return `resizing ${many}`;
    case "remove":
      return `removing ${many}`;
    case "connect":
      return count > 1 ? `connecting ${one} to ${names[1] ?? "an item"}` : `connecting ${one}`;
    case "disconnect":
      return count > 1 ? `unlinking ${one} and ${names[1] ?? "an item"}` : `unlinking ${one}`;
    case "arrange":
      return `arranging ${many}`;
    case "resolve":
      return `answering a pin on ${one}`;
  }
}

const ease = (t: number) => 1 - Math.pow(1 - t, 3);

export interface PersonPresence {
  actor: string;
  userId: string;
  name: string;
  point: Point | null;
}

export function personPresence(event: CursorEventLike, reportId: string, meId: string | null): PersonPresence | null {
  if (event.replayed || event.type !== PRESENCE_EVENT || event.payload.report_id !== reportId) return null;
  const userId = event.payload.user_id;
  if (typeof userId !== "string" || !userId || userId === meId) return null;
  const name = typeof event.payload.name === "string" && event.payload.name.trim() ? event.payload.name.trim() : "Someone";
  const { x, y } = event.payload;
  const point = event.payload.leaving === true || typeof x !== "number" || typeof y !== "number" ? null : { x, y };
  return { actor: `person:${userId}`, userId, name: shortName(name), point };
}

export interface SentPresence {
  x: number;
  y: number;
  item: string | null;
  at: number;
}

export function presenceDue(last: SentPresence | null, next: { x: number; y: number; item: string | null }, now: number): boolean {
  if (!last) return true;
  if (now - last.at < PRESENCE_GAP_MS) return false;
  return next.item !== last.item || Math.hypot(next.x - last.x, next.y - last.y) >= PRESENCE_MOVE_MIN;
}

export function positionAt(cursor: Cursor, now: number, still = false): Point {
  if (still) return cursor.to;
  const t = Math.min(1, Math.max(0, (now - cursor.startedAt) / (cursor.glide ?? GLIDE_MS)));
  const k = ease(t);
  return { x: cursor.from.x + (cursor.to.x - cursor.from.x) * k, y: cursor.from.y + (cursor.to.y - cursor.from.y) * k };
}

export function gliding(cursor: Cursor, now: number, still = false): boolean {
  return !still && now - cursor.startedAt < (cursor.glide ?? GLIDE_MS);
}

export function phaseAt(cursor: Cursor, now: number): "live" | "idle" | "gone" {
  const quiet = now - cursor.lastAt;
  return quiet >= DROP_MS ? "gone" : quiet >= IDLE_MS ? "idle" : "live";
}

export function nextPhaseIn(cursor: Cursor, now: number): number | null {
  const quiet = now - cursor.lastAt;
  if (quiet < IDLE_MS) return IDLE_MS - quiet;
  if (quiet < DROP_MS) return DROP_MS - quiet;
  return null;
}

export function moveCursor(cursors: ReadonlyMap<string, Cursor>, actor: string, to: Point, label: string, now: number, still = false, glide = GLIDE_MS): Map<string, Cursor> {
  const next = new Map(cursors);
  const prev = cursors.get(actor);
  if (!prev) {
    next.set(actor, { actor, from: to, to, startedAt: now, lastAt: now, trail: [], label, glide });
    return next;
  }
  const here = positionAt(prev, now, still);
  const moved = Math.hypot(prev.to.x - to.x, prev.to.y - to.y) > 1;
  const trail = still || !moved || glide < GLIDE_MS ? prev.trail : [...prev.trail, prev.to].slice(-TRAIL_MAX);
  next.set(actor, { actor, from: here, to, startedAt: now, lastAt: now, trail, label, glide });
  return next;
}

export function dropCursor(cursors: ReadonlyMap<string, Cursor>, actor: string): Map<string, Cursor> | null {
  if (!cursors.has(actor)) return null;
  const next = new Map(cursors);
  next.delete(actor);
  return next;
}

export function dropGone(cursors: ReadonlyMap<string, Cursor>, now: number): Map<string, Cursor> | null {
  const gone = [...cursors.values()].filter((c) => phaseAt(c, now) === "gone");
  if (!gone.length) return null;
  const next = new Map(cursors);
  gone.forEach((c) => next.delete(c.actor));
  return next;
}

export interface Placed {
  id: string;
  x: number;
  y: number;
}

export interface LabelSpot {
  stack: number;
  flip: boolean;
}

export function layoutLabels(placed: Placed[], width: number, label: { w: number; h: number }): Map<string, LabelSpot> {
  const order = [...placed].sort((a, b) => a.y - b.y || a.x - b.x || a.id.localeCompare(b.id));
  const spots = new Map<string, LabelSpot>();
  const taken: { x: number; top: number; flip: boolean }[] = [];
  for (const p of order) {
    const flip = p.x + label.w > width;
    const left = flip ? p.x - label.w : p.x;
    let stack = 0;
    while (taken.some((t) => Math.abs((t.flip ? t.x - label.w : t.x) - left) < label.w && Math.abs(t.top - (p.y + stack * label.h)) < label.h)) stack += 1;
    taken.push({ x: p.x, top: p.y + stack * label.h, flip });
    spots.set(p.id, { stack, flip });
  }
  return spots;
}

export function cursorTone(actor: string): number {
  let h = 0;
  for (let i = 0; i < actor.length; i += 1) h = (h * 31 + actor.charCodeAt(i)) >>> 0;
  return (h % CURSOR_TONES) + 1;
}

export function onScreen(x: number, y: number, width: number, height: number, margin = 48): boolean {
  return x >= -margin && y >= -margin && x <= width + margin && y <= height + margin;
}
