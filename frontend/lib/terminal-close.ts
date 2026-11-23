export type CloseChoice = "ask" | "minimize" | "close_all";

export const CLOSE_CHOICES: { value: CloseChoice; label: string }[] = [
  { value: "ask", label: "Ask" },
  { value: "minimize", label: "Always minimize" },
  { value: "close_all", label: "Always close all" },
];

export type CloseSource = "window" | "dock" | "escape" | "outside" | "last_terminal";

export type CloseOutcome = "quiet" | "ask" | "minimize" | "close_all";

export type PaneLife = { phase: string; native: string | null; local: boolean };

export type RunningShell = { device: string };

export type ClosePlan = { outcome: CloseOutcome; count: number; where: string };

export type NativeTerm = { id: string; exited: boolean };

export function savedNativeInventory(saved: readonly (string | null)[], listed: readonly NativeTerm[]): Map<string, boolean> {
  const wanted = new Set(saved.filter((id): id is string => !!id));
  return new Map(listed.filter((term) => wanted.has(term.id)).map((term) => [term.id, term.exited]));
}

export function runningShellKind(
  busy: boolean,
  pendingNative: string | null,
  savedNative: string | null,
  inventory: ReadonlyMap<string, boolean>,
  held: (native: string) => boolean,
): "saved" | "busy" | null {
  const native = pendingNative ?? savedNative;
  if (native && inventory.get(native) === false && !held(native)) return "saved";
  return !pendingNative && busy ? "busy" : null;
}

export function closeShortcutClosesArea(asks: boolean, tabs: number): boolean {
  return asks && tabs === 1;
}

export function shouldCloseStaleNative(requested: string | null, returned: string, explicitlyClosed: boolean): boolean {
  return explicitlyClosed || !requested || requested !== returned;
}

export async function settleRemoteOpen<T extends { session_id: string }>(
  opening: Promise<T>,
  current: () => boolean,
  close: (id: string) => Promise<unknown>,
  attach: (opened: T) => void,
): Promise<void> {
  const opened = await opening;
  if (!current()) {
    await close(opened.session_id).catch(() => {});
    return;
  }
  attach(opened);
}

export function paneRunning(pane: PaneLife): boolean {
  if (pane.phase === "starting" || pane.phase === "connecting" || pane.phase === "live") return true;
  return pane.phase === "idle" && pane.local && !!pane.native;
}

export function listOf(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

export function runningWhere(running: RunningShell[]): string {
  const devices = [...new Set(running.map((shell) => shell.device))];
  const count = running.length === 1 ? "1 terminal" : `${running.length} terminals`;
  return devices.length ? `${count} on ${listOf(devices)}` : count;
}

export function planClose(at: { running: RunningShell[]; choice: CloseChoice }): ClosePlan {
  const count = at.running.length;
  const where = runningWhere(at.running);
  if (!count) return { outcome: "quiet", count, where };
  return { outcome: at.choice, count, where };
}

export type CloseEffects = {
  quiet: (source: CloseSource) => void;
  minimize: () => void;
  closeAll: () => void;
  ask: (plan: ClosePlan) => void;
};

export function requestClose(at: { source: CloseSource; running: RunningShell[]; choice: CloseChoice }, effects: CloseEffects): ClosePlan {
  const plan = planClose(at);
  if (plan.outcome === "quiet") effects.quiet(at.source);
  else if (plan.outcome === "minimize") effects.minimize();
  else if (plan.outcome === "close_all") effects.closeAll();
  else effects.ask(plan);
  return plan;
}

export function closeHint(choice: CloseChoice): string {
  if (choice === "minimize") return "Minimizes, your terminals keep running. Change in Settings";
  if (choice === "close_all") return "Closes all terminals. Change in Settings";
  return "Close the terminal";
}

export function closeChoiceKey(userId: string | null): string {
  return `world.terminalClose.${userId ?? "anon"}`;
}

export function parseCloseChoice(raw: unknown): CloseChoice {
  return raw === "minimize" || raw === "close_all" ? raw : "ask";
}

export function loadCloseChoice(storage: Pick<Storage, "getItem"> | null, key: string): CloseChoice {
  try {
    return parseCloseChoice(storage?.getItem(key));
  } catch {
    return "ask";
  }
}

export function saveCloseChoice(storage: Pick<Storage, "setItem" | "removeItem"> | null, key: string, choice: CloseChoice): void {
  try {
    if (choice === "ask") storage?.removeItem(key);
    else storage?.setItem(key, choice);
  } catch {
    return;
  }
}

export type TerminalPresence = "closed" | "open" | "minimized";

export type PresenceEvent = "open" | "cover" | "minimize" | "close";

export function nextPresence(at: TerminalPresence, event: PresenceEvent): TerminalPresence {
  if (event === "open") return "open";
  if (event === "close") return "closed";
  if (event === "cover") return at === "open" ? "minimized" : at;
  return at === "closed" ? "closed" : "minimized";
}

export function presenceMounted(at: TerminalPresence): boolean {
  return at !== "closed";
}
