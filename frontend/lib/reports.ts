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
}

export interface Report extends Omit<ReportBrief, "excerpt" | "size"> {
  body: string;
  links: string[];
}

export interface ReportDetail {
  report: Report;
  backlinks: ReportBrief[];
  outlinks: ReportBrief[];
  can_edit: boolean;
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
}

export interface ReportPatch {
  title?: string;
  body?: string;
  append?: string;
  folder?: string;
  tags?: string[];
  pinned?: boolean;
  visibility?: "private" | "public";
}

export class ReportError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new ReportError(res.status, text || res.statusText);
  }
  return res.json() as Promise<T>;
}

const send = (url: string, method: string, body?: unknown) =>
  fetch(url, { method, headers: body === undefined ? undefined : { "content-type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });

export const REPORTS_STALE = "world:reports-stale";

export const reportsApi = {
  list: (filter: { query?: string; folder?: string; tag?: string } = {}) => {
    const q = new URLSearchParams();
    for (const [k, v] of Object.entries(filter)) if (v) q.set(k, v);
    return fetch(`/api/world/reports?${q}`, { cache: "no-store" }).then((r) => json<ReportBrief[]>(r));
  },
  find: (key: string) => fetch(`/api/world/reports/find?key=${encodeURIComponent(key)}`, { cache: "no-store" }).then((r) => json<ReportBrief>(r)),
  get: (id: string) => fetch(`/api/world/reports/${id}`, { cache: "no-store" }).then((r) => json<ReportDetail>(r)),
  create: (body: { title: string; body: string; folder?: string; tags?: string[] }) => send("/api/world/reports", "POST", body).then((r) => json<Report>(r)),
  update: (id: string, patch: ReportPatch) => send(`/api/world/reports/${id}`, "PATCH", patch).then((r) => json<Report>(r)),
  remove: async (id: string) => {
    const res = await send(`/api/world/reports/${id}`, "DELETE");
    if (!res.ok) throw new ReportError(res.status, res.statusText);
  },
  revisions: (id: string) => fetch(`/api/world/reports/${id}/revisions`, { cache: "no-store" }).then((r) => json<RevisionBrief[]>(r)),
  revision: (id: string, n: number) => fetch(`/api/world/reports/${id}/revisions/${n}`, { cache: "no-store" }).then((r) => json<Revision>(r)),
  restore: (id: string, n: number) => send(`/api/world/reports/${id}/revisions/${n}/restore`, "POST").then((r) => json<Report>(r)),
};

export function openReport(id?: string | null) {
  window.dispatchEvent(new CustomEvent("world:open-report", { detail: { id: id ?? null } }));
}

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
