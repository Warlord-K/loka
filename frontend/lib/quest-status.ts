export type QuestStage = "needs_you" | "working" | "in_review" | "waiting" | "done" | "stuck" | "cancelled";

export interface QuestLike {
  status: string;
  plan_waiting?: boolean | null;
}

export interface SessionLike {
  status: string;
  plan_state?: string | null;
}

export const CLOSED_QUEST_STATUSES: readonly string[] = ["done", "cancelled"];

export function questOpen(quest: QuestLike): boolean {
  return !CLOSED_QUEST_STATUSES.includes(quest.status);
}

export function questNeedsYou(quest: QuestLike): boolean {
  return quest.plan_waiting === true && questOpen(quest);
}

export function sessionAwaitsPlan(session: SessionLike): boolean {
  return session.plan_state === "awaiting" && session.status !== "archived";
}

export function questStage(quest: QuestLike): QuestStage {
  if (quest.status === "done") return "done";
  if (quest.status === "cancelled") return "cancelled";
  if (questNeedsYou(quest)) return "needs_you";
  if (quest.status === "in_progress") return "working";
  if (quest.status === "review" || quest.status === "plan_review") return "in_review";
  if (quest.status === "failed") return "stuck";
  return "waiting";
}

export function questChanged(event: { type: string }): boolean {
  return event.type.startsWith("task.");
}

export const QUEST_STAGE_LABEL: Record<QuestStage, string> = {
  needs_you: "Needs you",
  working: "Working",
  in_review: "In review",
  waiting: "Waiting to start",
  done: "Done",
  stuck: "Stuck",
  cancelled: "Cancelled",
};
