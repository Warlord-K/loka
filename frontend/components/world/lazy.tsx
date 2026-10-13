"use client";

import dynamic from "next/dynamic";
import { cn } from "@/lib/utils";
import { PANEL } from "./styles";
import { Shimmer } from "./work-ui";

function PanelLoading() {
  return (
    <div aria-busy="true" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <div className="h-[4.25rem] shrink-0 border-b-2 border-dashed border-w-line2" />
      <div className="grid flex-1 place-items-center text-w-sm text-w-muted">
        <Shimmer>Opening</Shimmer>
      </div>
    </div>
  );
}

const loadUiCard = () => import("./ui-card");
const loadTerminal = () => import("./terminal");
const loadReports = () => import("./reports-ui");
const loadQuestRoom = () => import("./quest-room");
const loadYou = () => import("./you-panel");
const loadChangelog = () => import("./changelog-ui");
const loadFolderWorkshop = () => import("./folder-workshop");
const loadCreateAgent = () => import("./create-agent-dialog");

export const UiCard = dynamic(() => loadUiCard().then((m) => m.UiCard), { ssr: false });
export const TerminalPanel = dynamic(() => loadTerminal().then((m) => m.TerminalPanel), { ssr: false, loading: PanelLoading });
export const ReportsPanel = dynamic(() => loadReports().then((m) => m.ReportsPanel), { ssr: false, loading: PanelLoading });
export const QuestRoom = dynamic(() => loadQuestRoom().then((m) => m.QuestRoom), { ssr: false, loading: PanelLoading });
export const YouPanel = dynamic(() => loadYou().then((m) => m.YouPanel), { ssr: false, loading: PanelLoading });
export const ChangelogPanel = dynamic(() => loadChangelog().then((m) => m.ChangelogPanel), { ssr: false });
export const FolderWorkshop = dynamic(() => loadFolderWorkshop().then((m) => m.FolderWorkshop), { ssr: false, loading: PanelLoading });
export const CreateAgentDialog = dynamic(() => loadCreateAgent().then((m) => m.CreateAgentDialog), { ssr: false });

const LOADERS = [loadUiCard, loadReports, loadQuestRoom, loadYou, loadCreateAgent, loadChangelog, loadFolderWorkshop, loadTerminal];

let preloaded = false;

export function preloadPanels(): () => void {
  if (preloaded || typeof window === "undefined") return () => {};
  const queue = [...LOADERS];
  let handle: number | null = null;
  let stopped = false;
  const hasIdle = typeof window.requestIdleCallback === "function";
  const idle = (fn: () => void) => (hasIdle ? window.requestIdleCallback(fn, { timeout: 4000 }) : window.setTimeout(fn, 1500));
  const cancel = (id: number) => (hasIdle ? window.cancelIdleCallback(id) : window.clearTimeout(id));
  const next = () => {
    const load = queue.shift();
    if (!load) {
      preloaded = true;
      handle = null;
      return;
    }
    void load()
      .catch(() => undefined)
      .finally(() => {
        if (!stopped) handle = idle(next);
      });
  };
  handle = idle(next);
  return () => {
    stopped = true;
    if (handle !== null) cancel(handle);
  };
}
