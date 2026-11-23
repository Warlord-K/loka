"use client";

import { useCallback, useEffect, useState } from "react";
import { latestOnly } from "@/lib/latest";

export interface TeamMember {
  team_id: string;
  member_kind: "agent" | "user";
  member_id: string;
  name: string;
  slug: string | null;
  emoji: string | null;
}

export interface TeamQuest {
  id: string;
  title: string;
  status: string;
  assigned_agent_id: string | null;
  pr_url: string | null;
  updated_at: string | null;
  parent_task_id: string | null;
}

export interface Team {
  id: string;
  owner_user_id: string;
  name: string;
  slug: string;
  purpose: string;
  rules: string;
  lead_agent_id: string | null;
  channel_id: string | null;
  channel_name: string | null;
  created_at: string;
  updated_at: string;
  members: TeamMember[];
  quests: TeamQuest[];
  open_quests: number;
  cost_today_usd: number;
  cost_week_usd: number;
}

export interface TeamDraft {
  name: string;
  purpose: string;
  rules: string;
  lead_agent_id: string | null;
  agent_ids: string[];
  user_ids: string[];
  channel?: string;
}

export const TEAMS_CHANGED = "world:teams-changed";

export class TeamRequestError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
  }
}

async function send<T>(url: string, method: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    cache: method === "GET" ? "no-store" : undefined,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new TeamRequestError(text || `Request failed (${res.status})`, res.status);
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

function changed() {
  window.dispatchEvent(new CustomEvent(TEAMS_CHANGED));
}

export const teamsApi = {
  list: () => send<Team[]>("/api/world/teams", "GET"),
  get: (id: string) => send<Team>(`/api/world/teams/${id}`, "GET"),
  create: async (draft: TeamDraft) => {
    const team = await send<Team>("/api/world/teams", "POST", draft);
    changed();
    return team;
  },
  update: async (id: string, draft: TeamDraft, previous: Team) => {
    const beforeAgents = new Set(previous.members.filter((m) => m.member_kind === "agent").map((m) => m.member_id));
    const beforePeople = new Set(previous.members.filter((m) => m.member_kind === "user").map((m) => m.member_id));
    const afterAgents = new Set(draft.agent_ids);
    const afterPeople = new Set(draft.user_ids);
    const removedAgents = [...beforeAgents].filter((member) => !afterAgents.has(member));
    const removedPeople = [...beforePeople].filter((member) => !afterPeople.has(member));
    const team = await send<Team>(`/api/world/teams/${id}`, "PATCH", {
      name: draft.name === previous.name ? undefined : draft.name,
      purpose: draft.purpose === previous.purpose ? undefined : draft.purpose,
      rules: draft.rules === previous.rules ? undefined : draft.rules,
      lead: draft.lead_agent_id === previous.lead_agent_id ? undefined : draft.lead_agent_id,
      add_agent_ids: [...afterAgents].filter((member) => !beforeAgents.has(member)),
      remove_agent_ids: removedAgents,
      add_user_ids: [...afterPeople].filter((member) => !beforePeople.has(member)),
      remove_user_ids: removedPeople,
    });
    changed();
    return team;
  },
  remove: async (id: string) => {
    await send(`/api/world/teams/${id}`, "DELETE");
    changed();
  },
  createQuest: async (teamId: string, title: string, description: string) => {
    const task = await send<{ id: string }>("/api/tasks", "POST", { title, description, team_id: teamId, kind: "doc" });
    changed();
    return task;
  },
};

export function useTeams(enabled = true) {
  const [teams, setTeams] = useState<Team[]>([]);
  const [teamLists] = useState(latestOnly);
  const load = useCallback(() => {
    if (!enabled) return;
    const isLatest = teamLists.begin();
    teamsApi
      .list()
      .then((list) => {
        if (isLatest()) setTeams(list);
      })
      .catch(() => {});
  }, [enabled, teamLists]);
  useEffect(() => {
    load();
    const onEvent = (e: Event) => {
      const type = (e as CustomEvent<{ type?: string; task_id?: string | null }>).detail?.type ?? "";
      if (type.startsWith("team.") || type === "task.finished" || type === "task.assigned" || type === "task.merged" || type === "task.cancelled") load();
    };
    window.addEventListener("world-event", onEvent);
    window.addEventListener(TEAMS_CHANGED, load);
    window.addEventListener("world-reconnected", load);
    return () => {
      teamLists.begin();
      window.removeEventListener("world-reconnected", load);
      window.removeEventListener("world-event", onEvent);
      window.removeEventListener(TEAMS_CHANGED, load);
    };
  }, [load, teamLists]);
  return { teams, reload: load };
}
