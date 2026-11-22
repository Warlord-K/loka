export type Accessory =
  | "feather"
  | "cap"
  | "beanie"
  | "bun"
  | "glasses"
  | "apron"
  | "briefcase"
  | "scarf"
  | "headband"
  | "beard"
  | "ears-pointed"
  | "ears-round"
  | "ears-long"
  | "mask"
  | "beak"
  | "snout"
  | "whiskers"
  | "horns"
  | "sword"
  | "wizard"
  | "hardhat"
  | "lantern"
  | "mortarboard"
  | "quill"
  | "cloak"
  | "tentacles"
  | "claws"
  | "eyestalks"
  | "spikes"
  | "wings"
  | "bug-wings"
  | "frog-eyes"
  | "fin"
  | "teeth"
  | "belly"
  | "antenna"
  | "antennae"
  | "visor"
  | "ghost-tail"
  | "gills"
  | "stripes"
  | "crown"
  | "cape"
  | "headphones"
  | "horn"
  | "cactus"
  | "mushroom-cap";

export interface LokaLook {
  skin: string;
  hair: string;
  shirt: string;
  pants: string;
  accent?: string;
  accessories?: Accessory[];
  body?: string;
}

export type Trade = "carpenter" | "smith" | "alchemist" | "wizard" | "warrior";

export interface LokaShopStyle {
  roof: string;
  wall: string;
  awning: [string, string];
}

export interface Resident {
  id: string;
  name: string;
  business: string;
  tagline: string;
  look: LokaLook;
  quest: Accessory[];
  shop: LokaShopStyle;
  trade: Trade;
}

export interface MapItem {
  id: string;
  kind: "building" | "decoration" | "critter" | "portal" | "npc";
  name: string;
  data: Record<string, unknown>;
  look?: LokaLook;
}
