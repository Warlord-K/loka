"use client";

import { useEffect, useRef, useState } from "react";
import { questChanged } from "@/lib/quest-status";
import { worldApi, type TaskBrief, type WorldEvent } from "@/lib/world";

export const QUEST_REFRESH_MS = 700;

const listeners = new Set<() => void>();
let wired = false;
let due: number | null = null;
let quests: Promise<TaskBrief[]> | null = null;
let last: TaskBrief[] | undefined;

function wire() {
  if (wired || typeof window === "undefined") return;
  wired = true;
  const stale = () => {
    if (due !== null) return;
    due = window.setTimeout(() => {
      due = null;
      quests = null;
      listeners.forEach((listener) => listener());
    }, QUEST_REFRESH_MS);
  };
  window.addEventListener("world-event", (e) => {
    if (questChanged((e as CustomEvent<WorldEvent>).detail)) stale();
  });
  window.addEventListener("world-reconnected", stale);
}

function loadQuests(): Promise<TaskBrief[]> {
  quests ??= worldApi
    .tasks()
    .then((list) => {
      last = list;
      return list;
    })
    .catch(() => {
      quests = null;
      return last ?? [];
    });
  return quests;
}

export function useQuestsChanged(onChange: () => void) {
  const latest = useRef(onChange);
  latest.current = onChange;
  useEffect(() => {
    const listener = () => latest.current();
    wire();
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  }, []);
}

export function useLiveQuests(): TaskBrief[] | undefined {
  const [list, setList] = useState(() => last);
  const read = useRef(() => {});
  useEffect(() => {
    let live = true;
    read.current = () => void loadQuests().then((value) => live && setList(value));
    read.current();
    return () => {
      live = false;
    };
  }, []);
  useQuestsChanged(() => read.current());
  return list;
}
