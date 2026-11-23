export type EmbedKind = "terminal" | "quest" | "browser";

export type CanvasEmbed = { kind: EmbedKind; target: string };

export type EmbedSpec = { label: string; pick: string; size: { w: number; h: number }; placeable: boolean; keeps: boolean };

export const EMBEDS: Record<EmbedKind, EmbedSpec> = {
  terminal: { label: "Terminal", pick: "A live shell on one of your devices", size: { w: 560, h: 340 }, placeable: true, keeps: true },
  quest: { label: "Running quest", pick: "A quest's checklist and progress, live", size: { w: 380, h: 300 }, placeable: true, keeps: false },
  browser: { label: "Agent browser", pick: "What an agent's browser shows", size: { w: 640, h: 420 }, placeable: false, keeps: false },
};

export const EMBED_KINDS = Object.keys(EMBEDS) as EmbedKind[];

export const PLACEABLE_EMBEDS = EMBED_KINDS.filter((kind) => EMBEDS[kind].placeable);

export const EMBED_DRAG = "application/x-loka-embed";

export function readEmbed(raw: unknown): CanvasEmbed | null {
  if (!raw || typeof raw !== "object") return null;
  const { kind, target } = raw as { kind?: unknown; target?: unknown };
  if (typeof kind !== "string" || typeof target !== "string" || !target.trim()) return null;
  return EMBED_KINDS.includes(kind as EmbedKind) ? { kind: kind as EmbedKind, target: target.trim() } : null;
}

export function terminalTarget(device: string, shell: string | null = null): string {
  return shell ? `${device}/${shell}` : device;
}

export function parseTerminalTarget(target: string): { device: string; shell: string | null } | null {
  const [device, shell, extra] = target.split("/");
  if (extra !== undefined || !/^[0-9a-f-]{32,36}$/i.test(device ?? "")) return null;
  if (shell !== undefined && !/^[A-Za-z0-9-]{1,64}$/.test(shell)) return null;
  return { device, shell: shell ?? null };
}

export type EmbedSight = { mounted: boolean; onScreen: boolean; pageVisible: boolean };

export function embedLive(sight: EmbedSight): boolean {
  return sight.mounted && sight.onScreen && sight.pageVisible;
}

export type BodyMode = "kept" | "shown" | "paused";

export function bodyMode(kind: EmbedKind, live: boolean): BodyMode {
  if (EMBEDS[kind].keeps) return "kept";
  return live ? "shown" : "paused";
}

export type StageEntry = { key: string; kind: EmbedKind };

export function stagePlan(started: ReadonlySet<string>, present: readonly StageEntry[]): { keep: string[]; gone: string[] } {
  const kept = new Set(present.filter((e) => EMBEDS[e.kind].keeps).map((e) => e.key));
  return { keep: [...started].filter((key) => kept.has(key)), gone: [...started].filter((key) => !kept.has(key)) };
}

export function startsShellBySelf(by: string | undefined): boolean {
  return by === "person";
}

export function orphanKeys(stored: readonly string[], prefix: string, present: ReadonlySet<string>): string[] {
  return stored.filter((key) => key.startsWith(prefix) && !present.has(key.slice(prefix.length)));
}

export const MIN_SCREEN_SCALE = 0.25;
export const MAX_SCREEN_SCALE = 4;

export function screenScale(zoom: number): number {
  if (!Number.isFinite(zoom) || zoom <= 0) return 1;
  return Math.min(MAX_SCREEN_SCALE, Math.max(MIN_SCREEN_SCALE, zoom));
}

export type EmbedRun = { live: boolean; round: number };

export function nextRun(prev: EmbedRun, live: boolean): EmbedRun {
  if (live === prev.live) return prev;
  return { live, round: live ? prev.round + 1 : prev.round };
}

export type Box = { x: number; y: number; w: number; h: number };

export function embedBox(kind: EmbedKind, center: { x: number; y: number }, index = 0): Box {
  const { w, h } = EMBEDS[kind].size;
  return { x: Math.round(center.x - w / 2 + index * (w + 32)), y: Math.round(center.y - h / 2), w, h };
}

export type EmbedDrop = { embed: CanvasEmbed; caption?: string };

export function embedDragData(embed: CanvasEmbed, caption?: string): string {
  return JSON.stringify(caption ? { embed, caption: caption.slice(0, 400) } : { embed });
}

export function readEmbedDrag(data: string | null | undefined): EmbedDrop | null {
  if (!data) return null;
  try {
    const parsed = JSON.parse(data) as { embed?: unknown; caption?: unknown };
    const embed = readEmbed(parsed.embed);
    if (!embed || !EMBEDS[embed.kind].placeable) return null;
    return typeof parsed.caption === "string" && parsed.caption.trim() ? { embed, caption: parsed.caption.trim().slice(0, 400) } : { embed };
  } catch {
    return null;
  }
}

export function embedItem(drop: EmbedDrop, center: { x: number; y: number }, index = 0) {
  return { kind: "embed" as const, embed: drop.embed, ...(drop.caption ? { caption: drop.caption } : {}), ...embedBox(drop.embed.kind, center, index) };
}

export type StepLike = { title: string; status: string; position?: number };

export function currentStep<T extends StepLike>(steps: readonly T[]): T | null {
  const ordered = [...steps].sort((a, b) => (a.position ?? 0) - (b.position ?? 0));
  return ordered.find((s) => s.status === "active") ?? ordered.find((s) => s.status === "pending") ?? null;
}

export type ProgressEvent = { type: string; run_id?: string | null; payload?: Record<string, unknown> | null };

export function runProgress(event: ProgressEvent, runId: string | null): string | null {
  if (event.type !== "agent.progress" || !runId) return null;
  const run = event.run_id ?? (typeof event.payload?.run_id === "string" ? event.payload.run_id : null);
  const text = event.payload?.text;
  return run === runId && typeof text === "string" && text.trim() ? text : null;
}

export const LOG_TAIL = 4;

export function pushTail(tail: readonly string[], line: string, max = LOG_TAIL): string[] {
  const clean = line.replace(/\s+/g, " ").trim().slice(0, 200);
  if (!clean || tail[tail.length - 1] === clean) return tail as string[];
  return [...tail, clean].slice(-max);
}

export interface EmbedNodeLike {
  parentElement: unknown;
  remove(): void;
}

export interface EmbedPlace<N> {
  appendChild(node: N): unknown;
}

export interface EmbedSlots<N> {
  node(key: string): N;
  attach(key: string, here: EmbedPlace<N>): () => void;
  drop(key: string): void;
  clear(): void;
}

export function embedSlots<N extends EmbedNodeLike>(make: () => N, park: (node: N) => void): EmbedSlots<N> {
  const nodes = new Map<string, N>();
  const node = (key: string) => {
    const known = nodes.get(key);
    if (known) return known;
    const made = make();
    park(made);
    nodes.set(key, made);
    return made;
  };
  return {
    node,
    attach(key, here) {
      const made = node(key);
      if (made.parentElement !== here) here.appendChild(made);
      return () => {
        if (made.parentElement === here) park(made);
      };
    },
    drop(key) {
      nodes.get(key)?.remove();
      nodes.delete(key);
    },
    clear() {
      nodes.forEach((made) => made.remove());
      nodes.clear();
    },
  };
}
