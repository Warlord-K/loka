import type { AcademyTarget } from "./loka";
import type { Report, ReportBrief } from "./reports";

export type InkTool = "pen" | "pencil" | "marker" | "eraser";

export interface InkPoint {
  x: number;
  y: number;
  p: number;
  t: number;
}

export interface InkStroke {
  tool: InkTool;
  color: string;
  rgb?: string;
  size: number;
  points: InkPoint[];
}

export interface InkDoc {
  width: number;
  height: number;
  strokes: InkStroke[];
}

export interface Practice {
  subject: string;
  coach_agent_id: string | null;
}

export interface PracticeCheck {
  right: string;
  fix: string;
  next: string;
  score?: number | null;
}

export interface Attempt {
  id: string;
  report_id: string;
  subject: string;
  coach_agent_id: string | null;
  n: number;
  image_upload_id: string | null;
  note: string | null;
  status: "waiting" | "checked";
  check: PracticeCheck | null;
  coach_message_id: string | null;
  created_at: string;
  checked_at: string | null;
  stroke_count: number;
  grade_status?: "pending" | "passed" | "failed" | "error" | null;
  grade_score?: number | null;
  xp_awarded?: number | null;
}

export interface AttemptCard {
  id: string;
  report_id: string;
  title: string;
  subject: string;
  coach_agent_id: string | null;
  n: number;
  image_upload_id: string | null;
  status: "waiting" | "checked";
  check: PracticeCheck | null;
  short_check: string | null;
  created_at: string;
  checked_at: string | null;
}

export interface PracticeBrief extends ReportBrief {
  attempts: number;
  last_attempt_at: string | null;
}

export interface PracticeDetail {
  report: Report;
  practice: Practice;
  attempts: Attempt[];
}

export interface CoachOption {
  id: string;
  slug: string;
  name: string;
  expertise: string | null;
  is_main: boolean;
  paused_reason: string | null;
}

export interface Coaches {
  agents: CoachOption[];
  subject: string | null;
  default_coach_agent_id: string | null;
}

export class PracticeError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new PracticeError(res.status, text.trim() || "That did not work. Try again.");
  }
  return res.json() as Promise<T>;
}

const send = (url: string, method: string, body: unknown) =>
  fetch(url, { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body) });

export const practiceApi = {
  list: (subject?: string) =>
    fetch(`/api/world/practice${subject ? `?subject=${encodeURIComponent(subject)}` : ""}`, { cache: "no-store" }).then((r) => json<PracticeBrief[]>(r)),
  create: (body: { subject: string; title?: string; coach?: string }) => send("/api/world/practice", "POST", body).then((r) => json<{ report: Report; practice: Practice }>(r)),
  get: (id: string) => fetch(`/api/world/practice/${id}`, { cache: "no-store" }).then((r) => json<PracticeDetail>(r)),
  update: (id: string, patch: { coach_agent_id?: string; subject?: string; title?: string }) =>
    send(`/api/world/practice/${id}`, "PATCH", patch).then((r) => json<{ report: Report; practice: Practice }>(r)),
  coaches: (subject?: string) =>
    fetch(`/api/world/practice/coaches${subject ? `?subject=${encodeURIComponent(subject)}` : ""}`, { cache: "no-store" }).then((r) => json<Coaches>(r)),
  ready: (id: string, body: { image_upload_id: string; strokes: InkDoc; note?: string }) => send(`/api/world/practice/${id}/ready`, "POST", body).then((r) => json<Attempt>(r)),
  attempt: (id: string) => fetch(`/api/world/practice/attempts/${encodeURIComponent(id)}`, { cache: "no-store" }).then((r) => json<AttemptCard>(r)),
  resend: (id: string) => fetch(`/api/world/practice/attempts/${encodeURIComponent(id)}/resend`, { method: "POST" }).then((r) => json<Attempt>(r)),
};

export const COACH_WAIT_MS = 3 * 60 * 1000;

export function coachLate(createdAt: string, since: number | null, now: number): boolean {
  const start = Math.max(Date.parse(createdAt) || 0, since ?? 0);
  return now - start >= COACH_WAIT_MS;
}

export const PRACTICE_VOICE = "world:practice-voice";

const keptInReports = new Set<string>();

export function keepInReports(reportId: string) {
  keptInReports.add(reportId);
}

export function keptInReport(reportId: string): boolean {
  return keptInReports.has(reportId);
}

function toAcademy(detail: AcademyTarget) {
  window.dispatchEvent(new CustomEvent<AcademyTarget>("world:open-academy", { detail }));
}

export function openPractice(reportId: string, attemptId?: string | null) {
  toAcademy({ reportId, attemptId: attemptId ?? null });
}

export function startPractice() {
  toAcademy({ newPractice: true });
}

export const ATTEMPT_ITEM = "attempt-";

export const ATTEMPT_ID_SOURCE = "[0-9a-fA-F][0-9a-fA-F-]{7,35}";

export const ATTEMPT_ID = new RegExp(`^(?:${ATTEMPT_ID_SOURCE})$`);

export function attemptFromItem(item: string | null | undefined): string | null {
  return item && item.startsWith(ATTEMPT_ITEM) ? item.slice(ATTEMPT_ITEM.length).toLowerCase() : null;
}

export const SUBJECTS = [
  { value: "japanese", label: "Japanese" },
  { value: "drawing", label: "Drawing" },
];

export function cleanSubject(subject: string): string {
  return subject
    .split(/[\s_-]+/)
    .map((w) =>
      Array.from(w)
        .filter((c) => /[\p{L}\p{N}]/u.test(c))
        .join("")
        .toLowerCase(),
    )
    .filter(Boolean)
    .join("-")
    .slice(0, 40);
}

export function subjectLabel(subject: string): string {
  return subject
    .split("-")
    .filter(Boolean)
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join(" ");
}

export function practiceOf(value: unknown): Practice | null {
  if (!value || typeof value !== "object") return null;
  const v = value as { subject?: unknown; coach_agent_id?: unknown };
  if (typeof v.subject !== "string" || !v.subject) return null;
  return { subject: v.subject, coach_agent_id: typeof v.coach_agent_id === "string" ? v.coach_agent_id : null };
}

export function checkOf(value: unknown): PracticeCheck | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  if (typeof v.right !== "string" || typeof v.fix !== "string" || typeof v.next !== "string") return null;
  return { right: v.right, fix: v.fix, next: v.next, score: typeof v.score === "number" && Number.isFinite(v.score) ? v.score : null };
}

const READY = /^(?:(?:ok(?:ay)?|right|alright|so)\s+)?(?:i\s*am\s+|i'?m\s+)?ready(?:\s+now)?$/;

export function isReadyWord(text: string): boolean {
  const plain = text
    .toLowerCase()
    .replace(/[’]/g, "'")
    .replace(/[^\p{L}\p{N}'\s]+/gu, " ")
    .replace(/\s+/g, " ")
    .trim();
  return READY.test(plain);
}

export function boardSize(subject: string): { width: number; height: number } {
  return subject === "japanese" ? { width: 1024, height: 1024 } : { width: 1280, height: 960 };
}

export const INK_COLORS = ["w-strong", "w-danger", "w-link", "w-chart-3", "w-chart-4"] as const;

export const INK_COLOR_LABEL: Record<string, string> = {
  "w-strong": "Ink",
  "w-danger": "Red",
  "w-link": "Teal",
  "w-chart-3": "Violet",
  "w-chart-4": "Amber",
};

export const TOOL_SIZES: Record<InkTool, number[]> = {
  pen: [4, 8, 14],
  pencil: [2, 4, 7],
  marker: [14, 24, 40],
  eraser: [16, 32, 64],
};

export const TOOL_ALPHA: Record<InkTool, number> = { pen: 1, pencil: 0.78, marker: 0.42, eraser: 1 };

export function strokeWidth(tool: InkTool, size: number, p: number): number {
  const pressure = Math.min(1, Math.max(0, p));
  if (tool === "pen") return size * (0.3 + 1.1 * pressure);
  if (tool === "pencil") return size * (0.55 + 0.6 * pressure);
  return size;
}

export function smoothPressure(previous: number | null, next: number): number {
  return previous === null ? next : previous * 0.6 + next * 0.4;
}

export function pressureOf(pointerType: string, pressure: number, buttons: number): number {
  if (pointerType === "pen") return pressure > 0 ? pressure : buttons ? 0.5 : 0;
  return 0.5;
}

export function toBoard(clientX: number, clientY: number, rect: { left: number; top: number; width: number; height: number }, board: { width: number; height: number }) {
  const x = ((clientX - rect.left) / Math.max(1, rect.width)) * board.width;
  const y = ((clientY - rect.top) / Math.max(1, rect.height)) * board.height;
  return { x: Math.round(Math.min(board.width, Math.max(0, x)) * 10) / 10, y: Math.round(Math.min(board.height, Math.max(0, y)) * 10) / 10 };
}

export type PointerChoice = "draw" | "ignore" | "takeover";

export function pointerChoice(opts: { pointerType: string; penSeen: boolean; fingerDraw: boolean; activeId: number | null; activeType?: string | null; pointerId: number }): PointerChoice {
  if (opts.activeId !== null && opts.activeId !== opts.pointerId) return opts.pointerType === "pen" && opts.activeType === "touch" ? "takeover" : "ignore";
  if (opts.pointerType === "touch" && opts.penSeen && !opts.fingerDraw) return "ignore";
  return "draw";
}

export const PEN_SEEN_KEY = "world.practice.pen-seen";

export interface InkHistory {
  strokes: InkStroke[];
  past: InkStroke[][];
  future: InkStroke[][];
}

export type InkAction = { type: "add"; stroke: InkStroke } | { type: "undo" } | { type: "redo" } | { type: "clear" } | { type: "reset" };

export const EMPTY_INK: InkHistory = { strokes: [], past: [], future: [] };

const HISTORY_LIMIT = 200;

export function inkReducer(state: InkHistory, action: InkAction): InkHistory {
  switch (action.type) {
    case "add":
      if (action.stroke.points.length === 0) return state;
      return { strokes: [...state.strokes, action.stroke], past: [...state.past, state.strokes].slice(-HISTORY_LIMIT), future: [] };
    case "undo": {
      const previous = state.past[state.past.length - 1];
      if (!previous) return state;
      return { strokes: previous, past: state.past.slice(0, -1), future: [state.strokes, ...state.future] };
    }
    case "redo": {
      const [next, ...rest] = state.future;
      if (!next) return state;
      return { strokes: next, past: [...state.past, state.strokes], future: rest };
    }
    case "clear":
      if (state.strokes.length === 0) return state;
      return { strokes: [], past: [...state.past, state.strokes].slice(-HISTORY_LIMIT), future: [] };
    case "reset":
      return EMPTY_INK;
  }
}

export function inkDoc(width: number, height: number, strokes: InkStroke[]): InkDoc {
  return {
    width,
    height,
    strokes: strokes.map((s) => ({ ...s, points: s.points.map((pt) => ({ x: pt.x, y: pt.y, p: Math.round(pt.p * 1000) / 1000, t: Math.max(0, Math.round(pt.t)) })) })),
  };
}

export function hasInk(strokes: InkStroke[]): boolean {
  return strokes.some((s) => s.tool !== "eraser");
}

export function rgbHex(channels: string): string | null {
  const parts = channels.trim().split(/[\s,]+/).map(Number);
  if (parts.length < 3 || parts.slice(0, 3).some((n) => !Number.isFinite(n))) return null;
  return `#${parts
    .slice(0, 3)
    .map((n) => Math.round(Math.min(255, Math.max(0, n))).toString(16).padStart(2, "0"))
    .join("")}`;
}

export const LIMITS = { bytes: 2 * 1024 * 1024 - 4096, points: 100_000, strokes: 2000 };

export const TOO_FULL = "This page is too full to send. Start a new attempt.";

function lineDistance(p: InkPoint, a: InkPoint, b: InkPoint): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len = dx * dx + dy * dy;
  if (len === 0) return Math.hypot(p.x - a.x, p.y - a.y);
  const t = Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / len));
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

export function thinPoints(points: InkPoint[], epsilon: number, minStep = 0.5): InkPoint[] {
  const spaced: InkPoint[] = [];
  for (const pt of points) {
    const last = spaced[spaced.length - 1];
    if (!last || Math.hypot(pt.x - last.x, pt.y - last.y) >= minStep) spaced.push(pt);
  }
  const tail = points[points.length - 1];
  if (tail && spaced[spaced.length - 1] !== tail && spaced.length > 1) spaced[spaced.length - 1] = tail;
  if (spaced.length <= 2 || epsilon <= 0) return spaced;
  const keep = new Uint8Array(spaced.length);
  keep[0] = 1;
  keep[spaced.length - 1] = 1;
  const stack: [number, number][] = [[0, spaced.length - 1]];
  while (stack.length) {
    const [from, to] = stack.pop()!;
    let worst = -1;
    let at = -1;
    for (let i = from + 1; i < to; i++) {
      const d = lineDistance(spaced[i], spaced[from], spaced[to]) + Math.abs(spaced[i].p - spaced[from].p) * epsilon * 4;
      if (d > worst) {
        worst = d;
        at = i;
      }
    }
    if (at > 0 && worst > epsilon) {
      keep[at] = 1;
      stack.push([from, at], [at, to]);
    }
  }
  return spaced.filter((_, i) => keep[i]);
}

export function serdeBytes(doc: InkDoc): number {
  let whole = 0;
  const f64 = (n: number) => {
    if (Number.isInteger(n)) whole += 1;
  };
  f64(doc.width);
  f64(doc.height);
  for (const s of doc.strokes) {
    f64(s.size);
    for (const pt of s.points) {
      f64(pt.x);
      f64(pt.y);
      f64(pt.p);
      f64(pt.t);
    }
  }
  return new TextEncoder().encode(JSON.stringify(doc)).length + whole * 2;
}

export type PreparedInk = { ok: true; doc: InkDoc } | { ok: false; error: string };

const EPSILONS = [0.35, 1, 2.5, 5, 10];

export function prepareInk(doc: InkDoc, limits = LIMITS): PreparedInk {
  if (doc.strokes.length > limits.strokes) return { ok: false, error: TOO_FULL };
  for (const epsilon of EPSILONS) {
    const thinned = { ...doc, strokes: doc.strokes.map((s) => ({ ...s, points: thinPoints(s.points, epsilon) })) };
    const points = thinned.strokes.reduce((n, s) => n + s.points.length, 0);
    if (points <= limits.points && serdeBytes(thinned) <= limits.bytes) return { ok: true, doc: thinned };
  }
  return { ok: false, error: TOO_FULL };
}
