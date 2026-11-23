"use client";

import { createContext, createElement, useContext, useEffect, useMemo, useSyncExternalStore, type ReactNode } from "react";
import { questChanged } from "@/lib/quest-status";
import { createQuestStore, type QuestStore } from "@/lib/live-quests";
import { worldApi, type TaskBrief, type WorldEvent } from "@/lib/world";

export { QUEST_REFRESH_MS } from "@/lib/live-quests";

const emptyStore: QuestStore<TaskBrief> = {
  load: async () => [],
  invalidate: () => {},
  snapshot: () => undefined,
  subscribe: () => () => {},
  update: () => {},
  dispose: () => {},
};

const LiveQuestsContext = createContext<QuestStore<TaskBrief>>(emptyStore);

export function LiveQuestsProvider({ identity, children }: { identity: string | null; children: ReactNode }) {
  const store = useMemo(() => (identity ? createQuestStore(() => worldApi.tasks(), setTimeout, clearTimeout, false) : emptyStore), [identity]);
  useEffect(() => {
    if (store === emptyStore) return;
    const onEvent = (event: Event) => {
      if (questChanged((event as CustomEvent<WorldEvent>).detail)) store.invalidate();
    };
    const onReconnect = () => store.invalidate();
    window.addEventListener("world-event", onEvent);
    window.addEventListener("world-reconnected", onReconnect);
    return () => {
      window.removeEventListener("world-event", onEvent);
      window.removeEventListener("world-reconnected", onReconnect);
    };
  }, [store]);
  return createElement(LiveQuestsContext.Provider, { value: store }, children);
}

export function useLiveQuests(enabled = true): TaskBrief[] | undefined {
  const available = useContext(LiveQuestsContext);
  const store = enabled ? available : emptyStore;
  const list = useSyncExternalStore(store.subscribe, store.snapshot, store.snapshot);
  useEffect(() => {
    void store.load();
  }, [store]);
  return list;
}

export function useSetLiveQuests(enabled = true): (change: (current: TaskBrief[]) => TaskBrief[]) => void {
  const available = useContext(LiveQuestsContext);
  return (enabled ? available : emptyStore).update;
}
