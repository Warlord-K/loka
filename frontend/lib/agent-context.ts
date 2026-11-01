export type ContextPending = "compact" | "reset";

export type AgentContext = {
  used_tokens: number;
  max_tokens: number | null;
  compact_supported: boolean;
  compact_unsupported_reason: string | null;
  pending: ContextPending | null;
};

export const CONTEXT_WARN_RATIO = 0.8;
export const CONTEXT_EVENT = "agent.context";

function base(agentId: string): string {
  return `/api/world/agents/${encodeURIComponent(agentId)}/context`;
}

export function readContext(value: unknown): AgentContext | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  const used = Number(v.used_tokens ?? 0);
  const max = v.max_tokens == null ? null : Number(v.max_tokens);
  if (!Number.isFinite(used) || used < 0) return null;
  return {
    used_tokens: used,
    max_tokens: max !== null && Number.isFinite(max) && max > 0 ? max : null,
    compact_supported: v.compact_supported === true,
    compact_unsupported_reason: typeof v.compact_unsupported_reason === "string" ? v.compact_unsupported_reason : null,
    pending: v.pending === "compact" || v.pending === "reset" ? v.pending : null,
  };
}

export const CONTEXT_MISSING = "missing" as const;

export async function fetchAgentContext(agentId: string): Promise<AgentContext | typeof CONTEXT_MISSING | null> {
  try {
    const res = await fetch(base(agentId), { cache: "no-store" });
    if (res.status === 404) return CONTEXT_MISSING;
    if (!res.ok) return null;
    return readContext(await res.json());
  } catch {
    return null;
  }
}

export type ContextRequest = { queued: true } | { queued: false; pending: true } | { queued: false; refused: string };

async function post(path: string): Promise<ContextRequest> {
  const res = await fetch(path, { method: "POST" });
  const body = (await res.json().catch(() => ({}))) as { queued?: unknown; reason?: unknown };
  if (res.status === 409) return { queued: false, refused: typeof body.reason === "string" ? body.reason : "Not possible right now." };
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return body.queued === false ? { queued: false, pending: true } : { queued: true };
}

export function compactAgentContext(agentId: string): Promise<ContextRequest> {
  return post(`${base(agentId)}/compact`);
}

export async function resetAgentContext(agentId: string): Promise<ContextRequest> {
  try {
    return await post(`${base(agentId)}/reset`);
  } catch (err) {
    if (!(err instanceof Error) || !/^(404|405)\b/.test(err.message)) throw err;
    return post(`/api/world/agents/${encodeURIComponent(agentId)}/reset-context`);
  }
}

export function contextRatio(ctx: AgentContext): number | null {
  if (!ctx.max_tokens) return null;
  return Math.min(1, Math.max(0, ctx.used_tokens / ctx.max_tokens));
}
