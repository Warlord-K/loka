"use client";

import { useCallback, useEffect, useState } from "react";
import {
  LOKA_ROUTES,
  type AdventureCard,
  type Attempt,
  type ChallengeCard,
  type ChallengeDetail,
  type LokaError,
  type NpcLine,
  type NpcTalk,
  type OfferOutcome,
  type Progress,
  type Region,
  type RegionDetail,
} from "@/lib/loka";
import type { WorldEvent } from "@/lib/world";
import { PLAYER_LOOK } from "./engine";
import type { Accessory, LokaLook } from "./types";

export class LokaFailure extends Error {
  constructor(
    public status: number,
    public code: LokaError["error"] | "error",
    message: string,
  ) {
    super(message);
  }
}

async function call<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, { cache: "no-store", ...init, headers: init?.body ? { "content-type": "application/json" } : undefined });
  if (!res.ok) {
    let body: Partial<LokaError> | null = null;
    try {
      body = (await res.json()) as Partial<LokaError>;
    } catch {}
    throw new LokaFailure(res.status, body?.error ?? "error", body?.message ?? (res.status === 404 ? "That is not here any more." : `Something went wrong (${res.status}).`));
  }
  return (await res.json()) as T;
}

const post = <T,>(url: string, body: unknown) => call<T>(url, { method: "POST", body: JSON.stringify(body) });

export const lokaApi = {
  regions: () => call<Region[]>(LOKA_ROUTES.regions),
  region: (id: string) => call<RegionDetail>(LOKA_ROUTES.region(id)),
  npcLines: (id: string) => call<NpcLine[]>(LOKA_ROUTES.npcTalk(id)),
  talk: (id: string, text: string) => post<NpcTalk>(LOKA_ROUTES.npcTalk(id), { text }),
  challenges: () => call<ChallengeCard[]>(LOKA_ROUTES.challenges),
  challenge: (id: string) => call<ChallengeDetail>(LOKA_ROUTES.challenge(id)),
  submit: (id: string, answer: string) => post<Attempt>(LOKA_ROUTES.attempts(id), { answer }),
  attempt: (id: string) => call<Attempt>(LOKA_ROUTES.attempt(id)),
  adventures: () => call<AdventureCard[]>(LOKA_ROUTES.adventures),
  progress: () => call<Progress>(LOKA_ROUTES.progress),
  offer: (taskId: string | null) => post<OfferOutcome>(LOKA_ROUTES.offer, taskId ? { task_id: taskId } : {}),
  setAcademy: (on: boolean) => call<Progress>(LOKA_ROUTES.academy, { method: "PUT", body: JSON.stringify({ on }) }),
};

export function shareProgress(progress: Progress): void {
  window.dispatchEvent(new CustomEvent<Progress>("world-progress", { detail: progress }));
}

export function useSharedProgress(handler: (progress: Progress) => void): void {
  useEffect(() => {
    const on = (e: Event) => handler((e as CustomEvent<Progress>).detail);
    window.addEventListener("world-progress", on);
    return () => window.removeEventListener("world-progress", on);
  }, [handler]);
}

export function useWorldEvent(handler: (ev: WorldEvent) => void): void {
  useEffect(() => {
    const on = (e: Event) => handler((e as CustomEvent<WorldEvent>).detail);
    window.addEventListener("world-event", on);
    return () => window.removeEventListener("world-event", on);
  }, [handler]);
}

export const GEAR_ACCESSORY: Record<string, Accessory> = {
  "scholar-cap": "mortarboard",
  "golden-quill": "quill",
  "comet-cloak": "cloak",
};

const HATS: Accessory[] = ["beanie", "cap", "mortarboard", "wizard", "hardhat"];

export interface Cosmetics {
  gear: string[];
  decor: string[];
}

const COSMETICS_EVENT = "world:cosmetics";
const EMPTY: Cosmetics = { gear: [], decor: [] };

function cosmeticsKey(meId: string | null): string {
  return `world.cosmetics.${meId ?? "guest"}`;
}

export function readCosmetics(meId: string | null): Cosmetics {
  try {
    const raw = JSON.parse(window.localStorage.getItem(cosmeticsKey(meId)) ?? "null") as Partial<Cosmetics> | null;
    return {
      gear: Array.isArray(raw?.gear) ? raw.gear.filter((g): g is string => typeof g === "string") : [],
      decor: Array.isArray(raw?.decor) ? raw.decor.filter((g): g is string => typeof g === "string") : [],
    };
  } catch {
    return EMPTY;
  }
}

export function useCosmetics(meId: string | null): [Cosmetics, (next: Cosmetics) => void] {
  const [value, setValue] = useState<Cosmetics>(EMPTY);
  useEffect(() => {
    setValue(readCosmetics(meId));
    const update = () => setValue(readCosmetics(meId));
    window.addEventListener(COSMETICS_EVENT, update);
    return () => window.removeEventListener(COSMETICS_EVENT, update);
  }, [meId]);
  const save = useCallback(
    (next: Cosmetics) => {
      try {
        window.localStorage.setItem(cosmeticsKey(meId), JSON.stringify(next));
      } catch {}
      setValue(next);
      window.dispatchEvent(new Event(COSMETICS_EVENT));
    },
    [meId],
  );
  return [value, save];
}

export function playerLookWith(gear: string[]): LokaLook {
  const extra = gear.map((g) => GEAR_ACCESSORY[g]).filter((a): a is Accessory => Boolean(a));
  const hat = extra.some((a) => HATS.includes(a));
  const base = (PLAYER_LOOK.accessories ?? []).filter((a) => !hat || !HATS.includes(a));
  return { ...PLAYER_LOOK, accessories: [...base, ...extra] };
}

export function readVisited(meId: string | null): string[] {
  try {
    const raw = JSON.parse(window.localStorage.getItem(`world.visited.${meId ?? "guest"}`) ?? "[]") as unknown;
    return Array.isArray(raw) ? raw.filter((v): v is string => typeof v === "string") : [];
  } catch {
    return [];
  }
}

export function saveVisited(meId: string | null, ids: string[]): void {
  try {
    window.localStorage.setItem(`world.visited.${meId ?? "guest"}`, JSON.stringify(ids.slice(-60)));
  } catch {}
}
