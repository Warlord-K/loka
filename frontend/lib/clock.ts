import { useSyncExternalStore } from "react";

type Clock = { now: number; listeners: Set<() => void>; timer: number | null };

const clocks = new Map<number, Clock>();

function clockFor(period: number): Clock {
  let clock = clocks.get(period);
  if (!clock) {
    clock = { now: Date.now(), listeners: new Set(), timer: null };
    clocks.set(period, clock);
  }
  return clock;
}

function tick(clock: Clock): void {
  clock.now = Date.now();
  for (const fn of clock.listeners) fn();
}

function start(period: number, clock: Clock): void {
  if (clock.timer !== null || document.hidden) return;
  clock.timer = window.setInterval(() => tick(clock), period);
}

function stop(clock: Clock): void {
  if (clock.timer === null) return;
  window.clearInterval(clock.timer);
  clock.timer = null;
}

let visibilityBound = false;

function bindVisibility(): void {
  if (visibilityBound || typeof document === "undefined") return;
  visibilityBound = true;
  document.addEventListener("visibilitychange", () => {
    for (const [period, clock] of clocks) {
      if (document.hidden) stop(clock);
      else if (clock.listeners.size) {
        tick(clock);
        start(period, clock);
      }
    }
  });
}

function subscribe(period: number, fn: () => void): () => void {
  bindVisibility();
  const clock = clockFor(period);
  clock.listeners.add(fn);
  if (clock.listeners.size === 1) {
    clock.now = Date.now();
    start(period, clock);
  }
  return () => {
    clock.listeners.delete(fn);
    if (!clock.listeners.size) stop(clock);
  };
}

const subscribers = new Map<number, (fn: () => void) => () => void>();
const snapshots = new Map<number, () => number>();

function subscriberFor(period: number) {
  let fn = subscribers.get(period);
  if (!fn) {
    fn = (listener: () => void) => subscribe(period, listener);
    subscribers.set(period, fn);
  }
  return fn;
}

function snapshotFor(period: number) {
  let fn = snapshots.get(period);
  if (!fn) {
    fn = () => clockFor(period).now;
    snapshots.set(period, fn);
  }
  return fn;
}

const idle = () => () => {};

export function useNow(period = 1000, active = true): number {
  const snapshot = snapshotFor(period);
  return useSyncExternalStore(active ? subscriberFor(period) : idle, snapshot, snapshot);
}
