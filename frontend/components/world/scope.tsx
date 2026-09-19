"use client";

import { useSyncExternalStore } from "react";
import { UsersThree } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { TaskBrief, WorldAgent, WorldItem } from "@/lib/world";

const KEY = "world.everyone";
const listeners = new Set<() => void>();
let everyone: boolean | null = null;

function read(): boolean {
  if (everyone === null) {
    try {
      everyone = window.localStorage.getItem(KEY) === "1";
    } catch {
      everyone = false;
    }
  }
  return everyone;
}

export function everyoneNow(): boolean {
  return typeof window === "undefined" ? false : read();
}

export function setEveryone(next: boolean) {
  everyone = next;
  try {
    window.localStorage.setItem(KEY, next ? "1" : "0");
  } catch {}
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useEveryone(): boolean {
  return useSyncExternalStore(subscribe, read, () => false);
}

export function ownsAgent(agent: Pick<WorldAgent, "owner_user_id"> | undefined, meId: string | null | undefined): boolean {
  return !agent || !meId || !agent.owner_user_id || agent.owner_user_id === meId;
}

export function ownsItem(item: Pick<WorldItem, "owner_user_id">, meId: string | null | undefined): boolean {
  return !meId || !item.owner_user_id || item.owner_user_id === meId;
}

export function ownsQuest(task: TaskBrief, byId: Map<string, WorldAgent>, meId: string | null | undefined): boolean {
  if (!meId) return true;
  if (task.user_id === meId || task.assignee_user_id === meId) return true;
  const agent = task.assigned_agent_id ? byId.get(task.assigned_agent_id) : undefined;
  if (agent) return ownsAgent(agent, meId);
  return !task.user_id && !task.assignee_user_id;
}

export function ScopeToggle({ others, className }: { others: number; className?: string }) {
  const all = useEveryone();
  if (others === 0 && !all) return null;
  return (
    <button
      type="button"
      aria-pressed={all}
      onClick={() => setEveryone(!all)}
      title={all ? "Showing everyone's" : "Showing only yours"}
      className={cn(
        "inline-flex shrink-0 items-center gap-1.5 rounded-lg border-2 px-2.5 py-1 text-w-xs",
        all ? "border-w-ink bg-w-invert text-w-oninvert" : "border-w-line2 bg-w-card text-w-text2 hover:bg-w-sand",
        className,
      )}
    >
      <UsersThree size={14} weight="bold" />
      {all ? "Everyone's" : `Only mine${others ? ` · ${others} hidden` : ""}`}
    </button>
  );
}
