"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { Check, ClockCounterClockwise, Desktop, FolderOpen, FolderPlus, Hammer, Info, Trash, UsersThree, WifiSlash } from "@phosphor-icons/react";
import { AnimatePresence } from "framer-motion";
import { escapeTaken } from "@/lib/escape";
import { cn } from "@/lib/utils";
import { desktopApi, isDesktopApp, type SharedFolder } from "@/lib/desktop";
import { worldApi, type RunnerView, type SessionBrief, type WorldAgent } from "@/lib/world";
import { BTN_PRIMARY, BTN_SECONDARY, FOCUS, ITEM_CARD, PANEL } from "./styles";
import { AgentFace } from "./channel-ui";
import { requestWorkshop, workbenchSupport } from "./workbench/bridge";
import { Shimmer } from "./work-ui";
import { EmptyState, WindowHeader } from "./settings-shell";


function StepTitle({ step, title, description }: { step?: number; title: string; description?: string }) {
  return (
    <div className="mb-3 flex items-start gap-3">
      {step !== undefined && (
        <span className="mt-px grid h-6 w-6 shrink-0 place-items-center rounded-full bg-w-sand text-w-2xs font-bold tabular-nums text-w-text2">{step}</span>
      )}
      <div className="min-w-0">
        <h3 className="text-w-md font-semibold text-w-strong">{title}</h3>
        {description && <p className="mt-0.5 text-w-sm text-w-muted">{description}</p>}
      </div>
    </div>
  );
}
import { runtimeName } from "@/lib/model-names";

const LAST_KEY = "world.folderWorkshop";

interface Last {
  agent?: string;
  folder?: string;
}

function readLast(): Last {
  try {
    return JSON.parse(window.localStorage.getItem(LAST_KEY) ?? "{}") as Last;
  } catch {
    return {};
  }
}

function saveLast(last: Last) {
  try {
    window.localStorage.setItem(LAST_KEY, JSON.stringify(last));
  } catch {}
}

function onMac(runner: RunnerView | undefined): boolean {
  return !!runner && (runner.platform === "macos" || runner.capabilities?.platform === "macos");
}

interface PlacedFolder extends SharedFolder {
  runner: RunnerView;
}

function runnerFolders(runner: RunnerView | undefined): SharedFolder[] {
  const list = runner?.capabilities?.folders;
  return Array.isArray(list) ? (list as SharedFolder[]).filter((f) => typeof f?.id === "string") : [];
}

function ago(iso: string): string {
  const mins = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 60000));
  if (mins < 60) return mins <= 1 ? "just now" : `${mins} min ago`;
  const hours = Math.round(mins / 60);
  return hours < 24 ? `${hours} h ago` : `${Math.round(hours / 24)} d ago`;
}

export function FolderWorkshop({
  agents,
  meId,
  onClose,
  onHistory,
}: {
  agents: WorldAgent[];
  meId: string | null;
  onClose: () => void;
  onHistory: () => void;
}) {
  const [runners, setRunners] = useState<RunnerView[] | null>(null);
  const [local, setLocal] = useState<{ runner: string | null; folders: SharedFolder[] } | null>(null);
  const [recent, setRecent] = useState<SessionBrief[]>([]);
  const [agentId, setAgentId] = useState<string | null>(null);
  const [folderId, setFolderId] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [needsUpdate, setNeedsUpdate] = useState(false);
  const [dropped, setDropped] = useState<Set<string>>(() => new Set());
  const desktop = isDesktopApp();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !escapeTaken(e) && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const loadLocal = useCallback(async () => {
    if (!desktop) return;
    try {
      const status = await desktopApi.status();
      if (!status.features?.includes("folders")) {
        setNeedsUpdate(true);
        return;
      }
      setLocal({ runner: status.runner_id, folders: await desktopApi.folders() });
    } catch {
      setLocal(null);
    }
  }, [desktop]);

  useEffect(() => {
    void worldApi.runners().then(setRunners).catch(() => setRunners([]));
    void worldApi
      .sessions()
      .then((list) => setRecent(list.filter((s) => s.role === "direct" && s.status !== "archived").slice(0, 6)))
      .catch(() => {});
    void loadLocal();
  }, [loadLocal]);

  const mine = useMemo(() => agents.filter((a) => !meId || a.owner_user_id === meId), [agents, meId]);
  const macs = useMemo(() => (runners ?? []).filter((r) => onMac(r) && (!meId || r.owner_user_id === meId)), [runners, meId]);

  useEffect(() => {
    if (agentId || mine.length === 0) return;
    const last = readLast();
    setAgentId(mine.find((a) => a.id === last.agent)?.id ?? mine.find((a) => a.is_main)?.id ?? mine[0].id);
  }, [mine, agentId]);

  const agent = agents.find((a) => a.id === agentId);
  const folders = useMemo(() => {
    const seen = new Map<string, PlacedFolder>();
    for (const runner of macs) {
      const own = local && local.runner === runner.id ? local.folders : [];
      for (const f of [...own, ...runnerFolders(runner)]) {
        if (!seen.has(f.id) && !dropped.has(f.id)) seen.set(f.id, { ...f, runner });
      }
    }
    return [...seen.values()];
  }, [macs, local, dropped]);
  const folder = folders.find((f) => f.id === folderId);

  useEffect(() => {
    if (folderId && folders.some((f) => f.id === folderId)) return;
    const last = readLast();
    setFolderId(folders.find((f) => f.id === last.folder)?.id ?? folders[0]?.id ?? null);
  }, [folders, folderId]);

  const open = async (detail: { sessionId: string; agentId: string }) => {
    if ((await workbenchSupport()) !== "ready") {
      setProblem("Update the Loka app to open the workshop.");
      return;
    }
    requestWorkshop(detail);
    onClose();
  };

  const start = async () => {
    if (!agent || !folderId || busy) return;
    setBusy(true);
    setProblem(null);
    try {
      const made = await worldApi.startDirect(agent.id, { folder_id: folderId, folder_name: folder?.name, text: text.trim() || undefined });
      saveLast({ agent: agent.id, folder: folderId });
      await open({ sessionId: made.session_id, agentId: agent.id });
    } catch (error) {
      setProblem(error instanceof Error ? error.message : "Could not start the session.");
    } finally {
      setBusy(false);
    }
  };

  const addFolder = async () => {
    setProblem(null);
    try {
      const added = await desktopApi.addFolder();
      await loadLocal();
      if (added) setFolderId(added.id);
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    }
  };

  const removeFolder = async (f: PlacedFolder) => {
    setProblem(null);
    try {
      const left = await desktopApi.removeFolder(f.id);
      setLocal((current) => (current ? { ...current, folders: left } : current));
      setDropped((current) => new Set(current).add(f.id));
      if (folderId === f.id) setFolderId(null);
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    }
  };

  const nameOf = (id: string) => agents.find((a) => a.id === id)?.name ?? "Agent";
  const nativeAdd = desktop && !!local?.runner && macs.some((m) => m.id === local.runner) && !needsUpdate;

  return (
    <div role="dialog" aria-modal="true" aria-label="Workshop" className={cn(PANEL, "@container pointer-events-auto relative flex h-full w-full flex-col overflow-hidden")}>
      <WindowHeader
        title="Workshop"
        subtitle="Quick questions and edits in a folder on your Mac. No quest, branch or review"
        onClose={onClose}
        actions={
          <button type="button" onClick={onHistory} title="Session history (H)" className={BTN_SECONDARY}>
            <ClockCounterClockwise size={15} weight="bold" />
            <span className="hidden sm:inline">History</span>
          </button>
        }
      />

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto grid w-full max-w-4xl gap-8 px-4 py-6 sm:px-6 sm:py-8">
          {recent.length > 0 && (
            <section>
              <StepTitle title="Pick up where you left off" />
              <ul className="grid gap-2 sm:grid-cols-2">
                {recent.map((s) => (
                  <li key={s.id}>
                    <button
                      type="button"
                      onClick={() => void open({ sessionId: s.id, agentId: s.agent_id })}
                      className={cn(ITEM_CARD, FOCUS, "flex w-full items-center gap-3 px-3 py-2.5 text-left")}
                    >
                      <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-w-sand text-w-link">
                        <FolderOpen size={18} weight="duotone" />
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-w-base text-w-strong">{s.title ?? "Folder"}</span>
                        <span className="block truncate text-w-xs text-w-text2">
                          {nameOf(s.agent_id)} · {ago(s.updated_at)}
                        </span>
                      </span>
                      {s.running && <Shimmer className="shrink-0 text-w-xs">working</Shimmer>}
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}

          <section>
            <StepTitle step={1} title="Pick an agent" description="It works in your folder with its own persona and model." />
            {runners === null ? (
              <Shimmer className="text-w-sm">Loading your agents</Shimmer>
            ) : macs.length === 0 ? (
              <EmptyState icon={Desktop} title="No Mac paired yet" description="Folder sessions run on this Mac. Connect it in Settings, under Devices." />
            ) : mine.length === 0 ? (
              <EmptyState icon={UsersThree} title="You have no agents yet" description="Make one from the quest board first." />
            ) : (
              <div className="grid grid-cols-1 gap-2 @md:grid-cols-2 @3xl:grid-cols-3">
                {mine.map((a) => {
                  const on = a.id === agentId;
                  return (
                    <button
                      key={a.id}
                      type="button"
                      aria-pressed={on}
                      onClick={() => setAgentId(a.id)}
                      className={cn(ITEM_CARD, FOCUS, "relative flex items-center gap-3 px-3 py-2.5 text-left", on && "border-w-teal bg-w-aqua/60 hover:border-w-teal")}
                    >
                      <AgentFace agent={a} size={36} />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-w-sm font-semibold text-w-strong">{a.name}</span>
                        <span className="block truncate text-w-xs text-w-muted">{runtimeName(a.cli, a.model)}</span>
                      </span>
                      <span className={cn("grid h-5 w-5 shrink-0 place-items-center rounded-full border-2 transition-colors", on ? "border-w-teal bg-w-teal text-w-strong" : "border-w-line2")}>
                        {on && <Check size={11} weight="bold" />}
                      </span>
                    </button>
                  );
                })}
              </div>
            )}
            {agent && folder && agent.runner_id !== folder.runner.id && (
              <p className="mt-2 text-w-xs text-w-muted">
                {agent.name} works in this folder on {folder.runner.name} and stays where it lives for everything else.
              </p>
            )}
          </section>

          {agent && macs.length > 0 && (
            <section>
              <div className="flex flex-wrap items-start justify-between gap-3">
                <StepTitle step={2} title={`Choose a folder on ${macs[0].name}`} description="Edits land in your real files, not a copy." />
                <div className="mb-3 flex items-center gap-2">
                  {nativeAdd && (
                    <button type="button" onClick={() => void addFolder()} className={BTN_SECONDARY}>
                      <FolderPlus size={15} weight="bold" />
                      Share a folder
                    </button>
                  )}
                </div>
              </div>
              {needsUpdate && <p className="mb-2 text-w-sm text-w-danger">Update the Loka app to share folders from this Mac.</p>}
              {folders.length === 0 ? (
                nativeAdd ? (
                  <EmptyState icon={FolderPlus} title="No folders shared yet" description="Share a project folder and the agent can read and edit it." />
                ) : (
                  <EmptyState icon={WifiSlash} title="Open the Loka app" description="Folders are shared from the Loka app on this Mac." />
                )
              ) : (
                <ul className="grid gap-2 sm:grid-cols-2">
                  {folders.map((f) => (
                    <li key={f.id} className="relative">
                      <button
                        type="button"
                        aria-pressed={f.id === folderId}
                        onClick={() => setFolderId(f.id)}
                        className={cn(ITEM_CARD, FOCUS, "flex w-full items-center gap-3 px-3 py-2.5 pr-10 text-left", f.id === folderId && "border-w-teal bg-w-aqua/60 hover:border-w-teal")}
                      >
                        <span className={cn("grid h-9 w-9 shrink-0 place-items-center rounded-lg", f.id === folderId ? "bg-w-teal text-w-strong" : "bg-w-sand text-w-link")}>
                          <FolderOpen size={18} weight="duotone" />
                        </span>
                        <span className="min-w-0">
                          <span className="block truncate text-w-base text-w-strong">{f.name}</span>
                          <span className="block truncate font-mono text-w-2xs text-w-text2">{f.path}</span>
                          <span className="flex items-center gap-1 truncate text-w-2xs text-w-muted">
                            <Desktop size={11} weight="bold" className="shrink-0" />
                            {f.runner.name}
                            {f.runner.status !== "online" && " (offline)"}
                          </span>
                        </span>
                      </button>
                      {local?.runner === f.runner.id && nativeAdd && (
                        <button
                          type="button"
                          onClick={() => void removeFolder(f)}
                          aria-label={`Stop sharing ${f.name}`}
                          title="Stop sharing"
                          className="absolute right-2 top-1/2 -translate-y-1/2 rounded-md p-1 text-w-muted hover:bg-w-rose hover:text-w-danger"
                        >
                          <Trash size={14} weight="bold" />
                        </button>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </section>
          )}

          {agent && folders.length > 0 && (
            <section>
              <StepTitle step={3} title="First message" description="Optional. You can also start empty and type in the workshop." />
              <textarea
                value={text}
                onChange={(e) => setText(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    void start();
                  }
                }}
                rows={3}
                placeholder={`Ask ${agent.name} something about this folder, or say what to change`}
                className="w-full resize-y rounded-xl border-2 border-w-line2 bg-w-field px-3.5 py-3 text-w-base outline-none transition-colors placeholder:text-w-faint focus:border-w-teal focus:ring-2 focus:ring-w-teal/25"
              />
              <p className="mt-2 flex items-center gap-1.5 text-w-xs text-w-muted">
                <Info size={13} weight="bold" className="shrink-0" />
                {agent.name} will not commit or push unless you ask.
              </p>
            </section>
          )}

          {problem && (
            <p role="alert" className="rounded-xl border-2 border-w-danger/40 bg-w-rose p-3 text-w-sm text-w-danger">
              {problem}
            </p>
          )}
        </div>
      </div>

      <div className="flex shrink-0 items-center justify-end gap-3 border-t-2 border-dashed border-w-line2 px-4 py-3 sm:px-5">
        <p className="mr-auto hidden min-w-0 truncate text-w-xs text-w-muted sm:block">
          {!agent ? "Pick an agent to start" : !folderId ? "Pick a folder to start" : `${agent.name} in ${folders.find((f) => f.id === folderId)?.name ?? "your folder"}`}
        </p>
        <button
          type="button"
          disabled={!agent || !folderId || busy}
          onClick={() => void start()}
          className={BTN_PRIMARY}
        >
          <Hammer size={16} weight="bold" />
          {busy ? "Opening..." : "Open workshop"}
        </button>
      </div>
    </div>
  );
}
