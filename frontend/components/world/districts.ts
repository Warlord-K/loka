import type { Region, RegionTheme } from "@/lib/loka";
import { CIVIC, type CivicPlace } from "./places";

export interface District {
  id: string;
  name: string;
  theme: RegionTheme;
  seed: number;
  description: string;
  campus: boolean;
}

export type Edge = "n" | "s" | "e" | "w";

export interface Link {
  area: string;
  edge: Edge;
  at: number;
  to: string;
}

export const DISTRICTS: District[] = [
  { id: "university", name: "University", theme: "academy", seed: 9903, description: "The campus, with a classroom for every skill.", campus: true },
  { id: "workshop", name: "Workshop Quarter", theme: "city", seed: 5501, description: "The Forge, your devices and the shell.", campus: false },
  { id: "hilltop", name: "Hilltop", theme: "mountain", seed: 7702, description: "The Library and the Observatory.", campus: false },
];

export const LINKS: Link[] = [
  { area: "hearth", edge: "w", at: 24, to: "university" },
  { area: "university", edge: "e", at: 18, to: "hearth" },
  { area: "hearth", edge: "e", at: 24, to: "workshop" },
  { area: "workshop", edge: "w", at: 18, to: "hearth" },
  { area: "hearth", edge: "n", at: 39, to: "hilltop" },
  { area: "hilltop", edge: "s", at: 27, to: "hearth" },
];

export function linksOf(area: string): Link[] {
  return LINKS.filter((l) => l.area === area);
}

export function areaName(id: string): string {
  return id === "hearth" ? "Home" : districtOf(id)?.name ?? "Loka";
}

export function districtOf(id: string): District | null {
  return DISTRICTS.find((d) => d.id === id) ?? null;
}

export function civicIn(districtId: string | null): CivicPlace[] {
  return CIVIC.filter((c) => (c.district ?? null) === districtId);
}

export function districtRegion(d: District): Region {
  return {
    id: d.id,
    owner_user_id: "",
    name: d.name,
    slug: d.id,
    theme: d.theme,
    seed: d.seed,
    visibility: "public",
    status: "active",
    created_by_agent: null,
    data: { description: d.description },
    created_at: "",
    updated_at: "",
  };
}
