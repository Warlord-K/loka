import type { CanvasDoc, CanvasEdge, CanvasItem, CanvasMark, CanvasOp } from "./reports";

export const MARK_UNDO_MS = 5000;

export interface MarkDelete {
  mark: CanvasMark;
  item: CanvasItem | null;
  edges: CanvasEdge[];
  done?: boolean;
}

export function planMarkDelete(mark: CanvasMark, doc: CanvasDoc): MarkDelete {
  const host = mark.kind === "sticky" && mark.item_id ? doc.items.find((i) => i.id === mark.item_id) : undefined;
  const item = host && host.kind === "sticky" ? host : null;
  const edges = item ? doc.edges.filter((e) => e.from === item.id || e.to === item.id) : [];
  return { mark, item, edges };
}

export function hideMarkDeletes(marks: CanvasMark[], doc: CanvasDoc, pending: Iterable<MarkDelete>): { marks: CanvasMark[]; doc: CanvasDoc } {
  const markIds = new Set<string>();
  const itemIds = new Set<string>();
  for (const p of pending) {
    markIds.add(p.mark.id);
    if (p.item) itemIds.add(p.item.id);
  }
  if (markIds.size === 0) return { marks, doc };
  return {
    marks: marks.filter((m) => !markIds.has(m.id)),
    doc: itemIds.size
      ? { ...doc, items: doc.items.filter((i) => !itemIds.has(i.id)), edges: doc.edges.filter((e) => !itemIds.has(e.from) && !itemIds.has(e.to)) }
      : doc,
  };
}

export function settleMarkDelete(pending: Map<string, MarkDelete>, id: string, ok: boolean): Map<string, MarkDelete> {
  const entry = pending.get(id);
  if (!entry || (ok && entry.done)) return pending;
  const next = new Map(pending);
  if (ok) next.set(id, { ...entry, done: true });
  else next.delete(id);
  return next;
}

export function pruneMarkDeletes(pending: Map<string, MarkDelete>, marks: CanvasMark[], doc: CanvasDoc): Map<string, MarkDelete> {
  const markIds = new Set(marks.map((m) => m.id));
  const itemIds = new Set(doc.items.map((i) => i.id));
  const gone = [...pending.values()].filter((p) => p.done && !markIds.has(p.mark.id) && !(p.item && itemIds.has(p.item.id)));
  if (gone.length === 0) return pending;
  const next = new Map(pending);
  for (const p of gone) next.delete(p.mark.id);
  return next;
}

export function latestRequests() {
  let last = 0;
  return {
    start: () => ++last,
    isLatest: (n: number) => n === last,
  };
}

export function removeItemOps(p: MarkDelete): CanvasOp[] | null {
  return p.item ? [{ op: "remove", ids: [p.item.id] }] : null;
}

export function restoreItemOps(p: MarkDelete): CanvasOp[] | null {
  return p.item ? [{ op: "add", items: [p.item] }] : null;
}

export function restoreEdgeOps(p: MarkDelete, doc: CanvasDoc): CanvasOp[] | null {
  const here = new Set(doc.items.map((i) => i.id));
  const kept = p.edges.filter((e) => here.has(e.from) && here.has(e.to) && !doc.edges.some((d) => d.from === e.from && d.to === e.to));
  if (kept.length === 0) return null;
  return [{ op: "connect", edges: kept.map(({ from, to, label }) => (label === undefined ? { from, to } : { from, to, label })) }];
}

export interface MarkDeleteApi {
  removeMark: () => Promise<void>;
  changeCanvas: (ops: CanvasOp[]) => Promise<CanvasDoc | null>;
  isGone: (error: unknown) => boolean;
}

export type MarkDeleteResult = { ok: true; doc: CanvasDoc | null } | { ok: false; doc: CanvasDoc | null };

export async function commitMarkDelete(p: MarkDelete, api: MarkDeleteApi): Promise<MarkDeleteResult> {
  const remove = removeItemOps(p);
  let doc: CanvasDoc | null = null;
  if (remove) {
    try {
      doc = await api.changeCanvas(remove);
    } catch {
      return { ok: false, doc: null };
    }
  }
  try {
    await api.removeMark();
    return { ok: true, doc };
  } catch (e) {
    if (api.isGone(e)) return { ok: true, doc };
    return { ok: false, doc: await restoreMarkItem(p, api) };
  }
}

async function restoreMarkItem(p: MarkDelete, api: MarkDeleteApi): Promise<CanvasDoc | null> {
  const restore = restoreItemOps(p);
  if (!restore) return null;
  let doc: CanvasDoc | null;
  try {
    doc = await api.changeCanvas(restore);
  } catch {
    return null;
  }
  const edges = doc && restoreEdgeOps(p, doc);
  if (!edges) return doc;
  try {
    return (await api.changeCanvas(edges)) ?? doc;
  } catch {
    return doc;
  }
}

export async function commitMarkDeleteOnExit(p: MarkDelete, api: MarkDeleteApi): Promise<void> {
  const remove = removeItemOps(p);
  await Promise.allSettled([api.removeMark(), ...(remove ? [api.changeCanvas(remove)] : [])]);
}
