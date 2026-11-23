export const QUEST_REFRESH_MS = 700;

type Schedule = (run: () => void, delay: number) => ReturnType<typeof setTimeout>;
type Cancel = (timer: ReturnType<typeof setTimeout>) => void;

export interface QuestStore<T> {
  load: (force?: boolean) => Promise<T[]>;
  invalidate: () => void;
  snapshot: () => T[] | undefined;
  subscribe: (listener: () => void) => () => void;
  update: (change: (current: T[]) => T[]) => void;
  dispose: () => void;
}

export function createQuestStore<T>(
  request: () => Promise<T[]>,
  schedule: Schedule = setTimeout,
  cancel: Cancel = clearTimeout,
  enabled = true,
): QuestStore<T> {
  const listeners = new Set<() => void>();
  let list: T[] | undefined;
  let active: { id: number; promise: Promise<T[]> } | null = null;
  let due: ReturnType<typeof setTimeout> | null = null;
  let epoch = 0;
  let requestId = 0;
  let stale = false;
  let queued = false;

  const notify = () => listeners.forEach((listener) => listener());
  const stop = () => {
    enabled = false;
    epoch++;
    stale ||= active !== null;
    active = null;
    queued = false;
    if (due !== null) cancel(due);
    due = null;
  };
  const load = (force = false): Promise<T[]> => {
    if (!enabled) return Promise.resolve(list ?? []);
    if (active) return active.promise;
    if (!force && list && (!stale || due !== null)) return Promise.resolve(list);
    const id = ++requestId;
    const requestedEpoch = epoch;
    stale = false;
    const promise = request()
      .then((next) => {
        if (enabled && requestedEpoch === epoch) {
          list = next;
          notify();
        }
        return list ?? [];
      })
      .catch(() => {
        if (enabled && requestedEpoch === epoch) {
          stale = true;
          if (!list) {
            list = [];
            notify();
          }
        }
        return list ?? [];
      })
      .finally(() => {
        if (active?.id !== id) return;
        active = null;
        if (queued && enabled && listeners.size > 0) {
          queued = false;
          void load(true);
        }
      });
    active = { id, promise };
    return promise;
  };
  const invalidate = () => {
    stale = true;
    if (!enabled || listeners.size === 0 || due !== null) return;
    due = schedule(() => {
      due = null;
      if (active) queued = true;
      else void load(true);
    }, QUEST_REFRESH_MS);
  };
  return {
    load,
    invalidate,
    snapshot: () => list,
    subscribe: (listener) => {
      if (listeners.size === 0 && !enabled) {
        enabled = true;
        epoch++;
      }
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) stop();
      };
    },
    update: (change) => {
      if (!enabled) return;
      list = change(list ?? []);
      notify();
    },
    dispose: () => {
      stop();
      listeners.clear();
    },
  };
}
