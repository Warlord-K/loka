import type { LokaItem, Region, RegionTheme, Weather } from "@/lib/loka";
import { lookFor, type WorldAgent } from "@/lib/world";
import { makeBarrel, makeBench, makeBush, makeCrate, makeReeds, makeRock, makeTree, type Sprite, type TreeKind } from "./art";
import { buildSheet, type Dir } from "./characters";
import { context, makeCanvas, rect, rgbOf, rng, shade } from "./pixels";
import { areaName, civicIn, linksOf, type District } from "./districts";
import { frost, makeCactus, makeIceRock } from "./regions-art";
import type { LokaLook, MapItem } from "./types";
import {
  BRIDGE,
  GRASS,
  HEARTH,
  kitBlock,
  kitFree,
  kitLamp,
  kitReserve,
  makeKit,
  MAP_H,
  MAP_W,
  paintWater,
  PATH,
  PLAZA,
  decorSprite,
  arrivalFor,
  carveExits,
  nearExit,
  openExits,
  placeAcademy,
  placeCampus,
  placeCivic,
  placeItems,
  REGION_GEO,
  placePortal,
  riverTop,
  seedOf,
  T,
  WATER,
  type Kit,
  type PortalSlot,
  type SceneWeather,
  type Spot,
  type World,
} from "./world-classic";

const ICE = 6;

type Prop = "bush" | "flowers" | "rock" | "cactus" | "ice" | "crate" | "barrel" | "reeds";

interface ThemeLook {
  ground: [string, string, string];
  specks: [string, string];
  path: [string, string, string];
  plaza: [string, string];
  backdrop: string;
  water: "river" | "ice" | "none";
  trees: TreeKind[];
  treeCount: number;
  frost: boolean;
  props: Prop[];
  lamps: number;
  weather: SceneWeather | null;
  critters: string[];
  flowers: boolean;
}

const MEADOW: ThemeLook = {
  ground: ["#7cc46a", "#73bd62", "#6ab35a"],
  specks: ["#93d47c", "#5ea24f"],
  path: ["#dcbc8c", "#c9a577", "#ead3a8"],
  plaza: ["#d4cab8", "#b3a896"],
  backdrop: "#1d3d22",
  water: "river",
  trees: ["green", "green", "blossom"],
  treeCount: 40,
  frost: false,
  props: ["bush", "flowers", "flowers", "rock"],
  lamps: 4,
  weather: null,
  critters: ["rabbit", "bird"],
  flowers: true,
};

const THEMES: Record<RegionTheme, ThemeLook> = {
  meadow: MEADOW,
  forest: {
    ...MEADOW,
    ground: ["#4f9a4e", "#478f47", "#3f853f"],
    specks: ["#5fae58", "#36763a"],
    path: ["#a88a5e", "#977a50", "#b99a6c"],
    backdrop: "#10261a",
    trees: ["pine", "pine", "green", "autumn"],
    treeCount: 95,
    props: ["bush", "bush", "rock"],
    lamps: 2,
    weather: "leaves",
    critters: ["fox", "owl", "bear"],
    flowers: false,
  },
  docks: {
    ...MEADOW,
    ground: ["#d8c69a", "#cfbb8d", "#c6b183"],
    specks: ["#e6d7af", "#b49e70"],
    path: ["#a8703f", "#9a6a3c", "#b07a48"],
    plaza: ["#b9a48a", "#9c8670"],
    backdrop: "#132a3a",
    trees: ["green"],
    treeCount: 14,
    props: ["crate", "barrel", "crate", "rock"],
    lamps: 10,
    critters: ["bird", "cat"],
    flowers: false,
  },
  academy: {
    ...MEADOW,
    ground: ["#86c48a", "#7ebc82", "#75b47a"],
    plaza: ["#d8cfe6", "#b9aecb"],
    backdrop: "#1f2140",
    trees: ["blossom", "blossom", "green"],
    treeCount: 34,
    lamps: 10,
    critters: ["owl", "cat"],
  },
  snow: {
    ...MEADOW,
    ground: ["#eef4f8", "#e4edf3", "#dbe6ee"],
    specks: ["#ffffff", "#c9d8e3"],
    path: ["#c9d3dc", "#b7c3cd", "#dde5ec"],
    plaza: ["#d5dee6", "#aebbc6"],
    backdrop: "#1c2a3a",
    water: "ice",
    trees: ["pine"],
    treeCount: 55,
    frost: true,
    props: ["ice", "rock", "ice"],
    lamps: 6,
    weather: "snow",
    critters: ["penguin", "fox"],
    flowers: false,
  },
  desert: {
    ...MEADOW,
    ground: ["#e7c98f", "#dfbf84", "#d6b47a"],
    specks: ["#f1d9a6", "#c7a066"],
    path: ["#c99b62", "#b8894f", "#d8ad74"],
    plaza: ["#d9b98a", "#b89466"],
    backdrop: "#3a2614",
    water: "none",
    trees: [],
    treeCount: 0,
    props: ["cactus", "cactus", "rock"],
    lamps: 3,
    weather: "clear",
    critters: ["rabbit"],
    flowers: false,
  },
  mountain: {
    ...MEADOW,
    ground: ["#8fa38a", "#869a81", "#7c9078"],
    specks: ["#a6b8a0", "#6a7d67"],
    path: ["#a9a196", "#958d82", "#bbb3a8"],
    backdrop: "#1e2526",
    water: "none",
    trees: ["pine"],
    treeCount: 40,
    props: ["rock", "rock", "rock", "bush"],
    lamps: 2,
    weather: "fog",
    critters: ["bear", "bird"],
    flowers: false,
  },
  swamp: {
    ...MEADOW,
    ground: ["#5d7d45", "#56743f", "#4e6b39"],
    specks: ["#6f8f52", "#425c31"],
    path: ["#7d6a4a", "#6d5c40", "#8c7856"],
    backdrop: "#141f12",
    trees: ["green", "autumn"],
    treeCount: 50,
    props: ["reeds", "reeds", "bush", "rock"],
    lamps: 3,
    weather: "fog",
    critters: ["bird", "mouse"],
    flowers: false,
  },
  city: {
    ...MEADOW,
    ground: ["#b9b2a6", "#b1aa9e", "#a9a296"],
    specks: ["#c8c1b5", "#99928a"],
    path: ["#8a8580", "#7c7772", "#9a958f"],
    plaza: ["#d4cab8", "#b3a896"],
    backdrop: "#1c1c22",
    trees: ["green"],
    treeCount: 16,
    props: ["crate", "barrel", "bush"],
    lamps: 18,
    critters: ["cat", "dog"],
    flowers: false,
  },
  island: {
    ...MEADOW,
    ground: ["#e9d9a8", "#e2d09c", "#d9c690"],
    specks: ["#f3e7bf", "#c4b07a"],
    backdrop: "#0f3346",
    trees: ["green", "green"],
    treeCount: 22,
    props: ["rock", "flowers", "bush"],
    lamps: 3,
    weather: "clear",
    critters: ["parrot", "bird"],
  },
};

export function themeOf(theme: string): ThemeLook {
  return THEMES[theme as RegionTheme] ?? MEADOW;
}

export function sceneWeather(weather: Weather | undefined, fallback: SceneWeather | null): SceneWeather | null {
  if (!weather) return fallback;
  if (weather === "rain" || weather === "storm") return "rain";
  if (weather === "wind") return "leaves";
  if (weather === "snow") return "snow";
  if (weather === "fog" || weather === "cloudy") return "fog";
  return "clear";
}

const PORTAL_SLOTS: PortalSlot[] = [
  { x: 27, y: 42, spur: null },
  { x: 27, y: 4, spur: null },
  { x: 10, y: 8, spur: [10, 8, 12, 18] },
  { x: 44, y: 8, spur: [44, 8, 46, 18] },
  { x: 10, y: 26, spur: [10, 20, 12, 27] },
  { x: 44, y: 26, spur: [44, 20, 46, 27] },
];

export type StaffLookup = Pick<WorldAgent, "id" | "slug" | "look" | "persona">[];

export function staffOf(data: Record<string, unknown>, agents: StaffLookup = []): StaffLookup[number] | null {
  const id = typeof data.agent_id === "string" ? data.agent_id : null;
  return id ? (agents.find((a) => a.id === id) ?? null) : null;
}

export function lookOfNpc(item: { id: string; name: string; data: Record<string, unknown> }, agents: StaffLookup = []): LokaLook {
  const staff = staffOf(item.data, agents);
  if (staff) return lookFor(staff);
  const look = (item.data.look && typeof item.data.look === "object" ? item.data.look : {}) as Record<string, unknown>;
  const base = lookFor({ slug: item.id, persona: typeof item.data.persona === "string" ? item.data.persona : null, look });
  const hex = (v: unknown) => (typeof v === "string" && /^#[0-9a-fA-F]{6}$/.test(v) ? v : null);
  return {
    ...base,
    skin: hex(look.skin) ?? base.skin,
    hair: hex(look.hair) ?? base.hair,
    shirt: hex(look.shirt) ?? base.shirt,
    pants: hex(look.pants) ?? base.pants,
    accent: hex(look.accent) ?? base.accent,
    accessories: Array.isArray(look.accessories) ? (look.accessories.filter((a) => typeof a === "string") as LokaLook["accessories"]) : base.accessories,
  };
}

export function mapItemOf(i: Pick<LokaItem, "id" | "kind" | "name" | "data">, agents: StaffLookup = []): MapItem | null {
  if (i.kind === "building" || i.kind === "decoration" || i.kind === "portal") return { id: i.id, kind: i.kind, name: i.name, data: i.data };
  if (i.kind === "npc") return { id: i.id, kind: "npc", name: i.name, data: i.data, look: lookOfNpc(i, agents) };
  if (i.kind === "critter") {
    const look = lookFor({ slug: i.id, persona: null, look: { species: i.data.species ?? "" } });
    const color = typeof i.data.color === "string" && /^#[0-9a-fA-F]{6}$/.test(i.data.color) ? i.data.color : null;
    return { id: i.id, kind: "critter", name: i.name, data: i.data, look: color ? { ...look, skin: color } : look };
  }
  return null;
}

const idx = (x: number, y: number) => y * MAP_W + x;
const inside = (x: number, y: number) => x >= 0 && y >= 0 && x < MAP_W && y < MAP_H;

export function buildRegionWorld(region: Region, items: MapItem[], cameFrom: string, opts: { district?: District | null; decor?: string[] } = {}): World {
  const theme = themeOf(region.theme);
  const r = rng((region.seed >>> 0) % 2147483646 + 1);
  const k = makeKit(r);
  const { tiles, solid, reserved } = k;
  const fill = (x0: number, y0: number, x1: number, y1: number, type: number) => {
    for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) if (inside(x, y)) tiles[idx(x, y)] = type;
  };
  const plazaC = { x: 28, y: 17.5 };
  for (let y = 0; y < MAP_H; y++) {
    for (let x = 0; x < MAP_W; x++) {
      const dx = (x + 0.5 - plazaC.x) / 6.2;
      const dy = (y + 0.5 - plazaC.y) / 4.6;
      if (dx * dx + dy * dy <= 1) tiles[idx(x, y)] = PLAZA;
    }
  }
  const campus = Boolean(opts.district?.campus);
  const academy = region.theme === "academy";
  const civic = opts.district ? civicIn(opts.district.id) : [];
  const district = Boolean(opts.district);
  fill(district ? 16 : 4, 18, district ? 40 : 52, 20, PATH);
  fill(27, academy || district ? 12 : 4, 29, district ? 22 : 43, PATH);
  for (const c of civic) fill(c.slot[0] + 2, c.slot[1] + 5, c.slot[0] + 4, 18, PATH);
  const links = opts.district ? linksOf(region.id) : [];
  carveExits(k, links);
  const portals = opts.district ? [] : items.filter((i) => i.kind === "portal");
  const homeward = portals.find((p) => !p.data.to_region);
  const ordered: (MapItem | null)[] = opts.district ? [] : homeward ? [homeward, ...portals.filter((p) => p !== homeward)] : [null, ...portals];
  const slots = PORTAL_SLOTS.filter((sl) => !(academy && sl.y === 4)).slice(0, ordered.length);
  for (const sl of slots) if (sl.spur) fill(sl.spur[0], sl.spur[1], sl.spur[2], sl.spur[3], PATH);
  if (theme.water !== "none") {
    for (let x = 0; x < MAP_W; x++) {
      const top = riverTop(x);
      for (let y = top; y < top + 3; y++) tiles[idx(x, y)] = theme.water === "ice" ? ICE : WATER;
    }
    if (theme.water === "river" && !district) {
      for (let y = 28; y < 35; y++) {
        for (let x = 26; x < 30; x++) {
          if (tiles[idx(x, y)] === WATER) tiles[idx(x, y)] = BRIDGE;
          else if (x >= 27 && x < 29) tiles[idx(x, y)] = PATH;
        }
      }
      if (region.theme === "docks") {
        for (const px of [10, 17, 38, 45]) {
          const top = riverTop(px);
          for (let y = top - 1; y < top + 2; y++) tiles[idx(px, y)] = BRIDGE;
        }
      }
    }
  }
  for (let i = 0; i < MAP_W * MAP_H; i++) {
    if (tiles[i] === WATER) solid[i] = 1;
    if (tiles[i] !== GRASS) reserved[i] = 1;
  }
  for (let y = 0; y < MAP_H; y++) for (let x = 0; x < MAP_W; x++) if (x < 3 || y < 3 || x >= MAP_W - 3 || y >= MAP_H - 3) kitBlock(k, x, y);
  openExits(k, links, areaName);
  kitReserve(k, 21, 13, 36, 23);

  for (const c of civic) placeCivic(k, c.id, c.name, c.slot[0], c.slot[1]);
  if (campus) {
    kitReserve(k, 18, 5, 38, 14);
    placeCampus(k, 25, 7);
    const spots: [number, number][] = [[26, 13], [29, 13], [25, 13], [30, 13], [24, 13], [31, 13]];
    (opts.decor ?? []).slice(0, spots.length).forEach((id, i) => {
      const [x, y] = spots[i];
      const sprite = decorSprite(id);
      if (!sprite || solid[idx(x, y)]) return;
      kitBlock(k, x, y);
      k.objects.push({ x: x * T + 8, y: y * T + 13, sprite });
      if (id === "lantern-post" || id === "campfire") k.lights.push({ x: x * T + 8, y: y * T + (id === "campfire" ? 6 : -6), r: 30, color: [255, 190, 110], kind: "lamp", phase: r() * 10 });
    });
  } else if (academy) {
    kitReserve(k, 24, 6, 32, 13);
    placeAcademy(k, 25, 7);
  }

  ordered.forEach((p, i) => {
    const slot = slots[i];
    if (!slot) return;
    if (!p) {
      placePortal(k, `home:${region.id}`, "Home", null, slot.x, slot.y, "arch");
      return;
    }
    const to = typeof p.data.to_region === "string" ? p.data.to_region : null;
    placePortal(k, p.id, String(p.data.label || p.name), to, slot.x, slot.y, p.data.style === "sign" ? "sign" : "arch");
  });
  placeItems(
    k,
    items.filter((i) => i.kind !== "portal"),
    items.filter((i) => i.kind === "building"),
  );

  const bench = makeBench();
  for (const [x, y] of [[22, 15], [33, 15]]) {
    kitBlock(k, x, y);
    kitBlock(k, x + 1, y);
    k.objects.push({ x: (x + 1) * T, y: y * T + 13, sprite: bench });
  }
  const lampSpots: [number, number][] = [[21, 17], [34, 17], [26, 21], [29, 21], [26, 27], [29, 27], [10, 17], [46, 17], [26, 36], [29, 36], [18, 20], [38, 20], [26, 10], [29, 10], [14, 17], [42, 17], [26, 40], [29, 40]];
  const crowded = (x: number, y: number) => k.places.some((p) => Math.abs(p.door.x - (x * T + 8)) < 3 * T && Math.abs(p.door.y - (y * T + 8)) < 3 * T);
  const byPath = (x: number, y: number) => [[1, 0], [-1, 0], [0, 1], [0, -1]].some(([dx, dy]) => tiles[idx(x + dx, y + dy)] === PATH || tiles[idx(x + dx, y + dy)] === PLAZA);
  for (const [x, y] of lampSpots.slice(0, theme.lamps)) if (!solid[idx(x, y)] && tiles[idx(x, y)] !== PATH && byPath(x, y) && !crowded(x, y)) kitLamp(k, x, y);

  const tree = (kind: TreeKind): Sprite => (theme.frost ? frost(makeTree(kind, r)) : makeTree(kind, r));
  const border: TreeKind[] = theme.trees.length ? theme.trees : [];
  for (let y = -1; y < MAP_H + 2; y += 2) {
    for (let x = -1; x < MAP_W + 2; x += 2) {
      const edge = x < 3 || y < 3 || x >= MAP_W - 3 || y >= MAP_H - 3;
      if (!edge || nearExit(k, links, Math.max(0, Math.min(MAP_W - 1, x)), Math.max(0, Math.min(MAP_H - 1, y)))) continue;
      const jx = (r() - 0.5) * 10;
      const jy = (r() - 0.5) * 8;
      const sprite = border.length ? tree(border[Math.floor(r() * border.length)]) : region.theme === "desert" ? makeCactus(r) : makeRock(r);
      k.objects.push({ x: x * T + 8 + jx, y: y * T + 14 + jy + (x % 4 === 1 ? 8 : 0), sprite });
    }
  }
  let placed = 0;
  for (let n = 0; n < 1600 && placed < theme.treeCount; n++) {
    const x = 3 + Math.floor(r() * (MAP_W - 6));
    const y = 3 + Math.floor(r() * (MAP_H - 6));
    if (!kitFree(k, x, y, 1, 1)) continue;
    kitBlock(k, x, y);
    kitReserve(k, x - 1, y - 2, x + 2, y + 2);
    k.objects.push({ x: x * T + 8, y: y * T + 13, sprite: tree(theme.trees[Math.floor(r() * theme.trees.length)]) });
    placed++;
  }
  const crate = makeCrate();
  const barrel = makeBarrel();
  for (let n = 0, count = 0; n < 900 && count < 44; n++) {
    const x = 3 + Math.floor(r() * (MAP_W - 6));
    const y = 3 + Math.floor(r() * (MAP_H - 6));
    if (!kitFree(k, x, y, 0, 0)) continue;
    const prop = theme.props[Math.floor(r() * theme.props.length)];
    const sprite =
      prop === "cactus" ? makeCactus(r)
      : prop === "ice" ? makeIceRock(r)
      : prop === "rock" ? makeRock(r)
      : prop === "crate" ? crate
      : prop === "barrel" ? barrel
      : prop === "reeds" ? makeReeds(r)
      : makeBush(r, prop === "flowers" ? ["#ff9ab0", "#f2c14e", "#f6f1e7", "#b7a0ff"][Math.floor(r() * 4)] : null);
    if (prop !== "reeds") kitBlock(k, x, y);
    else kitReserve(k, x, y, x + 1, y + 1);
    k.objects.push({ x: x * T + 8, y: y * T + 13, sprite });
    count++;
  }

  const ambient = region.data.ambient;
  const species = ambient?.critters?.length ? ambient.critters.flatMap((c) => Array(Math.max(0, Math.min(6, c.count))).fill(c.species)) : theme.critters;
  species.slice(0, 12).forEach((sp: string, i: number) => {
    const rr = rng(seedOf(`${region.id}:${i}`));
    for (let n = 0; n < 200; n++) {
      const x = 4 + Math.floor(rr() * (MAP_W - 8));
      const y = 4 + Math.floor(rr() * (MAP_H - 8));
      if (!kitFree(k, x, y, 1, 1)) continue;
      kitReserve(k, x, y, x + 1, y + 1);
      const px = x * T + 8;
      const py = y * T + 13;
      k.critters.push({ id: `ambient:${region.id}:${i}`, name: sp, x: px, y: py, homeX: px, homeY: py, sheet: buildSheet(lookFor({ slug: `${region.id}${i}`, persona: null, look: { species: sp } })), dir: "down", target: null, wait: rr() * 4, anim: 0, moving: false, talk: false, greeting: null, say: null });
      break;
    }
  });

  const ground = paintRegionGround(tiles, reserved, theme, r);
  const waterFrames = theme.water === "river" ? paintWater(tiles, r, REGION_GEO) : [makeCanvas(1, 1)];
  const arrival = arrivalFor({ places: k.places, w: MAP_W, h: MAP_H }, cameFrom) ?? arrivalFor({ places: k.places, w: MAP_W, h: MAP_H }, HEARTH);
  const spawn = arrival ? { x: arrival.x, y: arrival.y } : { x: 28 * T, y: 25 * T + 8 };
  const spots = regionSpots(k, r);
  const time = ambient?.time_of_day;
  return {
    region: region.id,
    w: MAP_W,
    h: MAP_H,
    riverTop: (x: number) => riverTop(x),
    bridge: REGION_GEO.bridge,
    name: region.name,
    backdrop: theme.backdrop,
    fauna: { peacock: false, water: theme.water === "river", board: false, petals: theme.flowers },
    weather: sceneWeather(ambient?.weather, theme.weather),
    ambientTime: time && time !== "auto" ? time : null,
    tiles,
    solid,
    ground,
    waterFrames,
    waterTop: 27,
    objects: k.objects,
    lights: k.lights,
    npcs: [],
    critters: k.critters,
    placements: k.placements,
    plots: [],
    labels: k.labels,
    chimneys: k.chimneys,
    board: { x: -9999, y: -9999 },
    gate: spawn,
    spawn,
    plaza: { x: plazaC.x * T, y: plazaC.y * T },
    riverCenter: (x: number) => (riverTop(Math.floor(x / T)) + 1.5) * T,
    spots,
    marketLamp: null,
    places: k.places,
  };
}

function regionSpots(k: Kit, r: () => number): World["spots"] {
  const grass: { x: number; y: number }[] = [];
  const plaza: Spot[] = [];
  for (let y = 3; y < MAP_H - 3; y++) {
    for (let x = 3; x < MAP_W - 3; x++) {
      if (k.solid[idx(x, y)]) continue;
      if (k.tiles[idx(x, y)] === PLAZA && (x + y * 3) % 5 === 0) plaza.push({ x: x * T + 8, y: y * T + 13, face: (["down", "left", "right"] as Dir[])[(x + y) % 3] });
      if (k.tiles[idx(x, y)] === GRASS && !k.reserved[idx(x, y)]) grass.push({ x: x * T + 8, y: y * T + 12 });
    }
  }
  const pick = (n: number) => {
    const out: { x: number; y: number }[] = [];
    for (let i = 0; i < n && grass.length; i++) out.push(grass.splice(Math.floor(r() * grass.length), 1)[0]);
    return out;
  };
  return { read: [], board: [], benches: [], river: [], plaza, perches: pick(4), cats: pick(1) };
}

function paintRegionGround(tiles: Uint8Array, reserved: Uint8Array, theme: ThemeLook, r: () => number): HTMLCanvasElement {
  const w = MAP_W * T;
  const h = MAP_H * T;
  const c = makeCanvas(w, h);
  const ctx = context(c);
  const img = ctx.createImageData(w, h);
  const d = img.data;
  const tones = theme.ground.map(rgbOf);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const n = Math.sin(x * 0.041) + Math.cos(y * 0.047) + Math.sin((x + y) * 0.019) + Math.sin(x * 0.013 - y * 0.017) * 0.9;
      const v = n + ((((x & 3) * 7 + (y & 3) * 5) % 16) - 7.5) / 16 * 0.7;
      const tone = tones[v > 1.1 ? 0 : v < -1.1 ? 2 : 1];
      const i = (y * w + x) * 4;
      d[i] = tone[0];
      d[i + 1] = tone[1];
      d[i + 2] = tone[2];
      d[i + 3] = 255;
    }
  }
  ctx.putImageData(img, 0, 0);
  const at = (x: number, y: number) => (inside(x, y) ? tiles[idx(x, y)] : GRASS);
  for (let y = 0; y < MAP_H; y++) {
    for (let x = 0; x < MAP_W; x++) {
      const type = tiles[idx(x, y)];
      const px = x * T;
      const py = y * T;
      if (type === GRASS || type === WATER) {
        for (let i = 0; i < 6; i++) rect(ctx, px + Math.floor(r() * T), py + Math.floor(r() * T), 1, 1, theme.specks[i % 2]);
        if (theme.flowers && !reserved[idx(x, y)] && r() < 0.1) {
          const col = ["#ffffff", "#ffe066", "#ff9ab0", "#b7a0ff"][Math.floor(r() * 4)];
          const fx = px + 2 + Math.floor(r() * 12);
          const fy = py + 2 + Math.floor(r() * 12);
          rect(ctx, fx - 1, fy, 3, 1, col);
          rect(ctx, fx, fy - 1, 1, 3, col);
        }
      } else if (type === PATH || type === BRIDGE) {
        const [base, dark, light] = theme.path;
        rect(ctx, px, py, T, T, type === BRIDGE ? "#9a6a3c" : base);
        if (type === BRIDGE) {
          for (let row = 0; row < T; row += 4) {
            rect(ctx, px, py + row, T, 3, row % 8 ? "#a8703f" : "#b07a48");
            rect(ctx, px, py + row + 3, T, 1, "#6e4a2e");
          }
        } else for (let i = 0; i < 10; i++) rect(ctx, px + Math.floor(r() * T), py + Math.floor(r() * T), 1, 1, i % 3 ? dark : light);
      } else if (type === PLAZA) {
        rect(ctx, px, py, T, T, theme.plaza[1]);
        for (let sy = 0; sy < 2; sy++) {
          for (let sx = 0; sx < 2; sx++) {
            const tone = r() < 0.5 ? theme.plaza[0] : shade(theme.plaza[0], -0.06);
            rect(ctx, px + sx * 8 + 1, py + sy * 8 + 1, 7, 7, tone);
          }
        }
      } else if (type === ICE) {
        rect(ctx, px, py, T, T, "#bfe3f2");
        if (r() < 0.5) rect(ctx, px + Math.floor(r() * 10), py + Math.floor(r() * 12), 5, 1, "#ffffff");
        if (r() < 0.3) rect(ctx, px + Math.floor(r() * 12), py + Math.floor(r() * 12), 3, 1, "#8fc3dc");
      }
      if ((type === GRASS || type === PATH) && at(x, y + 1) === WATER) {
        rect(ctx, px, py + T - 4, T, 4, "#a8844f");
        rect(ctx, px, py + T - 2, T, 2, "#7d5f38");
      }
      if ((type === GRASS || type === PATH) && at(x, y - 1) === WATER) {
        rect(ctx, px, py, T, 3, "#a8844f");
        rect(ctx, px, py, T, 1, "#6e5230");
      }
    }
  }
  return c;
}
