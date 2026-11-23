import { canonicalBody, speciesFor, withoutHeadwear } from "@/lib/looks";
import {
  accessoryView,
  bodyFor,
  parseManifest,
  type AccessorySpec,
  type AccessoryView,
  type BodySpec,
  type Facing,
  type Frame,
  type Mask,
  mirrored,
  wornLayer,
  type Point,
  type Rect,
  type SpriteManifest,
  type Swap,
  type View,
  viewOf,
  withArtOnly,
} from "@/lib/sprite-manifest";
import { context, makeCanvas, rgbOf, shade } from "./pixels";
import type { LokaLook } from "./types";

export const STYLE_A = "/sprites/style-a/manifest.json";
export const LOAD_TIMEOUT_MS = 8000;
export const RETRY_MS = 30_000;
export const RETRY_MAX_MS = 5 * 60_000;
export const FIGURE_CACHE = 48;

export interface Pose {
  idle: HTMLCanvasElement[];
  walk: HTMLCanvasElement[];
  anchor: Point;
  flip: boolean;
}

export interface Figure {
  poses: Record<Facing, Pose>;
  density: number;
  height: number;
}

export interface ThingImage {
  img: HTMLCanvasElement;
  ax: number;
  ay: number;
  density: number;
  sign: Rect | null;
  points: Record<string, Point[]>;
}

interface SpriteSet {
  manifest: SpriteManifest;
  atlas: HTMLImageElement;
  base: string;
  sheets: Map<string, HTMLImageElement | "loading" | "failed">;
}

interface ThingRef {
  key: string;
  colours: Record<string, string>;
  night: boolean;
}

let active: SpriteSet | null = null;
let enabled = false;
let wanted = false;
let atlasFailed = false;
const inflight = new Set<string>();
const retryTimers = new Map<string, ReturnType<typeof setTimeout>>();
const listeners = new Set<() => void>();
let version = 0;
const pending = new WeakSet<object>();
let loading: Promise<boolean> | null = null;
const looks = new WeakMap<object, LokaLook>();
const things = new WeakMap<object, ThingRef>();
const figureBySheet = new WeakMap<object, { version: number; look: LokaLook; key: string | null }>();
const figureByLook = new Map<string, Figure | null>();
const thingCache = new Map<string, ThingImage>();

export function rememberLook(sheet: object, look: LokaLook): void {
  looks.set(sheet, look);
}

export function rememberThing(sprite: object, key: string, colours: Record<string, string> = {}, night = false): void {
  things.set(sprite, { key, colours, night });
}

export function thingKey(sprite: object): string | null {
  return things.get(sprite)?.key ?? null;
}

export function newArtOn(): boolean {
  return enabled && active !== null;
}

export function retryDelay(tries: number): number {
  return Math.min(RETRY_MS * 2 ** tries, RETRY_MAX_MS);
}

export type NewArtStatus = "ok" | "failed" | "partial";

export function newArtStatus(): NewArtStatus {
  if (!wanted) return "ok";
  if (atlasFailed) return "failed";
  return active && [...active.sheets.values()].includes("failed") ? "partial" : "ok";
}

export function onNewArtStatus(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

let artChanges = 0;

function announce(): void {
  artChanges += 1;
  for (const listener of listeners) listener();
}

export function artVersion(): number {
  return artChanges;
}

let choice = 0;

export async function chooseNewArt(on: boolean): Promise<boolean> {
  const mine = ++choice;
  wanted = on;
  if (on) retryFailedSheets();
  announce();
  const ready = on ? await loadSprites() : false;
  if (mine !== choice) return wanted && active !== null;
  return ready;
}

export function showNewArt(on: boolean): void {
  const next = on && active !== null;
  if (enabled === next) return;
  enabled = next;
  version += 1;
  announce();
}

function loadImage(src: string, ms: number): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    const timer = setTimeout(() => {
      img.onload = img.onerror = null;
      img.src = "";
      reject(new Error(`Sprite image ${src} took too long`));
    }, ms);
    img.onload = () => {
      clearTimeout(timer);
      resolve(img);
    };
    img.onerror = () => {
      clearTimeout(timer);
      reject(new Error(`Sprite sheet ${src} did not load`));
    };
    img.src = src;
  });
}

async function fetchManifest(url: string): Promise<SpriteManifest | null> {
  const timeout = new AbortController();
  const timer = setTimeout(() => timeout.abort(), LOAD_TIMEOUT_MS);
  try {
    const res = await fetch(url, { signal: timeout.signal });
    return res.ok ? parseManifest(await res.json()) : null;
  } finally {
    clearTimeout(timer);
  }
}

export function loadSprites(url = STYLE_A): Promise<boolean> {
  if (active) return Promise.resolve(true);
  loading ??= (async () => {
    try {
      const manifest = await fetchManifest(url);
      if (!manifest) return false;
      const base = new URL(url, window.location.href).href;
      const atlas = await loadImage(new URL(manifest.atlas, base).href, LOAD_TIMEOUT_MS);
      active = { manifest, atlas, base, sheets: new Map() };
      version += 1;
      figureByLook.clear();
      thingCache.clear();
      return true;
    } catch (error) {
      console.warn("Sprite atlas failed to load, keeping the drawn art", error);
      return false;
    }
  })().then((ok) => {
    if (!ok) loading = null;
    if (atlasFailed === ok) {
      atlasFailed = !ok;
      announce();
    }
    return ok;
  });
  return loading;
}

function sheetOf(set: SpriteSet, name: string | null): HTMLImageElement | null | undefined {
  if (!name) return set.atlas;
  const hit = set.sheets.get(name);
  if (hit === "failed") return null;
  if (hit && hit !== "loading") return hit;
  if (!hit) {
    set.sheets.set(name, "loading");
    fetchSheet(set, name, 0);
  }
  return undefined;
}

function cancelRetry(name: string): void {
  clearTimeout(retryTimers.get(name));
  retryTimers.delete(name);
}

function fetchSheet(set: SpriteSet, name: string, tries: number): void {
  if (inflight.has(name)) return;
  cancelRetry(name);
  inflight.add(name);
  loadImage(new URL(name, set.base).href, LOAD_TIMEOUT_MS).then(
    (img) => {
      inflight.delete(name);
      const recovered = set.sheets.get(name) === "failed";
      set.sheets.set(name, img);
      if (recovered) {
        figureByLook.clear();
        version += 1;
      }
      announce();
    },
    (error) => {
      inflight.delete(name);
      console.warn("Sprite sheet failed to load, keeping the drawn art", error);
      if (set.sheets.get(name) !== "failed") {
        set.sheets.set(name, "failed");
        version += 1;
        announce();
      }
      retryTimers.set(
        name,
        setTimeout(() => {
          retryTimers.delete(name);
          if (wanted && set.sheets.get(name) === "failed") fetchSheet(set, name, tries + 1);
        }, retryDelay(tries)),
      );
    },
  );
}

function retryFailedSheets(): void {
  const set = active;
  if (!set) return;
  for (const [name, sheet] of set.sheets) if (sheet === "failed") fetchSheet(set, name, 0);
}

function cachedFigure(key: string): Figure | null | undefined {
  if (!figureByLook.has(key)) return undefined;
  const figure = figureByLook.get(key) ?? null;
  figureByLook.delete(key);
  figureByLook.set(key, figure);
  return figure;
}

function keepFigure(key: string, figure: Figure | null): void {
  figureByLook.delete(key);
  figureByLook.set(key, figure);
  if (figureByLook.size <= FIGURE_CACHE) return;
  const oldest = figureByLook.keys().next().value;
  if (oldest !== undefined) figureByLook.delete(oldest);
}

function innate(body: string): string[] {
  return speciesFor(body)?.features ?? [];
}

export function figureOf(sheet: object): Figure | null {
  if (!active || !enabled) return null;
  const hit = figureBySheet.get(sheet);
  if (hit && hit.version === version) {
    if (hit.key === null) return null;
    const cached = cachedFigure(hit.key);
    if (cached !== undefined) return cached;
  }
  const raw = looks.get(sheet);
  if (!raw) return null;
  const look = hit && hit.version === version ? hit.look : withArtOnly(active.manifest, withoutHeadwear(raw), canonicalBody, innate);
  const id = figureKey(active, look);
  const figure = id ? figureFor(active, look, id) : null;
  if (figure === undefined) {
    pending.add(sheet);
    return null;
  }
  pending.delete(sheet);
  figureBySheet.set(sheet, { version, look, key: figure && id ? id.key : null });
  return figure;
}

export function figurePending(sheet: object): boolean {
  return Boolean(active && enabled && pending.has(sheet));
}

function swapMap(swaps: Record<string, Swap>, colours: Record<string, string>): Map<number, [number, number, number]> {
  const out = new Map<number, [number, number, number]>();
  for (const [hex, swap] of Object.entries(swaps)) {
    const target = colours[swap.role];
    if (target) out.set(parseInt(hex.slice(1), 16), rgbOf(shade(target, swap.shade)));
  }
  return out;
}

function cut(atlas: HTMLImageElement, at: Point, size: Point, map: Map<number, [number, number, number]>, mirror = false): HTMLCanvasElement {
  const c = makeCanvas(size[0], size[1]);
  const ctx = context(c, { willReadFrequently: map.size > 0 });
  if (mirror) ctx.setTransform(-1, 0, 0, 1, size[0], 0);
  ctx.drawImage(atlas, at[0], at[1], size[0], size[1], 0, 0, size[0], size[1]);
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  if (!map.size) return c;
  const img = ctx.getImageData(0, 0, size[0], size[1]);
  const d = img.data;
  for (let i = 0; i < d.length; i += 4) {
    if (!d[i + 3]) continue;
    const to = map.get((d[i] << 16) | (d[i + 1] << 8) | d[i + 2]);
    if (!to) continue;
    d[i] = to[0];
    d[i + 1] = to[1];
    d[i + 2] = to[2];
  }
  ctx.putImageData(img, 0, 0);
  return c;
}

export const HAIR_FLOOR = 0.3;

function lightAtLeast(hex: string, floor: number): string {
  const [r, g, b] = rgbOf(hex).map((v) => v / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (l >= floor) return hex;
  const lift = (floor - l) * 255;
  return `#${[r, g, b].map((v) => Math.min(255, Math.round(v * 255 + lift)).toString(16).padStart(2, "0")).join("")}`;
}

function coloursOf(look: LokaLook): Record<string, string> {
  const accent = look.accent ?? look.shirt;
  return { skin: look.skin, hair: lightAtLeast(look.hair, HAIR_FLOOR), shirt: look.shirt, pants: look.pants, accent, suit: look.pants, tie: accent };
}

interface Worn {
  spec: AccessorySpec;
  art: Partial<Record<View, { img: HTMLCanvasElement; cut: AccessoryView }>>;
}

function wornAt(w: Worn, view: View, frame: Frame): { img: HTMLCanvasElement; x: number; y: number; behind: boolean } | null {
  const slot = frame.slots[w.spec.slot];
  const art = w.art[view];
  const layer = art ? wornLayer(frame, w.spec.slot, art.cut) : "hidden";
  if (!slot || !art || layer === "hidden") return null;
  return { img: art.img, x: slot[0] - art.cut.pivot[0], y: slot[1] - art.cut.pivot[1], behind: layer === "under" };
}

interface FigureId {
  body: string;
  key: string;
}

function figureKey(set: SpriteSet, look: LokaLook): FigureId | null {
  const body = bodyFor(set.manifest, look, canonicalBody, innate);
  return body ? { body, key: JSON.stringify([body, look.skin, look.hair, look.shirt, look.pants, look.accent, look.accessories ?? []]) } : null;
}

function figureFor(set: SpriteSet, look: LokaLook, { body, key: cacheKey }: FigureId): Figure | null | undefined {
  const cached = cachedFigure(cacheKey);
  if (cached !== undefined) return cached;
  const spec = set.manifest.bodies[body];
  const own = innate(canonicalBody(look.body ?? "human"));
  const names = (look.accessories ?? []).filter((a) => !spec.features.includes(a) && !own.includes(a));
  const bodySheet = sheetOf(set, spec.atlas);
  const sheets = names.map((a) => sheetOf(set, set.manifest.accessories[a].atlas));
  if (bodySheet === undefined || sheets.includes(undefined)) return undefined;
  if (bodySheet === null || sheets.includes(null)) {
    keepFigure(cacheKey, null);
    return null;
  }
  const colours = coloursOf(look);
  const bodyMap = swapMap(spec.swaps, colours);
  const worn: Worn[] = names.map((a, i) => {
    const acc = set.manifest.accessories[a];
    const map = swapMap(acc.swaps, colours);
    const art: Worn["art"] = {};
    for (const view of ["front", "side", "back"] as View[]) {
      const c = accessoryView(acc, spec, view);
      if (c) art[view] = { img: cut(sheets[i] as HTMLImageElement, c.at, c.size, map, c.mirror), cut: c };
    }
    return { spec: acc, art };
  });
  const pad = padding(spec, worn);
  const w = spec.size[0] + pad.left + pad.right;
  const h = spec.size[1] + pad.top + pad.bottom;
  const compose = (view: View, frame: Frame) => {
    const c = makeCanvas(w, h);
    const ctx = context(c);
    const layer = (behind: boolean) => {
      for (const item of worn) {
        const at = wornAt(item, view, frame);
        if (at && at.behind === behind) ctx.drawImage(at.img, at.x + pad.left, at.y + pad.top);
      }
    };
    layer(true);
    ctx.drawImage(cut(bodySheet, frame.at, spec.size, bodyMap), pad.left, pad.top);
    layer(false);
    return c;
  };
  const anchor: Point = [spec.anchor[0] + pad.left, spec.anchor[1] + pad.top];
  const built = new Map<View, Pose>();
  const poseFor = (facing: Facing): Pose => {
    const { view, mirror } = viewOf(spec, facing);
    let pose = built.get(view);
    if (!pose) {
      const frames = spec.views[view] ?? spec.views.front;
      pose = { idle: frames.idle.map((f) => compose(view, f)), walk: frames.walk.map((f) => compose(view, f)), anchor, flip: false };
      built.set(view, pose);
    }
    return mirror ? { ...pose, anchor: [mirrored(anchor[0], w), anchor[1]], flip: true } : pose;
  };
  const figure: Figure = {
    poses: { down: poseFor("down"), up: poseFor("up"), left: poseFor("left"), right: poseFor("right") },
    density: spec.density,
    height: anchor[1] / spec.density,
  };
  keepFigure(cacheKey, figure);
  return figure;
}

export async function figurePreview(look: LokaLook): Promise<string | null> {
  if (!enabled || !active) return null;
  for (let tries = 0; tries < 80; tries++) {
    const shown = withArtOnly(active.manifest, withoutHeadwear(look), canonicalBody, innate);
    const id = figureKey(active, shown);
    const figure = id ? figureFor(active, shown, id) : null;
    if (figure === null) return null;
    if (figure) return figure.poses.down.idle[0].toDataURL();
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return null;
}

const PORTRAIT_SHARE = 0.62;
const PORTRAIT_RATIO = 14 / 17;
const portraits = new WeakMap<Figure, HTMLCanvasElement>();

export function figurePortrait(look: LokaLook): HTMLCanvasElement | null | undefined {
  if (!enabled || !active) return null;
  const shown = withArtOnly(active.manifest, withoutHeadwear(look), canonicalBody, innate);
  const id = figureKey(active, shown);
  const figure = id ? figureFor(active, shown, id) : null;
  if (!figure) return figure;
  const hit = portraits.get(figure);
  if (hit) return hit;
  const pose = figure.poses.down;
  const src = pose.idle[0];
  const data = context(src, { willReadFrequently: true }).getImageData(0, 0, src.width, src.height).data;
  let top = 0;
  while (top < src.height && !rowHasInk(data, src.width, top)) top += 1;
  const h = Math.max(1, Math.round((pose.anchor[1] - top) * PORTRAIT_SHARE));
  const w = Math.max(1, Math.round(h * PORTRAIT_RATIO));
  const out = makeCanvas(w, h);
  context(out).drawImage(src, Math.round(pose.anchor[0] - w / 2), top, w, h, 0, 0, w, h);
  portraits.set(figure, out);
  return out;
}

function rowHasInk(data: Uint8ClampedArray, width: number, y: number): boolean {
  for (let x = 0; x < width; x++) if (data[(y * width + x) * 4 + 3]) return true;
  return false;
}

function padding(spec: BodySpec, worn: Worn[]): { left: number; right: number; top: number; bottom: number } {
  const out = { left: 0, right: 0, top: 0, bottom: 0 };
  for (const [view, frames] of Object.entries(spec.views) as [View, BodySpec["views"]["front"]][]) {
    for (const frame of [...frames.idle, ...frames.walk]) {
      for (const item of worn) {
        const at = wornAt(item, view, frame);
        if (!at) continue;
        out.left = Math.max(out.left, -at.x);
        out.top = Math.max(out.top, -at.y);
        out.right = Math.max(out.right, at.x + at.img.width - spec.size[0]);
        out.bottom = Math.max(out.bottom, at.y + at.img.height - spec.size[1]);
      }
    }
  }
  return out;
}

function maskedCut(atlas: HTMLImageElement, rect: Rect, swaps: Map<number, [number, number, number]>, masks: [Mask, string][]): HTMLCanvasElement {
  const c = cut(atlas, [rect[0], rect[1]], [rect[2], rect[3]], swaps);
  if (!masks.length) return c;
  const ctx = context(c, { willReadFrequently: true });
  const img = ctx.getImageData(0, 0, c.width, c.height);
  const d = img.data;
  for (const [mask, colour] of masks) {
    const m = cut(atlas, mask.at, [rect[2], rect[3]], new Map());
    const md = context(m, { willReadFrequently: true }).getImageData(0, 0, m.width, m.height).data;
    const base = Math.max(...rgbOf(mask.base)) || 1;
    const to = rgbOf(colour);
    for (let i = 0; i < d.length; i += 4) {
      if (!md[i + 3] || !d[i + 3]) continue;
      const v = Math.max(d[i], d[i + 1], d[i + 2]);
      const amount = v <= base ? v / base - 1 : (v - base) / (255 - base || 1);
      for (let k = 0; k < 3; k++) d[i + k] = amount < 0 ? to[k] * (1 + amount) : to[k] + (255 - to[k]) * amount;
    }
  }
  ctx.putImageData(img, 0, 0);
  return c;
}

export function thingOf(sprite: object, seed: number): ThingImage | null {
  if (!active || !enabled) return null;
  const ref = things.get(sprite);
  const spec = ref && active.manifest.things[ref.key];
  if (!ref || !spec) return null;
  const variant = Math.abs(Math.floor(seed)) % spec.frames.length;
  const rect: Rect = (ref.night && spec.night[variant]) || spec.frames[variant];
  const masks = Object.entries(spec.masks).flatMap(([role, mask]) => (ref.colours[role] ? [[mask, ref.colours[role]] as [Mask, string]] : []));
  const cacheKey = JSON.stringify([ref.key, rect, masks.map(([, c]) => c), ref.colours]);
  const hit = thingCache.get(cacheKey);
  if (hit) return hit;
  const made: ThingImage = {
    img: maskedCut(active.atlas, rect, swapMap(spec.swaps, ref.colours), masks),
    ax: spec.anchor[0],
    ay: spec.anchor[1],
    density: spec.density,
    sign: spec.sign,
    points: spec.points,
  };
  thingCache.set(cacheKey, made);
  return made;
}

export function propOf(key: string, frame = 0): ThingImage | null {
  if (!active || !enabled) return null;
  const spec = active.manifest.things[key];
  if (!spec) return null;
  const count = spec.frames.length;
  const index = ((Math.floor(frame) % count) + count) % count;
  const cacheKey = `prop:${key}:${index}`;
  const hit = thingCache.get(cacheKey);
  if (hit) return hit;
  const made: ThingImage = {
    img: maskedCut(active.atlas, spec.frames[index], new Map(), []),
    ax: spec.anchor[0],
    ay: spec.anchor[1],
    density: spec.density,
    sign: spec.sign,
    points: spec.points,
  };
  thingCache.set(cacheKey, made);
  return made;
}

export function heldGearOf(sheet: object): string[] {
  const set = active;
  if (!set || !enabled) return [];
  const worn = looks.get(sheet)?.accessories ?? [];
  return worn.filter((a) => !set.manifest.accessories[a] && set.manifest.things[`gear_${a}`]);
}

export interface Placed {
  art: ThingImage;
  x: number;
  y: number;
  w: number;
  h: number;
  at: (p: Point) => { x: number; y: number };
}

export function placeArt(sprite: { img: HTMLCanvasElement; ax: number; ay: number }, ox: number, oy: number): Placed | null {
  const art = thingOf(sprite, ox * 7 + oy * 13);
  if (!art) return null;
  const d = art.density;
  const cx = ox - sprite.ax + sprite.img.width / 2;
  const x = Math.round(cx * d - art.ax) / d;
  const y = Math.round(oy * d - art.ay) / d;
  return { art, x, y, w: art.img.width / d, h: art.img.height / d, at: (p) => ({ x: x + p[0] / d, y: y + p[1] / d }) };
}

export function rekey<T extends { img: HTMLCanvasElement; ax: number; ay: number }>(sprite: T, key: string): T {
  const copy = { ...sprite };
  rememberThing(copy, key);
  return copy;
}
