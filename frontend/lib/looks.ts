import type { Accessory, LokaLook } from "@/components/world/types";
import type { WorldAgent } from "./world";

export function hash(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

export function pick<T>(list: T[], seed: number, salt: number): T {
  return list[(seed >>> salt) % list.length];
}

const SKINS = ["#f1c7a1", "#e0ac83", "#c68a5e", "#8d5a3b", "#f6d6bd"];
const HAIRS = ["#2b1d16", "#5a3a22", "#c9a14a", "#8e3b2e", "#1f2a44", "#e9e3d5"];
const SHIRTS = ["#e8b64a", "#16a39a", "#e0663d", "#6c5ce7", "#2f80ed", "#e85d75", "#3f8a48", "#9b7be0"];
const PANTS = ["#35506e", "#3a2a26", "#4b5563", "#2f3e46", "#5b3b6b"];

interface Species {
  names: string[];
  declaredOnly?: string[];
  skin: string;
  hair: string;
  shirt?: string;
  pants?: string;
  features: Accessory[];
}

const SPECIES: Species[] = [
  { names: ["octopus", "squid"], skin: "#e8768f", hair: "#c24f6e", features: ["tentacles"] },
  { names: ["axolotl"], skin: "#f5a9c4", hair: "#e886aa", features: ["gills"] },
  { names: ["crab", "lobster"], skin: "#e0563f", hair: "#b23a2a", features: ["eyestalks", "claws"] },
  { names: ["dragon"], skin: "#c8524a", hair: "#8e2f2f", features: ["spikes", "wings"] },
  { names: ["dinosaur", "dino", "t-rex", "trex", "raptor"], skin: "#7cc46a", hair: "#3f8a45", features: ["spikes"] },
  { names: ["frog", "toad"], skin: "#8fcf5c", hair: "#5ea33f", features: ["frog-eyes"] },
  { names: ["penguin"], skin: "#f6f1e7", hair: "#26232a", shirt: "#2b2730", pants: "#2b2730", features: ["beak", "belly"] },
  { names: ["duckling", "duck", "goose"], skin: "#f6d24a", hair: "#e8b830", features: ["beak"] },
  { names: ["shark"], skin: "#9fb4c4", hair: "#5b7488", features: ["fin", "teeth"] },
  { names: ["robot", "droid", "cyborg", "automaton"], declaredOnly: ["android"], skin: "#b9c3cc", hair: "#7d8a96", shirt: "#8d99a6", pants: "#5f6b75", features: ["antenna", "visor"] },
  { names: ["ghost", "phantom", "spectre", "specter"], declaredOnly: ["spirit"], skin: "#eef0f6", hair: "#d3d7e4", features: ["ghost-tail"] },
  { names: ["bumblebee", "bee", "wasp", "hornet"], skin: "#f2c230", hair: "#2b2730", shirt: "#f2c230", features: ["antennae", "stripes", "bug-wings"] },
  { names: ["raccoon"], skin: "#a7adb4", hair: "#5b5f66", features: ["ears-pointed", "mask"] },
  { names: ["red panda", "panda"], skin: "#f4f1ea", hair: "#26232a", features: ["ears-round", "mask"] },
  { names: ["fox"], skin: "#f1a25e", hair: "#d0612b", features: ["ears-pointed", "snout"] },
  { names: ["cat", "kitten", "kitty", "tiger", "lion"], skin: "#f0b26c", hair: "#c27a32", features: ["ears-pointed", "whiskers"] },
  { names: ["dog", "puppy", "pup", "wolf"], skin: "#c79a6b", hair: "#7a5434", features: ["ears-round", "snout"] },
  { names: ["bear"], skin: "#a8774f", hair: "#6b4529", features: ["ears-round", "snout"] },
  { names: ["sloth"], skin: "#c8a983", hair: "#7c6046", features: ["mask"] },
  { names: ["rabbit", "bunny", "hare"], skin: "#efe6dc", hair: "#d8c8b8", features: ["ears-long", "whiskers"] },
  { names: ["owl"], skin: "#b48a5e", hair: "#7b5a3a", features: ["ears-pointed", "beak"] },
  { names: ["peacock"], skin: "#1f8f9c", hair: "#1c3e7a", features: ["beak", "feather"] },
  { names: ["parrot", "bird", "chicken"], skin: "#57b65a", hair: "#d8433a", features: ["beak"] },
  { names: ["mouse", "rat", "hamster"], skin: "#c9b8aa", hair: "#8d7b6e", features: ["ears-round", "whiskers"] },
];

export const SPECIES_OPTIONS: string[] = [
  "person", "octopus", "bunny", "crab", "dino", "dragon", "frog", "penguin", "duck", "shark", "robot", "ghost",
  "axolotl", "bee", "fox", "cat", "dog", "panda", "bear", "raccoon", "sloth", "owl", "peacock", "parrot", "mouse",
];

export const LOOK_ACCESSORIES: Accessory[] = [
  "glasses", "headphones", "crown", "wizard", "beanie", "cap", "hardhat", "mortarboard", "headband", "bun",
  "beard", "scarf", "cape", "cloak", "apron",
];

export const HATS: Accessory[] = ["beanie", "cap", "mortarboard", "wizard", "hardhat", "crown"];

const ACCESSORY_ALIASES: Record<string, Accessory> = {
  "wizard hat": "wizard",
  "hard hat": "hardhat",
  "grad cap": "mortarboard",
  "graduation cap": "mortarboard",
  hat: "cap",
  "baseball cap": "cap",
  spectacles: "glasses",
  goggles: "glasses",
  headset: "headphones",
  tiara: "crown",
};

const HUMAN_ACCESSORIES: Accessory[] = ["cap", "beanie", "bun", "glasses", "scarf", "headband", "beard"];

function escape(word: string): string {
  return word.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&");
}

function forms(name: string): string[] {
  const out = [name, `${name}s`, `${name}es`];
  if (name.endsWith("f")) out.push(`${name.slice(0, -1)}ves`);
  if (/[^aeiou]y$/.test(name)) out.push(`${name.slice(0, -1)}ies`);
  return out;
}

export function wordsMatch(names: string[], words: string): boolean {
  return new RegExp(`\\b(?:${names.flatMap(forms).map(escape).join("|")})\\b`).test(words);
}

export function speciesFor(declared: string, words = ""): Species | undefined {
  const d = declared.trim().toLowerCase();
  if (d) return SPECIES.find((s) => wordsMatch([...s.names, ...(s.declaredOnly ?? [])], d));
  return SPECIES.find((s) => wordsMatch(s.names, words));
}

const OLD_BIRD = { skin: "#57b65a", hair: "#d8433a" };

function paletteFor(species: Species | undefined, palette: string): Pick<Species, "skin" | "hair" | "shirt" | "pants"> | undefined {
  if (!species) return undefined;
  const bird = species.names.includes("duck") || species.names.includes("penguin");
  if (bird && !palette.trim()) return OLD_BIRD;
  return species;
}

export function withGear(accessories: Accessory[], gear: Accessory[]): Accessory[] {
  const gearHat = gear.find((a) => HATS.includes(a));
  const kept = gearHat ? accessories.filter((a) => !HATS.includes(a)) : accessories;
  const added = gear.filter((a) => !HATS.includes(a) || a === gearHat);
  return Array.from(new Set([...kept, ...added]));
}

const GEAR: [RegExp, Accessory[]][] = [
  [/\b(?:vikings?|horns?)\b/, ["horns"]],
  [/\bhelmets?\b/, ["horns"]],
  [/\b(?:wizards?|staffs?)\b|(?<!\bhard )\bhats?\b/, ["wizard"]],
  [/\bhard ?hats?\b|\bconstruction\b/, ["hardhat"]],
  [/\b(?:swords?|blades?|axes?)\b/, ["sword"]],
  [/\b(?:lanterns?|lamps?|torch(?:es)?)\b/, ["lantern"]],
  [/\b(?:crowns?|royal|kings?|queens?)\b/, ["crown"]],
  [/\b(?:capes?|superhero(?:es)?)\b/, ["cape"]],
  [/\b(?:headphones|headsets?)\b/, ["headphones"]],
];

export function gearAccessories(words: string): Accessory[] {
  const lower = words.toLowerCase();
  return Array.from(new Set(GEAR.filter(([re]) => re.test(lower)).flatMap(([, acc]) => acc)));
}

function accessoryList(value: unknown): string[] {
  const list = Array.isArray(value) ? value : typeof value === "string" ? value.split(",") : [];
  return list.filter((item): item is string => typeof item === "string" && Boolean(item.trim())).map((item) => item.trim());
}

function resolveAccessory(item: string): Accessory | undefined {
  const key = item.toLowerCase();
  return ACCESSORY_ALIASES[key] ?? ((LOOK_ACCESSORIES as readonly string[]).includes(key) ? (key as Accessory) : undefined);
}

export function unknownAccessories(value: unknown): string[] {
  return accessoryList(value).filter((item) => !resolveAccessory(item));
}

export function declaredAccessories(value: unknown): Accessory[] {
  const out: Accessory[] = [];
  for (const item of accessoryList(value)) {
    const acc = resolveAccessory(item);
    if (acc && !out.includes(acc)) out.push(acc);
  }
  return out;
}

export function text(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(text).join(" ");
  if (value && typeof value === "object") return Object.values(value).map(text).join(" ");
  return "";
}

export function lookFor(agent: Pick<WorldAgent, "slug" | "look" | "persona">): LokaLook {
  const seed = hash(agent.slug);
  const look = agent.look ?? {};
  const words = `${text(look)} ${agent.persona ?? ""}`.toLowerCase();
  const declared = text(look.species);
  const species = speciesFor(declared, words);
  const colours = paletteFor(species, text(look.palette));
  const chosen = declaredAccessories(look.accessories);
  const accessories: Accessory[] = species ? [...species.features] : chosen.length ? [] : [pick(HUMAN_ACCESSORIES, seed, 7)];
  const firstHat = chosen.find((a) => HATS.includes(a));
  for (const a of chosen) {
    if (HATS.includes(a) && a !== firstHat) continue;
    if (!accessories.includes(a)) accessories.push(a);
  }
  if (/\b(glasses|spectacles)\b/.test(words) && !accessories.includes("glasses")) accessories.push("glasses");
  if (/\bscarf\b/.test(words) && !accessories.includes("scarf")) accessories.push("scarf");
  if (/\b(spiky|punk|rock)\b/.test(words) && !species && !accessories.includes("headband")) accessories.push("headband");
  return {
    skin: colours?.skin ?? pick(SKINS, seed, 3),
    hair: colours?.hair ?? pick(HAIRS, seed, 5),
    shirt: colours?.shirt ?? pick(SHIRTS, seed, 9),
    pants: colours?.pants ?? pick(PANTS, seed, 11),
    accent: pick(SHIRTS, seed, 13),
    accessories,
  };
}
