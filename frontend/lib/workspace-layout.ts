export interface WorkspaceLayout {
  workspace: boolean;
  channels: boolean;
  chat: boolean;
  channel: string | null;
}

export interface WorkspaceStore {
  last: string | null;
  canvases: Record<string, WorkspaceLayout & { at: number }>;
}

export const DEFAULT_LAYOUT: WorkspaceLayout = { workspace: false, channels: true, chat: true, channel: null };

export const EMPTY_STORE: WorkspaceStore = { last: null, canvases: {} };

export const MAX_CANVASES = 60;

export const OPEN_WORKSPACE = "world:open-workspace";

const ID = /^[A-Za-z0-9-]{6,64}$/;

const id = (value: unknown): string | null => (typeof value === "string" && ID.test(value) ? value : null);

export function layoutKey(userId: string | null): string {
  return `world.workspace.${userId ?? "anon"}`;
}

function layoutOf(value: unknown): WorkspaceLayout & { at: number } {
  const fields = value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
  return {
    workspace: typeof fields.workspace === "boolean" ? fields.workspace : DEFAULT_LAYOUT.workspace,
    channels: typeof fields.channels === "boolean" ? fields.channels : DEFAULT_LAYOUT.channels,
    chat: typeof fields.chat === "boolean" ? fields.chat : DEFAULT_LAYOUT.chat,
    channel: id(fields.channel),
    at: typeof fields.at === "number" && Number.isFinite(fields.at) ? fields.at : 0,
  };
}

function capped(canvases: WorkspaceStore["canvases"]): WorkspaceStore["canvases"] {
  const kept = Object.entries(canvases).sort((a, b) => b[1].at - a[1].at).slice(0, MAX_CANVASES);
  return Object.fromEntries(kept);
}

export function readStore(raw: string | null): WorkspaceStore {
  if (!raw) return EMPTY_STORE;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return EMPTY_STORE;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return EMPTY_STORE;
  const fields = parsed as Record<string, unknown>;
  const last = id(fields.last);
  if (!fields.canvases || typeof fields.canvases !== "object" || Array.isArray(fields.canvases)) {
    return last ? { last, canvases: { [last]: { ...layoutOf(fields), workspace: true } } } : EMPTY_STORE;
  }
  const canvases: WorkspaceStore["canvases"] = {};
  for (const [key, value] of Object.entries(fields.canvases as Record<string, unknown>)) {
    if (id(key)) canvases[key] = layoutOf(value);
  }
  return { last: last && canvases[last]?.workspace ? last : null, canvases: capped(canvases) };
}

export function layoutFor(store: WorkspaceStore, canvas: string | null | undefined): WorkspaceLayout {
  const saved = canvas ? store.canvases[canvas] : undefined;
  return saved ? { workspace: saved.workspace, channels: saved.channels, chat: saved.chat, channel: saved.channel } : DEFAULT_LAYOUT;
}

export function remember(store: WorkspaceStore, canvas: string | null | undefined, change: Partial<WorkspaceLayout>, now: number): WorkspaceStore {
  const key = id(canvas);
  if (!key) return store;
  const next = layoutOf({ ...layoutFor(store, key), ...change, at: now });
  const canvases = capped({ ...store.canvases, [key]: next });
  const last = next.workspace ? key : store.last === key ? null : store.last;
  return { last: last && canvases[last] ? last : null, canvases };
}
