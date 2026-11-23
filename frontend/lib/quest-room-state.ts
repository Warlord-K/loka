export type QuestCardIdentity = { taskId: string; cardId: string };
export type HeldQuestCard<T> = { taskId: string; card: T };

export type QuestSendSnapshot = {
  taskId: string;
  draft: string;
  attachmentKeys: string[];
  interrupt: boolean;
};

export function questCardKey(identity: QuestCardIdentity): string {
  return `${identity.taskId}:${identity.cardId}`;
}

export function visibleQuestCard<T extends { id: string }>(activeTaskId: string, pageTaskId: string, incoming: T | null, current: T | null, settled: Set<string>): T | null {
  if (activeTaskId !== pageTaskId) return current;
  if (!incoming) return null;
  return settled.has(questCardKey({ taskId: activeTaskId, cardId: incoming.id })) ? current : incoming;
}

export function retainedQuestCard<T extends { id: string }>(activeTaskId: string, pageTaskId: string, incoming: T | null, current: HeldQuestCard<T> | null, settled: Set<string>): HeldQuestCard<T> | null {
  if (activeTaskId !== pageTaskId) return current;
  if (!incoming || settled.has(questCardKey({ taskId: activeTaskId, cardId: incoming.id }))) {
    return current?.taskId === activeTaskId ? current : null;
  }
  return { taskId: activeTaskId, card: incoming };
}

export function cardAfterSettlement(current: QuestCardIdentity | null, settled: QuestCardIdentity): QuestCardIdentity | null {
  return current && questCardKey(current) === questCardKey(settled) ? null : current;
}

export function draftAfterQuestSend(snapshot: QuestSendSnapshot, activeTaskId: string, currentDraft: string): string {
  return snapshot.taskId === activeTaskId && snapshot.draft === currentDraft ? "" : currentDraft;
}

export function attachmentKeysAfterQuestSend(snapshot: QuestSendSnapshot, activeTaskId: string): string[] {
  return snapshot.taskId === activeTaskId ? snapshot.attachmentKeys : [];
}

export function interruptAfterQuestSend(snapshot: QuestSendSnapshot, activeTaskId: string, current: boolean): boolean {
  return snapshot.taskId === activeTaskId && snapshot.interrupt === current ? false : current;
}
