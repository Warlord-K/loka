"use client";

import { useMemo } from "react";
import { cn } from "@/lib/utils";
import { HATS, LOOK_ACCESSORIES, SPECIES_OPTIONS, lookFor } from "@/lib/looks";
import type { Accessory } from "./types";
import { portraitUrl } from "./characters";
import { AgentAvatar } from "./channel-ui";
import { WCombobox } from "./controls";
import { FOCUS, PILL_INK, PILL_NEUTRAL } from "./styles";

const ACCESSORY_LABELS: Partial<Record<Accessory, string>> = {
  wizard: "Wizard hat",
  hardhat: "Hard hat",
  mortarboard: "Grad cap",
  bun: "Hair bun",
};

function label(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

const SPECIES_CHOICES = SPECIES_OPTIONS.map((value) => ({ value, label: label(value), hint: value === "person" ? "default" : undefined }));

export function toggleAccessory(list: Accessory[], item: Accessory): Accessory[] {
  if (list.includes(item)) return list.filter((a) => a !== item);
  const kept = HATS.includes(item) ? list.filter((a) => !HATS.includes(a)) : list;
  return [...kept, item];
}

export function LookFields({
  slug,
  look,
  species,
  onSpecies,
  accessories,
  onAccessories,
}: {
  slug: string;
  look?: Record<string, unknown>;
  species: string;
  onSpecies: (value: string) => void;
  accessories: Accessory[];
  onAccessories: (value: Accessory[]) => void;
}) {
  const preview = useMemo(
    () => portraitUrl(lookFor({ slug: slug || "new-agent", persona: "", look: { ...(look ?? {}), species: species.trim() || "person", accessories } }), 6),
    [slug, look, species, accessories],
  );
  return (
    <div className="flex gap-3">
      <AgentAvatar portrait={preview} size="5rem" tall soft className="mt-6" />
      <div className="min-w-0 flex-1 space-y-2.5">
        <label className="block">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Species</span>
          <WCombobox ariaLabel="Species" className="w-full" value={species} onChange={onSpecies} placeholder="Person" options={SPECIES_CHOICES} />
        </label>
        <div>
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Accessories</span>
          <div className="flex flex-wrap gap-1.5">
            {LOOK_ACCESSORIES.map((item) => {
              const on = accessories.includes(item);
              return (
                <button
                  key={item}
                  type="button"
                  aria-pressed={on}
                  onClick={() => onAccessories(toggleAccessory(accessories, item))}
                  className={cn(on ? PILL_INK : PILL_NEUTRAL, FOCUS, "h-8 px-2 text-w-xs", !on && "hover:border-w-ink hover:text-w-ink")}
                >
                  {ACCESSORY_LABELS[item] ?? label(item)}
                </button>
              );
            })}
          </div>
          <p className="mt-1.5 text-w-xs text-w-muted">One hat at a time. Any species not in the list shows as a person.</p>
        </div>
      </div>
    </div>
  );
}
