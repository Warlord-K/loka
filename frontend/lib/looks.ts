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
  match: RegExp;
  skin: string;
  hair: string;
  features: Accessory[];
}

const SPECIES: Species[] = [
  { match: /raccoon/, skin: "#a7adb4", hair: "#5b5f66", features: ["ears-pointed", "mask"] },
  { match: /panda/, skin: "#f4f1ea", hair: "#26232a", features: ["ears-round", "mask"] },
  { match: /fox/, skin: "#f1a25e", hair: "#d0612b", features: ["ears-pointed", "snout"] },
  { match: /cat|kitten|tiger|lion/, skin: "#f0b26c", hair: "#c27a32", features: ["ears-pointed", "whiskers"] },
  { match: /dog|pup|wolf/, skin: "#c79a6b", hair: "#7a5434", features: ["ears-round", "snout"] },
  { match: /bear/, skin: "#a8774f", hair: "#6b4529", features: ["ears-round", "snout"] },
  { match: /sloth/, skin: "#c8a983", hair: "#7c6046", features: ["mask"] },
  { match: /rabbit|bunny|hare/, skin: "#efe6dc", hair: "#d8c8b8", features: ["ears-long"] },
  { match: /owl/, skin: "#b48a5e", hair: "#7b5a3a", features: ["ears-pointed", "beak"] },
  { match: /peacock/, skin: "#1f8f9c", hair: "#1c3e7a", features: ["beak", "feather"] },
  { match: /parrot|bird|duck|chicken|penguin/, skin: "#57b65a", hair: "#d8433a", features: ["beak"] },
  { match: /mouse|rat|hamster/, skin: "#c9b8aa", hair: "#8d7b6e", features: ["ears-round", "whiskers"] },
];

const HUMAN_ACCESSORIES: Accessory[] = ["cap", "beanie", "bun", "glasses", "scarf", "headband", "beard"];

export function text(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(text).join(" ");
  if (value && typeof value === "object") return Object.values(value).map(text).join(" ");
  return "";
}

export function lookFor(agent: Pick<WorldAgent, "slug" | "look" | "persona">): LokaLook {
  const seed = hash(agent.slug);
  const words = `${text(agent.look)} ${agent.persona ?? ""}`.toLowerCase();
  const declared = text(agent.look.species).toLowerCase();
  const species = declared ? SPECIES.find((s) => s.match.test(declared)) : SPECIES.find((s) => s.match.test(words));
  const accessories: Accessory[] = species ? [...species.features] : [pick(HUMAN_ACCESSORIES, seed, 7)];
  if (/glasses|spectacles/.test(words) && !accessories.includes("glasses")) accessories.push("glasses");
  if (/scarf/.test(words) && !accessories.includes("scarf")) accessories.push("scarf");
  if (/spiky|punk|rock/.test(words) && !species) accessories.push("headband");
  return {
    skin: species?.skin ?? pick(SKINS, seed, 3),
    hair: species?.hair ?? pick(HAIRS, seed, 5),
    shirt: pick(SHIRTS, seed, 9),
    pants: pick(PANTS, seed, 11),
    accent: pick(SHIRTS, seed, 13),
    accessories,
  };
}
