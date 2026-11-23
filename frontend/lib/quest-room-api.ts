import { WorldApiError } from "./world";
import type { Asking, DecisionSummary, MemberRole, MixSlot, RoomAsking, RoomMember, Runtime } from "./quest-board";

export { cleanVariants } from "./quest-board";
export type { Asking, DecisionSummary, MixSlot, RoomAsking, RoomMember, Runtime };

export const ASKING_MODES: { value: Asking; label: string; hint: string }[] = [
  { value: "off", label: "Off", hint: "Quests guess or ask you" },
  { value: "ask_agent", label: "Ask the agent", hint: "Quests ask their agent first" },
  { value: "crew", label: "Crew", hint: "Helpers split big quests" },
];

export const CREW_SIZES = [2, 3, 4] as const;
export const DEFAULT_CREW = 3;
export const MAX_CREW = CREW_SIZES[CREW_SIZES.length - 1];

export interface CrewPreview {
  defaults: Runtime[];
}

export function roomServes(room: { members?: unknown } | null | undefined): boolean {
  return Array.isArray(room?.members);
}

export function askingServed(agent: { asking?: unknown } | null | undefined): boolean {
  return typeof agent?.asking === "string";
}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const res = await fetch(path, { cache: "no-store", ...init, headers: { "content-type": "application/json", ...(init?.headers ?? {}) } });
  if (!res.ok) {
    const raw = (await res.text().catch(() => "")).trim();
    let reason = "";
    try {
      const body = JSON.parse(raw) as { error?: unknown };
      if (typeof body?.error === "string") reason = body.error;
    } catch {
      if (raw && raw.length <= 300 && !raw.startsWith("<")) reason = raw;
    }
    throw new WorldApiError(res.status, reason || (res.status === 409 ? "That was already settled." : `Could not do that (${res.status}).`));
  }
  return res;
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  return (await request(path, init)).json() as Promise<T>;
}

async function callEmpty(path: string, init?: RequestInit): Promise<void> {
  await request(path, init);
}

const json = (body: unknown): RequestInit => ({ method: "POST", body: JSON.stringify(body) });

export const roomApi = {
  crewPreview: (agentId: string, signal?: AbortSignal) => call<CrewPreview>(`/api/world/agents/${agentId}/crew-preview`, { signal }),
  members: (taskId: string) => call<{ members: RoomMember[] }>(`/api/world/tasks/${taskId}/members`).then((r) => r.members),
  addMember: (taskId: string, kind: "agent" | "person", id: string, role: MemberRole = "guest") =>
    call<RoomMember>(`/api/world/tasks/${taskId}/members`, json({ member_kind: kind, member_id: id, role })),
  removeMember: (taskId: string, kind: "agent" | "person", id: string) =>
    callEmpty(`/api/world/tasks/${taskId}/members/${kind}/${id}`, { method: "DELETE" }),
  post: (taskId: string, body: string, to?: string[]) =>
    call<{ message: string | null; asked: string[] }>(`/api/world/tasks/${taskId}/room/messages`, json({ body, resolve_mentions: false, ...(to === undefined ? {} : { to }) })),
  answer: (questionId: string, answer: string, refs: string[] = []) => call<unknown>(`/api/world/questions/${questionId}/answer`, json({ answer, refs })),
  park: (questionId: string) => call<{ todo_id: string }>(`/api/world/questions/${questionId}/park`, json({})),
};
