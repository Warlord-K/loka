"use client";

import { cn } from "@/lib/utils";
import { residentFor, type WorldAgent } from "@/lib/world";
import { portraitUrl } from "./characters";

const PALETTE = ["#0fa3a3", "#e0663d", "#7b5cc4", "#3f8f4f", "#c99a1a", "#c4507a", "#3a6fb5", "#8a5a3b"];

function seeded(id: string): string {
  let h = 0;
  for (const ch of id) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return PALETTE[h % PALETTE.length];
}

function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return "?";
  return (parts.length === 1 ? parts[0].slice(0, 2) : `${parts[0][0]}${parts[parts.length - 1][0]}`).toUpperCase();
}

export function PersonAvatar({ id, name, size = 36, soft = false, className }: { id: string; name: string; size?: number; soft?: boolean; className?: string }) {
  return (
    <span
      aria-hidden
      style={{ width: size, height: size, backgroundColor: seeded(id), fontSize: Math.round(size * 0.38) }}
      className={cn("inline-flex shrink-0 select-none items-center justify-center font-bold text-white", soft ? "rounded-md" : "rounded-lg border-2 border-w-ink", className)}
    >
      {initials(name)}
    </span>
  );
}

const portraitCache = new Map<string, string>();

export function portraitOf(agent: WorldAgent, scale = 3): string | null {
  if (typeof document === "undefined") return null;
  const look = residentFor(agent).look;
  const key = `${scale}:${JSON.stringify(look)}`;
  let url = portraitCache.get(key);
  if (!url) {
    url = portraitUrl(look, scale);
    portraitCache.set(key, url);
  }
  return url;
}

type Size = number | string;

const SPRITE_RATIO = 14 / 17;

function length(size: Size): string {
  return typeof size === "number" ? `${size}px` : size;
}

function sharpScale(size: Size): number {
  return typeof size === "number" && size <= 40 ? 3 : 6;
}

export function AgentFace({ agent, size = 22, soft = true, tall, rounded, className }: { agent: WorldAgent; size?: Size; soft?: boolean; tall?: boolean; rounded?: string; className?: string }) {
  return <AgentAvatar portrait={portraitOf(agent, sharpScale(size))} emoji={agent.avatar_emoji} size={size} soft={soft} tall={tall} rounded={rounded} className={className} />;
}

export function AgentAvatar({
  portrait,
  emoji,
  size = 36,
  soft = false,
  tall = false,
  rounded,
  className,
}: {
  portrait?: string | null;
  emoji?: string | null;
  size?: Size;
  soft?: boolean;
  tall?: boolean;
  rounded?: string;
  className?: string;
}) {
  const height = length(size);
  const width = tall ? `calc(${height} * ${SPRITE_RATIO.toFixed(4)})` : height;
  return (
    <span
      aria-hidden
      style={{ width, height }}
      className={cn(
        "inline-flex shrink-0 items-end justify-center overflow-hidden bg-w-mint",
        rounded ?? (soft ? "rounded-md" : "rounded-lg"),
        soft ? "ring-1 ring-inset ring-w-line2" : "border-2 border-w-ink",
        className,
      )}
    >
      {portrait ? (
        <img src={portrait} alt="" className="block h-full w-auto max-w-none [image-rendering:pixelated]" />
      ) : (
        <span className="self-center" style={{ fontSize: `calc(${height} * 0.5)` }}>
          {emoji ?? "🤖"}
        </span>
      )}
    </span>
  );
}

export const GROUP_GAP_MS = 5 * 60 * 1000;

export function startsGroup(
  current: { author_kind: string; author_user_id: string | null; author_agent_id: string | null; created_at: string },
  previous?: { author_kind: string; author_user_id: string | null; author_agent_id: string | null; created_at: string },
): boolean {
  if (!previous) return true;
  if (previous.author_kind !== current.author_kind) return true;
  if ((previous.author_user_id ?? previous.author_agent_id) !== (current.author_user_id ?? current.author_agent_id)) return true;
  if (new Date(previous.created_at).toDateString() !== new Date(current.created_at).toDateString()) return true;
  return new Date(current.created_at).getTime() - new Date(previous.created_at).getTime() > GROUP_GAP_MS;
}
