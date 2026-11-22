import type { LokaLook, MapItem, Resident } from "./types";
import {
  makeBanyan,
  makeBarrel,
  makeBench,
  makeBuilding,
  makeBush,
  makeCrate,
  makeLamp,
  makeNoticeboard,
  makePlanter,
  makeReeds,
  makeRock,
  makeSignpost,
  makeStake,
  makeTree,
  type Sprite,
  type TreeKind,
} from "./art";
import { withGear } from "@/lib/looks";
import { buildSheet, type CharacterSheet, type Dir } from "./characters";
import { CIVIC, parseOpens, type CivicId } from "./places";
import { makeCivic } from "./places-art";
import { areaName, civicIn, linksOf, type Edge, type Link } from "./districts";
import { makeArch, makeBanner, makeCampfire, makeLantern, makeWaySign } from "./regions-art";
import { placeArt, type Placed } from "./sprites";
import { context, makeCanvas, rect, rgbOf, rng, shade } from "./pixels";

export const T = 16;
export const MAP_W = 56;
export const MAP_H = 46;

export const GRASS = 0;
export const PATH = 1;
export const PLAZA = 2;
export const WATER = 3;
export const BRIDGE = 4;
export const SOIL = 5;

export interface WorldObject {
  x: number;
  y: number;
  sprite: Sprite;
  night?: Sprite;
}

export interface Light {
  x: number;
  y: number;
  r: number;
  color: [number, number, number];
  kind: "lamp" | "window" | "fairy";
  phase: number;
}

export type NpcMode = "home" | "working";

export type Activity = "home" | "work" | "read" | "board" | "wander" | "visit" | "chase";

export interface Spot {
  x: number;
  y: number;
  face: Dir;
}

export interface Npc {
  resident: Resident;
  x: number;
  y: number;
  tx: number;
  ty: number;
  homeX: number;
  homeY: number;
  sheet: CharacterSheet;
  questSheet: CharacterSheet;
  dir: Dir;
  mode: NpcMode;
  path: { x: number; y: number }[];
  anim: number;
  moving: boolean;
  status: string;
  detail: string | null;
  guide: boolean;
  jobs: string[];
  idleAt: number | null;
  phase: number;
  strike: number;
  activity: Activity;
  spot: Spot | null;
  arrived: boolean;
  wait: number;
  roams: number;
  hold: number;
  asking: number;
  chasedAt: number;
  chats: number;
  visit: { id: string; text: string; tries: number } | null;
  say: { text: string; until: number } | null;
  cloudUntil: number;
  house: { x: number; y: number } | null;
}

export interface Critter {
  id: string;
  name: string;
  x: number;
  y: number;
  homeX: number;
  homeY: number;
  sheet: CharacterSheet;
  dir: Dir;
  target: { x: number; y: number } | null;
  wait: number;
  anim: number;
  moving: boolean;
  talk: boolean;
  greeting: string | null;
  say: { text: string; until: number } | null;
  staffed?: boolean;
}

export interface Plot {
  index: number;
  x: number;
  y: number;
  tx: number;
  ty: number;
}

export interface Label {
  x: number;
  y: number;
  w: number;
  text: string;
}

export interface Spots {
  read: Spot[];
  board: Spot[];
  benches: Spot[];
  river: Spot[];
  plaza: Spot[];
  perches: { x: number; y: number }[];
  cats: { x: number; y: number }[];
}

export interface Place {
  id: string;
  kind: "civic" | "house" | "building" | "portal" | "academy" | "campus" | "exit";
  ref: string;
  to?: string | null;
  zone?: [number, number, number, number];
  edge?: Edge;
  name: string;
  door: { x: number; y: number };
  goals: [number, number][];
  anchors: Record<string, { x: number; y: number }>;
}

export type SceneWeather = "clear" | "rain" | "leaves" | "snow" | "fog";

export interface Fauna {
  peacock: boolean;
  water: boolean;
  board: boolean;
  petals: boolean;
}

export interface World {
  region: RegionId;
  w: number;
  h: number;
  riverTop: (x: number) => number;
  bridge: [number, number];
  name: string;
  backdrop: string;
  fauna: Fauna;
  weather: SceneWeather | null;
  ambientTime: "dawn" | "day" | "dusk" | "night" | null;
  tiles: Uint8Array;
  solid: Uint8Array;
  ground: HTMLCanvasElement;
  waterFrames: HTMLCanvasElement[];
  waterTop: number;
  objects: WorldObject[];
  lights: Light[];
  npcs: Npc[];
  critters: Critter[];
  placements: Record<string, { x: number; y: number }>;
  plots: Plot[];
  labels: Label[];
  chimneys: { x: number; y: number; owner?: string }[];
  board: { x: number; y: number };
  gate: { x: number; y: number };
  spawn: { x: number; y: number };
  plaza: { x: number; y: number };
  riverCenter: (x: number) => number;
  spots: Spots;
  marketLamp: Light | null;
  places: Place[];
}

export type RegionId = string;

export const HEARTH: RegionId = "hearth";

export const HEARTH_W = 80;
export const HEARTH_H = 62;

const TOWN_SLOTS: [number, number][] = [
  [28, 31],
  [48, 31],
  [62, 31],
  [10, 6],
  [62, 6],
  [26, 6],
  [48, 6],
];

const MEADOW_SLOTS: [number, number][] = [
  [8, 50],
  [22, 50],
  [50, 50],
  [64, 50],
];

export function riverTop(x: number, base = 30): number {
  return base + Math.round(Math.sin(x * 0.17 + 0.6) * 1.1);
}

export interface Geo {
  w: number;
  h: number;
  river: number;
  bridge: [number, number];
  plaza: { x: number; y: number };
}

export const REGION_GEO: Geo = { w: MAP_W, h: MAP_H, river: 30, bridge: [26, 29], plaza: { x: 28, y: 17.5 } };
const HEARTH_GEO: Geo = { w: HEARTH_W, h: HEARTH_H, river: 44, bridge: [38, 41], plaza: { x: 40, y: 23.5 } };

export const LOT_W = 9;
export const LOT_H = 7;
const DOOR_ROW = 5;

function blockFootprint(k: Kit, tx: number, ty: number): void {
  for (let y = ty + 2; y < ty + 5; y++) for (let x = tx - 1; x < tx + LOT_W - 1; x++) kitBlock(k, x, y);
}

type Spot2 = { x: number; y: number };

function artSign(art: Placed | null, fallback: Spot2 & { w: number }): Spot2 & { w: number } {
  if (!art?.art.sign) return fallback;
  const [sx, sy, sw, sh] = art.art.sign;
  return { ...art.at([sx + sw / 2, sy + sh / 2]), w: sw / art.art.density };
}

export function artPoints(art: Placed | null, name: string, fallback: Spot2[]): Spot2[] {
  if (!art) return fallback;
  return (art.art.points[name] ?? []).map(art.at);
}

const ART_ANCHORS: Record<string, string> = { clock: "clock_face", clockRim: "clock_rim", flagTop: "flag" };
const LANTERN_ABOVE_DOOR = 32;

function roofTop(art: Placed | null, ty: number): number {
  return art ? Math.min(ty - 1, Math.floor(art.y / T)) : ty - 1;
}

export function placeCivic(k: Kit, id: CivicId, name: string, tx: number, ty: number): void {
  const s = makeCivic(id);
  const baseX = tx * T;
  const baseY = (ty + 5) * T;
  const ox = baseX - 4;
  const oy = baseY - s.day.img.height;
  const art = placeArt(s.day, baseX, baseY);
  kitReserve(k, tx - 1, ty - 1, tx + LOT_W - 1, ty + LOT_H - 1);
  kitBuild(k, tx - 1, roofTop(art, ty), tx + LOT_W - 1, ty + LOT_H - 1);
  k.objects.push({ x: baseX, y: baseY, sprite: s.day, night: s.night });
  blockFootprint(k, tx, ty);
  const sign = artSign(art, { x: ox + s.sign.x, y: oy + s.sign.y, w: s.sign.w });
  if (sign.w) k.labels.push({ ...sign, text: name });
  const chimney = artPoints(art, "chimney", s.chimney ? [{ x: ox + s.chimney.x, y: oy + s.chimney.y }] : [])[0];
  if (chimney) k.chimneys.push({ ...chimney, owner: `civic:${id}` });
  const anchors: Record<string, Spot2> = {};
  for (const key of new Set([...Object.keys(s.anchors), ...Object.keys(ART_ANCHORS)])) {
    const p = s.anchors[key];
    const spot = artPoints(art, ART_ANCHORS[key] ?? key, p ? [{ x: ox + p.x, y: oy + p.y }] : [])[0];
    if (spot) anchors[key] = spot;
  }
  const door = artPoints(art, "door", [])[0];
  if (id === "library" && door) anchors.glow = { x: door.x, y: door.y - LANTERN_ABOVE_DOOR };
  k.places.push({ id: `civic:${id}`, kind: "civic", ref: id, name, anchors, ...doorOf(tx, ty) });
  if (id === "forge") {
    kitBlock(k, tx + 5, ty + 5);
    kitBlock(k, tx + 6, ty + 5);
  }
}

const kin = (k: Kit, x: number, y: number) => x >= 0 && y >= 0 && x < k.w && y < k.h;
const kix = (k: Kit, x: number, y: number) => y * k.w + x;

export interface Kit {
  r: () => number;
  w: number;
  h: number;
  tiles: Uint8Array;
  solid: Uint8Array;
  reserved: Uint8Array;
  built: Uint8Array;
  kept: Uint8Array;
  objects: WorldObject[];
  lights: Light[];
  labels: Label[];
  chimneys: { x: number; y: number; owner?: string }[];
  places: Place[];
  placements: Record<string, { x: number; y: number }>;
  critters: Critter[];
  portalSlots: PortalSlot[];
}

export function makeKit(r: () => number, w = MAP_W, h = MAP_H): Kit {
  return {
    r,
    w,
    h,
    tiles: new Uint8Array(w * h),
    solid: new Uint8Array(w * h),
    reserved: new Uint8Array(w * h),
    built: new Uint8Array(w * h),
    kept: new Uint8Array(w * h),
    objects: [],
    lights: [],
    labels: [],
    chimneys: [],
    places: [],
    placements: {},
    critters: [],
    portalSlots: [],
  };
}

export function kitBuild(k: Kit, x0: number, y0: number, x1: number, y1: number): void {
  for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) if (kin(k, x, y)) k.built[kix(k, x, y)] = 1;
}

export function kitBlock(k: Kit, x: number, y: number): void {
  if (!kin(k, x, y)) return;
  k.solid[kix(k, x, y)] = 1;
  k.reserved[kix(k, x, y)] = 1;
}

export function kitReserve(k: Kit, x0: number, y0: number, x1: number, y1: number): void {
  for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) if (kin(k, x, y)) k.reserved[kix(k, x, y)] = 1;
}

export function kitKeep(k: Kit, x0: number, y0: number, x1: number, y1: number): void {
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      if (!kin(k, x, y)) continue;
      k.reserved[kix(k, x, y)] = 1;
      k.kept[kix(k, x, y)] = 1;
    }
  }
}

let lampSprites: [Sprite, Sprite] | null = null;

export function kitLamp(k: Kit, tx: number, ty: number): Light {
  lampSprites ??= [makeLamp(false), makeLamp(true)];
  kitBlock(k, tx, ty);
  const x = tx * T + 8;
  const y = ty * T + 13;
  k.objects.push({ x, y, sprite: lampSprites[0], night: lampSprites[1] });
  const head = artPoints(placeArt(lampSprites[0], x, y), "light", [{ x, y: y - 22 }])[0] ?? { x, y: y - 22 };
  const light: Light = { ...head, r: 46, color: [255, 200, 120], kind: "lamp", phase: k.r() * 10 };
  k.lights.push(light);
  return light;
}

export function kitFree(k: Kit, x: number, y: number, rx: number, ry: number): boolean {
  for (let yy = y - ry; yy <= y + ry; yy++) {
    for (let xx = x - rx; xx <= x + rx; xx++) if (!kin(k, xx, yy) || k.reserved[kix(k, xx, yy)]) return false;
  }
  return true;
}

export function kitSpot(k: Kit, rr: () => number, rx: number, ry: number, tries = 1500): { x: number; y: number } | null {
  for (let n = 0; n < tries; n++) {
    const x = 4 + rx + Math.floor(rr() * (k.w - 8 - rx * 2));
    const y = 4 + ry + Math.floor(rr() * (k.h - 8 - ry * 2));
    if (kitFree(k, x, y, rx, ry)) return { x, y };
  }
  return null;
}

const hexColor = (v: unknown, fallback: string) => (typeof v === "string" && /^#[0-9a-fA-F]{6}$/.test(v) ? v : fallback);

export const doorOf = (tx: number, ty: number) => ({
  door: { x: tx * T + 48, y: (ty + 5) * T + 8 },
  goals: [[tx + 2, ty + 5], [tx + 3, ty + 5]] as [number, number][],
});

export function placeBuilding(k: Kit, item: MapItem, tx: number, ty: number): void {
  const rr = rng(seedOf(item.id));
  const style = {
    roof: hexColor(item.data.roof, ["#b8513a", "#3f6fb0", "#6c4f9e", "#2f7a5a"][Math.floor(rr() * 4)]),
    wall: hexColor(item.data.wall, "#f3e6c8"),
    awning: [hexColor(item.data.awning, "#e8b64a"), "#fbf3e0"] as [string, string],
  };
  const b = makeBuilding(style);
  const baseX = tx * T;
  const baseY = (ty + 5) * T;
  k.objects.push({ x: baseX, y: baseY, sprite: b.day, night: b.night });
  blockFootprint(k, tx, ty);
  const art = placeArt(b.day, baseX, baseY);
  k.labels.push({ ...artSign(art, { x: baseX - 4 + b.sign.x, y: baseY - 96 + b.sign.y, w: b.sign.w }), text: item.name.slice(0, 18) });
  const chimney = artPoints(art, "chimney", [{ x: baseX - 4 + b.chimney.x, y: baseY - 96 + b.chimney.y }])[0];
  if (chimney) k.chimneys.push(chimney);
  k.placements[item.id] = { x: (tx + 3) * T, y: (ty + 6) * T + 8 };
  if (parseOpens(item.data.opens)) k.places.push({ id: `item:${item.id}`, kind: "building", ref: item.id, name: item.name, anchors: {}, ...doorOf(tx, ty) });
}

export function placeCampus(k: Kit, tx: number, ty: number): void {
  for (const [wx, text] of [[tx - LOT_W, "Lecture halls"], [tx + LOT_W, "Study rooms"]] as [number, string][]) {
    const wing = makeBuilding({ roof: "#3f6fb0", wall: "#efe7f8", awning: ["#4b3a8c", "#fbf3e0"] });
    const baseY = (ty + 6) * T;
    k.objects.push({ x: wx * T, y: baseY, sprite: wing.day, night: wing.night });
    const art = placeArt(wing.day, wx * T, baseY);
    k.labels.push({ ...artSign(art, { x: wx * T - 4 + wing.sign.x, y: baseY - 96 + wing.sign.y, w: wing.sign.w }), text });
    blockFootprint(k, wx, ty + 1);
  }
  placeAcademy(k, tx, ty);
  const place = k.places[k.places.length - 1];
  place.id = "campus";
  place.kind = "campus";
  place.ref = "campus";
  place.name = "University";
  k.labels[k.labels.length - 1].text = "University";
  k.placements.campus = k.placements.academy;
  delete k.placements.academy;
}

export function placeAcademy(k: Kit, tx: number, ty: number): void {
  const b = makeBuilding({ roof: "#4b3a8c", wall: "#f1e9ff", awning: ["#e8b64a", "#fbf3e0"] });
  const baseX = tx * T;
  const baseY = (ty + 5) * T;
  k.objects.push({ x: baseX, y: baseY, sprite: b.day, night: b.night });
  blockFootprint(k, tx, ty);
  const art = placeArt(b.day, baseX, baseY);
  k.labels.push({ ...artSign(art, { x: baseX - 4 + b.sign.x, y: baseY - 96 + b.sign.y, w: b.sign.w }), text: "Academy" });
  k.placements.academy = { x: (tx + 3) * T, y: (ty + 6) * T + 8 };
  k.places.push({ id: "academy", kind: "academy", ref: "academy", name: "Academy", anchors: {}, ...doorOf(tx, ty) });
}

export function placePortal(k: Kit, id: string, name: string, to: string | null, tx: number, ty: number, style: "arch" | "sign"): void {
  kitReserve(k, tx - 2, ty - 2, tx + 4, ty + 2);
  kitBuild(k, tx - 2, ty - 2, tx + 4, ty + 2);
  k.placements[id] = { x: (tx + 1) * T, y: (ty + 1) * T + 8 };
  const door = { x: (tx + 1) * T, y: ty * T + 12 };
  if (style === "arch") {
    const sprite = makeArch(seedOf(to ?? "hearth"));
    kitBlock(k, tx - 1, ty);
    kitBlock(k, tx + 2, ty);
    k.objects.push({ x: (tx + 1) * T, y: ty * T + 15, sprite });
    const art = placeArt(sprite, (tx + 1) * T, ty * T + 15);
    k.labels.push({ x: (tx + 1) * T, y: (art ? art.y : ty * T + 15 - sprite.ay) - 4, w: 26, text: name.slice(0, 18) });
  } else {
    const sprite = makeWaySign();
    kitBlock(k, tx + 2, ty);
    k.objects.push({ x: (tx + 2) * T + 8, y: ty * T + 14, sprite });
    const art = placeArt(sprite, (tx + 2) * T + 8, ty * T + 14);
    k.labels.push({ x: (tx + 2) * T + 8, y: (art ? art.y + art.h * 0.3 : ty * T + 14 - sprite.ay) - 3, w: 22, text: name.slice(0, 18) });
  }
  k.places.push({ id: `portal:${id}`, kind: "portal", ref: id, name, to, anchors: {}, door, goals: [[tx, ty], [tx + 1, ty]], zone: [tx, ty, tx + 1, ty] });
}

export interface PortalSlot {
  x: number;
  y: number;
  spur: [number, number, number, number] | null;
}

function edgeTiles(k: Kit, link: Link, depth = 40): [number, number][] {
  const out: [number, number][] = [];
  for (let d = 0; d < depth; d++) {
    const row: [number, number][] =
      link.edge === "w" ? [[d, link.at], [d, link.at + 1]]
      : link.edge === "e" ? [[k.w - 1 - d, link.at], [k.w - 1 - d, link.at + 1]]
      : link.edge === "n" ? [[link.at, d], [link.at + 1, d]]
      : [[link.at, k.h - 1 - d], [link.at + 1, k.h - 1 - d]];
    if (depth === 40 && d > 0 && row.every(([x, y]) => k.tiles[kix(k, x, y)] === PATH)) break;
    out.push(...row);
  }
  return out;
}

export function carveExits(k: Kit, links: Link[]): void {
  for (const l of links) for (const [x, y] of edgeTiles(k, l)) k.tiles[kix(k, x, y)] = PATH;
}

export function nearExit(k: Kit, links: Link[], x: number, y: number): boolean {
  return links.some((l) => {
    if (l.edge === "w" || l.edge === "e") {
      const ex = l.edge === "w" ? 0 : k.w - 1;
      return Math.abs(x - ex) < 4 && y >= l.at - 2 && y <= l.at + 3;
    }
    const ey = l.edge === "n" ? 0 : k.h - 1;
    return Math.abs(y - ey) < 4 && x >= l.at - 2 && x <= l.at + 3;
  });
}

export function openExits(k: Kit, links: Link[], nameOf: (id: string) => string): void {
  const sign = makeSignpost();
  for (const l of links) {
    for (const [x, y] of edgeTiles(k, l, 4)) {
      k.solid[kix(k, x, y)] = 0;
      kitKeep(k, x, y, x + 1, y + 1);
    }
    const name = nameOf(l.to);
    const side = l.edge === "w" || l.edge === "e";
    const zone: [number, number, number, number] =
      l.edge === "w" ? [0, l.at, 0, l.at + 1]
      : l.edge === "e" ? [k.w - 1, l.at, k.w - 1, l.at + 1]
      : l.edge === "n" ? [l.at, 0, l.at + 1, 0]
      : [l.at, k.h - 1, l.at + 1, k.h - 1];
    const door = side ? { x: zone[0] * T + 8, y: (l.at + 1) * T } : { x: (l.at + 1) * T, y: zone[1] * T + 8 };
    const goals: [number, number][] = side ? [[zone[0], l.at], [zone[0], l.at + 1]] : [[l.at, zone[1]], [l.at + 1, zone[1]]];
    k.places.push({ id: `exit:${l.to}`, kind: "exit", ref: l.to, name, to: l.to, anchors: {}, door, goals, zone, edge: l.edge });
    const [sx, sy] =
      l.edge === "w" ? [3, l.at - 1]
      : l.edge === "e" ? [k.w - 4, l.at - 1]
      : l.edge === "n" ? [l.at + 2, 3]
      : [l.at + 2, k.h - 4];
    if (k.solid[kix(k, sx, sy)] && !(sx < 3 || sy < 3 || sx >= k.w - 3 || sy >= k.h - 3)) continue;
    k.solid[kix(k, sx, sy)] = 1;
    kitKeep(k, sx, sy, sx + 1, sy + 1);
    k.objects.push({ x: sx * T + 8, y: sy * T + 13, sprite: sign });
    k.labels.push({ x: sx * T + 8, y: sy * T + 13 - 17.5, w: 18, text: `To ${name}`.slice(0, 20) });
  }
}

export function arrivalFor(world: Pick<World, "places" | "w" | "h">, cameFrom: string): { x: number; y: number; dir: Dir } | null {
  const exit = world.places.find((p) => p.kind === "exit" && p.to === cameFrom);
  if (exit?.zone && exit.edge) {
    const [x0, y0] = exit.zone;
    if (exit.edge === "w") return { x: 2 * T + 8, y: (y0 + 1) * T + 4, dir: "right" };
    if (exit.edge === "e") return { x: (world.w - 3) * T + 8, y: (y0 + 1) * T + 4, dir: "left" };
    if (exit.edge === "n") return { x: (x0 + 1) * T, y: 2 * T + 13, dir: "down" };
    return { x: (x0 + 1) * T, y: (world.h - 3) * T + 13, dir: "up" };
  }
  const portal = world.places.find((p) => p.kind === "portal" && (p.to ?? HEARTH) === cameFrom);
  if (!portal) return null;
  const below = portal.door.y < (world.h * T) / 2;
  return { x: portal.door.x, y: portal.door.y + (below ? 2.5 : -2.5) * T, dir: below ? "down" : "up" };
}

export function addFolk(k: Kit, item: MapItem, tx: number, ty: number, look: LokaLook): void {
  const x = tx * T + 8;
  const y = ty * T + 13;
  kitReserve(k, tx, ty, tx + 1, ty + 1);
  k.placements[item.id] = { x, y: y + T };
  k.critters.push({
    id: item.id,
    name: item.name,
    x,
    y,
    homeX: x,
    homeY: y,
    sheet: buildSheet(look),
    dir: "down",
    target: null,
    wait: k.r() * 3,
    anim: 0,
    moving: false,
    talk: true,
    greeting: typeof item.data.greeting === "string" ? item.data.greeting : null,
    say: null,
    staffed: typeof item.data.agent_id === "string",
  });
}

export function decorSprite(id: string): Sprite | null {
  if (id === "lantern-post") return makeLantern();
  if (id === "reading-bench") return makeBench();
  if (id === "campfire") return makeCampfire();
  if (id === "aurora-banner") return makeBanner("#7ee0d2");
  if (id.startsWith("banner:")) return makeBanner(["#e85d75", "#f2c14e", "#9b7be0", "#3f6fb0", "#2f7a5a", "#ff9a5a"][seedOf(id) % 6]);
  return null;
}

export function lotRoof(): number {
  const b = makeBuilding({ roof: "#b8513a", wall: "#f3e6c8", awning: ["#e8b64a", "#fbf3e0"] });
  const art = placeArt(b.day, 0, 5 * T);
  return Math.max(1, Math.ceil(-(art ? art.y : 5 * T - b.day.ay) / T));
}

export function findLot(k: Kit, rr: () => number, roof = lotRoof()): { x: number; y: number } | null {
  for (let n = 0; n < 4000; n++) {
    const tx = 3 + Math.floor(rr() * (k.w - LOT_W - 4));
    const ty = roof + 2 + Math.floor(rr() * (k.h - LOT_H - roof - 2));
    let ok = true;
    for (let y = ty - roof; y < ty + LOT_H && ok; y++) {
      for (let x = tx - 1; x < tx + LOT_W - 1 && ok; x++) {
        if (!kin(k, x, y) || k.kept[kix(k, x, y)] || (y <= ty + DOOR_ROW && k.built[kix(k, x, y)]) || (y > ty && k.reserved[kix(k, x, y)])) ok = false;
      }
    }
    if (ok) return { x: tx, y: ty };
  }
  return null;
}

export function placeItems(k: Kit, items: MapItem[], pendingBuildings: MapItem[]): void {
  const bench = makeBench();
  const crate = makeCrate();
  const barrel = makeBarrel();
  const signpost = makeSignpost();
  const roof = lotRoof();
  for (const item of items) {
    const rr = rng(seedOf(item.id));
    if (item.kind === "building") {
      if (!pendingBuildings.includes(item)) continue;
      const spot = findLot(k, rr, roof);
      if (!spot) continue;
      kitReserve(k, spot.x - 1, spot.y + 1, spot.x + LOT_W - 1, spot.y + LOT_H);
      kitBuild(k, spot.x - 1, spot.y - roof, spot.x + LOT_W - 1, spot.y + DOOR_ROW + 1);
      placeBuilding(k, item, spot.x, spot.y);
      continue;
    }
    if (item.kind === "decoration") {
      const sprite = String(item.data.sprite ?? "bush");
      const count = Math.max(1, Math.min(12, Number(item.data.count ?? 1)));
      const anchor = kitSpot(k, rr, 2, 2);
      if (!anchor) continue;
      k.placements[item.id] = { x: anchor.x * T + 8, y: (anchor.y + 2) * T + 8 };
      let placed = 0;
      for (let ring = 0; ring <= 5 && placed < count; ring++) {
        for (let dy = -ring; dy <= ring && placed < count; dy++) {
          for (let dx = -ring; dx <= ring && placed < count; dx++) {
            if (Math.max(Math.abs(dx), Math.abs(dy)) !== ring || (dx + dy) % 2 !== 0) continue;
            const x = anchor.x + dx;
            const y = anchor.y + dy;
            if (!kitFree(k, x, y, 0, 0)) continue;
            const color = hexColor(item.data.color, "#ff9ab0");
            if (sprite === "lamp") {
              kitReserve(k, x, y, x + 1, y + 1);
              kitLamp(k, x, y);
            } else {
              const spr =
                sprite === "tree" ? makeTree("green", rr)
                : sprite === "blossom" ? makeTree("blossom", rr)
                : sprite === "autumn" ? makeTree("autumn", rr)
                : sprite === "pine" ? makeTree("pine", rr)
                : sprite === "flowers" ? makeBush(rr, color)
                : sprite === "rock" ? makeRock(rr)
                : sprite === "bench" ? bench
                : sprite === "planter" ? makePlanter(color)
                : sprite === "crate" ? crate
                : sprite === "barrel" ? barrel
                : sprite === "sign" ? signpost
                : makeBush(rr, null);
              kitBlock(k, x, y);
              kitReserve(k, x, y, x + 1, y + 1);
              k.objects.push({ x: x * T + 8, y: y * T + 13, sprite: spr });
              if (sprite === "sign") k.labels.push({ x: x * T + 8, y: y * T + 13 - 17.5, w: 18, text: item.name.slice(0, 14) });
            }
            placed++;
          }
        }
      }
      continue;
    }
    if (item.kind === "portal") {
      const spot = k.portalSlots.shift();
      if (!spot) continue;
      const to = typeof item.data.to_region === "string" ? item.data.to_region : null;
      placePortal(k, item.id, String(item.data.label || item.name), to, spot.x, spot.y, item.data.style === "sign" ? "sign" : "arch");
      continue;
    }
    if (item.kind === "npc" && item.look) {
      const spot = kitSpot(k, rr, 1, 1);
      if (spot) addFolk(k, item, spot.x, spot.y, item.look);
      continue;
    }
    if (item.kind === "critter" && item.look) {
      const spot = kitSpot(k, rr, 1, 1);
      if (!spot) continue;
      const x = spot.x * T + 8;
      const y = spot.y * T + 13;
      k.placements[item.id] = { x, y: y + T };
      k.critters.push({
        id: item.id,
        name: item.name,
        x,
        y,
        homeX: x,
        homeY: y,
        sheet: buildSheet(item.look),
        dir: "down",
        target: null,
        wait: rr() * 3,
        anim: 0,
        moving: false,
        talk: false,
        greeting: null,
        say: null,
      });
    }
  }
}

export function seedOf(id: string): number {
  let h = 2166136261;
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0) % 2147483646 + 1;
}

export function buildWorld(guide: Resident | null, residents: Resident[], items: MapItem[] = []): World {
  const g = HEARTH_GEO;
  const W = g.w;
  const H = g.h;
  const r = rng(20260927);
  const k = makeKit(r, W, H);
  const { tiles, solid, reserved } = k;
  const idx = (x: number, y: number) => y * W + x;
  const inside = (x: number, y: number) => x >= 0 && y >= 0 && x < W && y < H;
  const top = (x: number) => riverTop(x, g.river);

  const fill = (x0: number, y0: number, x1: number, y1: number, type: number) => {
    for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) if (inside(x, y)) tiles[idx(x, y)] = type;
  };
  const block = (x: number, y: number) => kitBlock(k, x, y);
  const reserve = (x0: number, y0: number, x1: number, y1: number) => kitReserve(k, x0, y0, x1, y1);

  const plazaC = g.plaza;
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      const dx = (x + 0.5 - plazaC.x) / 7.2;
      const dy = (y + 0.5 - plazaC.y) / 5.6;
      if (dx * dx + dy * dy <= 1) tiles[idx(x, y)] = PLAZA;
    }
  }

  const assigned = TOWN_SLOTS.concat(MEADOW_SLOTS).map((slot, i) => ({ slot, resident: residents[i] ?? null, i }));
  const civic = civicIn(null);

  fill(8, 11, 72, 13, PATH);
  fill(39, 4, 41, 19, PATH);
  fill(3, 24, 77, 26, PATH);
  fill(10, 36, 70, 38, PATH);
  fill(39, 27, 41, 57, PATH);
  fill(8, 56, 72, 58, PATH);
  const streetOf = (ty: number) => (ty < 11 ? 11 : ty < 24 ? 24 : ty < 36 ? 36 : 56);
  for (const c of civic) {
    const [tx, ty] = c.slot;
    if (ty + 5 < streetOf(ty)) fill(tx + 2, ty + 5, tx + 4, streetOf(ty), PATH);
  }
  for (const { slot, resident, i } of assigned) {
    const [tx, ty] = slot;
    const meadow = i >= TOWN_SLOTS.length;
    const street = streetOf(ty);
    if (resident || !meadow) fill(tx + 2, ty + 5, tx + 4, street, PATH);
    else fill(tx + 3, ty + 5, tx + 4, street, PATH);
  }

  const links = linksOf(HEARTH);
  carveExits(k, links);
  const portalCount = items.filter((i) => i.kind === "portal").length;
  k.portalSlots = HEARTH_PORTAL_SLOTS.slice(0, portalCount);
  for (const slot of k.portalSlots) if (slot.spur) fill(slot.spur[0], slot.spur[1], slot.spur[2], slot.spur[3], PATH);

  for (let x = 0; x < W; x++) {
    const t = top(x);
    for (let y = t; y < t + 3; y++) tiles[idx(x, y)] = WATER;
  }
  const [b0, b1] = g.bridge;
  for (let y = g.river - 2; y < g.river + 5; y++) {
    for (let x = b0; x <= b1; x++) {
      if (tiles[idx(x, y)] === WATER) tiles[idx(x, y)] = BRIDGE;
      else if (x > b0 && x < b1) tiles[idx(x, y)] = PATH;
    }
  }

  for (let i = 0; i < W * H; i++) {
    if (tiles[i] === WATER) solid[i] = 1;
    if (tiles[i] !== GRASS) reserved[i] = 1;
  }

  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      if (x < 3 || y < 3 || x >= W - 3 || y >= H - 3) block(x, y);
    }
  }

  const { objects, lights, labels, chimneys, places } = k;
  const npcs: Npc[] = [];
  const plots: Plot[] = [];
  const crate = makeCrate();
  const barrel = makeBarrel();
  const signpost = makeSignpost();
  const stake = makeStake();

  const addLamp = (tx: number, ty: number) => kitLamp(k, tx, ty);

  const addNpc = (resident: Resident, tx: number, ty: number, dir: Dir, house: { x: number; y: number } | null, isGuide = false) => {
    reserve(tx, ty, tx + 1, ty + 1);
    const x = tx * T + 8;
    const y = ty * T + 13;
    const quest = { ...resident.look, accessories: withGear(resident.look.accessories ?? [], resident.quest) };
    npcs.push({
      resident,
      tx,
      ty,
      x,
      y,
      homeX: x,
      homeY: y,
      sheet: buildSheet(resident.look),
      questSheet: buildSheet(quest),
      dir,
      mode: "home",
      path: [],
      anim: 0,
      moving: false,
      status: "idle",
      detail: null,
      guide: isGuide,
      jobs: [],
      idleAt: null,
      phase: npcs.length * 0.37,
      strike: -1,
      activity: "home",
      spot: null,
      arrived: true,
      wait: 3 + ((npcs.length * 7) % 9),
      roams: 0,
      hold: 0,
      asking: 0,
      chasedAt: 0,
      chats: 0,
      visit: null,
      say: null,
      cloudUntil: 0,
      house,
    });
  };

  openExits(k, links, areaName);

  let marketLamp: Light | null = null;
  for (const c of civic) {
    const [tx, ty] = c.slot;
    placeCivic(k, c.id, c.name, tx, ty);
    if (c.id === "market") marketLamp = addLamp(tx + LOT_W - 1, ty + 4);
    else addLamp(tx - 2, ty + 4);
  }
  const buildingQueue = items.filter((i) => i.kind === "building");
  const roof = lotRoof();

  for (const { slot, resident, i } of assigned) {
    const [tx, ty] = slot;
    const meadow = i >= TOWN_SLOTS.length;
    reserve(tx - 1, ty - 1, tx + LOT_W - 1, ty + LOT_H - 1);
    kitBuild(k, tx - 1, ty - roof, tx + LOT_W - 1, ty + LOT_H - 1);
    const claimed = resident ? null : buildingQueue.shift();
    if (claimed) {
      placeBuilding(k, claimed, tx, ty);
      addLamp(tx - 2, ty + 4);
      continue;
    }
    if (!resident) {
      fill(tx, ty + 1, tx + 6, ty + 5, SOIL);
      for (const [sx, sy] of [[tx, ty + 1], [tx + 6, ty + 1], [tx, ty + 5], [tx + 6, ty + 5]]) {
        objects.push({ x: sx * T, y: sy * T, sprite: stake });
      }
      block(tx + 3, ty + 4);
      objects.push({ x: tx * T + 56, y: ty * T + 4 * T + 14, sprite: signpost });
      labels.push({ x: tx * T + 56, y: ty * T + 4 * T + 14 - 17.5, w: 18, text: "Open plot" });
      plots.push({ index: i, tx, ty, x: tx * T + 56, y: (ty + 5) * T + 4 });
      if (!meadow) addLamp(tx - 2, ty + 4);
      continue;
    }
    const b = makeBuilding(resident.shop);
    const baseX = tx * T;
    const baseY = (ty + 5) * T;
    objects.push({ x: baseX, y: baseY, sprite: b.day, night: b.night });
    blockFootprint(k, tx, ty);
    const art = placeArt(b.day, baseX, baseY);
    labels.push({ ...artSign(art, { x: baseX - 4 + b.sign.x, y: baseY - 96 + b.sign.y, w: b.sign.w }), text: resident.business });
    const chimney = artPoints(art, "chimney", [{ x: baseX - 4 + b.chimney.x, y: baseY - 96 + b.chimney.y }])[0];
    if (chimney) chimneys.push({ ...chimney, owner: resident.id });
    addNpc(resident, tx + 4, ty + 5, "down", { x: baseX + 44, y: baseY - 100 });
    places.push({ id: `house:${resident.id}`, kind: "house", ref: resident.id, name: resident.business, anchors: {}, ...doorOf(tx, ty) });
    addLamp(tx - 2, ty + 4);
    block(tx + LOT_W - 1, ty + 4);
    objects.push({ x: (tx + LOT_W - 1) * T + 8, y: (ty + 4) * T + 13, sprite: r() > 0.5 ? crate : barrel });
  }

  if (guide) addNpc(guide, 37, 27, "down", null, true);

  const cx = Math.round(plazaC.x);
  const cy = Math.round(plazaC.y);
  const banyan = makeBanyan(r);
  objects.push({ x: cx * T, y: (cy + 1) * T, sprite: banyan });
  for (let y = cy - 2; y < cy + 1; y++) for (let x = cx - 2; x < cx + 2; x++) block(x, y);
  kitKeep(k, cx - 4, cy - 4, cx + 4, cy + 2);
  if (!placeArt(banyan, cx * T, (cy + 1) * T)) banyan.bulbs.forEach((b, i) => {
    lights.push({
      x: cx * T - banyan.ax + b.x,
      y: (cy + 1) * T - banyan.ay + b.y,
      r: 14,
      color: rgbOf(b.color),
      kind: "fairy",
      phase: i * 0.7,
    });
  });

  const bench = makeBench();
  for (const [x, y] of HEARTH_BENCHES) {
    block(x, y);
    block(x + 1, y);
    objects.push({ x: (x + 1) * T, y: y * T + 13, sprite: bench });
  }
  const planterColors = ["#e85d75", "#f2c14e", "#9b7be0", "#ff9a5a"];
  [[cx - 5, cy - 4], [cx + 4, cy - 4], [cx - 6, cy + 4], [cx + 5, cy + 4]].forEach(([x, y], i) => {
    block(x, y);
    objects.push({ x: x * T + 8, y: y * T + 14, sprite: makePlanter(planterColors[i]) });
  });
  const plazaLamps = [[cx - 7, cy], [cx + 6, cy], [cx - 4, cy - 6], [cx + 3, cy - 6]].map(([x, y]) => addLamp(x, y));
  for (const [x, y] of [[b0, g.river - 3], [b1, g.river - 3], [b0, g.river + 5], [b1, g.river + 5]]) addLamp(x, y);
  for (const [x, y] of [[16, 56], [30, 56], [50, 56], [64, 56]]) {
    if (tiles[idx(x, y)] === PATH && !solid[idx(x, y - 1)] && tiles[idx(x, y - 1)] === GRASS) addLamp(x, y - 1);
  }

  const [nx, ny] = HEARTH_BOARD;
  block(nx, ny);
  block(nx + 1, ny);
  objects.push({ x: (nx + 1) * T, y: ny * T + 14, sprite: makeNoticeboard() });
  reserve(cx - 2, cy + 4, cx + 3, cy + 9);

  const trees: TreeKind[] = ["green", "green", "green", "blossom", "autumn", "pine"];
  const borderTree = () => makeTree(r() > 0.45 ? "pine" : r() > 0.3 ? "green" : "autumn", r);
  for (let y = -1; y < H + 2; y += 2) {
    for (let x = -1; x < W + 2; x += 2) {
      const edge = x < 3 || y < 3 || x >= W - 3 || y >= H - 3;
      if (!edge) continue;
      const ex = Math.max(0, Math.min(W - 1, x));
      const ey = Math.max(0, Math.min(H - 1, y));
      if (tiles[idx(ex, ey)] === WATER && y > 0 && y < H - 1) continue;
      if (nearExit(k, links, ex, ey)) continue;
      const jx = (r() - 0.5) * 10;
      const jy = (r() - 0.5) * 8;
      objects.push({ x: x * T + 8 + jx, y: y * T + 14 + jy + (x % 4 === 1 ? 8 : 0), sprite: borderTree() });
    }
  }

  const freeBox = (x: number, y: number, rx: number, ry: number) => kitFree(k, x, y, rx, ry);
  placeItems(k, items, buildingQueue);
  const critters = k.critters;

  let placed = 0;
  for (let n = 0; n < 2400 && placed < 110; n++) {
    const x = 3 + Math.floor(r() * (W - 6));
    const y = 3 + Math.floor(r() * (H - 6));
    if (!freeBox(x, y, 1, 1)) continue;
    const kind = trees[Math.floor(r() * trees.length)];
    block(x, y);
    reserve(x - 1, y - 2, x + 2, y + 2);
    objects.push({ x: x * T + 8, y: y * T + 13, sprite: makeTree(kind, r) });
    placed++;
  }

  const bushFlowers = [null, "#f6f1e7", "#ff9ab0", "#f2c14e", null];
  for (let n = 0, count = 0; n < 1800 && count < 95; n++) {
    const x = 3 + Math.floor(r() * (W - 6));
    const y = 3 + Math.floor(r() * (H - 6));
    if (!freeBox(x, y, 0, 0)) continue;
    block(x, y);
    const rock = r() < 0.25;
    objects.push({ x: x * T + 8, y: y * T + 13, sprite: rock ? makeRock(r) : makeBush(r, bushFlowers[Math.floor(r() * bushFlowers.length)]) });
    count++;
  }

  for (let x = 3; x < W - 3; x++) {
    if (x > b0 - 3 && x < b1 + 3) continue;
    const t = top(x);
    for (const y of [t - 1, t + 3]) {
      if (tiles[idx(x, y)] === GRASS && !reserved[idx(x, y)] && r() < 0.35) {
        objects.push({ x: x * T + 4 + r() * 8, y: y === t - 1 ? y * T + 15 : y * T + 6, sprite: makeReeds(r) });
      }
    }
  }

  const ground = paintGround(tiles, reserved, r, g);
  const waterFrames = paintWater(tiles, r, g);
  const spots = findSpots(tiles, solid, reserved, rng(seedOf("hearth-spots")), g, {
    read: hearthReadSpots(),
    board: [[nx, ny + 1, "up"], [nx + 1, ny + 1, "up"], [nx - 1, ny, "right"], [nx + 2, ny, "left"]],
    benches: HEARTH_BENCHES.map(([x, y], i) => [x + i, y + 1, "down"] as [number, number, Dir]),
    river: [8, 14, 20, 28, 52, 58, 66, 72],
  });

  return {
    region: HEARTH,
    w: W,
    h: H,
    riverTop: top,
    bridge: g.bridge,
    name: "Home",
    backdrop: "#17361f",
    fauna: { peacock: true, water: true, board: true, petals: true },
    weather: null,
    ambientTime: null,
    tiles,
    solid,
    ground,
    waterFrames,
    waterTop: g.river - 3,
    objects,
    lights,
    npcs,
    critters,
    placements: k.placements,
    plots,
    labels,
    chimneys,
    board: { x: (nx + 1) * T, y: (ny + 1) * T + 4 },
    gate: arrivalFor({ places, w: W, h: H }, "workshop") ?? { x: 73 * T + 8, y: 25 * T + 11 },
    spawn: { x: cx * T, y: 31 * T + 8 },
    plaza: { x: plazaC.x * T, y: plazaC.y * T },
    riverCenter: (x: number) => (top(Math.floor(x / T)) + 1.5) * T,
    spots,
    marketLamp: marketLamp ?? plazaLamps[0] ?? null,
    places,
  };
}

const HEARTH_BENCHES: [number, number][] = [[34, 21], [45, 21]];
const HEARTH_PORTAL_SLOTS: PortalSlot[] = [
  { x: 34, y: 52, spur: [34, 52, 36, 56] },
  { x: 44, y: 52, spur: [44, 52, 46, 56] },
  { x: 19, y: 6, spur: [19, 6, 21, 11] },
  { x: 56, y: 6, spur: [56, 6, 58, 11] },
];
const HEARTH_BOARD: [number, number] = [43, 28];

function hearthReadSpots(): [number, number, Dir][] {
  const lib = CIVIC.find((c) => c.id === "library" && !c.district);
  if (!lib) return [[35, 22, "down"], [45, 22, "down"], [37, 27, "down"], [42, 27, "down"]];
  const [tx, ty] = lib.slot;
  return [[tx + 1, ty + 6, "up"], [tx + 4, ty + 6, "up"], [tx, ty + 7, "up"], [tx + 5, ty + 7, "up"]];
}

function findSpots(
  tiles: Uint8Array,
  solid: Uint8Array,
  reserved: Uint8Array,
  r: () => number,
  g: Geo,
  fixed: { read: [number, number, Dir][]; board: [number, number, Dir][]; benches: [number, number, Dir][]; river: number[] },
): Spots {
  const idx = (x: number, y: number) => y * g.w + x;
  const free = (x: number, y: number) => x >= 0 && y >= 0 && x < g.w && y < g.h && !solid[idx(x, y)];
  const at = (list: [number, number, Dir][]): Spot[] =>
    list.filter(([x, y]) => free(x, y)).map(([x, y, face]) => ({ x: x * T + 8, y: y * T + 13, face }));
  const river: [number, number, Dir][] = [];
  for (const x of fixed.river) {
    river.push([x, riverTop(x, g.river) - 1, "down"]);
    river.push([x, riverTop(x, g.river) + 3, "up"]);
  }
  const plaza: [number, number, Dir][] = [];
  const grass: { x: number; y: number }[] = [];
  for (let y = 3; y < g.h - 3; y++) {
    for (let x = 3; x < g.w - 3; x++) {
      if (!free(x, y)) continue;
      if (tiles[idx(x, y)] === PLAZA && (x + y * 3) % 5 === 0) plaza.push([x, y, (["down", "left", "right"] as Dir[])[(x + y) % 3]]);
      if (tiles[idx(x, y)] === GRASS && !reserved[idx(x, y)]) grass.push({ x: x * T + 8, y: y * T + 12 });
    }
  }
  const pick = (n: number) => {
    const out: { x: number; y: number }[] = [];
    for (let i = 0; i < n && grass.length; i++) out.push(grass.splice(Math.floor(r() * grass.length), 1)[0]);
    return out;
  };
  return {
    read: at(fixed.read),
    board: at(fixed.board),
    benches: at(fixed.benches),
    river: at(river),
    plaza: at(plaza),
    perches: pick(9),
    cats: pick(3),
  };
}

const GRASS_TONES = ["#72bb60", "#6ab45a", "#63ad55"];

function tileAt(tiles: Uint8Array, x: number, y: number, g: Geo): number {
  if (x < 0 || y < 0 || x >= g.w || y >= g.h) return GRASS;
  return tiles[y * g.w + x];
}

const BAYER = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

function paintGrass(ctx: CanvasRenderingContext2D, g: Geo): void {
  const w = g.w * T;
  const h = g.h * T;
  const img = ctx.createImageData(w, h);
  const d = img.data;
  const tones = GRASS_TONES.map(rgbOf);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const n =
        Math.sin(x * 0.041) + Math.cos(y * 0.047) + Math.sin((x + y) * 0.019) + Math.sin(x * 0.013 - y * 0.017) * 0.9;
      const v = n + ((BAYER[(y & 3) * 4 + (x & 3)] - 7.5) / 16) * 0.7;
      const tone = tones[v > 1.1 ? 0 : v < -1.1 ? 2 : 1];
      const i = (y * w + x) * 4;
      d[i] = tone[0];
      d[i + 1] = tone[1];
      d[i + 2] = tone[2];
      d[i + 3] = 255;
    }
  }
  ctx.putImageData(img, 0, 0);
}

function paintGround(tiles: Uint8Array, reserved: Uint8Array, r: () => number, g: Geo): HTMLCanvasElement {
  const c = makeCanvas(g.w * T, g.h * T);
  const ctx = context(c);
  paintGrass(ctx, g);
  const at = (x: number, y: number) => tileAt(tiles, x, y, g);
  for (let y = 0; y < g.h; y++) {
    for (let x = 0; x < g.w; x++) {
      const type = at(x, y);
      const px = x * T;
      const py = y * T;
      if (type === GRASS || type === WATER) {
        for (let i = 0; i < 7; i++) rect(ctx, px + Math.floor(r() * T), py + Math.floor(r() * T), 1, 1, i % 2 ? "#89be69" : "#58924f");
        if (r() < 0.35) {
          const gx = px + 2 + Math.floor(r() * 11);
          const gy = py + 3 + Math.floor(r() * 10);
          rect(ctx, gx, gy, 1, 2, "#4f884b");
          rect(ctx, gx + 2, gy, 1, 2, "#4f884b");
          rect(ctx, gx + 1, gy + 1, 1, 2, "#4f884b");
        }
        if (!reserved[y * g.w + x] && r() < 0.12) {
          const col = ["#ffffff", "#ffe066", "#ff9ab0", "#b7a0ff"][Math.floor(r() * 4)];
          for (let n = 0; n < 3; n++) {
            const fx = px + 2 + Math.floor(r() * 12);
            const fy = py + 2 + Math.floor(r() * 12);
            rect(ctx, fx - 1, fy, 3, 1, col);
            rect(ctx, fx, fy - 1, 1, 3, col);
            rect(ctx, fx, fy, 1, 1, "#f2a541");
          }
        }
      } else if (type === PATH || type === BRIDGE) {
        rect(ctx, px, py, T, T, "#d7b486");
        for (let i = 0; i < 10; i++) rect(ctx, px + Math.floor(r() * T), py + Math.floor(r() * T), 1, 1, i % 3 ? "#c49d73" : "#e5cba0");
        if (r() < 0.4) rect(ctx, px + Math.floor(r() * 13), py + Math.floor(r() * 13), 2, 2, "#b38d67");
      } else if (type === PLAZA) {
        rect(ctx, px, py, T, T, "#aea291");
        for (let sy = 0; sy < 2; sy++) {
          for (let sx = 0; sx < 2; sx++) {
            const off = (y + sy) % 2 ? 4 : 0;
            const bx = px + sx * 8 + off - (off && sx === 1 ? 8 : 0);
            const tone = ["#cfc3b2", "#c6b9a7", "#d7ccbb", "#bfb19e"][Math.floor(r() * 4)];
            rect(ctx, bx + 1, py + sy * 8 + 1, 7, 7, tone);
            rect(ctx, bx + 1, py + sy * 8 + 1, 7, 1, shade(tone, 0.15));
          }
        }
        const d = Math.hypot(x + 0.5 - g.plaza.x, (y + 0.5 - g.plaza.y) * 1.3);
        if (d > 3.6 && d < 4.4) rect(ctx, px, py, T, T, "rgba(120,100,80,0.18)");
      } else if (type === SOIL) {
        rect(ctx, px, py, T, T, "#9c734c");
        for (let row = 1; row < T; row += 4) rect(ctx, px, py + row, T, 1, "#855f3d");
        for (let i = 0; i < 5; i++) rect(ctx, px + Math.floor(r() * T), py + Math.floor(r() * T), 1, 1, "#ba8c61");
      }
    }
  }
  for (let y = 0; y < g.h; y++) {
    for (let x = 0; x < g.w; x++) {
      const type = at(x, y);
      const px = x * T;
      const py = y * T;
      if (type === PATH || type === PLAZA) {
        const edges: [number, number][] = [[0, -1], [0, 1], [-1, 0], [1, 0]];
        for (const [dx, dy] of edges) {
          if (at(x + dx, y + dy) !== GRASS) continue;
          for (let i = 0; i < T; i++) {
            const depth = Math.floor(r() * 3);
            for (let n = 0; n < depth; n++) {
              const ex = dx === 0 ? px + i : dx < 0 ? px + n : px + T - 1 - n;
              const ey = dy === 0 ? py + i : dy < 0 ? py + n : py + T - 1 - n;
              rect(ctx, ex, ey, 1, 1, n === depth - 1 ? "#5e9653" : "#6fa957");
            }
          }
        }
      }
      if (type === GRASS || type === PATH) {
        if (at(x, y + 1) === WATER) {
          rect(ctx, px, py + T - 4, T, 4, "#a07d4f");
          rect(ctx, px, py + T - 2, T, 2, "#775a38");
          for (let i = 0; i < 4; i++) rect(ctx, px + Math.floor(r() * T), py + T - 5, 2, 1, "#896e45");
        }
        if (at(x, y - 1) === WATER) {
          rect(ctx, px, py, T, 3, "#a07d4f");
          rect(ctx, px, py, T, 1, "#694d30");
        }
        if (at(x - 1, y) === WATER) {
          rect(ctx, px, py, 3, T, "#a07d4f");
          rect(ctx, px, py, 1, T, "#694d30");
        }
        if (at(x + 1, y) === WATER) {
          rect(ctx, px + T - 3, py, 3, T, "#a07d4f");
          rect(ctx, px + T - 1, py, 1, T, "#694d30");
        }
      }
      if (type === BRIDGE) {
        rect(ctx, px, py, T, T, "#93643d");
        for (let row = 0; row < T; row += 4) {
          rect(ctx, px, py + row, T, 3, row % 8 ? "#a06940" : "#a87349");
          rect(ctx, px, py + row + 3, T, 1, "#69462e");
        }
        if (x === g.bridge[0]) {
          rect(ctx, px, py, 3, T, "#563925");
          rect(ctx, px + 1, py, 1, T, "#744e34");
        }
        if (x === g.bridge[1]) {
          rect(ctx, px + T - 3, py, 3, T, "#563925");
          rect(ctx, px + T - 2, py, 1, T, "#744e34");
        }
      }
    }
  }
  return c;
}

const WATER_FRAMES = 12;

export function paintWater(tiles: Uint8Array, r: () => number, g: Geo): HTMLCanvasElement[] {
  const top = g.river - 3;
  const rows = 9;
  const frames: HTMLCanvasElement[] = [];
  const pads: { x: number; y: number; flower: boolean }[] = [];
  const at = (x: number, y: number) => tileAt(tiles, x, y, g);
  for (let n = 0; n < Math.round(g.w * 0.46); n++) {
    const x = 3 * T + Math.floor(r() * (g.w - 6) * T);
    const tx = Math.floor(x / T);
    if (tx > g.bridge[0] - 2 && tx < g.bridge[1] + 2) continue;
    const y = (riverTop(tx, g.river) + 0.4 + r() * 2.2) * T;
    pads.push({ x, y: y - top * T, flower: r() < 0.3 });
  }
  for (let f = 0; f < WATER_FRAMES; f++) {
    const c = makeCanvas(g.w * T, rows * T);
    const ctx = context(c);
    const phase = (f / WATER_FRAMES) * Math.PI * 2;
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < g.w; x++) {
        if (at(x, y + top) !== WATER) continue;
        const px = x * T;
        const py = y * T;
        rect(ctx, px, py, T, T, "#3a88ad");
        const above = at(x, y + top - 1) !== WATER;
        const below = at(x, y + top + 1) !== WATER;
        if (above) {
          rect(ctx, px, py, T, 3, "#2c6e91");
          rect(ctx, px, py + 3, T, 1, "#337aa0");
        }
        if (below) rect(ctx, px, py + T - 2, T, 2, "#2f7499");
        const landLeft = at(x - 1, y + top) !== WATER && at(x - 1, y + top) !== BRIDGE;
        const landRight = at(x + 1, y + top) !== WATER && at(x + 1, y + top) !== BRIDGE;
        if (landLeft && x > 0) rect(ctx, px, py, 3, T, "#2c6e91");
        if (landRight && x < g.w - 1) rect(ctx, px + T - 3, py, 3, T, "#2c6e91");
        for (let yy = 0; yy < T; yy += 2) {
          for (let xx = 0; xx < T; xx++) {
            const wx = px + xx;
            const wy = py + yy;
            const v = Math.sin(wx * 0.19 + phase + Math.sin(wy * 0.31) * 1.5) + Math.sin(wy * 0.7 - phase * 2 + wx * 0.05);
            if (v > 1.55) rect(ctx, wx, wy, 2, 1, "#8fd3e8");
            else if (v > 1.3) rect(ctx, wx, wy, 1, 1, "#5fb0d0");
          }
        }
        if (above && (x + f) % 3 === 0) rect(ctx, px + ((f * 3) % 12), py + 1, 3, 1, "#cdeef7");
      }
    }
    for (const p of pads) {
      const bob = Math.round(Math.sin(phase + p.x) * 0.6);
      rect(ctx, p.x - 3, p.y - 2 + bob, 7, 4, "#3f8a48");
      rect(ctx, p.x - 2, p.y - 3 + bob, 5, 6, "#3f8a48");
      rect(ctx, p.x - 2, p.y - 2 + bob, 3, 2, "#57a852");
      rect(ctx, p.x, p.y + bob, 3, 1, "#3a88ad");
      if (p.flower) {
        rect(ctx, p.x - 1, p.y - 3 + bob, 3, 2, "#fbd3e0");
        rect(ctx, p.x, p.y - 3 + bob, 1, 1, "#f2c14e");
      }
    }
    frames.push(c);
  }
  return frames;
}
