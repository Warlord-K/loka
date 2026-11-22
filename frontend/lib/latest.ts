export function latestOnly() {
  let counter = 0;
  return {
    begin() {
      const mine = ++counter;
      return () => mine === counter;
    },
  };
}

export async function applyLatest<T>(read: () => T | Promise<T>, current: () => boolean, apply: (value: T) => void) {
  const value = await read();
  if (current()) apply(value);
}

const RECHECK_FIRST_MS = 20_000;
const RECHECK_CAP_MS = 60_000;
const RECHECK_LIMIT = 24;

export function recheckDelay(attempt: number): number | null {
  if (attempt >= RECHECK_LIMIT) return null;
  return Math.min(RECHECK_FIRST_MS * 1.5 ** attempt, RECHECK_CAP_MS);
}

export function startRechecks(
  run: () => void,
  schedule: (fn: () => void, ms: number) => unknown = (fn, ms) => setTimeout(fn, ms),
  cancel: (handle: unknown) => void = (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
): () => void {
  let handle: unknown;
  let stopped = false;
  const arm = (attempt: number) => {
    const delay = recheckDelay(attempt);
    if (stopped || delay === null) return;
    handle = schedule(() => {
      run();
      arm(attempt + 1);
    }, delay);
  };
  arm(0);
  return () => {
    stopped = true;
    cancel(handle);
  };
}
