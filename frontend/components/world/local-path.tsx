"use client";

import { createContext, useState, type ReactNode } from "react";
import * as PopoverPrimitive from "@radix-ui/react-popover";
import { Check, Copy, FolderOpen, MagnifyingGlass } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { useCopy } from "@/lib/copy";
import { desktopApi, isDesktopApp, type PlacedPath } from "@/lib/desktop";
import { worldApi, type WorldAgent } from "@/lib/world";
import { MENU, MENU_ITEM } from "./controls";
import { currentWorkshop, requestWorkshop, workbenchSupport } from "./workbench/bridge";
import { Shimmer } from "./shimmer";

export const LocalPathContext = createContext<WorldAgent | null>(null);

export interface LocalPath {
  path: string;
  line: number | null;
}

const HOME_PATH = /^(~|\/Users\/[^/\s]+)\/\S/;

export function parseLocalPath(raw: string): LocalPath | null {
  let text = raw.trim().replace(/^file:\/\//, "");
  if (!HOME_PATH.test(text) || text.length > 1024 || /[\n\r\0]/.test(text)) return null;
  text = text.replace(/[.,;!?)\]]+$/, "");
  let line: number | null = null;
  const anchor = text.match(/#L(\d+)(?:-L?\d+)?$/);
  if (anchor) {
    line = Number(anchor[1]);
    text = text.slice(0, anchor.index);
  } else {
    const suffix = text.match(/:(\d+)(?::\d+)?$/);
    if (suffix) {
      line = Number(suffix[1]);
      text = text.slice(0, suffix.index);
    }
  }
  text = text.replace(/\/+$/, "");
  if (!HOME_PATH.test(`${text}/`) || text.split("/").some((part) => part === ".." || part === ".")) return null;
  return { path: text, line: line && line > 0 ? line : null };
}

const BARE_PATH = /(^|\s|(?<!\])\()((?:~|\/Users\/[^/\s]+)\/[^\s`'"<>()[\]]+)/g;

export function codeLocalPaths(text: string): string {
  return text.replace(BARE_PATH, (whole, lead: string, path: string) => {
    const trail = path.match(/[.,;!?:]+$/)?.[0] ?? "";
    const core = trail ? path.slice(0, -trail.length) : path;
    return parseLocalPath(core) ? `${lead}\`${core}\`${trail}` : whole;
  });
}

function absolute(path: string, root: string): string {
  if (!path.startsWith("~/")) return path;
  const home = root.match(/^\/Users\/[^/]+/)?.[0];
  return home ? `${home}${path.slice(1)}` : path;
}

async function localRunner(): Promise<string | null> {
  if (!isDesktopApp()) return null;
  try {
    const status = await desktopApi.status();
    return status.features?.includes("open_paths") ? status.runner_id : null;
  } catch {
    return null;
  }
}

function plainError(error: unknown, fallback: string): string {
  const text = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  if (/not allowed here/i.test(text)) return "Update the Loka app on that Mac to open paths from chat.";
  return text || fallback;
}

async function placePath(runnerId: string, path: string): Promise<PlacedPath> {
  if ((await localRunner()) === runnerId) return desktopApi.folderForPath(path);
  throw new Error("Open the Loka app on this Mac to open paths from chat.");
}

export async function openLocalPath(agent: WorldAgent, target: LocalPath): Promise<void> {
  const here = currentWorkshop();
  if (here && (!here.runnerId || !agent.runner_id || here.runnerId === agent.runner_id)) {
    const full = absolute(target.path, here.root);
    if (full === here.root || full.startsWith(`${here.root}/`)) {
      requestWorkshop({ sessionId: here.sessionId, agentId: here.agentId, file: full.slice(here.root.length + 1) || null, line: target.line });
      return;
    }
  }
  if (!agent.runner_id) throw new Error(`${agent.name} is not on one of your Macs, so this path cannot open here.`);
  if ((await workbenchSupport()) !== "ready") throw new Error("Update the Loka app to open the Workshop.");
  const placed = await placePath(agent.runner_id, target.path);
  const key = `folders/${placed.folder.id}`;
  const sessions = await worldApi.sessions(agent.id).catch(() => []);
  const open = sessions.find((s) => s.role === "direct" && s.working_dir === key && s.status !== "archived");
  const sessionId = open?.id ?? (await worldApi.startDirect(agent.id, { folder_id: placed.folder.id, folder_name: placed.folder.name })).session_id;
  requestWorkshop({ sessionId, agentId: agent.id, file: placed.is_dir ? null : placed.file, line: placed.is_dir ? null : target.line });
}

export function PathLink({ agent, target, children }: { agent: WorldAgent; target: LocalPath; children?: ReactNode }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<"workshop" | "finder" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [finder, setFinder] = useState(false);
  const [copied, copy] = useCopy();

  const onOpenChange = (next: boolean) => {
    setOpen(next);
    if (!next) return;
    setProblem(null);
    void localRunner().then((runner) => setFinder(!!runner && runner === agent.runner_id));
  };

  const act = async (kind: "workshop" | "finder") => {
    setBusy(kind);
    setProblem(null);
    try {
      if (kind === "finder") await desktopApi.revealPath(target.path);
      else await openLocalPath(agent, target);
      setOpen(false);
    } catch (error) {
      setProblem(plainError(error, kind === "finder" ? "Finder could not show that." : "Could not open that in the Workshop."));
    } finally {
      setBusy(null);
    }
  };

  return (
    <PopoverPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <PopoverPrimitive.Trigger asChild>
        <button
          type="button"
          title={`Open ${target.path}`}
          className="rounded-md bg-w-sand px-1 py-px text-left font-mono text-[0.82em] text-w-link underline decoration-w-link/40 decoration-dotted underline-offset-2 transition-colors hover:bg-w-teal/20 hover:decoration-w-link"
        >
          {children ?? target.path}
        </button>
      </PopoverPrimitive.Trigger>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          side="bottom"
          align="start"
          sideOffset={6}
          collisionPadding={12}
          className={cn(MENU, "w-72 animate-w-modal-in data-[state=closed]:animate-w-modal-out")}
          onClick={(e) => e.stopPropagation()}
        >
          <p className="truncate px-2.5 pb-1.5 pt-1 font-mono text-w-2xs text-w-muted" title={target.path}>
            {target.path}
            {target.line ? `:${target.line}` : ""}
          </p>
          <button type="button" disabled={busy !== null} onClick={() => void act("workshop")} className={cn(MENU_ITEM, "w-full")}>
            <FolderOpen size={15} weight="bold" className="shrink-0 text-w-muted" />
            {busy === "workshop" ? <Shimmer>Opening in the Workshop</Shimmer> : <span>Open in Workshop with {agent.name}</span>}
          </button>
          {finder && (
            <button type="button" disabled={busy !== null} onClick={() => void act("finder")} className={cn(MENU_ITEM, "w-full")}>
              <MagnifyingGlass size={15} weight="bold" className="shrink-0 text-w-muted" />
              {busy === "finder" ? <Shimmer>Opening Finder</Shimmer> : <span>Show in Finder</span>}
            </button>
          )}
          <button type="button" onClick={() => void copy(target.path)} className={cn(MENU_ITEM, "w-full")}>
            {copied ? <Check size={15} weight="bold" className="shrink-0 text-w-ok" /> : <Copy size={15} weight="bold" className="shrink-0 text-w-muted" />}
            <span>{copied ? "Copied" : "Copy path"}</span>
          </button>
          {problem && (
            <p role="alert" className="mx-1 mb-1 mt-1.5 rounded-md bg-w-rose px-2 py-1.5 text-w-xs text-w-danger">
              {problem}
            </p>
          )}
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}
