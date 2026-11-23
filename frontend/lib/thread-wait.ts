export const RUN_END_EVENTS: ReadonlySet<string> = new Set(["run.completed", "run.failed", "run.timed_out"]);

export const THINKING_WINDOW_MS = 30 * 60 * 1000;

export function threadWaiting(o: { working: boolean | null; lastIsPerson: boolean; lastAt: number; now: number }): boolean {
  if (o.working !== null) return o.working;
  return o.lastIsPerson && o.now - o.lastAt < THINKING_WINDOW_MS;
}

export function chatWaiting(busy: boolean | undefined): boolean {
  return busy === true;
}
