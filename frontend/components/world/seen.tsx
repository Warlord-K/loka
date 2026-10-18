"use client";

import { useSyncExternalStore } from "react";
import { cn } from "@/lib/utils";
import type { ChannelMessage, WorldAgent, WorldEvent } from "@/lib/world";
import { AgentFace } from "./channel-ui";

export const SEEN_EVENT = "world-seen";

let seen = new Map<string, string[]>();
const listeners = new Set<() => void>();
let attached = false;

function onSeen(ev: WorldEvent) {
  const agentId = ev.agent_id;
  const ids = ev.payload.seen;
  if (!agentId || !Array.isArray(ids) || !ids.length) return;
  const channel = typeof ev.payload.channel_id === "string" ? ev.payload.channel_id : null;
  const next = new Map(seen);
  for (const id of ids) {
    const key = channel ? `c:${id}` : `m:${id}`;
    const had = next.get(key) ?? [];
    if (!had.includes(agentId)) next.set(key, [...had, agentId]);
  }
  seen = next;
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void) {
  if (!attached && typeof window !== "undefined") {
    attached = true;
    window.addEventListener(SEEN_EVENT, (e) => onSeen((e as CustomEvent<WorldEvent>).detail));
  }
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

const snapshot = () => seen;
const empty = new Map<string, string[]>();
const serverSnapshot = () => empty;

function useSeen() {
  return useSyncExternalStore(subscribe, snapshot, serverSnapshot);
}

export function seenText(names: string[]): string {
  if (names.length === 1) return `Seen by ${names[0]}`;
  if (names.length === 2) return `Seen by ${names[0]} and ${names[1]}`;
  return `Seen by ${names[0]}, ${names[1]} and ${names.length - 2} more`;
}

function SeenRow({ agents, className }: { agents: WorldAgent[]; className?: string }) {
  if (!agents.length) return null;
  const text = seenText(agents.map((a) => a.name));
  return (
    <p title={text} className={cn("mt-1 flex items-center justify-end gap-1.5 text-w-2xs text-w-muted", className)}>
      <span className="flex -space-x-1">
        {agents.slice(0, 5).map((a) => (
          <AgentFace key={a.id} agent={a} size={16} />
        ))}
      </span>
      <span className="truncate">{text}</span>
    </p>
  );
}

export function ChannelSeen({ messages, meId, agents, className }: { messages: ChannelMessage[]; meId: string | null; agents: WorldAgent[]; className?: string }) {
  const live = useSeen();
  const last = messages[messages.length - 1];
  if (!last || !meId || last.author_kind !== "user" || last.author_user_id !== meId) return null;
  const ids = [...new Set([...(last.seen_by ?? []), ...(live.get(`c:${last.id}`) ?? [])])];
  const readers = ids.map((id) => agents.find((a) => a.id === id)).filter((a): a is WorldAgent => !!a);
  return <SeenRow agents={readers} className={className} />;
}
