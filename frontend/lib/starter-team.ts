"use client";

export type StarterJob = "coder" | "reviewer";

export interface StarterSeat {
  seat: string;
  job: StarterJob;
  job_label: string;
  duty: string;
  name: string;
  emoji: string;
  cli: string;
  model: string;
  agent_id: string | null;
  exists: boolean;
}

export interface StarterPlan {
  coordinator: string | null;
  seats: StarterSeat[];
  team: string | null;
  rule: boolean;
  complete: boolean;
}

export interface StarterBuilt extends StarterPlan {
  created: string[];
  schedules: number;
  notes: number;
  summary: string;
}

async function send<T>(method: string, body?: unknown): Promise<T> {
  const res = await fetch("/api/world/starter-team", {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || `Request failed (${res.status})`);
  }
  return (await res.json()) as T;
}

export const starterTeamApi = {
  plan: () => send<StarterPlan>("GET"),
  build: (team: boolean) => send<StarterBuilt>("POST", { team }),
};
