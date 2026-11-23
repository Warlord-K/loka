import { lokaApi } from "@/components/world/loka-client";
import { resolveMessage } from "@/components/world/message-links";
import { reportsApi } from "./reports";
import { practiceApi } from "./practice";
import { teamsApi } from "./teams";
import { worldApi } from "./world";
import type { GuardedDetailIntent } from "./exact-world-navigation";

export type DetailIntent = GuardedDetailIntent;

export async function readNavigationDetail<T>(intent: DetailIntent, load: () => Promise<T>): Promise<T | undefined> {
  const result = await intent.read(load);
  if (result.status === "error") throw result.error;
  return result.status === "ready" ? result.value : undefined;
}

export const navigationDetails = {
  agent: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => worldApi.agent(id)),
  team: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => teamsApi.get(id)),
  item: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => worldApi.item(id)),
  region: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => lokaApi.region(id)),
  quest: (intent: DetailIntent, id: string, after?: number, since?: string, focus?: string) => readNavigationDetail(intent, () => worldApi.questRoom(id, after, since, focus)),
  reportFind: (intent: DetailIntent, title: string) => readNavigationDetail(intent, () => reportsApi.find(title)),
  report: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => reportsApi.get(id)),
  reportRevisions: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => reportsApi.revisions(id)),
  reportRevision: (intent: DetailIntent, id: string, revision: number) => readNavigationDetail(intent, () => reportsApi.revision(id, revision)),
  practiceAttempt: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => practiceApi.attempt(id)),
  todo: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => worldApi.todo(id)),
  channel: (intent: DetailIntent, id: string) => readNavigationDetail(intent, () => worldApi.channel(id)),
  channelReplies: (intent: DetailIntent, id: string, messageId: number) => readNavigationDetail(intent, () => worldApi.channelReplies(id, messageId)),
  messageReplies: (intent: DetailIntent, id: string, messageId: string) => readNavigationDetail(intent, () => worldApi.messageReplies(id, messageId)),
  message: (intent: DetailIntent, ref: string) => readNavigationDetail(intent, () => resolveMessage(ref, true)),
};

export const navigationCollections = {
  people: (intent: DetailIntent) => readNavigationDetail(intent, () => worldApi.people()),
  channels: (intent: DetailIntent) => readNavigationDetail(intent, () => worldApi.channels()),
};
