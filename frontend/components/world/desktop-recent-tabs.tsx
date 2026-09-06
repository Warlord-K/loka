"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import type { WorldAgent } from "@/lib/world";
import { openRecentAgentId, parseRecentAgentIds, pruneRecentAgentIds, updateRecentAgentState, type RecentAgentState } from "@/lib/recent-agent-tabs";

export function useRecentAgentTabs(userId: string | undefined, agents: WorldAgent[], rosterReady: boolean) {
  const [recent, setRecent] = useState<RecentAgentState>({ key: null, ids: [] });
  const key = userId ? `world.recentAgents.${userId}` : null;
  const visible = useMemo(() => new Set(agents.map((agent) => agent.id)), [agents]);
  const hiddenKey = userId ? `world.recentAgentsHidden.${userId}` : null;
  const [hidden, setHidden] = useState<string[]>([]);
  useEffect(() => {
    if (!hiddenKey) return setHidden([]);
    try {
      setHidden(parseRecentAgentIds(window.localStorage.getItem(hiddenKey)));
    } catch {
      setHidden([]);
    }
  }, [hiddenKey]);
  const saveHidden = useCallback(
    (update: (ids: string[]) => string[]) => {
      setHidden((current) => {
        const next = update(current);
        if (hiddenKey) {
          try {
            window.localStorage.setItem(hiddenKey, JSON.stringify(next));
          } catch {}
        }
        return next;
      });
    },
    [hiddenKey],
  );

  useEffect(() => {
    if (!key) {
      setRecent({ key: null, ids: [] });
      return;
    }
    setRecent({ key, ids: parseRecentAgentIds(window.localStorage.getItem(key)) });
  }, [key]);

  useEffect(() => {
    if (!rosterReady || recent.key !== key) return;
    setRecent((current) => {
      if (current.key !== key) return current;
      const ids = pruneRecentAgentIds(current.ids, visible);
      return ids.length === current.ids.length ? current : { ...current, ids };
    });
  }, [key, recent.key, rosterReady, visible]);

  useEffect(() => {
    if (!rosterReady || !key || recent.key !== key) return;
    try {
      window.localStorage.setItem(key, JSON.stringify(recent.ids));
    } catch {}
  }, [key, recent, rosterReady]);

  const open = useCallback((id: string) => {
    setRecent((current) => updateRecentAgentState(current, key, (ids) => openRecentAgentId(ids, id)));
    saveHidden((ids) => (ids.includes(id) ? ids.filter((item) => item !== id) : ids));
  }, [key, saveHidden]);

  const close = useCallback((id: string) => {
    setRecent((current) => updateRecentAgentState(current, key, (ids) => ids.filter((item) => item !== id)));
    saveHidden((ids) => (ids.includes(id) ? ids : [...ids, id].slice(-50)));
  }, [key, saveHidden]);

  return { ids: recent.key === key ? recent.ids : [], hidden, open, close };
}
