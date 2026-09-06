"use client";

import { useEffect, useState } from "react";

export type SoundKind = "done" | "failed" | "notify" | "reply" | "timer";

export type SoundPrefs = {
  on: boolean;
  volume: number;
  kinds: Record<SoundKind, boolean>;
};

export const SOUND_KINDS: { kind: SoundKind; label: string; hint: string }[] = [
  { kind: "done", label: "Quest done", hint: "a quest finished or its PR merged" },
  { kind: "failed", label: "Quest trouble", hint: "a quest failed or an agent got stuck" },
  { kind: "notify", label: "Notifications", hint: "anything new in the bell" },
  { kind: "reply", label: "Replies", hint: "the agent you are talking to replied while this tab is in the background" },
  { kind: "timer", label: "Timers", hint: "your timers and to-do nudges" },
];

const KEY = "world.sounds";
const DEBOUNCE_MS = 1500;
const DEFAULTS: SoundPrefs = { on: true, volume: 0.5, kinds: { done: true, failed: true, notify: true, reply: true, timer: true } };

type Note = { f: number; at: number; len: number; wave?: OscillatorType; to?: number; peak?: number };

const PATTERNS: Record<SoundKind, Note[]> = {
  done: [
    { f: 783.99, at: 0, len: 0.09, wave: "square", peak: 0.09 },
    { f: 1046.5, at: 0.08, len: 0.09, wave: "square", peak: 0.09 },
    { f: 1318.51, at: 0.16, len: 0.09, wave: "square", peak: 0.09 },
    { f: 1567.98, at: 0.24, len: 0.32, wave: "triangle", peak: 0.22 },
  ],
  failed: [
    { f: 392, at: 0, len: 0.16, wave: "square", peak: 0.08 },
    { f: 311.13, at: 0.15, len: 0.34, wave: "square", to: 277.18, peak: 0.08 },
  ],
  notify: [
    { f: 880, at: 0, len: 0.12, wave: "triangle", peak: 0.2 },
    { f: 1174.66, at: 0.1, len: 0.22, wave: "triangle", peak: 0.2 },
  ],
  reply: [{ f: 1318.51, at: 0, len: 0.14, wave: "sine", to: 1567.98, peak: 0.14 }],
  timer: [
    { f: 880, at: 0, len: 0.5, peak: 0.25 },
    { f: 1174.66, at: 0.16, len: 0.5, peak: 0.25 },
    { f: 1567.98, at: 0.32, len: 0.5, peak: 0.25 },
  ],
};

let ctx: AudioContext | null = null;
let prefs: SoundPrefs | null = null;
let holds = 0;
const last = new Map<SoundKind, number>();
const listeners = new Set<(p: SoundPrefs) => void>();

function clamp(v: unknown): number {
  return typeof v === "number" && Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : DEFAULTS.volume;
}

export function soundPrefs(): SoundPrefs {
  if (prefs) return prefs;
  let saved: Partial<SoundPrefs> | null = null;
  try {
    saved = JSON.parse(window.localStorage.getItem(KEY) ?? "null") as Partial<SoundPrefs> | null;
  } catch {}
  prefs = {
    on: typeof saved?.on === "boolean" ? saved.on : DEFAULTS.on,
    volume: clamp(saved?.volume),
    kinds: { ...DEFAULTS.kinds },
  };
  for (const k of Object.keys(DEFAULTS.kinds) as SoundKind[]) {
    const v = saved?.kinds?.[k];
    if (typeof v === "boolean") prefs.kinds[k] = v;
  }
  return prefs;
}

export function setSoundPrefs(next: SoundPrefs) {
  prefs = { on: next.on, volume: clamp(next.volume), kinds: { ...next.kinds } };
  try {
    window.localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {}
  for (const l of listeners) l(prefs);
}

export function useSoundPrefs(): [SoundPrefs, (p: SoundPrefs) => void] {
  const [p, setP] = useState<SoundPrefs>(DEFAULTS);
  useEffect(() => {
    setP(soundPrefs());
    listeners.add(setP);
    return () => {
      listeners.delete(setP);
    };
  }, []);
  return [p, setSoundPrefs];
}

function audio(): AudioContext | null {
  try {
    if (!ctx || ctx.state === "closed") ctx = new AudioContext();
    if (ctx.state === "suspended") void ctx.resume().catch(() => undefined);
    return ctx;
  } catch {
    return null;
  }
}

export function unlockSounds() {
  if (typeof window === "undefined") return;
  const unlock = () => {
    window.removeEventListener("pointerdown", unlock, true);
    window.removeEventListener("keydown", unlock, true);
    audio();
  };
  window.addEventListener("pointerdown", unlock, true);
  window.addEventListener("keydown", unlock, true);
}

export function holdSounds(): () => void {
  holds += 1;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    holds = Math.max(0, holds - 1);
  };
}

function render(kind: SoundKind, volume: number) {
  const ac = audio();
  if (!ac || volume <= 0) return;
  const start = ac.currentTime + 0.01;
  for (const n of PATTERNS[kind]) {
    const o = ac.createOscillator();
    const g = ac.createGain();
    const t = start + n.at;
    const peak = Math.max(0.0002, (n.peak ?? 0.2) * volume);
    o.type = n.wave ?? "triangle";
    o.frequency.setValueAtTime(n.f, t);
    if (n.to) o.frequency.exponentialRampToValueAtTime(n.to, t + n.len);
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(peak, t + 0.015);
    g.gain.exponentialRampToValueAtTime(0.0001, t + n.len);
    o.connect(g).connect(ac.destination);
    o.start(t);
    o.stop(t + n.len + 0.05);
  }
}

export function playSound(kind: SoundKind): boolean {
  try {
    const p = soundPrefs();
    if (!p.on || !p.kinds[kind] || holds > 0) return false;
    const now = Date.now();
    if (now - (last.get(kind) ?? 0) < DEBOUNCE_MS) return false;
    last.set(kind, now);
    render(kind, p.volume);
    return true;
  } catch {
    return false;
  }
}

export function previewSound(kind: SoundKind, volume?: number) {
  try {
    render(kind, clamp(volume ?? soundPrefs().volume));
  } catch {}
}
