import type { LokaLook } from "@/components/world/types";

export type View = "front" | "side" | "back";
export type Facing = "down" | "up" | "left" | "right";
export type Point = [number, number];
export type Rect = [number, number, number, number];
export type Swap = { role: string; shade: number };

export type Layer = "over" | "under";

export interface Frame {
  at: Point;
  slots: Record<string, Point>;
  layers: Record<string, Layer>;
}

export interface ViewFrames {
  idle: Frame[];
  walk: Frame[];
}

export interface BodySpec {
  species: string[];
  atlas: string | null;
  classes: { head: number; body: number } | null;
  density: number;
  size: Point;
  anchor: Point;
  sideFaces: "left" | "right";
  features: string[];
  views: Partial<Record<View, ViewFrames>> & { front: ViewFrames; side: ViewFrames };
  swaps: Record<string, Swap>;
}

export interface AccessoryView {
  at: Point;
  size: Point;
  pivot: Point;
  behind: boolean;
  mirror: boolean;
}

export type AccessoryCut = Partial<Record<View, AccessoryView>>;

export interface AccessorySpec {
  slot: string;
  basis: "head" | "body";
  atlas: string | null;
  sideFaces: "left" | "right" | null;
  sizes: Record<number, AccessoryCut>;
  swaps: Record<string, Swap>;
}

export interface Mask {
  at: Point;
  base: string;
}

export interface ThingSpec {
  density: number;
  anchor: Point;
  frames: Rect[];
  night: Rect[];
  swaps: Record<string, Swap>;
  sign: Rect | null;
  points: Record<string, Point[]>;
  masks: Record<string, Mask>;
}

export interface SpriteManifest {
  version: 1;
  atlas: string;
  sources: Record<string, string>;
  bodies: Record<string, BodySpec>;
  accessories: Record<string, AccessorySpec>;
  things: Record<string, ThingSpec>;
}

export const DENSITIES = [1, 2, 4];
export const SNAP = 4;
export const VIEW_W = 320;
export const VIEW_H = 192;
export const MIN_CSS = 2;
export const MAX_DPR = 2;
export const ART_GROWTH = 1.25;

type Raw = Record<string, unknown>;

function fail(path: string, want: string): never {
  throw new Error(`Sprite manifest: ${path} must be ${want}`);
}

function record(value: unknown, path: string): Raw {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail(path, "an object");
  return value as Raw;
}

function optionalRecord(value: unknown, path: string): Raw {
  return value === undefined ? {} : record(value, path);
}

function offset(value: unknown, path: string): Point {
  if (!Array.isArray(value) || value.length !== 2 || !value.every((n) => Number.isInteger(n))) fail(path, "two whole numbers");
  return [value[0], value[1]];
}

const SHEET = /^(?!.*\.\.)[\w.-]+(?:\/[\w.-]+)*\.png$/;

function sheet(value: unknown, path: string): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string" || !SHEET.test(value)) fail(path, "a .png path next to manifest.json");
  return value;
}

function point(value: unknown, path: string, positive = false): Point {
  const ok = Array.isArray(value) && value.length === 2 && value.every((n) => Number.isInteger(n) && (positive ? n > 0 : n >= 0));
  if (!ok) fail(path, positive ? "two positive integers" : "two whole numbers");
  return [value[0], value[1]];
}

function rect(value: unknown, path: string): Rect {
  const ok = Array.isArray(value) && value.length === 4 && value.every((n) => Number.isInteger(n) && n >= 0) && value[2] > 0 && value[3] > 0;
  if (!ok) fail(path, "[x, y, w, h] with a positive size");
  return [value[0], value[1], value[2], value[3]];
}

function strings(value: unknown, path: string): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || !value.every((s) => typeof s === "string" && s.trim())) fail(path, "a list of names");
  return value.map((s: string) => s.trim().toLowerCase());
}

function density(value: unknown, path: string): number {
  if (!DENSITIES.includes(value as number)) fail(path, DENSITIES.join(", ") + " or omitted");
  return value as number;
}

const HEX = /^#[0-9a-f]{6}$/;

function swaps(value: unknown, path: string): Record<string, Swap> {
  const out: Record<string, Swap> = {};
  for (const [hex, swap] of Object.entries(optionalRecord(value, path))) {
    const key = hex.toLowerCase();
    if (!HEX.test(key)) fail(`${path} key ${hex}`, "a #rrggbb colour");
    if (typeof swap === "string" && swap.trim()) out[key] = { role: swap.trim(), shade: 0 };
    else if (Array.isArray(swap) && swap.length === 2 && typeof swap[0] === "string" && typeof swap[1] === "number" && Math.abs(swap[1]) <= 1) {
      out[key] = { role: swap[0], shade: swap[1] };
    } else fail(`${path}.${hex}`, `a role name or [role, shade between -1 and 1]`);
  }
  return out;
}

function slotPoints(value: unknown, path: string): Record<string, Point> {
  const out: Record<string, Point> = {};
  for (const [slot, p] of Object.entries(optionalRecord(value, path))) out[slot] = offset(p, `${path}.${slot}`);
  return out;
}

function frames(value: unknown, path: string, defaults: Record<string, Point>, size: Point): Frame[] {
  if (value === undefined) return [];
  if (!Array.isArray(value)) fail(path, "a list of frames");
  return value.map((f, i) => {
    const at = Array.isArray(f) ? point(f, `${path}[${i}]`) : point(record(f, `${path}[${i}]`).at, `${path}[${i}].at`);
    const own = Array.isArray(f) ? {} : slotPoints((f as Raw).slots, `${path}[${i}].slots`);
    const layers: Record<string, Layer> = {};
    for (const [slot, layer] of Object.entries(Array.isArray(f) ? {} : optionalRecord((f as Raw).layers, `${path}[${i}].layers`))) {
      if (layer !== "over" && layer !== "under") fail(`${path}[${i}].layers.${slot}`, `"over" or "under"`);
      layers[slot] = layer;
    }
    const slots = { ...defaults, ...own };
    for (const [slot, p] of Object.entries(slots)) {
      if (Math.abs(p[0]) > size[0] * 2 || Math.abs(p[1]) > size[1] * 2) fail(`${path}[${i}] slot ${slot}`, "near the frame");
    }
    return { at, slots, layers };
  });
}

function viewFrames(value: unknown, path: string, size: Point): ViewFrames {
  const v = record(value, path);
  const defaults = slotPoints(v.slots, `${path}.slots`);
  const walk = frames(v.walk, `${path}.walk`, defaults, size);
  const idle = frames(v.idle, `${path}.idle`, defaults, size);
  if (!walk.length) fail(`${path}.walk`, "at least one frame");
  return { walk, idle: idle.length ? idle : walk.slice(0, 1) };
}

function body(value: unknown, path: string): BodySpec {
  const b = record(value, path);
  const size = point(b.size, `${path}.size`, true);
  const anchor = point(b.anchor, `${path}.anchor`);
  if (anchor[0] > size[0] || anchor[1] > size[1]) fail(`${path}.anchor`, "inside the frame");
  const views = record(b.views, `${path}.views`);
  const front = viewFrames(views.front, `${path}.views.front`, size);
  const side = viewFrames(views.side, `${path}.views.side`, size);
  const back = views.back === undefined ? undefined : viewFrames(views.back, `${path}.views.back`, size);
  const species = strings(b.species, `${path}.species`);
  if (!species.length) fail(`${path}.species`, "at least one species name");
  const faces = b.sideFaces ?? "left";
  if (faces !== "left" && faces !== "right") fail(`${path}.sideFaces`, `"left" or "right"`);
  let classes: BodySpec["classes"] = null;
  if (b.classes !== undefined) {
    const c = record(b.classes, `${path}.classes`);
    if (!Number.isInteger(c.head) || !Number.isInteger(c.body)) fail(`${path}.classes`, "{ head, body } size classes");
    classes = { head: c.head as number, body: c.body as number };
  }
  return {
    species,
    atlas: sheet(b.atlas, `${path}.atlas`),
    classes,
    density: density(b.density ?? 4, `${path}.density`),
    size,
    anchor,
    sideFaces: faces,
    features: strings(b.features, `${path}.features`),
    views: back ? { front, side, back } : { front, side },
    swaps: swaps(b.swaps, `${path}.swaps`),
  };
}

const VIEWS: View[] = ["front", "side", "back"];

function accessoryCut(value: unknown, path: string): AccessoryCut {
  const raw = record(value, path);
  const out: AccessoryCut = {};
  for (const view of VIEWS) {
    if (raw[view] === undefined) continue;
    const v = record(raw[view], `${path}.${view}`);
    const [x, y, w, h] = rect(v.rect, `${path}.${view}.rect`);
    if (v.layer !== undefined && v.layer !== "over" && v.layer !== "under") fail(`${path}.${view}.layer`, `"over" or "under"`);
    out[view] = { at: [x, y], size: [w, h], pivot: offset(v.pivot, `${path}.${view}.pivot`), behind: v.layer === "under", mirror: false };
  }
  if (!out.front) fail(`${path}.front`, "a front view");
  return out;
}

function accessory(value: unknown, path: string): AccessorySpec {
  const a = record(value, path);
  if (typeof a.slot !== "string" || !a.slot.trim()) fail(`${path}.slot`, "a slot name");
  const basis = a.basis === "body" ? "body" : "head";
  const sizes: Record<number, AccessoryCut> = {};
  if (a.sizes !== undefined) {
    for (const [key, cut] of Object.entries(record(a.sizes, `${path}.sizes`))) {
      if (!/^\d+$/.test(key)) fail(`${path}.sizes key ${key}`, "a whole number size class");
      sizes[Number(key)] = accessoryCut(cut, `${path}.sizes.${key}`);
    }
    if (!Object.keys(sizes).length) fail(`${path}.sizes`, "at least one size class");
  } else {
    const size = point(a.size, `${path}.size`, true);
    const pivot = point(a.pivot, `${path}.pivot`);
    const raw = record(a.views, `${path}.views`);
    const behind = strings(a.behind, `${path}.behind`);
    if (!behind.every((v) => v === "front" || v === "side" || v === "back")) fail(`${path}.behind`, "a list of front, side or back");
    const cut: AccessoryCut = {};
    for (const view of VIEWS) {
      if (raw[view] === undefined) continue;
      cut[view] = { at: point(raw[view], `${path}.views.${view}`), size, pivot, behind: behind.includes(view), mirror: false };
    }
    if (!cut.front) fail(`${path}.views.front`, "two whole numbers");
    sizes[0] = cut;
  }
  const faces = a.sideFaces ?? null;
  if (faces !== null && faces !== "left" && faces !== "right") fail(`${path}.sideFaces`, `"left" or "right"`);
  return { slot: a.slot.trim(), basis, atlas: sheet(a.atlas, `${path}.atlas`), sideFaces: faces, sizes, swaps: swaps(a.swaps, `${path}.swaps`) };
}

export function accessoryView(spec: AccessorySpec, body: BodySpec, view: View): AccessoryView | null {
  const wanted = body.classes ? (spec.basis === "body" ? body.classes.body : body.classes.head) : 0;
  const keys = Object.keys(spec.sizes).map(Number).sort((a, b) => Math.abs(a - wanted) - Math.abs(b - wanted) || b - a);
  const cut = spec.sizes[keys[0]];
  const own = cut?.[view];
  if (!own) return null;
  if (view !== "side" || !spec.sideFaces || spec.sideFaces === body.sideFaces) return own;
  return { ...own, pivot: [mirrored(own.pivot[0], own.size[0]), own.pivot[1]], mirror: true };
}

function thing(value: unknown, path: string): ThingSpec {
  const t = record(value, path);
  const list = (v: unknown, p: string) => (v === undefined ? [] : Array.isArray(v) ? v.map((r, i) => rect(r, `${p}[${i}]`)) : fail(p, "a list of [x, y, w, h]"));
  const frameList = list(t.frames, `${path}.frames`);
  if (!frameList.length) fail(`${path}.frames`, "at least one [x, y, w, h]");
  const night = list(t.night, `${path}.night`);
  if (night.length && night.length !== frameList.length) fail(`${path}.night`, "one rect per frame");
  const [, , w, h] = frameList[0];
  const inside = (p: Point, at: string) => {
    if (p[0] > w || p[1] > h) fail(at, "inside the frame");
    return p;
  };
  const points: Record<string, Point[]> = {};
  for (const [name, list] of Object.entries(optionalRecord(t.points, `${path}.points`))) {
    if (!Array.isArray(list)) fail(`${path}.points.${name}`, "a list of [x, y]");
    points[name] = list.map((p, i) => inside(point(p, `${path}.points.${name}[${i}]`), `${path}.points.${name}[${i}]`));
  }
  const masks: Record<string, Mask> = {};
  for (const [role, m] of Object.entries(optionalRecord(t.masks, `${path}.masks`))) {
    const raw = record(m, `${path}.masks.${role}`);
    if (typeof raw.base !== "string" || !HEX.test(raw.base.toLowerCase())) fail(`${path}.masks.${role}.base`, "a #rrggbb colour");
    masks[role] = { at: point(raw.at, `${path}.masks.${role}.at`), base: raw.base.toLowerCase() };
  }
  return {
    density: density(t.density ?? 2, `${path}.density`),
    anchor: inside(point(t.anchor, `${path}.anchor`), `${path}.anchor`),
    frames: frameList,
    night,
    swaps: swaps(t.swaps, `${path}.swaps`),
    sign: t.sign === undefined ? null : rect(t.sign, `${path}.sign`),
    points,
    masks,
  };
}

function entries<T>(value: unknown, path: string, parse: (v: unknown, p: string) => T): Record<string, T> {
  return Object.fromEntries(Object.entries(optionalRecord(value, path)).map(([key, v]) => [key.toLowerCase(), parse(v, `${path}.${key}`)]));
}

export function parseManifest(raw: unknown): SpriteManifest {
  const m = record(raw, "manifest");
  if (m.version !== 1) fail("version", "1");
  if (typeof m.atlas !== "string" || !/^[\w.-]+\.png$/.test(m.atlas)) fail("atlas", "a file name ending in .png next to manifest.json");
  const sources: Record<string, string> = {};
  for (const [name, url] of Object.entries(m.sources && typeof m.sources === "object" && !Array.isArray(m.sources) ? m.sources : {})) {
    if (typeof url === "string" && /^https:\/\//.test(url)) sources[name] = url;
  }
  return {
    version: 1,
    atlas: m.atlas,
    sources,
    bodies: entries(m.bodies, "bodies", body),
    accessories: entries(m.accessories, "accessories", accessory),
    things: entries(m.things, "things", thing),
  };
}

export function classicScale(cssWidth: number, cssHeight: number, dpr: number): number {
  const css = Math.max(MIN_CSS, Math.floor(Math.min(cssWidth / VIEW_W, cssHeight / VIEW_H)));
  return Math.max(1, Math.round(css * Math.min(Math.max(dpr || 1, 1), MAX_DPR)));
}

export function snapScale(cssWidth: number, cssHeight: number, dpr: number): number {
  const step = Math.min(Math.max(dpr || 1, 1), MAX_DPR) >= MAX_DPR ? SNAP / 2 : SNAP;
  const wanted = classicScale(cssWidth, cssHeight, dpr) / ART_GROWTH;
  return Math.max(SNAP, Math.round(wanted / step) * step);
}

export function withArtOnly(manifest: SpriteManifest, look: LokaLook, canonical: (species: string) => string, innate: (body: string) => string[] = () => []): LokaLook {
  const worn = look.accessories ?? [];
  const own = innate(canonical(look.body ?? "human"));
  const drawnIn = new Set(Object.values(manifest.bodies).flatMap((b) => b.features));
  const kept = worn.filter((a) => manifest.accessories[a] || own.includes(a) || drawnIn.has(a));
  return kept.length === worn.length ? look : { ...look, accessories: kept };
}

export function bodyFor(manifest: SpriteManifest, look: LokaLook, canonical: (species: string) => string, innate: (body: string) => string[] = () => []): string | null {
  const wanted = canonical(look.body ?? "human");
  const own = innate(wanted);
  for (const [key, spec] of Object.entries(manifest.bodies)) {
    if (!spec.species.some((s) => canonical(s) === wanted)) continue;
    const worn: string[] = (look.accessories ?? []).filter((a) => !own.includes(a));
    if (!spec.features.every((f) => worn.includes(f))) continue;
    if (worn.every((a) => spec.features.includes(a) || manifest.accessories[a])) return key;
  }
  return null;
}

export function viewOf(spec: BodySpec, facing: Facing): { view: View; mirror: boolean } {
  if (facing === "down") return { view: "front", mirror: false };
  if (facing === "up") return { view: spec.views.back ? "back" : "front", mirror: false };
  return { view: "side", mirror: facing !== spec.sideFaces };
}

export function wornLayer(frame: Frame, slot: string, cut: AccessoryView): Layer | "hidden" {
  if (frame.layers[slot] === "under") return "hidden";
  return cut.behind ? "under" : "over";
}

export function mirrored(x: number, width: number): number {
  return width - 1 - x;
}

export function walkFrame(cycle: number, count: number): number {
  if (count <= 0 || !Number.isFinite(cycle)) return 0;
  return ((Math.floor(cycle * count) % count) + count) % count;
}

export function spriteGrid(density: number, scale: number): number {
  return scale % density === 0 ? density : scale;
}

export function placed(anchor: Point, density: number, x: number, y: number, scale = density): Point {
  const grid = spriteGrid(density, scale);
  return [Math.round((x - anchor[0] / density) * grid) / grid, Math.round((y - anchor[1] / density) * grid) / grid];
}
