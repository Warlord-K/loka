export interface RecentAgentState {
  key: string | null;
  ids: string[];
}

export function parseRecentAgentIds(value: string | null): string[] {
  try {
    const parsed = JSON.parse(value ?? "[]") as unknown;
    return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
  } catch {
    return [];
  }
}

export function updateRecentAgentState(
  state: RecentAgentState,
  key: string | null,
  update: (ids: string[]) => string[],
): RecentAgentState {
  return state.key === key ? { ...state, ids: update(state.ids) } : state;
}

export function pruneRecentAgentIds(ids: string[], visible: Set<string>): string[] {
  return ids.filter((id) => visible.has(id));
}

export function openRecentAgentId(ids: string[], id: string): string[] {
  return ids.includes(id) ? ids : [...ids, id].slice(-9);
}
