export type Accessory =
  | "feather"
  | "cap"
  | "beanie"
  | "bun"
  | "glasses"
  | "apron"
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
  | "cloak";

export interface LokaLook {
  skin: string;
  hair: string;
  shirt: string;
  pants: string;
  accent?: string;
  accessories?: Accessory[];
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
