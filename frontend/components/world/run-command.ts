"use client";

import { createContext } from "react";

export type RunCommand = { run: (command: string, runOn?: string) => Promise<void>; where: string; who: string };

export const RunCommandContext = createContext<RunCommand | null>(null);

export const OPEN_TERMINAL_EVENT = "world:open-terminal";
