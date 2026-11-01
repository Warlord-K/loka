"use client";

import { useSyncExternalStore } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { cn } from "@/lib/utils";
import type { WorldEvent } from "@/lib/world";
import { Shimmer } from "./work-ui";

export const TYPING_EVENT = "world-typing";
const TTL_MS = 180_000;

interface Typist {
  agentId: string;
  name: string;
  channelId: string | null;
  replyTo: number | null;
  until: number;
}

let typists: Typist[] = [];
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | null = null;
let attached = false;

function publish(next: Typist[]) {
  if (next === typists) return;
  typists = next;
  schedulePrune();
  listeners.forEach((l) => l());
}

function schedulePrune() {
  if (timer) clearTimeout(timer);
  timer = null;
  if (!typists.length) return;
  const soonest = Math.min(...typists.map((t) => t.until));
  timer = setTimeout(() => publish(typists.filter((t) => t.until > Date.now())), Math.max(250, soonest - Date.now()));
}

function str(v: unknown): string | null {
  return typeof v === "string" && v ? v : null;
}

function without(match: (t: Typist) => boolean) {
  if (typists.some(match)) publish(typists.filter((t) => !match(t)));
}

function onTyping(ev: WorldEvent) {
  const agentId = ev.agent_id;
  if (!agentId) return;
  const channelId = str(ev.payload.channel_id);
  const replyTo = typeof ev.payload.reply_to === "number" ? ev.payload.reply_to : null;
  const same = (t: Typist) => t.agentId === agentId && t.channelId === channelId && t.replyTo === replyTo;
  if (ev.payload.typing !== true) return without(same);
  const typist = { agentId, channelId, replyTo, name: str(ev.payload.agent) ?? "An agent", until: Date.now() + TTL_MS };
  publish([...typists.filter((t) => !same(t)), typist]);
}

function onWorld(ev: WorldEvent) {
  const agentId = ev.agent_id;
  if (ev.type === "agent.progress" && agentId && typists.some((t) => t.agentId === agentId)) {
    publish(typists.map((t) => (t.agentId === agentId ? { ...t, until: Date.now() + TTL_MS } : t)));
    return;
  }
  if (ev.type === "message.created" && agentId && ev.payload.author_kind === "agent" && !ev.payload.author_agent_id) {
    return without((t) => t.agentId === agentId && t.channelId === null);
  }
  const author = str(ev.payload.author_agent_id);
  const channelId = str(ev.payload.channel_id);
  if (ev.type === "channel.message" && author && channelId) without((t) => t.agentId === author && t.channelId === channelId);
}

function attach() {
  if (attached || typeof window === "undefined") return;
  attached = true;
  window.addEventListener(TYPING_EVENT, (e) => onTyping((e as CustomEvent<WorldEvent>).detail));
  window.addEventListener("world-event", (e) => onWorld((e as CustomEvent<WorldEvent>).detail));
}

function subscribe(listener: () => void) {
  attach();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

const snapshot = () => typists;
const empty: Typist[] = [];
const serverSnapshot = () => empty;

export function typingText(names: string[]): string {
  if (names.length === 1) return `${names[0]} is typing`;
  if (names.length === 2) return `${names[0]} and ${names[1]} are typing`;
  return `${names.length} agents are typing`;
}

export function TypingLine({ agentId, channelId, replyTo = null, className }: { agentId?: string; channelId?: string; replyTo?: number | null; className?: string }) {
  const all = useSyncExternalStore(subscribe, snapshot, serverSnapshot);
  const names = all
    .filter((t) => (channelId ? t.channelId === channelId && t.replyTo === replyTo : t.channelId === null && t.agentId === agentId))
    .map((t) => t.name);
  const text = names.length ? typingText(names) : null;
  return (
    <AnimatePresence initial={false}>
      {text && (
        <motion.p
          key="typing"
          role="status"
          aria-live="polite"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.15 }}
          className={cn("truncate px-1 pb-1 text-w-2xs", className)}
        >
          <Shimmer>{text}</Shimmer>
        </motion.p>
      )}
    </AnimatePresence>
  );
}
