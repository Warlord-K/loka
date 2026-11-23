import { EMBEDS, type CanvasEmbed } from "./canvas-embeds";

export interface ReportBrief {
  id: string;
  owner_user_id: string;
  author_agent_id: string | null;
  author_user_id: string | null;
  title: string;
  slug: string;
  tags: string[];
  folder: string;
  pinned: boolean;
  visibility: "private" | "public";
  revision: number;
  task_id: string | null;
  created_at: string;
  updated_at: string;
  excerpt: string;
  size: number;
  kind: ReportKind;
  open_marks: number;
  item_count: number;
  practice?: { subject: string; coach_agent_id?: string | null } | null;
  challenge_id?: string | null;
}

export type ReportKind = "doc" | "canvas";

export type CanvasKind = "image" | "video" | "model" | "note" | "sticky" | "ref" | "run" | "frame" | "ink" | "ui" | "embed";

export type StickyColor = "lemon" | "rose" | "mint" | "aqua" | "honey" | "sand";

export interface CanvasItem {
  id: string;
  kind: CanvasKind;
  x: number;
  y: number;
  w: number;
  h: number;
  text?: string;
  caption?: string;
  upload_id?: string;
  target?: string;
  command?: string;
  color?: StickyColor;
  locked?: boolean;
  points?: [number, number][];
  spec?: unknown;
  embed?: CanvasEmbed;
  by?: string;
}

export interface CanvasEdge {
  id: string;
  from: string;
  to: string;
  label?: string;
}

export interface CanvasDoc {
  items: CanvasItem[];
  edges: CanvasEdge[];
}

export type CanvasOp =
  | { op: "add"; items: Partial<CanvasItem>[]; edges?: Omit<CanvasEdge, "id">[] }
  | { op: "update"; items: (Partial<CanvasItem> & { id: string })[] }
  | { op: "remove"; ids: string[] }
  | { op: "connect"; edges: (Omit<CanvasEdge, "id"> & { id?: string })[] }
  | { op: "disconnect"; ids: string[] };

export interface CanvasMark {
  id: string;
  report_id: string;
  item_id: string | null;
  x: number;
  y: number;
  kind: "pin" | "sticky";
  body: string;
  status: "open" | "resolved";
  reply: string | null;
  author_user_id: string | null;
  resolved_by_agent_id: string | null;
  resolved_by_user_id: string | null;
  created_at: string;
  resolved_at: string | null;
}

export interface Report extends Omit<ReportBrief, "excerpt" | "size" | "open_marks" | "item_count"> {
  body: string;
  links: string[];
  canvas: CanvasDoc | null;
}

export interface ReportDetail {
  report: Report;
  backlinks: ReportBrief[];
  outlinks: ReportBrief[];
  can_edit: boolean;
  marks: CanvasMark[];
  lesson?: string | null;
}

export interface RevisionBrief {
  revision: number;
  title: string;
  folder: string;
  tags: string[];
  author_agent_id: string | null;
  author_user_id: string | null;
  created_at: string;
  size: number;
}

export interface Revision extends Omit<RevisionBrief, "size"> {
  body: string;
  canvas: CanvasDoc | null;
}

export interface ReportPatch {
  title?: string;
  body?: string;
  append?: string;
  folder?: string;
  tags?: string[];
  pinned?: boolean;
  visibility?: "private" | "public";
  base_updated_at?: string;
}

export class ReportError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

export class StaleReport extends ReportError {
  constructor(
    public latest: Report,
    message: string,
  ) {
    super(409, message);
  }
}

async function updated(res: Response): Promise<Report> {
  if (res.status === 409) {
    const text = await res.text().catch(() => "");
    let body: { error?: string; report?: Report } | null = null;
    try {
      body = JSON.parse(text);
    } catch {}
    if (body?.report) throw new StaleReport(body.report, body.error || "Someone saved a newer version of this report.");
    throw new ReportError(409, body?.error || text || res.statusText);
  }
  return json<Report>(res);
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new ReportError(res.status, text || res.statusText);
  }
  return res.json() as Promise<T>;
}

type SendOptions = { keepalive?: boolean };

const send = (url: string, method: string, body?: unknown, opts: SendOptions = {}) =>
  fetch(url, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    keepalive: opts.keepalive,
  });

export const REPORTS_STALE = "world:reports-stale";

export const reportsApi = {
  list: (filter: { query?: string; folder?: string; tag?: string } = {}) => {
    const q = new URLSearchParams();
    for (const [k, v] of Object.entries(filter)) if (v) q.set(k, v);
    return fetch(`/api/world/reports?${q}`, { cache: "no-store" }).then((r) => json<ReportBrief[]>(r));
  },
  find: (key: string) => fetch(`/api/world/reports/find?key=${encodeURIComponent(key)}`, { cache: "no-store" }).then((r) => json<ReportBrief>(r)),
  get: (id: string) => fetch(`/api/world/reports/${id}`, { cache: "no-store" }).then((r) => json<ReportDetail>(r)),
  create: (body: { title: string; body: string; folder?: string; tags?: string[]; canvas?: CanvasDoc }) => send("/api/world/reports", "POST", body).then((r) => json<Report>(r)),
  changeCanvas: (id: string, ops: CanvasOp[], opts?: SendOptions) => send(`/api/world/reports/${id}/canvas`, "POST", { ops }, opts).then((r) => json<Report>(r)),
  addMark: (id: string, mark: { item_id?: string | null; x: number; y: number; kind: "pin" | "sticky"; body: string }) =>
    send(`/api/world/reports/${id}/marks`, "POST", mark).then((r) => json<CanvasMark>(r)),
  presence: (id: string, at: { x?: number; y?: number; item?: string | null; leaving?: boolean }, opts?: SendOptions) =>
    send(`/api/world/reports/${id}/presence`, "POST", at, opts).then(() => undefined, () => undefined),
  setMark: (id: string, mark: string, resolved: boolean, reply?: string) =>
    send(`/api/world/reports/${id}/marks/${mark}`, "PATCH", { resolved, reply }).then((r) => json<CanvasMark>(r)),
  removeMark: async (id: string, mark: string, opts?: SendOptions) => {
    const res = await send(`/api/world/reports/${id}/marks/${mark}`, "DELETE", undefined, opts);
    if (!res.ok) throw new ReportError(res.status, res.statusText);
  },
  update: (id: string, patch: ReportPatch) => send(`/api/world/reports/${id}`, "PATCH", patch).then(updated),
  remove: async (id: string) => {
    const res = await send(`/api/world/reports/${id}`, "DELETE");
    if (!res.ok) throw new ReportError(res.status, res.statusText);
  },
  revisions: (id: string) => fetch(`/api/world/reports/${id}/revisions`, { cache: "no-store" }).then((r) => json<RevisionBrief[]>(r)),
  revision: (id: string, n: number) => fetch(`/api/world/reports/${id}/revisions/${n}`, { cache: "no-store" }).then((r) => json<Revision>(r)),
  restore: (id: string, n: number) => send(`/api/world/reports/${id}/revisions/${n}/restore`, "POST").then((r) => json<Report>(r)),
};

export function openReport(id?: string | null, item?: string | null) {
  window.dispatchEvent(new CustomEvent("world:open-report", { detail: { id: id ?? null, item: item ?? null } }));
}

export function canvasItemLabel(item: CanvasItem): string {
  const text = item.caption || item.text || item.target || item.command || (item.kind === "ui" ? "Card" : item.embed ? EMBEDS[item.embed.kind]?.label ?? "" : "");
  return text.replace(/\s+/g, " ").trim().slice(0, 160);
}

const REF_LABEL: Record<string, string> = { quest: "Quest", report: "Report", todo: "To-do", msg: "Message", agent: "Agent" };

export function canvasItemKind(item: CanvasItem): string {
  if (item.kind === "embed") return item.embed ? `Live ${EMBEDS[item.embed.kind]?.label.toLowerCase() ?? "embed"}` : CANVAS_KIND_LABEL.embed;
  return item.kind === "ref" ? (REF_LABEL[(item.target ?? "").split(":")[0]] ?? "Card") : CANVAS_KIND_LABEL[item.kind];
}

export const CANVAS_KIND_LABEL: Record<CanvasKind, string> = {
  image: "Image",
  video: "Video",
  model: "3D model",
  note: "Note",
  sticky: "Sticky",
  ref: "Card",
  run: "Step",
  frame: "Frame",
  ink: "Drawing",
  ui: "Card",
  embed: "Live embed",
};

export function headingSlug(text: string): string {
  return (
    text
      .toLowerCase()
      .replace(/[`*_~[\]()]/g, "")
      .replace(/[^\p{L}\p{N}]+/gu, "-")
      .replace(/^-+|-+$/g, "") || "section"
  );
}

export interface Heading {
  depth: number;
  text: string;
  id: string;
}

export function headingsOf(body: string): Heading[] {
  const out: Heading[] = [];
  let fenced = false;
  for (const line of body.split("\n")) {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;
    const m = line.match(/^(#{1,4})\s+(.+?)\s*#*\s*$/);
    if (!m) continue;
    const text = m[2]
      .replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g, (_, t: string, alias?: string) => alias ?? t)
      .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/[*_`~]/g, "");
    out.push({ depth: m[1].length, text, id: headingSlug(text) });
  }
  return out;
}

export function wikify(body: string, known?: (title: string) => boolean): string {
  return body
    .split(/(```[\s\S]*?```|~~~[\s\S]*?~~~|`[^`\n]*`)/g)
    .map((part, i) =>
      i % 2
        ? part
        : part.replace(/\[\[([^\]\n|#]+)(?:#[^\]\n|]*)?(?:\|([^\]\n]+))?\]\]/g, (_, target: string, alias?: string) => {
            const label = (alias ?? target).trim().replace(/[[\]]/g, "");
            const missing = known && !known(target.trim()) ? "~" : "";
            return `[${label}](#wiki-${missing}${encodeURIComponent(target.trim())})`;
          }),
    )
    .join("");
}

export function folderParts(folder: string): string[] {
  return folder ? folder.split("/") : [];
}
