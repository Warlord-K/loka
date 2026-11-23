"use client";

import { useCallback, useEffect, useState } from "react";

const PREFIX = "world.draft.";
const EVENT = "world:drafts";
const memory = new Map<string, string>();

export function readDraft(key: string): string {
  const held = memory.get(key);
  if (held !== undefined) return held;
  try {
    const stored = window.localStorage.getItem(PREFIX + key) ?? "";
    memory.set(key, stored);
    return stored;
  } catch {
    return "";
  }
}

export function saveDraft(key: string, value: string) {
  if ((memory.get(key) ?? "") === value) return;
  memory.set(key, value);
  try {
    if (value.trim()) window.localStorage.setItem(PREFIX + key, value);
    else window.localStorage.removeItem(PREFIX + key);
  } catch {}
  window.dispatchEvent(new CustomEvent<string>(EVENT, { detail: key }));
}

export function useDraft(key: string): [string, (next: string | ((cur: string) => string)) => void] {
  const [state, setState] = useState(() => ({ key, value: readDraft(key) }));
  const value = state.key === key ? state.value : readDraft(key);
  useEffect(() => {
    if (state.key !== key) setState({ key, value: readDraft(key) });
  }, [key, state.key]);
  useEffect(() => {
    const update = (event: Event) => {
      if ((event as CustomEvent<string>).detail === key) setState({ key, value: readDraft(key) });
    };
    window.addEventListener(EVENT, update);
    return () => window.removeEventListener(EVENT, update);
  }, [key]);
  const set = useCallback(
    (next: string | ((cur: string) => string)) => {
      const resolved = typeof next === "function" ? next(readDraft(key)) : next;
      saveDraft(key, resolved);
      setState({ key, value: resolved });
    },
    [key],
  );
  return [value, set];
}

export function useDraftKeys(keys: string[]): Set<string> {
  const joined = keys.join("|");
  const compute = useCallback(() => new Set(joined.split("|").filter((k) => k && readDraft(k).trim())), [joined]);
  const [found, setFound] = useState<Set<string>>(() => new Set());
  useEffect(() => {
    setFound(compute());
    const update = () => setFound(compute());
    const external = (e: StorageEvent) => {
      if (e.key?.startsWith(PREFIX)) memory.delete(e.key.slice(PREFIX.length));
      update();
    };
    window.addEventListener(EVENT, update);
    window.addEventListener("storage", external);
    return () => {
      window.removeEventListener(EVENT, update);
      window.removeEventListener("storage", external);
    };
  }, [compute]);
  return found;
}
