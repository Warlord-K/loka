"use client";

import { createContext, useEffect, useMemo, useState } from "react";
import { worldApi, type RunnerView, type WorldAgent } from "@/lib/world";
import { desktopApi, isDesktopApp } from "@/lib/desktop";
import type { MessageQuote } from "./quote";

export type RunCommand = {
  run: (command: string, runOn?: string) => Promise<void>;
  where: string;
  who: string;
  agentId: string;
  ownerId: string;
  home: string | null;
};

export const RunCommandContext = createContext<RunCommand | null>(null);

export const TerminalTargetContext = createContext<{ type: (command: string) => void; where: string } | null>(null);

export const OPEN_TERMINAL_EVENT = "world:open-terminal";

export type OpenTerminal = { command: string; runnerId: string | null; agentId?: string; quote?: MessageQuote };

export function openInTerminal(detail: OpenTerminal) {
  window.dispatchEvent(new CustomEvent<OpenTerminal>(OPEN_TERMINAL_EVENT, { detail }));
}

export function runCommandFor(agent: WorldAgent, meId: string | null, opts: { where?: string; onRan?: () => void } = {}): RunCommand | null {
  if (!meId || agent.owner_user_id !== meId) return null;
  return {
    where: opts.where ?? (agent.runner_name ? `on ${agent.runner_name}` : "on this agent's device"),
    who: agent.name,
    agentId: agent.id,
    ownerId: meId,
    home: agent.runner_id,
    run: async (command: string, runOn?: string) => {
      try {
        await worldApi.runCommand(agent.id, command, runOn && runOn !== agent.runner_id ? runOn : undefined);
      } catch (err) {
        const status = err instanceof Error ? parseInt(err.message, 10) : NaN;
        if (status === 409) throw new Error(`${runOn && runOn !== agent.runner_id ? "That device" : `${agent.name}'s device`} is offline or not set, so the command could not run. Check Devices, then try again.`);
        if (status === 403) throw new Error("Only the owner of this agent can run commands.");
        throw new Error("That did not start. Try again.");
      }
      opts.onRan?.();
    },
  };
}

export function useRunCommand(agent: WorldAgent | null | undefined, meId: string | null, opts: { where?: string; onRan?: () => void } = {}): RunCommand | null {
  const { where, onRan } = opts;
  return useMemo(
    () => (agent ? runCommandFor(agent, meId, { where, onRan }) : null),
    [agent?.id, agent?.name, agent?.runner_id, agent?.runner_name, agent?.owner_user_id, meId, where, onRan],
  );
}

const DEVICES_FRESH_MS = 60_000;
let deviceCache: { owner: string; at: number; list: RunnerView[] } | null = null;
let deviceLoad: Promise<RunnerView[]> | null = null;

function loadDevices(owner: string): Promise<RunnerView[]> {
  if (deviceCache?.owner === owner && Date.now() - deviceCache.at < DEVICES_FRESH_MS) return Promise.resolve(deviceCache.list);
  if (!deviceLoad) {
    deviceLoad = worldApi
      .runners()
      .then((all) => {
        const list = all.filter((r) => r.status === "online" && r.owner_user_id === owner);
        deviceCache = { owner, at: Date.now(), list };
        return list;
      })
      .finally(() => {
        deviceLoad = null;
      });
  }
  return deviceLoad;
}

export function useOwnDevices(owner: string | null, wanted: boolean): RunnerView[] | null {
  const [list, setList] = useState<RunnerView[] | null>(() => (owner && deviceCache?.owner === owner ? deviceCache.list : null));
  useEffect(() => {
    if (!owner || !wanted) return;
    let gone = false;
    loadDevices(owner)
      .then((found) => {
        if (!gone) setList(found);
      })
      .catch(() => {
        if (!gone) setList((prev) => prev ?? []);
      });
    return () => {
      gone = true;
    };
  }, [owner, wanted]);
  return list;
}

export type AppRunner = { id: string | null; homeTerminal: boolean };

let appRunner: Promise<AppRunner> | null = null;

export function readAppRunner(): Promise<AppRunner> {
  if (!isDesktopApp()) return Promise.resolve({ id: null, homeTerminal: false });
  appRunner ??= desktopApi
    .status()
    .then((status) => ({
      id: status.status === "unpaired" ? null : status.runner_id,
      homeTerminal: !!status.features?.includes("home_terminal"),
    }))
    .catch(() => {
      appRunner = null;
      return { id: null, homeTerminal: false };
    });
  return appRunner;
}

export function useAppRunner(): AppRunner | null {
  const [found, setFound] = useState<AppRunner | null>(isDesktopApp() ? null : { id: null, homeTerminal: false });
  useEffect(() => {
    let gone = false;
    void readAppRunner().then((r) => {
      if (!gone) setFound(r);
    });
    return () => {
      gone = true;
    };
  }, []);
  return found;
}
