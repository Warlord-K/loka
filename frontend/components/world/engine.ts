import type { LokaLook, Trade } from "./types";
import { makePeacock } from "./art";
import { buildSheet, type CharacterSheet, type Dir } from "./characters";
import type { Room, RoomGlow } from "./interior";
import { context, ellipse, makeCanvas } from "./pixels";
import { figureOf, figurePending, heldGearOf, newArtOn, placeArt, rememberThing, thingKey, thingOf, type Placed, type ThingImage } from "./sprites";
import { T, type Activity, type Light, type Npc, type Place, type Spot, type World, type WorldObject } from "./world";
import { drawBookArt, drawHeldArt, drawIconArt, drawParticleArt, drawPips, drawPipsArt, drawRune, drawSlime, drawSlimeArt, drawStation, drawStationArt, drawStatusArt, drawTool, drawToolArt, slimeArtAt, slimeAt, stationAt, STRIKE_PERIOD, tempo, WORK_LIGHT } from "./work";
import { TEXT_SCALE_EVENT } from "@/lib/text-scale";
import { MAX_DPR, placed, snapScale, walkFrame } from "@/lib/sprite-manifest";

export type Target =
  | { kind: "resident"; id: string }
  | { kind: "plot"; index: number }
  | { kind: "board" }
  | { kind: "peacock" }
  | { kind: "place"; id: string }
  | { kind: "thing"; id: string }
  | { kind: "folk"; id: string };

export interface PlaceSigns {
  nextTimer: { label: string; at: number } | null;
  unread: number;
  runs: number;
  devices: { id: string; state: "busy" | "online" | "offline" }[];
  sightingAt: number;
  reading: boolean;
}

interface Grid {
  w: number;
  h: number;
  solid: Uint8Array;
}

export type TimeOfDay = "day" | "dusk" | "night" | "clock";

export interface EngineOptions {
  fontFamily: string;
  reducedMotion: boolean;
  touch: boolean;
  onFocus: (target: Target | null) => void;
  onInteract: (target: Target) => void;
  onStart: () => void;
  onLeave: (placeId: string) => void;
  onExit?: (to: string) => void;
  fadeIn?: boolean;
  playerLook?: LokaLook;
}

const TIME_VALUE: Record<Exclude<TimeOfDay, "clock">, number> = { day: 0, dusk: 1, night: 2 };

const CLOCK_STOPS: [number, number][] = [
  [0, 2],
  [5, 2],
  [6.5, 1],
  [7.5, 0],
  [17, 0],
  [19, 1],
  [21, 2],
  [24, 2],
];

export interface MinimapPalette {
  player: string;
  ring: string;
  edge: string;
  place: string;
  exit: string;
  npc: string;
  house: string;
}

export function clockTimeValue(at: Date = new Date()): number {
  const h = at.getHours() + at.getMinutes() / 60;
  for (let i = 1; i < CLOCK_STOPS.length; i++) {
    const [h1, v1] = CLOCK_STOPS[i];
    const [h0, v0] = CLOCK_STOPS[i - 1];
    if (h <= h1) return v0 + ((v1 - v0) * (h - h0)) / (h1 - h0);
  }
  return 2;
}

export const PLAYER_LOOK: LokaLook = {
  skin: "#f1c7a1",
  hair: "#2b1d16",
  shirt: "#e8b64a",
  pants: "#35506e",
  accent: "#16a39a",
  accessories: ["beanie"],
};

const AMBIENT: [number, number, number, number][] = [
  [0, 0, 0, 0],
  [72, 34, 98, 0.4],
  [10, 16, 52, 0.7],
];

const GRADE: [number, number, number, number][] = [
  [255, 238, 200, 0.12],
  [255, 138, 84, 0.26],
  [90, 120, 220, 0.16],
];

interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  life: number;
  max: number;
  size: number;
  kind: "smoke" | "petal" | "heart" | "spark" | "chip" | "bubble" | "rune" | "hit" | "rocket" | "flare";
  color: string;
  glyph?: number;
}

interface Firefly {
  x: number;
  y: number;
  phase: number;
  speed: number;
}

interface Point {
  x: number;
  y: number;
}

interface Cat {
  x: number;
  y: number;
  homeX: number;
  homeY: number;
  target: Point | null;
  wait: number;
  facing: number;
  anim: number;
  sleeping: boolean;
  coat: string;
  dark: string;
}

interface Bird {
  x: number;
  y: number;
  homeX: number;
  homeY: number;
  vx: number;
  vy: number;
  state: "ground" | "flying" | "gone";
  timer: number;
  phase: number;
}

interface Drop {
  x: number;
  y: number;
  speed: number;
  sway: number;
}

export type Weather = "clear" | "rain" | "leaves" | "snow" | "fog";

const MAX_PARTICLES = 420;
const FRAME_MS = 1000 / 60;
const PAUSED_FRAME_MS = 1000 / 30;
const NPC_SPEED = 38;
const FIREWORK_COLORS = ["#ffd45a", "#ff6f91", "#7ee0d2", "#9fdcff", "#c39bff", "#9bf08b"];

export function weatherAt(at: Date = new Date()): Weather {
  let h = (at.getFullYear() * 400 + at.getMonth() * 32 + at.getDate()) * 24 + at.getHours();
  h = Math.imul(h ^ (h >>> 15), 2246822507);
  h = Math.imul(h ^ (h >>> 13), 3266489909);
  const v = ((h ^ (h >>> 16)) >>> 0) % 100;
  return v < 18 ? "rain" : v < 42 ? "leaves" : "clear";
}

const DRIFT_CELL = 320;
const CODE_HEIGHT = 20;
const CODE_WALK_FRAMES = 4;
const IDLE_FPS = 6;
const SHADOW_RX = 0.42;
const SHADOW_RY = 0.07;
const ART_SHADOW = "rgba(38,24,34,0.22)";
const ART_INK = "#411334";
const ART_PAPER = "#fef9e5";
const TOOL_REACH = 3;
const TOOL_LIFT = 2;
const BOOK_LIFT = 3;
const HAND_REACH = 0.2;
const STEADY_GLOW = 0.5;
const TOOL_GEAR: Partial<Record<Trade, string>> = { warrior: "sword" };
const HAND_LIFT = 0.3;
const SLIME_PIPS_ABOVE = 22;

function critterArt(key: string): object {
  const ref = {};
  rememberThing(ref, key);
  return ref;
}

const DUCK = critterArt("critter_duck");
const DUCKLING = critterArt("critter_duckling");
const DUCK_FRAME: Record<string, number> = { "#f6f1e7": 0, "#e8b64a": 1, "#8a6a4a": 2 };
const SPARROW = critterArt("critter_sparrow");
const CAT = critterArt("critter_cat");
const CAT_GREY = critterArt("critter_cat_grey");
const KOI = critterArt("critter_koi");
const PEACOCK = critterArt("critter_peacock");
const PEACOCK_FAN = critterArt("critter_peacock_fan");

function drift(d: { x: number; y: number }, vx0: number, vy0: number, vw: number, vh: number, draw: (x: number, y: number) => void): void {
  const bx = d.x * DRIFT_CELL;
  const by = d.y * DRIFT_CELL;
  for (let y = Math.floor((vy0 - by) / DRIFT_CELL) * DRIFT_CELL + by; y < vy0 + vh; y += DRIFT_CELL) {
    if (y < vy0 - 4) continue;
    for (let x = Math.floor((vx0 - bx) / DRIFT_CELL) * DRIFT_CELL + bx; x < vx0 + vw; x += DRIFT_CELL) {
      if (x >= vx0 - 4) draw(x, y);
    }
  }
}

function dirOf(dx: number, dy: number): Dir {
  return Math.abs(dx) > Math.abs(dy) + 0.01 ? (dx > 0 ? "right" : "left") : dy > 0 ? "down" : "up";
}

const SMALL_TALK = ["Morning!", "How is your quest going?", "Seen the ducks today?", "Coffee later?", "Nice weather for it.", "Any good PRs lately?", "I could use a break.", "The river looks calm.", "Back to it soon.", "Did the build pass?"];

function shortSay(text: string): string {
  const flat = text.replace(/[`*_#>\[\]]/g, "").replace(/\s+/g, " ").trim();
  if (!flat) return "...";
  return flat.length > 30 ? `${flat.slice(0, 29).trimEnd()}...` : flat;
}

export function targetKey(t: Target | null): string {
  if (!t) return "";
  if (t.kind === "resident") return `r:${t.id}`;
  if (t.kind === "plot") return `p:${t.index}`;
  if (t.kind === "place") return `pl:${t.id}`;
  if (t.kind === "thing") return `th:${t.id}`;
  if (t.kind === "folk") return `f:${t.id}`;
  return t.kind;
}

function lerpColor(a: number[], b: number[], t: number): number[] {
  return a.map((v, i) => v + (b[i] - v) * t);
}

export class LokaEngine {
  private ctx: CanvasRenderingContext2D;
  private light = makeCanvas(1, 1);
  private lctx = context(this.light);
  private w = 1;
  private h = 1;
  private P = 3;
  private hudScale = 1;
  private offX = 0;
  private offY = 0;
  private cam: Point;
  private player: { x: number; y: number; dir: Dir; moving: boolean; anim: number };
  private playerSheet: CharacterSheet;
  private keys = new Set<string>();
  private path: Point[] = [];
  private pending: Target | null = null;
  private stuck = 0;
  private retried = false;
  private paused = false;
  private due = 0;
  private started = false;
  private timeTarget = 1;
  private timeValue = 1;
  private focus: Target | null = null;
  private focusKey = "";
  private outdoor: Grid;
  private room: Room | null = null;
  private roomPlace: string | null = null;
  private pendingRoom: Room | null = null;
  private pendingPlace: string | null = null;
  private fade: { t: number; dur: number; mid: (() => void) | null } | null = null;
  private placeById = new Map<string, Place>();
  private artCache = new WeakMap<WorldObject, [Placed | null, Placed | null]>();
  private veils: [number, number, number, number][] = [];
  private signs: PlaceSigns = { nextTimer: null, unread: 0, runs: 0, devices: [], sightingAt: 0, reading: false };
  private particles: Particle[] = [];
  private fireflies: Firefly[] = [];
  private smokeTimer = 0;
  private petalTimer = 0;
  private marker: { x: number; y: number; t: number } | null = null;
  private raf = 0;
  private last = 0;
  private t = 0;
  private peacockSprite = makePeacock();
  private shadow = makeCanvas(12, 4);
  private peacock = { angle: 0.6, x: 0, y: 0, facing: 1, open: 0, openTarget: 0, pauseUntil: 0, dirSign: 1 };
  private ducks = [
    { base: 12 * T, phase: 0, color: "#f6f1e7", small: false },
    { base: 12 * T, phase: -0.12, color: "#f2d98a", small: true },
    { base: 12 * T, phase: -0.22, color: "#f2d98a", small: true },
    { base: 14 * T, phase: 0.6, color: "#e8b64a", small: false },
    { base: 44 * T, phase: 2.1, color: "#f6f1e7", small: false },
    { base: 38 * T, phase: 4.2, color: "#8a6a4a", small: false },
  ];
  private cats: Cat[] = [];
  private birds: Bird[] = [];
  private rain: Drop[] = [];
  private leaves: Drop[] = [];
  private weather: Weather = "clear";
  private weatherAmount = 0;
  private weatherCheck = 0;
  private fish = { active: false, x: 0, y: 0, t: 0, dir: 1, next: 6 };
  private shells = 0;
  private shellTimer = 0;
  private lampAlarmUntil = 0;
  private budgetPaused = false;
  private teammates = new Map<string, Set<string>>();
  private cleanup: (() => void)[] = [];
  private npcById = new Map<string, Npc>();
  private portalLatch: string | null = null;
  private exitLatch = false;
  private idx = (x: number, y: number) => y * this.world.w + x;

  constructor(
    private canvas: HTMLCanvasElement,
    private world: World,
    private opts: EngineOptions,
  ) {
    this.ctx = context(canvas);
    this.playerSheet = buildSheet(opts.playerLook ?? PLAYER_LOOK);
    this.player = { x: world.spawn.x, y: world.spawn.y, dir: "up", moving: false, anim: 0 };
    this.cam = { x: world.plaza.x, y: world.plaza.y + 2 * T };
    for (const npc of world.npcs) this.npcById.set(npc.resident.id, npc);
    for (const place of world.places) this.placeById.set(place.id, place);
    this.outdoor = { w: this.world.w, h: this.world.h, solid: world.solid };
    const sctx = context(this.shadow);
    sctx.fillStyle = "rgba(20,24,20,0.28)";
    sctx.fillRect(2, 0, 8, 4);
    sctx.fillRect(0, 1, 12, 2);
    const seedRand = (() => {
      let s = 7;
      return () => ((s = (s * 16807) % 2147483647) / 2147483647);
    })();
    for (let i = 0; i < 70; i++) {
      const x = (3 + seedRand() * (this.world.w - 6)) * T;
      const y = (3 + seedRand() * (this.world.h - 6)) * T;
      this.fireflies.push({ x, y, phase: seedRand() * 100, speed: 0.5 + seedRand() });
    }
    world.spots.cats.forEach((c, i) => {
      this.cats.push({ x: c.x, y: c.y, homeX: c.x, homeY: c.y, target: null, wait: 2 + i * 3, facing: 1, anim: 0, sleeping: i === 1, coat: i ? "#8a8f99" : "#e39b4a", dark: i ? "#5d626c" : "#b06a2a" });
    });
    for (const p of world.spots.perches) {
      for (let i = 0; i < 3; i++) {
        const x = p.x - 5 + i * 5 + seedRand() * 2;
        const y = p.y - 1 + seedRand() * 3;
        this.birds.push({ x, y, homeX: x, homeY: y, vx: 0, vy: 0, state: "ground", timer: 0, phase: seedRand() * 10 });
      }
    }
    for (let i = 0; i < 110; i++) this.rain.push({ x: seedRand(), y: seedRand(), speed: 0.8 + seedRand() * 0.4, sway: 0 });
    for (let i = 0; i < 26; i++) this.leaves.push({ x: seedRand(), y: seedRand(), speed: 0.6 + seedRand() * 0.6, sway: seedRand() * 10 });
    this.weather = world.weather ?? weatherAt();
    this.weatherAmount = opts.reducedMotion || this.weather === "clear" ? 0 : 1;
    if (opts.fadeIn && !opts.reducedMotion) this.fade = { t: 0.2, dur: 0.4, mid: null };
    this.bind();
    this.resize();
    this.last = performance.now();
    this.raf = requestAnimationFrame(this.loop);
  }

  destroy(): void {
    cancelAnimationFrame(this.raf);
    for (const fn of this.cleanup) fn();
  }

  setPlayerLook(look: LokaLook): void {
    this.playerSheet = buildSheet(look);
  }

  travelOut(done: () => void): void {
    this.begin();
    this.transition(done, false);
  }

  approach(id: string, text: string): boolean {
    const npc = this.npcById.get(id);
    if (!npc || this.room) return false;
    const pl = this.player;
    const px = Math.floor(pl.x / T);
    const py = Math.floor((pl.y - 2) / T);
    const tile = this.walkable(px + 1, py) ? this.idx(px + 1, py) : this.walkable(px - 1, py) ? this.idx(px - 1, py) : this.nearestWalkable(px, py + 1);
    if (tile === null) return false;
    const x = (tile % this.world.w) * T + 8;
    const y = Math.floor(tile / this.world.w) * T + 13;
    npc.say = { text, until: this.t + 40 };
    npc.visit = null;
    npc.activity = "visit";
    npc.spot = { x, y, face: dirOf(pl.x - x, pl.y - y) };
    npc.arrived = false;
    const path = this.opts.reducedMotion ? null : this.findPath(npc.x, npc.y, new Set([tile]));
    if (path) {
      path.push({ x, y });
      npc.path = path;
    } else {
      npc.path = [];
      npc.x = x;
      npc.y = y;
      this.arriveNpc(npc);
    }
    return true;
  }

  hush(id: string): void {
    const npc = this.npcById.get(id);
    if (npc) npc.say = null;
  }

  folkSay(id: string, text: string): void {
    const c = this.world.critters.find((x) => x.id === id);
    if (c) c.say = { text: shortSay(text), until: this.t + 6 };
  }

  setPaused(paused: boolean): void {
    this.paused = paused;
    this.keys.clear();
    if (paused) {
      this.path = [];
      this.pending = null;
    }
  }

  setTime(time: TimeOfDay): void {
    this.timeTarget = time === "clock" ? clockTimeValue() : TIME_VALUE[time];
  }

  begin(): void {
    if (this.started) return;
    this.started = true;
    this.opts.onStart();
  }

  playerPosition(): Point {
    if (this.room && this.roomPlace) {
      const place = this.placeById.get(this.roomPlace);
      if (place) return { x: place.door.x, y: place.door.y + 6 };
    }
    return { x: this.player.x, y: this.player.y };
  }

  roomPosition(): Point | null {
    return this.room ? { x: this.player.x, y: this.player.y } : null;
  }

  currentPlace(): string | null {
    return this.room ? this.roomPlace : null;
  }

  placeOf(id: string): Place | null {
    return this.placeById.get(id) ?? null;
  }

  mapAspect(): number {
    return this.world.h / this.world.w;
  }

  minimapKey(): string {
    const me = this.playerPosition();
    const parts = [this.world.w, this.world.h, this.world.places.length, Math.round(me.x * 2), Math.round(me.y * 2)];
    for (const npc of this.world.npcs) parts.push(Math.round(npc.x), Math.round(npc.y));
    return parts.join(",");
  }

  drawMinimap(canvas: HTMLCanvasElement, palette: MinimapPalette): void {
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const world = this.world;
    const scale = Math.min(canvas.width / (world.w * T), canvas.height / (world.h * T));
    const ox = (canvas.width - world.w * T * scale) / 2;
    const oy = (canvas.height - world.h * T * scale) / 2;
    const at = (x: number, y: number): [number, number] => [ox + x * scale, oy + y * scale];
    ctx.imageSmoothingEnabled = false;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(world.ground, ox, oy, world.w * T * scale, world.h * T * scale);
    const unit = Math.max(2, Math.round(canvas.width / 110));
    const dot = (x: number, y: number, r: number, fill: string, edge: string | null) => {
      const [px, py] = at(x, y);
      ctx.beginPath();
      ctx.arc(px, py, r, 0, Math.PI * 2);
      ctx.fillStyle = fill;
      ctx.fill();
      if (edge) {
        ctx.lineWidth = Math.max(1, unit / 2);
        ctx.strokeStyle = edge;
        ctx.stroke();
      }
    };
    for (const p of world.places) {
      if (p.kind === "house") dot(p.door.x, p.door.y, unit * 0.8, palette.house, null);
    }
    for (const p of world.places) {
      if (p.kind === "exit") dot(p.door.x, p.door.y, unit * 1.1, palette.exit, palette.edge);
      else if (p.kind !== "house") dot(p.door.x, p.door.y, unit, palette.place, palette.edge);
    }
    for (const npc of world.npcs) dot(npc.x, npc.y, unit * 0.9, palette.npc, palette.edge);
    const me = this.playerPosition();
    const [px, py] = at(me.x, me.y);
    ctx.beginPath();
    ctx.arc(px, py, unit * 2.6, 0, Math.PI * 2);
    ctx.lineWidth = unit;
    ctx.strokeStyle = palette.ring;
    ctx.stroke();
    dot(me.x, me.y, unit * 1.3, palette.player, palette.edge);
  }

  setSigns(signs: PlaceSigns): void {
    this.signs = signs;
  }

  enterRoom(room: Room, placeId: string, opts: { at?: Point; instant?: boolean } = {}): void {
    if (this.room && this.roomPlace === placeId) {
      if (this.room.spec.id === room.spec.id) {
        this.updateRoom(room);
        return;
      }
      this.transition(() => {
        this.room = room;
        this.exitLatch = false;
        const at = opts.at ?? room.entry;
        this.player.x = at.x;
        this.player.y = at.y;
        this.player.dir = opts.at ? "down" : "up";
        this.cam = { x: at.x, y: at.y - 8 };
        this.resetMotion();
      }, Boolean(opts.instant));
      return;
    }
    this.pendingRoom = room;
    if (this.fade && this.pendingPlace === placeId) return;
    this.pendingPlace = placeId;
    this.begin();
    this.transition(() => {
      this.room = this.pendingRoom ?? room;
      this.roomPlace = placeId;
      this.pendingRoom = null;
      this.pendingPlace = null;
      const at = opts.at ?? room.entry;
      this.player.x = at.x;
      this.player.y = at.y;
      this.player.dir = "up";
      this.cam = { x: at.x, y: at.y - 8 };
      this.resetMotion();
    }, Boolean(opts.instant));
  }

  updateRoom(room: Room): void {
    if (!this.room || this.room.spec.id !== room.spec.id || this.room.w !== room.w || this.room.h !== room.h) return;
    this.room = room;
    this.focusKey = "";
  }

  leaveRoom(instant = false): void {
    if (!this.room) return;
    const placeId = this.roomPlace;
    this.transition(() => {
      const place = placeId ? this.placeById.get(placeId) : null;
      this.room = null;
      this.roomPlace = null;
      const at = place ? { x: place.door.x, y: place.door.y + 6 } : this.world.spawn;
      this.player.x = at.x;
      this.player.y = at.y;
      this.player.dir = "down";
      this.cam = { x: at.x, y: at.y - 8 };
      this.resetMotion();
      if (placeId) this.opts.onLeave(placeId);
    }, instant);
  }

  labelOf(t: Target): string {
    if (t.kind === "resident") {
      const npc = this.npcById.get(t.id);
      return npc ? `Talk to ${npc.resident.name}${npc.detail && npc.mode === "home" ? ` · ${npc.detail.slice(0, 40)}` : ""}` : "Talk";
    }
    if (t.kind === "place") {
      const place = this.placeById.get(t.id);
      if (!place) return "Go inside";
      if (place.kind === "portal") return `Travel to ${place.name}`;
      if (place.kind === "exit") return `Road to ${place.name}`;
      return place.kind === "building" ? `Open ${place.name}` : `Enter ${place.name}`;
    }
    if (t.kind === "folk") {
      const c = this.world.critters.find((x) => x.id === t.id);
      return c ? `Talk to ${c.name}` : "Talk";
    }
    if (t.kind === "thing") return this.room?.things.find((th) => th.id === t.id)?.label ?? "Look";
    if (t.kind === "plot") return "Look at the plot";
    if (t.kind === "board") return "Read the board";
    return "Say hi";
  }

  private transition(mid: () => void, instant: boolean): void {
    if (instant || this.opts.reducedMotion) {
      this.fade = null;
      mid();
      return;
    }
    this.fade = { t: 0, dur: 0.4, mid };
  }

  private resetMotion(): void {
    this.path = [];
    this.pending = null;
    this.marker = null;
    this.keys.clear();
    this.focusKey = "";
    this.focus = null;
    this.opts.onFocus(null);
  }

  private grid(): Grid {
    return this.room ?? this.outdoor;
  }

  placePlayer(p: Point, dir?: Dir): void {
    this.player.x = p.x;
    this.player.y = p.y;
    if (dir) this.player.dir = dir;
    this.cam = { x: p.x, y: p.y - 8 };
  }

  walkToPlacement(id: string): boolean {
    const p = this.world.placements[id];
    if (!p) return false;
    this.begin();
    if (this.room) this.leaveRoom(true);
    const tile = this.nearestWalkable(Math.floor(p.x / T), Math.floor(p.y / T));
    if (tile === null) return false;
    if (this.navigate(new Set([tile]))) {
      this.pending = null;
      this.marker = { x: (tile % this.world.w) * T + 8, y: Math.floor(tile / this.world.w) * T + 10, t: 0 };
      return true;
    }
    return false;
  }

  walkTo(target: Target): void {
    this.begin();
    const goal = this.pointOf(target);
    if (goal && Math.hypot(goal.x - this.player.x, goal.y - this.player.y) < 30) {
      this.interact(target);
      return;
    }
    const goals = this.goalsFor(target);
    if (this.navigate(goals)) this.pending = target;
  }

  interact(target: Target): void {
    const p = this.pointOf(target);
    if (p) this.face(p);
    if (target.kind === "peacock") {
      this.peacock.openTarget = 1;
      this.peacock.pauseUntil = this.t + 4.5;
      for (let i = 0; i < 7; i++) {
        this.particles.push({
          x: this.peacock.x + (Math.random() - 0.5) * 16,
          y: this.peacock.y - 18,
          vx: (Math.random() - 0.5) * 14,
          vy: -14 - Math.random() * 10,
          life: 0,
          max: 1.4 + Math.random() * 0.6,
          size: 1,
          kind: "heart",
          color: ["#ff6f91", "#ff9ab0", "#7ee0d2"][i % 3],
        });
      }
      return;
    }
    this.opts.onInteract(target);
  }

  resize(): void {
    const box = this.canvas.getBoundingClientRect();
    const newArt = newArtOn();
    const dpr = newArt ? Math.min(Math.max(window.devicePixelRatio || 1, 1), MAX_DPR) : Math.min(window.devicePixelRatio || 1, 2);
    this.hudScale = (parseFloat(getComputedStyle(document.documentElement).fontSize) || 16) / 16;
    this.w = Math.max(1, Math.round(box.width * dpr));
    this.h = Math.max(1, Math.round(box.height * dpr));
    this.canvas.width = this.w;
    this.canvas.height = this.h;
    const css = Math.max(2, Math.floor(Math.min(box.width / (T * 20), box.height / (T * 12))));
    this.P = newArt ? snapScale(box.width, box.height, dpr) : Math.max(1, Math.round(css * dpr));
    this.light.width = Math.ceil(this.w / 4);
    this.light.height = Math.ceil(this.h / 4);
    this.ctx.imageSmoothingEnabled = false;
  }

  private bind(): void {
    const onKeyDown = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable)) return;
      if (this.paused) return;
      const k = e.key.toLowerCase();
      const move = this.dirKey(k);
      if (move) {
        e.preventDefault();
        this.keys.add(move);
        this.path = [];
        this.pending = null;
        this.begin();
      } else if (k === "shift") {
        this.keys.add("run");
      } else if (k === "e" || k === "enter" || k === " ") {
        if (el && el.tagName === "BUTTON" && k !== "e") return;
        e.preventDefault();
        this.begin();
        if (this.focus) this.interact(this.focus);
      }
    };
    const onKeyUp = (e: KeyboardEvent) => {
      const k = e.key.toLowerCase();
      const move = this.dirKey(k);
      if (move) this.keys.delete(move);
      if (k === "shift") this.keys.delete("run");
    };
    const onBlur = () => this.keys.clear();
    const onPointerDown = (e: PointerEvent) => {
      if (this.paused) return;
      if (this.fade) return;
      const p = this.toWorld(e);
      const hit = this.hitTest(p);
      if (hit) {
        this.walkTo(hit);
        return;
      }
      this.begin();
      const g = this.grid();
      const tile = this.nearestWalkable(Math.floor(p.x / T), Math.floor(p.y / T), g);
      if (tile === null) return;
      if (this.navigate(new Set([tile]))) {
        this.pending = null;
        this.marker = { x: (tile % g.w) * T + 8, y: Math.floor(tile / g.w) * T + 10, t: 0 };
      }
    };
    const onPointerMove = (e: PointerEvent) => {
      if (e.pointerType !== "mouse") return;
      this.canvas.style.cursor = !this.paused && this.hitTest(this.toWorld(e)) ? "pointer" : "default";
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", onBlur);
    this.canvas.addEventListener("pointerdown", onPointerDown);
    this.canvas.addEventListener("pointermove", onPointerMove);
    const ro = new ResizeObserver(() => this.resize());
    ro.observe(this.canvas);
    const onTextScale = () => this.resize();
    window.addEventListener(TEXT_SCALE_EVENT, onTextScale);
    this.cleanup.push(
      () => window.removeEventListener(TEXT_SCALE_EVENT, onTextScale),
      () => window.removeEventListener("keydown", onKeyDown),
      () => window.removeEventListener("keyup", onKeyUp),
      () => window.removeEventListener("blur", onBlur),
      () => this.canvas.removeEventListener("pointerdown", onPointerDown),
      () => this.canvas.removeEventListener("pointermove", onPointerMove),
      () => ro.disconnect(),
    );
  }

  private dirKey(k: string): Dir | null {
    if (k === "arrowup" || k === "w") return "up";
    if (k === "arrowdown" || k === "s") return "down";
    if (k === "arrowleft" || k === "a") return "left";
    if (k === "arrowright" || k === "d") return "right";
    return null;
  }

  private toWorld(e: PointerEvent): Point {
    const box = this.canvas.getBoundingClientRect();
    const sx = ((e.clientX - box.left) / box.width) * this.w;
    const sy = ((e.clientY - box.top) / box.height) * this.h;
    return { x: (sx - this.offX) / this.P, y: (sy - this.offY) / this.P };
  }

  private hitTest(p: Point): Target | null {
    if (this.room) {
      for (const th of this.room.things) {
        if (Math.abs(p.x - th.x) < 14 && p.y > th.y - th.lift - 4 && p.y < th.y) return { kind: "thing", id: th.id };
      }
      return null;
    }
    for (const npc of this.world.npcs) {
      if (Math.abs(p.x - npc.x) < 10 && p.y > npc.y - 24 && p.y < npc.y + 3) return { kind: "resident", id: npc.resident.id };
    }
    for (const c of this.world.critters) {
      if (c.talk && Math.abs(p.x - c.x) < 10 && p.y > c.y - 24 && p.y < c.y + 3) return { kind: "folk", id: c.id };
    }
    for (const place of this.world.places) {
      if (Math.abs(p.x - place.door.x) < 10 && p.y > place.door.y - 30 && p.y < place.door.y + 2) return { kind: "place", id: place.id };
    }
    for (const plot of this.world.plots) {
      if (Math.abs(p.x - plot.x) < 13 && p.y > plot.y - 34 && p.y < plot.y) return { kind: "plot", index: plot.index };
    }
    const b = this.world.board;
    if (this.world.fauna.board && Math.abs(p.x - b.x) < 18 && p.y > b.y - 36 && p.y < b.y) return { kind: "board" };
    const pc = this.peacock;
    if (this.world.fauna.peacock && Math.abs(p.x - pc.x) < 14 && p.y > pc.y - 26 && p.y < pc.y + 3) return { kind: "peacock" };
    return null;
  }

  private pointOf(t: Target): Point | null {
    if (t.kind === "place") return this.placeById.get(t.id)?.door ?? null;
    if (t.kind === "thing") {
      const th = this.room?.things.find((x) => x.id === t.id);
      return th ? { x: th.x, y: th.y } : null;
    }
    if (t.kind === "resident") {
      const npc = this.npcById.get(t.id);
      return npc ? { x: npc.x, y: npc.y } : null;
    }
    if (t.kind === "plot") {
      const plot = this.world.plots.find((p) => p.index === t.index);
      return plot ? { x: plot.x, y: plot.y } : null;
    }
    if (t.kind === "board") return this.world.board;
    if (t.kind === "folk") {
      const c = this.world.critters.find((x) => x.id === t.id);
      return c ? { x: c.x, y: c.y } : null;
    }
    return { x: this.peacock.x, y: this.peacock.y };
  }

  private walkable(x: number, y: number, g: Grid = this.outdoor): boolean {
    return x >= 0 && y >= 0 && x < g.w && y < g.h && !g.solid[y * g.w + x];
  }

  private ringAround(tx: number, ty: number, r = 1, g: Grid = this.outdoor): Set<number> {
    const out = new Set<number>();
    for (let dy = -r; dy <= r; dy++) {
      for (let dx = -r; dx <= r; dx++) if (this.walkable(tx + dx, ty + dy, g)) out.add((ty + dy) * g.w + tx + dx);
    }
    return out;
  }

  private goalsFor(t: Target): Set<number> {
    if (t.kind === "thing") {
      const th = this.room?.things.find((x) => x.id === t.id);
      if (!th || !this.room) return new Set();
      return th.goals.length ? new Set(th.goals) : this.ringAround(Math.floor(th.x / T), Math.floor(th.y / T) - 1, 1, this.room);
    }
    if (t.kind === "place") {
      const place = this.placeById.get(t.id);
      if (!place) return new Set();
      const s = new Set(place.goals.filter(([x, y]) => this.walkable(x, y)).map(([x, y]) => this.idx(x, y)));
      return s.size ? s : this.ringAround(Math.floor(place.door.x / T), Math.floor(place.door.y / T));
    }
    if (t.kind === "resident") {
      const npc = this.npcById.get(t.id);
      if (!npc) return new Set();
      const front = new Set<number>();
      for (const [dx, dy] of [[0, 1], [-1, 1], [1, 1]]) {
        if (this.walkable(npc.tx + dx, npc.ty + dy)) front.add(this.idx(npc.tx + dx, npc.ty + dy));
      }
      return front.size ? front : this.ringAround(npc.tx, npc.ty);
    }
    if (t.kind === "plot") {
      const plot = this.world.plots.find((p) => p.index === t.index);
      return plot ? this.ringAround(plot.tx + 3, plot.ty + 4) : new Set();
    }
    if (t.kind === "folk") {
      const c = this.world.critters.find((x) => x.id === t.id);
      return c ? this.ringAround(Math.floor(c.x / T), Math.floor((c.y - 2) / T)) : new Set();
    }
    if (t.kind === "board") {
      const s = new Set<number>();
      for (const b of this.world.spots.board) s.add(this.idx(Math.floor(b.x / T), Math.floor((b.y - 2) / T)));
      return s;
    }
    return this.ringAround(Math.floor(this.peacock.x / T), Math.floor((this.peacock.y - 2) / T));
  }

  private nearestWalkable(tx: number, ty: number, g: Grid = this.outdoor): number | null {
    for (let r = 0; r <= 4; r++) {
      let best: number | null = null;
      let bestD = Infinity;
      for (let dy = -r; dy <= r; dy++) {
        for (let dx = -r; dx <= r; dx++) {
          if (Math.max(Math.abs(dx), Math.abs(dy)) !== r) continue;
          if (!this.walkable(tx + dx, ty + dy, g)) continue;
          const d = dx * dx + dy * dy;
          if (d < bestD) {
            bestD = d;
            best = (ty + dy) * g.w + tx + dx;
          }
        }
      }
      if (best !== null) return best;
    }
    return null;
  }

  private navigate(goals: Set<number>): boolean {
    const path = this.findPath(this.player.x, this.player.y, goals, this.grid());
    if (!path) return false;
    this.path = path;
    this.stuck = 0;
    return true;
  }

  private findPath(fromX: number, fromY: number, goals: Set<number>, g: Grid = this.outdoor): Point[] | null {
    if (goals.size === 0) return null;
    const sx = Math.floor(fromX / T);
    const sy = Math.floor((fromY - 2) / T);
    if (sx < 0 || sy < 0 || sx >= g.w || sy >= g.h) return null;
    const start = sy * g.w + sx;
    const prev = new Int32Array(g.w * g.h).fill(-1);
    prev[start] = start;
    const queue = [start];
    let found = -1;
    for (let head = 0; head < queue.length; head++) {
      const cur = queue[head];
      if (goals.has(cur)) {
        found = cur;
        break;
      }
      const cx = cur % g.w;
      const cy = Math.floor(cur / g.w);
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          if (!dx && !dy) continue;
          const nx = cx + dx;
          const ny = cy + dy;
          if (!this.walkable(nx, ny, g)) continue;
          if (dx && dy && (!this.walkable(cx + dx, cy, g) || !this.walkable(cx, cy + dy, g))) continue;
          const n = ny * g.w + nx;
          if (prev[n] !== -1) continue;
          prev[n] = cur;
          queue.push(n);
        }
      }
    }
    if (found < 0) return null;
    const tiles: number[] = [];
    for (let c = found; c !== start; c = prev[c]) tiles.push(c);
    tiles.reverse();
    return tiles.map((c) => ({ x: (c % g.w) * T + 8, y: Math.floor(c / g.w) * T + 11 }));
  }

  setAgentState(id: string, status: string, detail: string | null): void {
    const npc = this.npcById.get(id);
    if (!npc) return;
    npc.status = status === "away" ? "idle" : status;
    npc.detail = detail;
  }

  setAgentJobs(id: string, jobs: string[], instant = false): void {
    const npc = this.npcById.get(id);
    if (!npc) return;
    const before = npc.jobs.length;
    npc.jobs = jobs;
    if (jobs.length) {
      npc.idleAt = null;
      if (npc.mode !== "working") {
        npc.mode = "working";
        if (!instant) this.pop(npc.x, npc.y - 30 - this.liftOf(npc.sheet), ["#f2c14e", "#ffffff", "#e0663d"], 7);
      } else if (!instant && jobs.length > before) this.pop(npc.x, npc.y - 30 - this.liftOf(npc.sheet), ["#f2c14e", "#ffffff"], 4);
      else if (!instant && jobs.length < before) this.burst(npc.x, npc.y - 22 - this.liftOf(npc.sheet), ["#ff6f91", "#f2c14e"], 3);
    } else if (npc.mode === "working") {
      if (instant) npc.mode = "home";
      else npc.idleAt = this.t + 2.5;
    }
  }

  agentMessage(fromId: string, toId: string, text: string): void {
    const from = this.npcById.get(fromId);
    if (!from || fromId === toId || !this.npcById.has(toId)) return;
    const say = shortSay(text);
    if (this.opts.reducedMotion || from.status === "waiting" || from.activity === "chase") {
      from.say = { text: say, until: this.t + 5 };
      return;
    }
    from.visit = { id: toId, text: say, tries: 0 };
    this.go(from, "visit");
  }

  agentAsks(id: string): void {
    const npc = this.npcById.get(id);
    if (npc) npc.asking = this.t + 120;
  }

  questFailed(id: string): void {
    const npc = this.npcById.get(id);
    if (npc) npc.cloudUntil = this.t + 240;
  }

  celebrate(): void {
    if (this.opts.reducedMotion) return;
    this.shells = Math.min(12, this.shells + 5);
  }

  budgetWarning(): void {
    this.lampAlarmUntil = this.t + 600;
  }

  setBudgetPaused(paused: boolean): void {
    this.budgetPaused = paused;
  }

  setTeams(groups: string[][]): void {
    const map = new Map<string, Set<string>>();
    for (const group of groups) {
      for (const id of group) {
        const mates = map.get(id) ?? new Set<string>();
        for (const other of group) if (other !== id) mates.add(other);
        map.set(id, mates);
      }
    }
    this.teammates = map;
  }

  private pop(x: number, y: number, colors: string[], n: number): void {
    if (this.opts.reducedMotion) return;
    for (let i = 0; i < n; i++) {
      const a = (i / n) * Math.PI * 2;
      this.particles.push({ x, y, vx: Math.cos(a) * 22, vy: Math.sin(a) * 22 - 6, life: 0, max: 0.45, size: 1, kind: "spark", color: colors[i % colors.length] });
    }
  }

  private emitWork(npc: Npc): void {
    if (this.opts.reducedMotion) return;
    const trade = npc.resident.trade;
    const st = stationAt(npc.homeX, npc.homeY);
    const r = Math.random;
    if (trade === "carpenter") {
      for (let i = 0; i < 3; i++)
        this.particles.push({ x: st.x + 2, y: st.y - 10, vx: (r() - 0.3) * 30, vy: -18 - r() * 14, life: 0, max: 0.7, size: 1, kind: "chip", color: r() < 0.5 ? "#e0b07a" : "#b07a45" });
      this.particles.push({ x: st.x + 2, y: st.y - 11, vx: 0, vy: 0, life: 0, max: 0.14, size: 1, kind: "hit", color: "#ffffff" });
    } else if (trade === "smith") {
      for (let i = 0; i < 6; i++)
        this.particles.push({ x: st.x + 1, y: st.y - 11, vx: (r() - 0.5) * 50, vy: -20 - r() * 26, life: 0, max: 0.5 + r() * 0.3, size: 1, kind: "spark", color: r() < 0.5 ? "#ffd45a" : "#ff9a3c" });
    } else if (trade === "alchemist") {
      this.particles.push({ x: st.x - 4 + r() * 8, y: st.y - 9, vx: 0, vy: -7 - r() * 5, life: 0, max: 1.1 + r() * 0.6, size: 1 + Math.round(r()), kind: "bubble", color: ["#7be06b", "#c39bff", "#6bd6e0"][Math.floor(r() * 3)] });
    } else if (trade === "wizard") {
      if (r() < 0.6)
        this.particles.push({ x: st.x - 5 + r() * 8, y: st.y - 13, vx: (r() - 0.5) * 4, vy: -6 - r() * 4, life: 0, max: 1.6, size: 1, kind: "rune", color: r() < 0.5 ? "#9fdcff" : "#c39bff", glyph: Math.floor(r() * 4) });
    } else {
      const s = newArtOn() ? slimeArtAt(st.x, st.y, 0) : slimeAt(st.x, st.y, 0, this.t);
      this.particles.push({ x: s.x, y: s.y - 4, vx: 0, vy: 0, life: 0, max: 0.22, size: 1, kind: "hit", color: "#ffffff" });
      for (let i = 0; i < 3; i++)
        this.particles.push({ x: s.x, y: s.y - 4, vx: (r() - 0.5) * 40, vy: -10 - r() * 20, life: 0, max: 0.4, size: 1, kind: "spark", color: npc.jobs[0] === "review" ? "#6bc7e8" : "#9bf08b" });
    }
  }

  private working(npc: Npc): boolean {
    return npc.mode === "working" && npc.activity === "work" && npc.arrived && !npc.moving && this.focusKey !== `r:${npc.resident.id}` && npc.dir === "left";
  }

  private burst(x: number, y: number, colors: string[], n: number): void {
    if (this.opts.reducedMotion) return;
    for (let i = 0; i < n; i++) {
      this.particles.push({
        x: x + (Math.random() - 0.5) * 10,
        y,
        vx: (Math.random() - 0.5) * 18,
        vy: -16 - Math.random() * 10,
        life: 0,
        max: 1.1 + Math.random() * 0.5,
        size: 1,
        kind: "heart",
        color: colors[i % colors.length],
      });
    }
  }

  private updateCritters(dt: number): void {
    const pl = this.player;
    for (const c of this.world.critters) {
      if (c.say && this.t > c.say.until) c.say = null;
      if (c.talk && (this.focusKey === `f:${c.id}` || (this.pending?.kind === "folk" && this.pending.id === c.id))) {
        c.target = null;
        c.moving = false;
        c.anim = 0;
        c.dir = dirOf(pl.x - c.x, pl.y - c.y);
        c.wait = 2;
        continue;
      }
      if (!c.target) {
        c.moving = false;
        c.anim = 0;
        c.wait -= dt;
        if (c.wait > 0 || this.opts.reducedMotion) continue;
        const reach = c.talk ? 3 : 6;
        const tx = c.homeX + (Math.random() - 0.5) * reach * T;
        const ty = c.homeY + (Math.random() - 0.5) * reach * T;
        if (this.walkable(Math.floor(tx / T), Math.floor((ty - 2) / T))) c.target = { x: tx, y: ty };
        else c.wait = 0.6;
        continue;
      }
      const vx = c.target.x - c.x;
      const vy = c.target.y - c.y;
      const d = Math.hypot(vx, vy);
      if (d < 1.5) {
        c.target = null;
        c.wait = 1.5 + Math.random() * 4;
        continue;
      }
      const step = Math.min(d, 26 * dt);
      const nx = c.x + (vx / d) * step;
      const ny = c.y + (vy / d) * step;
      if (this.blocked(nx, ny)) {
        c.target = null;
        c.wait = 1 + Math.random() * 2;
        continue;
      }
      c.x = nx;
      c.y = ny;
      c.dir = Math.abs(vx) > Math.abs(vy) ? (vx > 0 ? "right" : "left") : vy > 0 ? "down" : "up";
      c.moving = true;
      c.anim += dt * 6;
    }
  }

  private updateNpcs(dt: number): void {
    const pl = this.player;
    const held = this.pending?.kind === "resident" ? this.pending.id : null;
    for (const npc of this.world.npcs) {
      if (npc.mode === "working" && npc.idleAt !== null && this.t >= npc.idleAt) {
        npc.mode = "home";
        npc.idleAt = null;
        this.burst(npc.x, npc.y - 22 - this.liftOf(npc.sheet), ["#ff6f91", "#7ee0d2", "#f2c14e"], 8);
      }
      this.plan(npc, dt);
      const id = npc.resident.id;
      npc.hold = this.focusKey === `r:${id}` ? npc.hold + dt : 0;
      const frozen = held === id || (npc.hold > 0 && (npc.hold < 4 || this.paused));
      npc.moving = false;
      if (npc.path.length && !frozen) this.step(npc, dt);
      const ndx = pl.x - npc.x;
      const ndy = pl.y - npc.y;
      if (!npc.moving) npc.dir = Math.hypot(ndx, ndy) < 60 ? dirOf(ndx, ndy) : npc.arrived && npc.spot ? npc.spot.face : "down";
      if (npc.say && this.t > npc.say.until) npc.say = null;
      if ((!this.working(npc) && !this.reading(npc)) || npc.idleAt !== null) continue;
      const period = this.reading(npc) ? 1.6 : STRIKE_PERIOD[npc.resident.trade] / tempo(npc.jobs.length);
      const strike = Math.floor((this.t + npc.phase) / period);
      if (strike !== npc.strike) {
        npc.strike = strike;
        if (this.reading(npc)) this.emitPage(npc);
        else this.emitWork(npc);
      }
    }
  }

  private want(npc: Npc): Activity | null {
    if (npc.visit) return "visit";
    if (npc.activity === "visit" && npc.say) return "visit";
    if (npc.mode === "working" && npc.idleAt !== null) return npc.activity === "read" ? "read" : "work";
    if (npc.jobs.some((k) => k !== "review")) return "work";
    if (npc.jobs.length) return "read";
    if (npc.status === "waiting" || npc.asking > this.t) return this.room || this.opts.reducedMotion || !this.world.fauna.board ? "board" : "chase";
    return null;
  }

  private plan(npc: Npc, dt: number): void {
    const want = this.want(npc);
    if (want) {
      if (want !== npc.activity || (want === "visit" && npc.visit && !npc.path.length && npc.arrived) || (want === "chase" && this.chaseStale(npc))) this.go(npc, want);
      return;
    }
    if (npc.activity !== "home" && npc.activity !== "wander") {
      this.go(npc, "home");
      return;
    }
    if (!npc.arrived || this.opts.reducedMotion || npc.status === "thinking") return;
    npc.wait -= dt;
    if (npc.wait > 0) return;
    if (npc.activity === "wander" && npc.roams >= 2 + Math.floor((npc.phase * 10) % 3)) {
      npc.roams = 0;
      this.go(npc, "home");
    } else {
      npc.roams++;
      this.go(npc, "wander");
    }
  }

  private go(npc: Npc, activity: Activity): void {
    npc.activity = activity;
    let spot: Spot | null = null;
    if (activity === "work" || activity === "home") spot = { x: npc.homeX, y: npc.homeY, face: activity === "work" ? "left" : "down" };
    else if (activity === "read") spot = this.freeSpot(npc, this.world.spots.read);
    else if (activity === "board") spot = this.freeSpot(npc, this.world.spots.board);
    else if (activity === "visit") spot = this.visitSpot(npc);
    else if (activity === "chase") spot = this.chaseSpot(npc) ?? this.freeSpot(npc, this.world.spots.board);
    else spot = this.wanderSpot(npc);
    if (activity === "visit" && npc.visit) {
      npc.visit.tries++;
      if (!spot || npc.visit.tries > 2) {
        npc.say = { text: npc.visit.text, until: this.t + 5 };
        npc.visit = null;
        spot = null;
      }
    }
    if (!spot) {
      npc.spot = null;
      npc.path = [];
      npc.arrived = true;
      npc.wait = 3 + Math.random() * 5;
      return;
    }
    npc.spot = spot;
    npc.arrived = false;
    const instant = this.opts.reducedMotion && activity !== "wander";
    const path = instant ? null : this.findPath(npc.x, npc.y, new Set([this.idx(Math.floor(spot.x / T), Math.floor((spot.y - 2) / T))]));
    if (path) {
      path.push({ x: spot.x, y: spot.y });
      npc.path = path;
      return;
    }
    npc.path = [];
    if (activity === "wander") {
      npc.arrived = true;
      npc.wait = 2;
      return;
    }
    npc.x = spot.x;
    npc.y = spot.y;
    this.arriveNpc(npc);
  }

  private step(npc: Npc, dt: number): void {
    let left = NPC_SPEED * dt;
    while (left > 0 && npc.path.length) {
      const wp = npc.path[0];
      const vx = wp.x - npc.x;
      const vy = wp.y - npc.y;
      const d = Math.hypot(vx, vy);
      if (d <= left) {
        npc.x = wp.x;
        npc.y = wp.y;
        left -= d;
        npc.path.shift();
        continue;
      }
      npc.x += (vx / d) * left;
      npc.y += (vy / d) * left;
      npc.dir = dirOf(vx, vy);
      left = 0;
    }
    npc.moving = true;
    npc.anim += dt * 6;
    npc.tx = Math.floor(npc.x / T);
    npc.ty = Math.floor((npc.y - 2) / T);
    if (!npc.path.length) {
      npc.moving = false;
      npc.anim = 0;
      this.arriveNpc(npc);
    }
  }

  private arriveNpc(npc: Npc): void {
    npc.arrived = true;
    npc.tx = Math.floor(npc.x / T);
    npc.ty = Math.floor((npc.y - 2) / T);
    if (npc.activity === "wander") npc.wait = 4 + Math.random() * 8;
    else if (npc.activity === "home") npc.wait = 10 + Math.random() * 16;
    if (npc.activity !== "visit" || !npc.visit) return;
    const other = this.npcById.get(npc.visit.id);
    if (other && Math.hypot(other.x - npc.x, other.y - npc.y) > 40 && npc.visit.tries <= 2) return;
    if (other && npc.spot) npc.spot = { ...npc.spot, face: dirOf(other.x - npc.x, other.y - npc.y) };
    npc.say = { text: npc.visit.text, until: this.t + 5 };
    npc.visit = null;
  }

  private freeSpot(npc: Npc, spots: Spot[]): Spot | null {
    if (!spots.length) return null;
    const start = Math.floor(npc.phase * 10) % spots.length;
    for (let i = 0; i < spots.length; i++) {
      const s = spots[(start + i) % spots.length];
      if (!this.world.npcs.some((o) => o !== npc && o.spot && o.spot.x === s.x && o.spot.y === s.y)) return s;
    }
    return spots[start];
  }

  private visitSpot(npc: Npc): Spot | null {
    const other = npc.visit ? this.npcById.get(npc.visit.id) : null;
    if (!other) return null;
    const ox = Math.floor(other.x / T);
    const oy = Math.floor((other.y - 2) / T);
    const tile = this.walkable(ox, oy + 1) ? this.idx(ox, oy + 1) : this.walkable(ox + 1, oy) ? this.idx(ox + 1, oy) : this.nearestWalkable(ox - 1, oy);
    if (tile === null) return null;
    const x = (tile % this.world.w) * T + 8;
    const y = Math.floor(tile / this.world.w) * T + 13;
    return { x, y, face: dirOf(other.x - x, other.y - y) };
  }

  private chaseStale(npc: Npc): boolean {
    if (this.t - npc.chasedAt < 0.8 || this.focusKey === `r:${npc.resident.id}`) return false;
    const target = npc.spot;
    if (!target) return true;
    return Math.hypot(target.x - this.player.x, target.y - this.player.y) > 2.5 * T;
  }

  private chaseSpot(npc: Npc): Spot | null {
    npc.chasedAt = this.t;
    const px = Math.floor(this.player.x / T);
    const py = Math.floor((this.player.y - 2) / T);
    let best: number | null = null;
    let bestD = Infinity;
    for (let r = 1; r <= 2 && best === null; r++) {
      for (let dy = -r; dy <= r; dy++) {
        for (let dx = -r; dx <= r; dx++) {
          if (Math.max(Math.abs(dx), Math.abs(dy)) !== r || !this.walkable(px + dx, py + dy)) continue;
          const x = (px + dx) * T + 8;
          const y = (py + dy) * T + 13;
          if (this.world.npcs.some((o) => o !== npc && o.spot && Math.abs(o.spot.x - x) < 4 && Math.abs(o.spot.y - y) < 4)) continue;
          const d = Math.hypot(x - npc.x, y - npc.y);
          if (d < bestD) {
            bestD = d;
            best = this.idx(px + dx, py + dy);
          }
        }
      }
    }
    if (best === null) return null;
    const x = (best % this.world.w) * T + 8;
    const y = Math.floor(best / this.world.w) * T + 13;
    return { x, y, face: dirOf(this.player.x - x, this.player.y - y) };
  }

  private smallTalk(npc: Npc): Spot | null {
    const others = this.world.npcs.filter((o) => o !== npc && !o.guide && !o.visit && o.activity !== "chase" && o.status !== "waiting");
    const mates = this.teammates.get(npc.resident.id);
    const team = mates ? others.filter((o) => mates.has(o.resident.id)) : [];
    const pool = team.length > 0 && Math.random() < 0.7 ? team : others;
    const other = pool[Math.floor(Math.random() * pool.length)];
    if (!other) return null;
    npc.chats++;
    npc.visit = { id: other.resident.id, text: SMALL_TALK[Math.floor(Math.random() * SMALL_TALK.length)], tries: 0 };
    npc.activity = "visit";
    return this.visitSpot(npc);
  }

  private wanderSpot(npc: Npc): Spot | null {
    const sp = this.world.spots;
    if (Math.random() < 0.18) {
      const chat = this.smallTalk(npc);
      if (chat) return chat;
      npc.visit = null;
      npc.activity = "wander";
    }
    const roll = Math.random();
    let pool: Spot[] = roll < 0.25 ? sp.benches : roll < 0.5 ? sp.river : roll < 0.75 ? sp.plaza : [];
    if (!pool.length) {
      const others = this.world.npcs.filter((o) => o !== npc && !o.guide);
      const o = others[Math.floor(Math.random() * others.length)];
      if (o) {
        const tile = this.nearestWalkable(Math.floor(o.homeX / T) - 2, Math.floor((o.homeY - 2) / T) + 1);
        if (tile !== null) return { x: (tile % this.world.w) * T + 8, y: Math.floor(tile / this.world.w) * T + 13, face: "up" };
      }
      pool = sp.plaza;
    }
    if (!pool.length) return null;
    const s = pool[Math.floor(Math.random() * pool.length)];
    return this.world.npcs.some((o) => o !== npc && o.spot === s) ? null : s;
  }

  private reading(npc: Npc): boolean {
    return npc.activity === "read" && npc.arrived && !npc.moving && npc.jobs.length > 0 && this.focusKey !== `r:${npc.resident.id}`;
  }

  private emitPage(npc: Npc): void {
    if (this.opts.reducedMotion || this.particles.length >= MAX_PARTICLES) return;
    this.particles.push({ x: npc.x + 2, y: npc.y - 12, vx: 4 + Math.random() * 6, vy: -10 - Math.random() * 6, life: 0, max: 1.1, size: 1, kind: "chip", color: "#fbf3e0" });
  }

  private struck(npc: Npc): boolean {
    const period = STRIKE_PERIOD[npc.resident.trade] / tempo(npc.jobs.length);
    return ((this.t + npc.phase) % period) / period < 0.28;
  }

  private blocked(x: number, y: number, g: Grid = this.outdoor): boolean {
    const x0 = Math.floor((x - 4) / T);
    const x1 = Math.floor((x + 3.99) / T);
    const y0 = Math.floor((y - 4) / T);
    const y1 = Math.floor((y - 0.01) / T);
    for (let ty = y0; ty <= y1; ty++) for (let tx = x0; tx <= x1; tx++) if (!this.walkable(tx, ty, g)) return true;
    return false;
  }

  private face(p: Point): void {
    const dx = p.x - this.player.x;
    const dy = p.y - this.player.y;
    this.player.dir = Math.abs(dx) > Math.abs(dy) ? (dx > 0 ? "right" : "left") : dy > 0 ? "down" : "up";
  }

  private loop = (now: number) => {
    this.raf = requestAnimationFrame(this.loop);
    if (now < this.due - 1.5) return;
    const gap = this.paused && !this.fade ? PAUSED_FRAME_MS : FRAME_MS;
    this.due += gap;
    if (this.due < now) this.due = now + gap;
    const dt = Math.max(0, Math.min(0.05, (now - this.last) / 1000));
    this.last = now;
    this.update(dt);
    this.render();
  };

  private update(dt: number): void {
    this.t += dt;
    this.timeValue += (this.timeTarget - this.timeValue) * Math.min(1, dt * 1.6);
    const pl = this.player;
    let dx = 0;
    let dy = 0;
    let speed = this.keys.has("run") ? 120 : 74;
    if (this.fade) {
      this.fade.t += dt;
      if (this.fade.mid && this.fade.t >= this.fade.dur / 2) {
        const mid = this.fade.mid;
        this.fade.mid = null;
        mid();
      }
      if (this.fade && this.fade.t >= this.fade.dur) this.fade = null;
    }
    const g = this.grid();
    if (!this.paused && !this.fade) {
      if (this.keys.has("up")) dy -= 1;
      if (this.keys.has("down")) dy += 1;
      if (this.keys.has("left")) dx -= 1;
      if (this.keys.has("right")) dx += 1;
    }
    let following = false;
    if (!dx && !dy && this.path.length && !this.fade) {
      const wp = this.path[0];
      const vx = wp.x - pl.x;
      const vy = wp.y - pl.y;
      const d = Math.hypot(vx, vy);
      if (d < 2) {
        this.path.shift();
        if (!this.path.length) this.arrive();
      } else {
        dx = vx / d;
        dy = vy / d;
        speed = 88;
        following = true;
      }
    }
    if (dx || dy) {
      const len = Math.hypot(dx, dy);
      dx /= len;
      dy /= len;
      const step = speed * dt;
      const ox = pl.x;
      const oy = pl.y;
      if (!this.blocked(pl.x + dx * step, pl.y, g)) pl.x += dx * step;
      if (!this.blocked(pl.x, pl.y + dy * step, g)) pl.y += dy * step;
      const moved = Math.hypot(pl.x - ox, pl.y - oy);
      if (following) {
        this.stuck = moved < step * 0.2 ? this.stuck + dt : 0;
        if (this.stuck > 0.5) {
          this.path = [];
          this.pending = null;
        }
      }
      pl.dir = Math.abs(dx) > Math.abs(dy) + 0.01 ? (dx > 0 ? "right" : "left") : dy > 0 ? "down" : "up";
      pl.moving = moved > 0.01;
      pl.anim += dt * 8.5 * (speed / 74);
    } else {
      pl.moving = false;
      pl.anim = 0;
    }

    if (this.room && !this.fade && Math.floor(pl.x / T) + Math.floor((pl.y - 2) / T) * this.room.w === this.room.door) {
      const exit = this.room.spec.exitTo;
      if (!exit || !this.opts.onExit) this.leaveRoom();
      else if (!this.exitLatch) {
        this.exitLatch = true;
        this.opts.onExit(exit);
      }
    }
    if (!this.room && !this.fade && !this.paused && this.started) {
      let on: string | null = null;
      const tx = Math.floor(pl.x / T);
      const ty = Math.floor((pl.y - 2) / T);
      for (const p of this.world.places) if (p.zone && tx >= p.zone[0] && tx <= p.zone[2] && ty >= p.zone[1] && ty <= p.zone[3]) on = p.id;
      if (on && on !== this.portalLatch) {
        this.portalLatch = on;
        this.path = [];
        this.pending = null;
        this.interact({ kind: "place", id: on });
      } else if (!on) this.portalLatch = null;
    }

    this.updateNpcs(dt);
    this.updateCritters(dt);
    this.updateFocus();
    if (this.world.fauna.peacock) this.updatePeacock(dt);
    if (!this.room) {
      this.updateParticles(dt);
      this.updateAmbient(dt);
    }

    const target = this.started
      ? { x: pl.x, y: pl.y - 8 }
      : this.opts.reducedMotion
        ? { x: this.world.plaza.x, y: this.world.plaza.y + 2 * T }
        : {
            x: this.world.plaza.x + Math.cos(this.t * 0.07) * 9 * T,
            y: this.world.plaza.y + 2 * T + Math.sin(this.t * 0.1) * 6 * T,
          };
    const k = this.started ? Math.min(1, dt * 5) : Math.min(1, dt * 0.9);
    this.cam.x += (target.x - this.cam.x) * k;
    this.cam.y += (target.y - this.cam.y) * k;
    const halfW = this.w / this.P / 2;
    const halfH = this.h / this.P / 2;
    const mw = g.w * T;
    const mh = g.h * T;
    const dpr = this.w / Math.max(1, this.canvas.getBoundingClientRect().width);
    const padTop = this.room ? 0 : (80 * this.hudScale * dpr) / this.P;
    const padBottom = this.room ? 0 : (72 * this.hudScale * dpr) / this.P;
    const padSide = this.room ? 0 : (12 * dpr) / this.P;
    this.cam.x = mw + padSide * 2 <= halfW * 2 ? mw / 2 : Math.max(halfW - padSide, Math.min(mw - halfW + padSide, this.cam.x));
    this.cam.y = mh + padTop + padBottom <= halfH * 2 ? mh / 2 : Math.max(halfH - padTop, Math.min(mh - halfH + padBottom, this.cam.y));
    if (this.marker) {
      this.marker.t += dt;
      if (this.marker.t > 0.7 && !this.path.length) this.marker = null;
    }
  }

  private arrive(): void {
    const t = this.pending;
    this.pending = null;
    this.marker = null;
    if (!t) return;
    const p = this.pointOf(t);
    if (p && Math.hypot(p.x - this.player.x, p.y - this.player.y) < 36) {
      this.retried = false;
      this.interact(t);
    } else if (t.kind === "resident" && !this.retried) {
      this.retried = true;
      this.walkTo(t);
    } else this.retried = false;
  }

  private updateFocus(): void {
    if (this.paused) return;
    const pl = this.player;
    let best: Target | null = null;
    let bestD = Infinity;
    const consider = (t: Target, p: Point, range: number) => {
      const d = Math.hypot(p.x - pl.x, (p.y - pl.y) * 1.1);
      if (d < range && d < bestD) {
        best = t;
        bestD = d;
      }
    };
    if (this.room) {
      for (const th of this.room.things) consider({ kind: "thing", id: th.id }, th, 22);
    } else {
      for (const npc of this.world.npcs) consider({ kind: "resident", id: npc.resident.id }, npc, 34);
      for (const c of this.world.critters) if (c.talk) consider({ kind: "folk", id: c.id }, c, 30);
      for (const place of this.world.places) consider({ kind: "place", id: place.id }, place.door, 20);
      for (const plot of this.world.plots) consider({ kind: "plot", index: plot.index }, plot, 34);
      if (this.world.fauna.board) consider({ kind: "board" }, this.world.board, 32);
      if (this.world.fauna.peacock) consider({ kind: "peacock" }, this.peacock, 22);
    }
    const key = targetKey(best);
    if (key !== this.focusKey) {
      this.focusKey = key;
      this.focus = best;
      this.opts.onFocus(best);
    }
  }

  private updatePeacock(dt: number): void {
    const p = this.peacock;
    if (this.t >= p.pauseUntil) {
      p.openTarget = 0;
      p.angle += dt * 0.11 * p.dirSign;
      if (Math.random() < dt * 0.14) {
        p.pauseUntil = this.t + 2.5 + Math.random() * 3;
        p.openTarget = Math.random() < 0.45 ? 1 : 0;
      }
      if (Math.random() < dt * 0.025) p.dirSign *= -1;
    }
    const nx = this.world.plaza.x + Math.cos(p.angle) * 4.6 * T;
    const ny = this.world.plaza.y + 0.5 * T + Math.sin(p.angle) * 3.3 * T;
    if (Math.abs(nx - p.x) > 0.01) p.facing = nx > p.x ? 1 : -1;
    p.x = nx;
    p.y = ny;
    p.open += (p.openTarget - p.open) * Math.min(1, dt * 4);
  }

  private updateParticles(dt: number): void {
    const reduced = this.opts.reducedMotion;
    const vx0 = -this.offX / this.P;
    const vy0 = -this.offY / this.P;
    const vw = this.w / this.P;
    const vh = this.h / this.P;
    this.smokeTimer -= dt;
    if (this.smokeTimer <= 0 && !reduced) {
      this.smokeTimer = 0.45;
      for (const c of this.world.chimneys) {
        if (c.x < vx0 - 40 || c.x > vx0 + vw + 40 || c.y < vy0 - 80 || c.y > vy0 + vh + 40) continue;
        const forge = c.owner === "civic:forge";
        if (forge && !this.signs.runs) continue;
        const busy = forge || (c.owner ? this.npcById.get(c.owner)?.mode === "working" : false);
        this.particles.push({ x: c.x, y: c.y, vx: 2 + Math.random() * 3, vy: -9 - Math.random() * 4, life: 0, max: 3.2, size: 2, kind: "smoke", color: busy ? "busy" : "" });
        if (busy) this.particles.push({ x: c.x + 1, y: c.y - 2, vx: 3 + Math.random() * 3, vy: -12 - Math.random() * 4, life: 0, max: 2.6, size: 2, kind: "smoke", color: "busy" });
      }
    }
    this.petalTimer -= dt;
    if (this.petalTimer <= 0 && !reduced && this.world.fauna.petals && this.timeValue < 0.8 && this.weather === "clear" && this.particles.length < MAX_PARTICLES) {
      this.petalTimer = 0.35;
      this.particles.push({
        x: vx0 + Math.random() * vw,
        y: vy0 - 6,
        vx: 6 + Math.random() * 8,
        vy: 10 + Math.random() * 8,
        life: 0,
        max: vh / 12,
        size: 1,
        kind: "petal",
        color: Math.random() < 0.5 ? "#fbd3e0" : "#f5a3bd",
      });
    }
    this.shellTimer -= dt;
    if (this.shells > 0 && this.shellTimer <= 0 && !reduced) {
      this.shells--;
      this.shellTimer = 0.35 + Math.random() * 0.45;
      const color = FIREWORK_COLORS[Math.floor(Math.random() * FIREWORK_COLORS.length)];
      this.particles.push({ x: this.world.plaza.x + (Math.random() - 0.5) * 90, y: this.world.plaza.y + 10, vx: (Math.random() - 0.5) * 16, vy: -120 - Math.random() * 30, life: 0, max: 0.7 + Math.random() * 0.25, size: 1, kind: "rocket", color });
    }
    let n = 0;
    for (const p of this.particles) {
      p.life += dt;
      if (p.kind === "petal") p.x += Math.sin(this.t * 2 + p.max * 10) * 8 * dt;
      p.x += p.vx * dt;
      p.y += p.vy * dt;
      if (p.kind === "smoke") p.size = 2 + (p.life / p.max) * 4;
      if (p.kind === "heart") p.vy += 6 * dt;
      if (p.kind === "spark" || p.kind === "chip") p.vy += 70 * dt;
      if (p.kind === "bubble" || p.kind === "rune") p.x += Math.sin(this.t * 3 + p.max * 7) * 4 * dt;
      if (p.kind === "flare") {
        p.vy += 26 * dt;
        p.vx *= 1 - dt * 1.2;
      }
      if (p.kind === "rocket") p.vy += 60 * dt;
      if (p.life < p.max) this.particles[n++] = p;
      else if (p.kind === "rocket") this.explode(p.x, p.y, p.color);
    }
    this.particles.length = Math.min(n, this.particles.length);
    if (this.particles.length > MAX_PARTICLES) this.particles.splice(0, this.particles.length - MAX_PARTICLES);
    for (const f of this.fireflies) {
      f.x += Math.sin(this.t * 0.6 * f.speed + f.phase) * 7 * dt;
      f.y += Math.cos(this.t * 0.45 * f.speed + f.phase * 1.3) * 5 * dt;
    }
  }

  private explode(x: number, y: number, color: string): void {
    const count = Math.min(28, MAX_PARTICLES - this.particles.length);
    for (let i = 0; i < count; i++) {
      const a = (i / count) * Math.PI * 2;
      const v = 34 + Math.random() * 14;
      this.particles.push({ x, y, vx: Math.cos(a) * v, vy: Math.sin(a) * v, life: 0, max: 1 + Math.random() * 0.5, size: 1, kind: "flare", color: i % 4 === 0 ? "#ffffff" : color });
    }
  }

  private updateAmbient(dt: number): void {
    const reduced = this.opts.reducedMotion;
    const pl = this.player;
    this.weatherCheck -= dt;
    if (this.weatherCheck <= 0) {
      this.weatherCheck = 30;
      this.weather = this.world.weather ?? weatherAt();
    }
    const goal = reduced || this.weather === "clear" ? 0 : 1;
    this.weatherAmount += (goal - this.weatherAmount) * Math.min(1, dt * 0.25);
    if (this.weatherAmount > 0.01) {
      const vh = this.h / this.P;
      const vw = this.w / this.P;
      if (this.weather === "rain" || goal === 0) {
        for (const d of this.rain) {
          d.y += (d.speed * 230 * dt) / vh;
          d.x -= (40 * dt) / vw;
          if (d.y > 1) {
            d.y %= 1;
            d.x = Math.random();
          }
          if (d.x < 0) d.x = 1 + (d.x % 1);
        }
      }
      if (this.weather === "leaves" || this.weather === "snow" || goal === 0) {
        for (const d of this.leaves) {
          d.y += (d.speed * (this.weather === "snow" ? 22 : 16) * dt) / DRIFT_CELL;
          d.x += ((14 + Math.sin(this.t * 1.3 + d.sway) * 18) * dt) / DRIFT_CELL;
          if (d.y > 1) {
            d.y %= 1;
            d.x = Math.random();
          }
          if (d.x > 1) d.x %= 1;
        }
      }
    }
    for (const c of this.cats) {
      if (reduced) break;
      if (!c.target) {
        c.wait -= dt;
        if (c.wait > 0) continue;
        if (Math.random() < 0.3) {
          c.sleeping = !c.sleeping;
          c.wait = c.sleeping ? 14 + Math.random() * 20 : 2;
          continue;
        }
        c.sleeping = false;
        const tx = c.homeX + (Math.random() - 0.5) * 8 * T;
        const ty = c.homeY + (Math.random() - 0.5) * 5 * T;
        if (this.walkable(Math.floor(tx / T), Math.floor((ty - 2) / T))) c.target = { x: tx, y: ty };
        else c.wait = 0.8;
        continue;
      }
      const vx = c.target.x - c.x;
      const vy = c.target.y - c.y;
      const d = Math.hypot(vx, vy);
      if (d < 1.5) {
        c.target = null;
        c.wait = 3 + Math.random() * 8;
        c.anim = 0;
        continue;
      }
      const step = Math.min(d, 18 * dt);
      const nx = c.x + (vx / d) * step;
      const ny = c.y + (vy / d) * step;
      if (this.blocked(nx, ny)) {
        c.target = null;
        c.wait = 1 + Math.random() * 2;
        continue;
      }
      c.x = nx;
      c.y = ny;
      if (Math.abs(vx) > 0.2) c.facing = vx > 0 ? 1 : -1;
      c.anim += dt * 7;
    }
    for (const b of this.birds) {
      if (b.state === "ground") {
        if (Math.hypot(pl.x - b.x, pl.y - b.y) < 34 && this.started) {
          if (reduced) {
            b.state = "gone";
            b.timer = 20;
            continue;
          }
          b.state = "flying";
          const away = b.x >= pl.x ? 1 : -1;
          b.vx = away * (40 + Math.random() * 30);
          b.vy = -40 - Math.random() * 25;
          b.timer = 2.4;
        }
      } else if (b.state === "flying") {
        b.x += b.vx * dt;
        b.y += b.vy * dt;
        b.vy -= 18 * dt;
        b.timer -= dt;
        if (b.timer <= 0) {
          b.state = "gone";
          b.timer = 15 + Math.random() * 20;
        }
      } else {
        b.timer -= dt;
        if (b.timer <= 0 && Math.hypot(pl.x - b.homeX, pl.y - b.homeY) > 90) {
          b.state = "ground";
          b.x = b.homeX;
          b.y = b.homeY;
        }
      }
    }
    const f = this.fish;
    if (reduced || !this.world.fauna.water) return;
    if (f.active) {
      f.t += dt;
      if (f.t >= 0.75) {
        f.active = false;
        f.next = 4 + Math.random() * 9;
        this.splash(f.x + f.dir * 12, f.y);
      }
      return;
    }
    f.next -= dt;
    if (f.next > 0) return;
    const vx0 = -this.offX / this.P;
    const vw = this.w / this.P;
    const tx = Math.floor((vx0 + Math.random() * vw) / T);
    if (tx < 4 || tx > this.world.w - 5 || (tx > this.world.bridge[0] - 2 && tx < this.world.bridge[1] + 2)) {
      f.next = 1;
      return;
    }
    f.active = true;
    f.t = 0;
    f.dir = Math.random() < 0.5 ? -1 : 1;
    f.x = tx * T + 8;
    f.y = (this.world.riverTop(tx) + 1.2 + Math.random() * 1.2) * T;
    this.splash(f.x, f.y);
  }

  private splash(x: number, y: number): void {
    for (let i = 0; i < 4 && this.particles.length < MAX_PARTICLES; i++) {
      this.particles.push({ x, y, vx: (i - 1.5) * 12, vy: -16 - Math.random() * 10, life: 0, max: 0.4, size: 1, kind: "spark", color: "#cdeef7" });
    }
  }

  private render(): void {
    if (this.room) {
      this.renderRoom(this.room);
      this.drawFade();
      return;
    }
    const { ctx, P } = this;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalCompositeOperation = "source-over";
    ctx.imageSmoothingEnabled = false;
    ctx.fillStyle = this.world.backdrop;
    ctx.fillRect(0, 0, this.w, this.h);
    this.offX = Math.round(this.w / 2 - this.cam.x * P);
    this.offY = Math.round(this.h / 2 - this.cam.y * P);
    ctx.setTransform(P, 0, 0, P, this.offX, this.offY);
    ctx.drawImage(this.world.ground, 0, 0);
    if (this.world.fauna.water) {
      const frames = this.world.waterFrames;
      const wf = this.opts.reducedMotion ? frames[0] : frames[Math.floor(this.t * 5) % frames.length];
      ctx.drawImage(wf, 0, this.world.waterTop * T);
      this.drawDucks();
      this.drawFish();
    }
    this.drawBirds(false);

    const vx0 = -this.offX / P - 70;
    const vy0 = -this.offY / P - 20;
    const vx1 = vx0 + this.w / P + 140;
    const vy1 = vy0 + this.h / P + 140;
    const night = this.timeValue > 0.85;
    type Drawable = { y: number; draw: () => void; box?: [number, number, number, number] };
    const list: Drawable[] = [];
    this.veils = [];
    for (const o of this.world.objects) {
      if (o.x < vx0 || o.x > vx1 || o.y < vy0 || o.y > vy1) continue;
      const s = night && o.night ? o.night : o.sprite;
      const art = this.artOf(o, night);
      if (!art) {
        const x0 = o.x - s.ax;
        const y0 = o.y - s.ay;
        list.push({ y: o.y, draw: () => ctx.drawImage(s.img, Math.round(x0), Math.round(y0)), box: [x0, y0, x0 + s.img.width, y0 + s.img.height] });
        continue;
      }
      list.push({
        y: o.y,
        draw: () => {
          const ground = art.at([art.art.ax, art.art.ay]);
          ellipse(ctx, ground.x, ground.y, art.w * SHADOW_RX, art.w * SHADOW_RY, ART_SHADOW);
          ctx.drawImage(art.art.img, art.x, art.y, art.w, art.h);
        },
        box: [art.x, art.y, art.x + art.w, art.y + art.h],
      });
      const opening = art.art.points.opening;
      if (opening?.length === 2) {
        const [a, b] = opening.map(art.at);
        this.veils.push([a.x, a.y, b.x, b.y]);
      }
    }
    for (const npc of this.world.npcs) {
      if (npc.x < vx0 || npc.x > vx1 || npc.y < vy0 || npc.y > vy1) continue;
      const trade = npc.resident.trade;
      const atWork = npc.mode === "working" && npc.activity === "work";
      const sheet = atWork && trade === "warrior" ? npc.questSheet : npc.sheet;
      const busy = this.working(npc) && npc.idleAt === null;
      const struck = busy && !this.opts.reducedMotion && this.struck(npc);
      const reading = this.reading(npc);
      const step = npc.moving ? npc.anim : struck ? 1 : null;
      const hands = npc.y - this.liftOf(sheet) / 2;
      list.push({
        y: npc.y,
        draw: () => {
          this.drawCharacter(sheet, npc.dir, step, npc.x, npc.y, busy ? TOOL_GEAR[trade] : undefined);
          if (busy && !drawToolArt(ctx, trade, npc.x - TOOL_REACH, hands - TOOL_LIFT, struck)) drawTool(ctx, trade, npc.x, hands, struck, this.t);
          if (reading) this.drawBook(npc.x, hands);
        },
      });
      if (!atWork) continue;
      const st = stationAt(npc.homeX, npc.homeY);
      const t = this.opts.reducedMotion ? 0 : this.t;
      const art = newArtOn();
      if (trade === "warrior") {
        npc.jobs.slice(0, 3).forEach((kind, i) => {
          const s = art ? slimeArtAt(st.x, st.y, i) : slimeAt(st.x, st.y, i, t);
          const flash = i === 0 && struck ? 1 : 0;
          list.push({ y: s.y, draw: () => drawSlimeArt(ctx, kind, s.x, s.y, t, i, flash) || drawSlime(ctx, kind, s.x, s.y, t, i, flash) });
        });
      } else {
        const glow = struck ? 1 : 0.5 + Math.sin(this.t * 2 + npc.phase) * 0.3;
        const steady = this.opts.reducedMotion ? STEADY_GLOW : glow;
        list.push({ y: st.y - 1, draw: () => drawStationArt(ctx, trade, st.x, st.y, t, steady) || drawStation(ctx, trade, st.x, st.y, this.t, glow) });
      }
    }
    for (const c of this.world.critters) {
      if (c.x < vx0 || c.x > vx1 || c.y < vy0 || c.y > vy1) continue;
      const step = c.moving ? c.anim : null;
      list.push({
        y: c.y,
        draw: () => {
          this.drawCharacter(c.sheet, c.dir, step, c.x, c.y);
          if (c.staffed) this.drawStaffBadge(c.x, c.y - this.liftOf(c.sheet));
        },
      });
    }
    for (const c of this.cats) {
      if (c.x < vx0 || c.x > vx1 || c.y < vy0 || c.y > vy1) continue;
      list.push({ y: c.y, draw: () => this.drawCat(c) });
    }
    const pl = this.player;
    list.push({ y: pl.y, draw: () => this.drawCharacter(this.playerSheet, pl.dir, pl.moving ? pl.anim : null, pl.x, pl.y) });
    if (this.world.fauna.peacock) list.push({ y: this.peacock.y, draw: () => this.drawPeacock() });
    for (const place of this.world.places) {
      if (place.kind !== "civic" || place.door.x < vx0 - 60 || place.door.x > vx1 + 60 || place.door.y < vy0 || place.door.y > vy1 + 60) continue;
      list.push({ y: place.door.y - 7.5, draw: () => this.drawSign(place) });
    }
    list.sort((a, b) => a.y - b.y);
    for (const d of list) d.draw();
    const head = pl.y - 16 - this.liftOf(this.playerSheet);
    const hidden = list.some(
      (d) => d.box && d.y > pl.y && d.box[0] < pl.x + 5 && d.box[2] > pl.x - 5 && d.box[1] < pl.y - 4 && d.box[3] > head,
    );
    if (hidden) {
      ctx.globalAlpha = 0.45;
      this.drawCharacter(this.playerSheet, pl.dir, pl.moving ? pl.anim : null, pl.x, pl.y);
      ctx.globalAlpha = 1;
    }

    this.drawBirds(true);
    this.drawMarker();
    this.drawParticles();
    this.drawWeather();
    this.drawLabels();
    this.drawLighting();
    this.drawVeils();
    this.drawLampAlarm();
    ctx.setTransform(P, 0, 0, P, this.offX, this.offY);
    this.drawClouds();
    this.drawBubbles();
    this.drawSays();
    this.drawClockNote();
    this.drawPrompt();
    this.drawFade();
  }

  private renderRoom(room: Room): void {
    const { ctx, P } = this;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalCompositeOperation = "source-over";
    ctx.imageSmoothingEnabled = false;
    ctx.fillStyle = "#0d0907";
    ctx.fillRect(0, 0, this.w, this.h);
    this.offX = Math.round(this.w / 2 - this.cam.x * P);
    this.offY = Math.round(this.h / 2 - this.cam.y * P);
    ctx.setTransform(P, 0, 0, P, this.offX, this.offY);
    ctx.drawImage(room.ground, 0, 0);
    const pl = this.player;
    const rugs = room.objects.filter((o) => thingKey(o.sprite) === "room_rug");
    for (const o of rugs) {
      const art = this.artOf(o, false);
      if (art) ctx.drawImage(art.art.img, art.x, o.y, art.w, art.h);
      else ctx.drawImage(o.sprite.img, Math.round(o.x - o.sprite.ax), o.y);
    }
    const onFloor = room.glows.filter((glow) => glow.source && glow.floor !== undefined && thingOf(glow.source, 0));
    this.drawRoomGlows(onFloor.map((glow) => ({ ...glow, y: glow.floor ?? glow.y })));
    const list: { y: number; draw: () => void }[] = room.objects.filter((o) => !rugs.includes(o)).map((o) => {
      const art = this.artOf(o, false);
      if (!art) return { y: o.y, draw: () => ctx.drawImage(o.sprite.img, Math.round(o.x - o.sprite.ax), Math.round(o.y + 1 - o.sprite.ay)) };
      return {
        y: o.y,
        draw: () => {
          const ground = art.at([art.art.ax, art.art.ay]);
          ellipse(ctx, ground.x, ground.y + 1, art.w * SHADOW_RX, art.w * SHADOW_RY, ART_SHADOW);
          ctx.drawImage(art.art.img, art.x, art.y + 1, art.w, art.h);
        },
      };
    });
    list.push({ y: pl.y, draw: () => this.drawCharacter(this.playerSheet, pl.dir, pl.moving ? pl.anim : null, pl.x, pl.y) });
    list.sort((a, b) => a.y - b.y);
    for (const d of list) d.draw();
    this.drawMarker();
    this.drawRoomGlows(room.glows.filter((glow) => !onFloor.includes(glow)));
    this.drawPrompt();
  }

  private drawRoomGlows(glows: RoomGlow[]): void {
    const { ctx, P } = this;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalCompositeOperation = "lighter";
    for (const glow of glows) {
      const sx = glow.x * P + this.offX;
      const sy = glow.y * P + this.offY;
      const pulse = glow.pulse && !this.opts.reducedMotion ? 0.75 + Math.sin(this.t * 4 + glow.x) * 0.25 : 1;
      const rr = 18 * P;
      const [r, g, b] = glow.color;
      const grad = ctx.createRadialGradient(sx, sy, 0, sx, sy, rr);
      grad.addColorStop(0, `rgba(${r},${g},${b},${0.32 * pulse})`);
      grad.addColorStop(1, `rgba(${r},${g},${b},0)`);
      ctx.fillStyle = grad;
      ctx.fillRect(sx - rr, sy - rr, rr * 2, rr * 2);
    }
    ctx.globalCompositeOperation = "source-over";
    ctx.setTransform(P, 0, 0, P, this.offX, this.offY);
  }

  private drawFade(): void {
    const f = this.fade;
    if (!f) return;
    const half = f.dur / 2;
    const a = f.t < half ? f.t / half : Math.max(0, (f.dur - f.t) / half);
    const ctx = this.ctx;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = `rgba(8,6,5,${Math.min(1, a)})`;
    ctx.fillRect(0, 0, this.w, this.h);
    ctx.setTransform(this.P, 0, 0, this.P, this.offX, this.offY);
  }

  private drawSign(place: Place): void {
    const ctx = this.ctx;
    const s = this.signs;
    const still = this.opts.reducedMotion;
    if (place.ref === "clock") {
      const c = place.anchors.clock;
      if (!c) return;
      const now = new Date();
      const min = (now.getMinutes() + now.getSeconds() / 60) / 60;
      const hour = ((now.getHours() % 12) + min) / 12;
      const hand = (frac: number, len: number, color: string) => {
        const a = frac * Math.PI * 2 - Math.PI / 2;
        ctx.fillStyle = color;
        for (let i = 0; i <= len; i++) ctx.fillRect(Math.round(c.x + Math.cos(a) * i), Math.round(c.y + Math.sin(a) * i), 1, 1);
      };
      const rim = place.anchors.clockRim;
      const reach = rim ? Math.hypot(rim.x - c.x, rim.y - c.y) : 10;
      hand(hour, reach * 0.5, "#3a2a20");
      hand(min, reach * 0.8, "#3a2a20");
      ctx.fillStyle = s.nextTimer ? "#c0443a" : "#7a5a3a";
      ctx.fillRect(Math.round(c.x) - 1, Math.round(c.y) - 1, 2, 2);
    } else if (place.ref === "post") {
      const top = place.anchors.flagTop;
      const low = place.anchors.flagLow;
      if (!top || (!low && s.unread <= 0)) return;
      const y = s.unread > 0 || !low ? top.y + 1 : low.y - 7;
      const wave = still ? 0 : Math.floor(this.t * 3) % 2;
      ctx.fillStyle = "#c0443a";
      ctx.fillRect(Math.round(top.x) + 1, Math.round(y), 9, 6);
      ctx.fillRect(Math.round(top.x) + 10, Math.round(y) + wave, 1, 6 - wave);
      ctx.fillStyle = "#fbf3e0";
      ctx.fillRect(Math.round(top.x) + 1, Math.round(y) + 2, 9, 2);
    } else if (place.ref === "forge") {
      const devices = s.devices.slice(0, 3);
      const bx = Math.round(place.door.x + 34);
      const by = Math.round(place.door.y + 3);
      ctx.fillStyle = "#8d8478";
      ctx.fillRect(bx - 1, by - 3, 30, 4);
      ctx.fillStyle = "#a79d90";
      ctx.fillRect(bx - 1, by - 3, 30, 1);
      devices.forEach((d, i) => {
        const x = bx + i * 10;
        ctx.fillStyle = d.state === "offline" ? "#3a3f4a" : "#5f6a7a";
        ctx.fillRect(x, by - 15, 8, 12);
        ctx.fillStyle = "#1d2028";
        ctx.fillRect(x + 1, by - 13, 6, 4);
        ctx.fillStyle = d.state === "offline" ? "#2a2a30" : d.state === "busy" ? "#ffc94a" : "#5fe08a";
        ctx.fillRect(x + 2, by - 12, d.state === "busy" ? 4 : 2, 2);
        ctx.fillRect(x + 5, by - 7, 2, 2);
      });
    } else if (place.ref === "library") {
      const g = place.anchors.glow;
      if (!g || !s.reading) return;
      ctx.fillStyle = "#3a2a20";
      ctx.fillRect(Math.round(g.x) - 3, Math.round(g.y) - 4, 6, 6);
      ctx.fillStyle = still || Math.floor(this.t * 2) % 3 ? "#ffd77a" : "#fff0b8";
      ctx.fillRect(Math.round(g.x) - 2, Math.round(g.y) - 3, 4, 4);
    } else if (place.ref === "observatory") {
      const lens = place.anchors.lens;
      if (!lens || Date.now() - s.sightingAt > 600_000) return;
      const tw = still ? 1 : Math.floor(this.t * 3) % 3;
      const x = Math.round(lens.x) + 6;
      const y = Math.round(lens.y) - 8;
      ctx.fillStyle = "#ffe066";
      ctx.fillRect(x - tw, y, tw * 2 + 1, 1);
      ctx.fillRect(x, y - tw, 1, tw * 2 + 1);
      ctx.fillStyle = "#ffffff";
      ctx.fillRect(x, y, 1, 1);
    }
  }

  private drawClockNote(): void {
    const place = this.placeById.get("civic:clock");
    const c = place?.anchors.clock;
    if (!c) return;
    const { ctx, P } = this;
    const sx = c.x * P + this.offX;
    const sy = (c.y - 18) * P + this.offY;
    if (sx < -200 || sx > this.w + 200 || sy < -60 || sy > this.h + 60) return;
    const next = this.signs.nextTimer;
    let text = "No timers";
    if (next) {
      const mins = Math.max(0, Math.round((next.at - Date.now()) / 60000));
      const when = mins < 1 ? "now" : mins < 60 ? `${mins}m` : mins < 1440 ? `${Math.round(mins / 60)}h` : `${Math.round(mins / 1440)}d`;
      text = `${next.label.slice(0, 18)} · ${when}`;
    }
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    const size = Math.max(11, P * 3.4);
    ctx.font = this.font(size);
    ctx.textBaseline = "middle";
    ctx.textAlign = "center";
    const w = ctx.measureText(text).width + size;
    const h = size * 1.6;
    ctx.fillStyle = "rgba(38,24,34,0.82)";
    ctx.beginPath();
    ctx.roundRect(Math.round(sx - w / 2), Math.round(sy - h), w, h, h * 0.3);
    ctx.fill();
    ctx.fillStyle = next ? "#ffe9b0" : "#d8d0c4";
    ctx.fillText(text, Math.round(sx), Math.round(sy - h / 2 + 1));
    ctx.setTransform(P, 0, 0, P, this.offX, this.offY);
  }

  private artOf(o: WorldObject, night: boolean): Placed | null {
    let hit = this.artCache.get(o);
    if (!hit) {
      hit = [placeArt(o.sprite, o.x, o.y), o.night ? placeArt(o.night, o.x, o.y) : null];
      this.artCache.set(o, hit);
    }
    return night && o.night ? hit[1] : hit[0];
  }

  private drawVeils(): void {
    if (!this.veils.length) return;
    const { ctx, P } = this;
    const still = this.opts.reducedMotion;
    ctx.setTransform(P, 0, 0, P, this.offX, this.offY);
    ctx.globalCompositeOperation = "lighter";
    for (const [x0, y0, x1, y1] of this.veils) {
      const r = (x1 - x0) / 2;
      const cx = x0 + r;
      const alpha = still ? 0.5 : 0.5 + Math.sin(this.t * 2 + x0) * 0.15;
      const drift = still ? 0 : Math.sin(this.t * 1.3 + x0) * (y1 - y0) * 0.15;
      const grad = ctx.createLinearGradient(0, y0 + drift, 0, y1);
      grad.addColorStop(0, `rgba(255,233,168,${alpha})`);
      grad.addColorStop(0.6, `rgba(255,220,150,${alpha * 0.55})`);
      grad.addColorStop(1, "rgba(255,210,140,0)");
      ctx.fillStyle = grad;
      ctx.beginPath();
      ctx.moveTo(x0, y1);
      ctx.lineTo(x0, y0 + r);
      ctx.arc(cx, y0 + r, r, Math.PI, 0);
      ctx.lineTo(x1, y1);
      ctx.closePath();
      ctx.fill();
    }
    ctx.globalCompositeOperation = "source-over";
  }

  private liftOf(sheet: CharacterSheet): number {
    const figure = figureOf(sheet);
    return figure ? Math.max(0, figure.height - CODE_HEIGHT) : 0;
  }

  private heldSpots(gear: string[], dir: Dir, height: number, x: number, y: number): { gear: string; x: number; y: number; under: boolean }[] {
    const reach = height * HAND_REACH;
    const hand = y - height * HAND_LIFT;
    const ahead = dir === "left" ? -1 : 1;
    const spots =
      dir === "down" ? [{ x: x + reach, under: false }, { x: x - reach, under: false }]
      : dir === "up" ? [{ x: x - reach, under: true }, { x: x + reach, under: true }]
      : [{ x: x + ahead, under: false }, { x: x - ahead * 3, under: true }];
    return gear.slice(0, spots.length).map((g, i) => ({ gear: g, x: spots[i].x, y: hand, under: spots[i].under }));
  }

  private drawCharacter(sheet: CharacterSheet, dir: Dir, step: number | null, x: number, y: number, inTool?: string): void {
    const ctx = this.ctx;
    const figure = figureOf(sheet);
    if (figure) {
      const pose = figure.poses[dir];
      const idle = this.opts.reducedMotion ? 0 : walkFrame((this.t * IDLE_FPS + x * 0.37) / pose.idle.length, pose.idle.length);
      const img = step === null ? pose.idle[idle] : pose.walk[walkFrame(step / CODE_WALK_FRAMES, pose.walk.length)];
      const [px, py] = placed(pose.anchor, figure.density, x, y);
      const grow = figure.height / CODE_HEIGHT;
      ctx.drawImage(this.shadow, Math.round(x - 6 * grow), Math.round(y - 2), Math.round(12 * grow), 4);
      const w = img.width / figure.density;
      const h = img.height / figure.density;
      const held = this.heldSpots(heldGearOf(sheet).filter((g) => g !== inTool), dir, figure.height, x, y);
      for (const g of held) if (g.under) drawHeldArt(ctx, g.gear, g.x, g.y);
      if (pose.flip) {
        ctx.save();
        ctx.translate(px + w, py);
        ctx.scale(-1, 1);
        ctx.drawImage(img, 0, 0, w, h);
        ctx.restore();
      } else ctx.drawImage(img, px, py, w, h);
      for (const g of held) if (!g.under) drawHeldArt(ctx, g.gear, g.x, g.y);
      return;
    }
    ctx.drawImage(this.shadow, Math.round(x - 6), Math.round(y - 2));
    if (figurePending(sheet)) return;
    const frame = step === null ? 0 : Math.floor(step) % CODE_WALK_FRAMES;
    const img = sheet[dir][frame];
    const bob = frame % 2 === 1 ? -1 : 0;
    ctx.drawImage(img, Math.round(x - 7), Math.round(y - 20 + bob));
  }

  private drawDucks(): void {
    const ctx = this.ctx;
    for (const d of this.ducks) {
      const s = Math.sin(this.t * 0.11 + d.phase);
      const x = d.base + s * 70;
      const dir = Math.cos(this.t * 0.11 + d.phase) >= 0 ? 1 : -1;
      const fx = x / T;
      const f0 = Math.floor(fx);
      const frac = fx - f0;
      const top = this.world.riverTop(f0) * (1 - frac) + this.world.riverTop(f0 + 1) * frac;
      const y = Math.round((top + 1.6) * T + Math.sin(this.t * 1.7 + d.phase) * 0.7);
      const rx = Math.round(x);
      ctx.save();
      ctx.translate(rx, y);
      ctx.scale(dir, 1);
      ctx.fillStyle = "rgba(210,240,250,0.55)";
      ctx.fillRect(-7, 1, 3, 1);
      ctx.fillRect(-10, 2, 2, 1);
      if (this.drawArt(d.small ? DUCKLING : DUCK, DUCK_FRAME[d.color] ?? 0, 0, 1)) {
        ctx.restore();
        continue;
      }
      if (d.small) {
        ctx.fillStyle = d.color;
        ctx.fillRect(-2, -2, 4, 2);
        ctx.fillRect(1, -4, 2, 2);
        ctx.fillStyle = "#f29a3a";
        ctx.fillRect(3, -3, 1, 1);
        ctx.restore();
        continue;
      }
      ctx.fillStyle = d.color;
      ctx.fillRect(-4, -3, 8, 4);
      ctx.fillRect(-5, -2, 1, 2);
      ctx.fillStyle = "#2e7d4f";
      ctx.fillRect(2, -6, 3, 3);
      ctx.fillStyle = "#1a1a1a";
      ctx.fillRect(4, -5, 1, 1);
      ctx.fillStyle = "#f29a3a";
      ctx.fillRect(5, -4, 2, 1);
      ctx.fillStyle = "rgba(0,0,0,0.12)";
      ctx.fillRect(-3, -1, 5, 1);
      ctx.restore();
    }
  }

  private drawArt(ref: object, frame: number, x: number, y: number, flip = false, scale = 1, centre = false): ThingImage | null {
    const art = thingOf(ref, frame);
    if (!art) return null;
    const d = art.density;
    const w = (art.img.width / d) * scale;
    const h = (art.img.height / d) * scale;
    const left = Math.round((x - (art.ax / d) * scale) * d) / d;
    const top = Math.round((centre ? y - h / 2 : y - (art.ay / d) * scale) * d) / d;
    const ctx = this.ctx;
    if (!flip) {
      ctx.drawImage(art.img, left, top, w, h);
      return art;
    }
    ctx.save();
    ctx.translate(2 * x, 0);
    ctx.scale(-1, 1);
    ctx.drawImage(art.img, left, top, w, h);
    ctx.restore();
    return art;
  }

  private drawPeacock(): void {
    const ctx = this.ctx;
    const p = this.peacock;
    const s = this.peacockSprite;
    ctx.save();
    ctx.translate(Math.round(p.x), Math.round(p.y));
    if (p.facing < 0) ctx.scale(-1, 1);
    ctx.drawImage(this.shadow, -6, -2);
    const step = this.t < p.pauseUntil ? 0 : Math.floor(this.t * 6) % 2;
    if (thingOf(PEACOCK, 0)) {
      if (p.open > 0.05) this.drawArt(PEACOCK_FAN, 0, -9, -8, false, p.open);
      this.drawArt(PEACOCK, 0, 0, -step / 2);
      ctx.restore();
      return;
    }
    if (p.open > 0.05) {
      const fw = Math.max(2, Math.round(s.fan.width * p.open));
      const fh = Math.max(2, Math.round(s.fan.height * p.open));
      ctx.drawImage(s.fan, Math.round(-2 - fw / 2), Math.round(-6 - fh), fw, fh);
    }
    const walk = this.t < p.pauseUntil ? 0 : Math.floor(this.t * 6) % 2;
    const img = (p.open > 0.35 ? s.displaying : s.walk)[walk];
    ctx.drawImage(img, -s.ax, -s.ay);
    ctx.restore();
  }

  private drawBook(x: number, y: number): void {
    const ctx = this.ctx;
    if (drawBookArt(ctx, x, y - BOOK_LIFT)) return;
    const bx = Math.round(x) - 4;
    const by = Math.round(y) - 10;
    ctx.fillStyle = "#7a3b2e";
    ctx.fillRect(bx, by, 9, 5);
    ctx.fillStyle = "#fbf3e0";
    ctx.fillRect(bx + 1, by, 3, 4);
    ctx.fillRect(bx + 5, by, 3, 4);
    ctx.fillStyle = "#c9c2b4";
    ctx.fillRect(bx + 4, by, 1, 4);
    if (!this.opts.reducedMotion && Math.floor(this.t * 0.7) % 3 === 0) ctx.fillRect(bx + 5, by + 1, 2, 1);
  }

  private drawFish(): void {
    const f = this.fish;
    if (!f.active) return;
    const k = f.t / 0.75;
    const x = Math.round(f.x + f.dir * 12 * k);
    const y = Math.round(f.y - Math.sin(Math.PI * k) * 11);
    const ctx = this.ctx;
    if (this.drawArt(KOI, k < 0.5 ? 0 : 1, x, y, f.dir > 0, 1, true)) return;
    ctx.fillStyle = "#e88a4a";
    ctx.fillRect(x - 2, y - 1, 4, 2);
    ctx.fillStyle = "#f6d0a8";
    ctx.fillRect(x - 1, y, 2, 1);
    ctx.fillStyle = "#c0602e";
    ctx.fillRect(f.dir > 0 ? x - 3 : x + 2, k < 0.5 ? y : y - 1, 1, 2);
  }

  private drawBirds(flying: boolean): void {
    const ctx = this.ctx;
    for (const b of this.birds) {
      if (b.state === "gone" || (b.state === "flying") !== flying) continue;
      const x = Math.round(b.x);
      const y = Math.round(b.y);
      if (flying) {
        const up = Math.floor(this.t * 12 + b.phase) % 2 === 0;
        if (this.drawArt(SPARROW, up ? 2 : 3, x, y, false, 1, true)) continue;
        ctx.fillStyle = "#3b3240";
        ctx.fillRect(x, y, 1, 1);
        ctx.fillRect(x - 2, up ? y - 1 : y + 1, 2, 1);
        ctx.fillRect(x + 1, up ? y - 1 : y + 1, 2, 1);
        continue;
      }
      const peck = !this.opts.reducedMotion && Math.sin(this.t * 2.3 + b.phase) > 0.75 ? 1 : 0;
      ctx.fillStyle = "rgba(20,24,20,0.2)";
      ctx.fillRect(x - 1, y + 1, 4, 1);
      if (this.drawArt(SPARROW, peck, x + 1, y + 1)) continue;
      ctx.fillStyle = "#7a5a44";
      ctx.fillRect(x - 1, y - 2, 3, 2);
      ctx.fillStyle = "#5a4032";
      ctx.fillRect(x + 1, y - 3 + peck, 2, 2);
      ctx.fillStyle = "#e8b64a";
      ctx.fillRect(x + 3, y - 2 + peck, 1, 1);
    }
  }

  private drawCat(c: Cat): void {
    const ctx = this.ctx;
    ctx.save();
    ctx.translate(Math.round(c.x), Math.round(c.y));
    if (c.facing < 0) ctx.scale(-1, 1);
    ctx.fillStyle = "rgba(20,24,20,0.22)";
    ctx.fillRect(-4, -1, 9, 2);
    const bob = c.target && !c.sleeping ? (Math.floor(c.anim) % 2) / 2 : 0;
    if (this.drawArt(c.coat === "#8a8f99" ? CAT_GREY : CAT, c.sleeping ? 1 : 0, 0, 1 - bob)) {
      ctx.restore();
      return;
    }
    if (c.sleeping) {
      ctx.fillStyle = c.coat;
      ctx.fillRect(-4, -4, 8, 4);
      ctx.fillRect(-3, -5, 6, 1);
      ctx.fillStyle = c.dark;
      ctx.fillRect(-4, -1, 8, 1);
      ctx.fillRect(2, -4, 2, 2);
      ctx.fillRect(-5, -2, 2, 1);
      ctx.restore();
      return;
    }
    const step = c.target ? Math.floor(c.anim) % 2 : 0;
    ctx.fillStyle = c.coat;
    ctx.fillRect(-4, -6, 7, 3);
    ctx.fillRect(2, -9, 4, 4);
    ctx.fillRect(2, -10, 1, 1);
    ctx.fillRect(5, -10, 1, 1);
    ctx.fillStyle = c.dark;
    ctx.fillRect(-3, -3, 1, 3 - step);
    ctx.fillRect(1, -3, 1, 2 + step);
    ctx.fillRect(-1, -6, 1, 3);
    ctx.fillRect(-5, -9, 1, 4);
    ctx.fillRect(-6, -10, 1, 1);
    ctx.fillStyle = "#1a1a1a";
    ctx.fillRect(4, -8, 1, 1);
    ctx.restore();
  }

  private drawWeather(): void {
    const a = this.weatherAmount;
    if (a < 0.01) return;
    const ctx = this.ctx;
    const vx0 = -this.offX / this.P;
    const vy0 = -this.offY / this.P;
    const vw = this.w / this.P;
    const vh = this.h / this.P;
    if (this.weather === "rain") {
      ctx.fillStyle = `rgba(60,80,110,${0.12 * a})`;
      ctx.fillRect(vx0, vy0, vw, vh);
      ctx.fillStyle = `rgba(190,215,240,${0.55 * a})`;
      const count = Math.floor(this.rain.length * a);
      for (let i = 0; i < count; i++) {
        const d = this.rain[i];
        const x = Math.round(vx0 + d.x * vw);
        const y = Math.round(vy0 + d.y * vh);
        ctx.fillRect(x, y, 1, 3);
        ctx.fillRect(x - 1, y + 3, 1, 1);
      }
    } else if (this.weather === "snow") {
      ctx.fillStyle = `rgba(220,232,245,${0.1 * a})`;
      ctx.fillRect(vx0, vy0, vw, vh);
      ctx.fillStyle = `rgba(255,255,255,${0.9 * a})`;
      for (const d of this.leaves) {
        const size = d.speed > 0.9 ? 2 : 1;
        drift(d, vx0, vy0, vw, vh, (x, y) => ctx.fillRect(Math.round(x + Math.sin(this.t + d.sway) * 3), Math.round(y), size, size));
      }
      for (const d of this.rain) {
        const x = Math.round(vx0 + d.x * vw);
        const y = Math.round(vy0 + ((d.y + this.t * 0.03 * d.speed) % 1) * vh);
        ctx.fillRect(x, y, 1, 1);
      }
    } else if (this.weather === "fog") {
      ctx.fillStyle = `rgba(214,222,226,${0.26 * a})`;
      ctx.fillRect(vx0, vy0, vw, vh);
      ctx.fillStyle = `rgba(236,240,242,${0.18 * a})`;
      const gap = 56;
      const span = 340;
      const len = 220;
      for (let k = Math.floor(vy0 / gap) - 1; k * gap < vy0 + vh + gap; k++) {
        const i = ((k % 5) + 5) % 5;
        const y = k * gap + ((k * 23) % 17);
        const off = (this.t * (6 + i * 2) + k * 137) % span;
        for (let x = Math.floor((vx0 - len - off) / span) * span + off; x < vx0 + vw; x += span) {
          ctx.fillRect(Math.round(x), Math.round(y), len, 8 + i * 2);
        }
      }
    } else if (this.weather === "leaves") {
      const count = Math.floor(this.leaves.length * a);
      for (let i = 0; i < count; i++) {
        const d = this.leaves[i];
        ctx.fillStyle = i % 3 === 0 ? "#d9822b" : i % 3 === 1 ? "#c4552d" : "#e8b64a";
        const flip = Math.floor(this.t * 3 + d.sway) % 2;
        drift(d, vx0, vy0, vw, vh, (fx, fy) => {
          const x = Math.round(fx);
          const y = Math.round(fy);
          ctx.fillRect(x, y, 2, 1);
          ctx.fillRect(x + flip, y + 1, 1, 1);
        });
      }
    }
  }

  private drawClouds(): void {
    const ctx = this.ctx;
    for (const npc of this.world.npcs) {
      if (this.t >= npc.cloudUntil) continue;
      const fade = Math.min(1, (npc.cloudUntil - this.t) / 6);
      const cx = Math.round(npc.house ? npc.house.x : npc.x);
      const cy = Math.round(npc.house ? npc.house.y : npc.y - 46);
      const drift = this.opts.reducedMotion ? 0 : Math.round(Math.sin(this.t * 0.4 + npc.phase) * 3);
      ctx.globalAlpha = 0.85 * fade;
      ctx.fillStyle = "#c6ccd6";
      ctx.fillRect(cx - 12 + drift, cy - 4, 24, 6);
      ctx.fillRect(cx - 8 + drift, cy - 8, 10, 5);
      ctx.fillRect(cx + 1 + drift, cy - 10, 8, 7);
      ctx.fillStyle = "#9aa2b0";
      ctx.fillRect(cx - 12 + drift, cy + 1, 24, 2);
      ctx.fillStyle = "#8fc2e8";
      for (let i = 0; i < 6; i++) {
        const fall = this.opts.reducedMotion ? (i * 7) % 18 : (this.t * 40 + i * 13) % 26;
        ctx.fillRect(cx - 10 + i * 4 + drift, cy + 4 + Math.round(fall), 1, 2);
      }
      ctx.globalAlpha = 1;
    }
  }

  private drawSays(): void {
    const { ctx, P } = this;
    let any = false;
    const ink = newArtOn() ? ART_INK : "#261822";
    const paper = newArtOn() ? ART_PAPER : "#fbf3e0";
    const speakers: { x: number; y: number; say: { text: string } }[] = [];
    for (const npc of this.world.npcs) if (npc.say && this.focusKey !== `r:${npc.resident.id}`) speakers.push({ x: npc.x, y: npc.y - this.liftOf(npc.sheet), say: npc.say });
    for (const c of this.world.critters) if (c.say && this.focusKey !== `f:${c.id}`) speakers.push({ x: c.x, y: c.y - this.liftOf(c.sheet), say: c.say });
    for (const npc of speakers) {
      if (!any) {
        any = true;
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.font = this.font(Math.max(11, P * 3.4));
        ctx.textBaseline = "middle";
        ctx.textAlign = "center";
      }
      const size = Math.max(11, P * 3.4);
      const tw = ctx.measureText(npc.say.text).width;
      const w = tw + size * 1.2;
      const h = size * 1.7;
      const cx = Math.round(npc.x * P + this.offX);
      const top = Math.round((npc.y - 36) * P + this.offY - h);
      if (cx < -w || cx > this.w + w || top < -h * 2 || top > this.h) continue;
      const left = Math.round(cx - w / 2);
      ctx.fillStyle = ink;
      ctx.beginPath();
      ctx.roundRect(left - P / 2, top - P / 2, w + P, h + P, h * 0.3);
      ctx.fill();
      ctx.fillStyle = paper;
      ctx.beginPath();
      ctx.roundRect(left, top, w, h, h * 0.28);
      ctx.fill();
      ctx.beginPath();
      ctx.moveTo(cx - P * 1.5, top + h);
      ctx.lineTo(cx + P * 1.5, top + h);
      ctx.lineTo(cx, top + h + P * 1.6);
      ctx.fill();
      ctx.fillStyle = ink;
      ctx.fillText(npc.say.text, cx, top + h / 2 + 1);
    }
    if (any) ctx.setTransform(P, 0, 0, P, this.offX, this.offY);
  }

  private drawStaffBadge(x: number, y: number): void {
    const ctx = this.ctx;
    if (drawIconArt(ctx, "star", x + 6.5, y - 20.5)) return;
    const bx = Math.round(x + 4);
    const by = Math.round(y - 23);
    ctx.fillStyle = "#1d1b2a";
    ctx.fillRect(bx, by + 1, 5, 3);
    ctx.fillRect(bx + 1, by, 3, 5);
    ctx.fillStyle = "#ffd75e";
    ctx.fillRect(bx + 1, by + 2, 3, 1);
    ctx.fillRect(bx + 2, by + 1, 1, 3);
  }

  private drawMarker(): void {
    const m = this.marker;
    if (!m) return;
    const ctx = this.ctx;
    const r = 3 + Math.min(1, m.t / 0.3) * 3;
    const a = this.path.length ? 0.8 : Math.max(0, 0.8 - m.t);
    ctx.fillStyle = `rgba(255,248,220,${a})`;
    ctx.fillRect(Math.round(m.x - r), Math.round(m.y), 2, 1);
    ctx.fillRect(Math.round(m.x + r - 1), Math.round(m.y), 2, 1);
    ctx.fillRect(Math.round(m.x), Math.round(m.y - r / 2), 1, 1);
    ctx.fillRect(Math.round(m.x), Math.round(m.y + r / 2), 1, 1);
  }

  private drawParticles(): void {
    const ctx = this.ctx;
    const nightTone = this.timeValue > 1.2;
    for (const p of this.particles) {
      const k = p.life / p.max;
      if (this.drawParticleArt(p, k, nightTone)) continue;
      if (p.kind === "smoke") {
        const a = (p.color === "busy" ? 0.7 : 0.55) * (1 - k);
        ctx.fillStyle = nightTone ? `rgba(150,150,170,${a * 0.75})` : p.color === "busy" ? `rgba(225,222,230,${a})` : `rgba(240,240,245,${a})`;
        const s = Math.round(p.size);
        ctx.fillRect(Math.round(p.x - s / 2), Math.round(p.y - s / 2), s, s);
      } else if (p.kind === "spark") {
        ctx.fillStyle = p.color;
        ctx.globalAlpha = Math.max(0, 1 - k * k);
        ctx.fillRect(Math.round(p.x), Math.round(p.y), 1, 1);
        ctx.globalAlpha = 1;
      } else if (p.kind === "chip") {
        ctx.fillStyle = p.color;
        ctx.fillRect(Math.round(p.x), Math.round(p.y), Math.floor(this.t * 10 + p.max * 9) % 2 ? 2 : 1, 1);
      } else if (p.kind === "bubble") {
        ctx.fillStyle = p.color;
        const x = Math.round(p.x);
        const y = Math.round(p.y);
        if (k > 0.85) {
          ctx.fillRect(x - 1, y, 1, 1);
          ctx.fillRect(x + p.size, y, 1, 1);
          ctx.fillRect(x, y - 1, p.size, 1);
          ctx.fillRect(x, y + p.size, p.size, 1);
        } else {
          ctx.globalAlpha = 0.85;
          ctx.fillRect(x, y, p.size, p.size);
          ctx.globalAlpha = 1;
        }
      } else if (p.kind === "rune") {
        ctx.globalAlpha = Math.max(0, Math.min(1, k * 4, (1 - k) * 2));
        drawRune(ctx, p.glyph ?? 0, p.x, p.y, p.color);
        ctx.globalAlpha = 1;
      } else if (p.kind === "hit") {
        const r = 1 + Math.round(k * 3);
        const x = Math.round(p.x);
        const y = Math.round(p.y);
        ctx.fillStyle = p.color;
        ctx.globalAlpha = 1 - k;
        ctx.fillRect(x - r, y, r * 2 + 1, 1);
        ctx.fillRect(x, y - r, 1, r * 2 + 1);
        ctx.globalAlpha = 1;
      } else if (p.kind === "rocket") {
        ctx.fillStyle = "#fff2c4";
        ctx.fillRect(Math.round(p.x), Math.round(p.y), 1, 2);
        ctx.fillStyle = "rgba(255,200,120,0.5)";
        ctx.fillRect(Math.round(p.x), Math.round(p.y) + 2, 1, 2);
      } else if (p.kind === "flare") {
        ctx.fillStyle = p.color;
        ctx.globalAlpha = Math.max(0, 1 - k * k);
        const s = k < 0.3 ? 2 : 1;
        ctx.fillRect(Math.round(p.x), Math.round(p.y), s, s);
        ctx.globalAlpha = 1;
      } else if (p.kind === "petal") {
        ctx.fillStyle = p.color;
        ctx.fillRect(Math.round(p.x), Math.round(p.y), 2, 1);
        if (Math.floor(this.t * 4 + p.max) % 2) ctx.fillRect(Math.round(p.x), Math.round(p.y) + 1, 1, 1);
      } else {
        ctx.globalAlpha = Math.max(0, 1 - k);
        ctx.fillStyle = p.color;
        const x = Math.round(p.x);
        const y = Math.round(p.y);
        ctx.fillRect(x - 2, y - 1, 2, 2);
        ctx.fillRect(x + 1, y - 1, 2, 2);
        ctx.fillRect(x - 2, y, 5, 1);
        ctx.fillRect(x - 1, y + 1, 3, 1);
        ctx.fillRect(x, y + 2, 1, 1);
        ctx.globalAlpha = 1;
      }
    }
  }

  private drawParticleArt(p: Particle, k: number, nightTone: boolean): boolean {
    const ctx = this.ctx;
    const fade = Math.max(0, 1 - k);
    const flicker = Math.max(0, 1 - k * k);
    switch (p.kind) {
      case "smoke": {
        const size = p.size < 3 ? "smoke_s" : p.size < 4.5 ? "smoke_m" : "smoke_l";
        const tone = nightTone ? "#9696aa" : p.color === "busy" ? "#e1dee6" : "#f0f0f5";
        return drawParticleArt(ctx, size, p.x, p.y, tone, (p.color === "busy" ? 0.7 : 0.55) * fade * (nightTone ? 0.75 : 1));
      }
      case "spark":
        return drawParticleArt(ctx, "spark", p.x + 0.5, p.y + 0.5, p.color, flicker);
      case "chip":
        return drawParticleArt(ctx, "chip", p.x + 0.5, p.y + 0.5, p.color, 1, Math.floor(this.t * 10 + p.max * 9) % 2 === 1);
      case "bubble":
        if (k > 0.85) return drawParticleArt(ctx, "flare_s", p.x + 0.5, p.y + 0.5, p.color);
        return drawParticleArt(ctx, p.size > 1 ? "bubble_l" : "bubble_s", p.x + 0.5, p.y + 0.5, p.color, 0.85);
      case "rune":
        return drawParticleArt(ctx, `rune_${(p.glyph ?? 0) % 4}`, p.x + 1.5, p.y + 1.5, p.color, Math.max(0, Math.min(1, k * 4, (1 - k) * 2)));
      case "hit": {
        const r = 1 + Math.round(k * 3);
        return drawParticleArt(ctx, r <= 1 ? "hit_s" : r <= 3 ? "hit_m" : "hit_l", p.x + 0.5, p.y + 0.5, p.color, fade);
      }
      case "rocket":
        return drawParticleArt(ctx, "rocket", p.x + 0.5, p.y + 2, "#fff2c4");
      case "flare":
        return drawParticleArt(ctx, k < 0.3 ? "flare_l" : "flare_s", p.x + 0.5, p.y + 0.5, p.color, flicker);
      case "petal":
        return drawParticleArt(ctx, "petal", p.x + 1, p.y + 0.5, p.color, 1, Math.floor(this.t * 4 + p.max) % 2 === 1);
      case "heart":
        return drawParticleArt(ctx, "heart", p.x + 0.5, p.y + 0.5, p.color, fade);
    }
  }

  private font(size: number): string {
    return `${Math.max(8, Math.round(size))}px ${this.opts.fontFamily}`;
  }

  private drawLabels(): void {
    const { ctx, P } = this;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    for (const l of this.world.labels) {
      const sx = l.x * P + this.offX;
      const sy = l.y * P + this.offY;
      if (sx < -300 || sx > this.w + 300 || sy < -50 || sy > this.h + 50) continue;
      let size = P * 5.4;
      ctx.font = this.font(size);
      const max = l.w * P * 0.92;
      const width = ctx.measureText(l.text).width;
      if (width > max) {
        size = size * (max / width);
        ctx.font = this.font(size);
      }
      ctx.fillStyle = "#4a2f1d";
      ctx.fillText(l.text, Math.round(sx), Math.round(sy + P * 0.3));
    }
  }

  private drawLighting(): void {
    const v = this.timeValue;
    const { ctx, P } = this;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    const grade = v <= 1 ? lerpColor(GRADE[0], GRADE[1], v) : lerpColor(GRADE[1], GRADE[2], v - 1);
    ctx.globalCompositeOperation = "overlay";
    ctx.fillStyle = `rgba(${grade[0] | 0},${grade[1] | 0},${grade[2] | 0},${grade[3]})`;
    ctx.fillRect(0, 0, this.w, this.h);
    ctx.globalCompositeOperation = "source-over";
    if (v < 0.03) return;
    const amb = v <= 1 ? lerpColor(AMBIENT[0], AMBIENT[1], v) : lerpColor(AMBIENT[1], AMBIENT[2], v - 1);
    const strength = Math.min(1, v * 1.1);
    const l = this.lctx;
    const s = this.light.width / this.w;
    l.setTransform(1, 0, 0, 1, 0, 0);
    l.globalCompositeOperation = "source-over";
    l.clearRect(0, 0, this.light.width, this.light.height);
    l.fillStyle = `rgba(${amb[0] | 0},${amb[1] | 0},${amb[2] | 0},${amb[3]})`;
    l.fillRect(0, 0, this.light.width, this.light.height);
    l.globalCompositeOperation = "destination-out";
    const visible = this.world.lights.filter((li) => {
      const sx = li.x * P + this.offX;
      const sy = li.y * P + this.offY;
      const rr = li.r * P;
      return sx > -rr && sx < this.w + rr && sy > -rr && sy < this.h + rr && (li.kind !== "window" || v > 0.8);
    });
    const hole = (x: number, y: number, r: number, a: number) => {
      const sx = (x * P + this.offX) * s;
      const sy = (y * P + this.offY) * s;
      const rr = r * P * s;
      const g = l.createRadialGradient(sx, sy, 0, sx, sy, rr);
      g.addColorStop(0, `rgba(0,0,0,${a})`);
      g.addColorStop(1, "rgba(0,0,0,0)");
      l.fillStyle = g;
      l.fillRect(sx - rr, sy - rr, rr * 2, rr * 2);
    };
    for (const li of visible) {
      const flicker = li.kind === "lamp" ? 0.94 + Math.sin(this.t * 9 + li.phase) * 0.03 + Math.sin(this.t * 23 + li.phase) * 0.03 : 1;
      hole(li.x, li.y, li.r * flicker, (li.kind === "fairy" ? 0.45 : 0.88) * strength);
    }
    hole(this.player.x, this.player.y - 8, 30, 0.3 * strength);
    for (const npc of this.world.npcs) {
      const color = WORK_LIGHT[npc.resident.trade];
      if (npc.mode !== "working" || npc.activity !== "work" || !color) continue;
      const st = stationAt(npc.homeX, npc.homeY);
      const sx = st.x * P + this.offX;
      const sy = st.y * P + this.offY;
      if (sx < -200 || sx > this.w + 200 || sy < -200 || sy > this.h + 200) continue;
      const light: Light = { x: st.x, y: st.y - 10, r: 26, color, kind: "lamp", phase: npc.phase * 10 };
      visible.push(light);
      hole(light.x, light.y, light.r * (0.9 + Math.sin(this.t * 7 + light.phase) * 0.08), 0.8 * strength);
    }
    ctx.imageSmoothingEnabled = true;
    ctx.drawImage(this.light, 0, 0, this.w, this.h);
    ctx.imageSmoothingEnabled = false;
    ctx.globalCompositeOperation = "lighter";
    for (const li of visible) {
      const sx = li.x * P + this.offX;
      const sy = li.y * P + this.offY;
      const rr = li.r * P * (li.kind === "fairy" ? 0.7 : 0.6);
      const twinkle = li.kind === "fairy" ? 0.6 + Math.sin(this.t * 3 + li.phase) * 0.4 : 1;
      const a = (li.kind === "fairy" ? 0.32 : li.kind === "window" ? 0.2 : 0.26) * strength * twinkle;
      const [r, g, b] = li.color;
      const grad = ctx.createRadialGradient(sx, sy, 0, sx, sy, rr);
      grad.addColorStop(0, `rgba(${r},${g},${b},${a})`);
      grad.addColorStop(1, `rgba(${r},${g},${b},0)`);
      ctx.fillStyle = grad;
      ctx.fillRect(sx - rr, sy - rr, rr * 2, rr * 2);
    }
    const fa = Math.max(0, Math.min(1, (v - 0.55) / 0.8));
    if (fa > 0) {
      for (const f of this.fireflies) {
        const sx = f.x * P + this.offX;
        const sy = f.y * P + this.offY;
        if (sx < -20 || sx > this.w + 20 || sy < -20 || sy > this.h + 20) continue;
        const tw = Math.max(0, Math.sin(this.t * 2.2 * f.speed + f.phase));
        const a = fa * tw;
        if (a < 0.02) continue;
        const rr = P * 5;
        const grad = ctx.createRadialGradient(sx, sy, 0, sx, sy, rr);
        grad.addColorStop(0, `rgba(230,255,150,${0.55 * a})`);
        grad.addColorStop(1, "rgba(230,255,150,0)");
        ctx.fillStyle = grad;
        ctx.fillRect(sx - rr, sy - rr, rr * 2, rr * 2);
        ctx.fillStyle = `rgba(250,255,210,${a})`;
        ctx.fillRect(Math.round(sx - P / 2), Math.round(sy - P / 2), P, P);
      }
    }
    ctx.globalCompositeOperation = "source-over";
  }

  private drawLampAlarm(): void {
    const lamp = this.world.marketLamp;
    if (!lamp || (!this.budgetPaused && this.t >= this.lampAlarmUntil)) return;
    const { ctx, P } = this;
    const sx = lamp.x * P + this.offX;
    const sy = lamp.y * P + this.offY;
    if (sx < -200 || sx > this.w + 200 || sy < -200 || sy > this.h + 200) return;
    const fade = this.budgetPaused ? 1 : Math.min(1, (this.lampAlarmUntil - this.t) / 20);
    const a = (this.opts.reducedMotion ? 0.5 : 0.42 + Math.sin(this.t * 1.4) * 0.08) * fade;
    const rr = 34 * P;
    const grad = ctx.createRadialGradient(sx, sy, 0, sx, sy, rr);
    grad.addColorStop(0, `rgba(255,70,60,${a})`);
    grad.addColorStop(1, "rgba(255,70,60,0)");
    ctx.globalCompositeOperation = "lighter";
    ctx.fillStyle = grad;
    ctx.fillRect(sx - rr, sy - rr, rr * 2, rr * 2);
    ctx.globalCompositeOperation = "source-over";
    ctx.fillStyle = `rgba(255,90,80,${0.7 * fade})`;
    ctx.fillRect(Math.round(sx - 2 * P), Math.round(sy - 2 * P), 4 * P, 4 * P);
  }

  private drawBubbles(): void {
    const ctx = this.ctx;
    for (const npc of this.world.npcs) {
      const lift = this.liftOf(npc.sheet);
      const t = this.opts.reducedMotion ? 0 : this.t;
      const pips = (x: number, y: number) => drawPipsArt(ctx, npc.jobs, x, y + 6, t) || drawPips(ctx, npc.jobs, x, y, t);
      if (this.reading(npc)) pips(npc.x, npc.y - 27 - lift);
      else if (npc.mode === "working" && npc.activity === "work" && npc.jobs.length) {
        const st = stationAt(npc.homeX, npc.homeY);
        const warrior = npc.resident.trade === "warrior";
        const above = warrior ? (newArtOn() ? SLIME_PIPS_ABOVE : 16) : 26;
        pips(warrior ? st.x - 2 : st.x, st.y - above);
      }
      if (npc.moving) continue;
      if (this.focusKey === `r:${npc.resident.id}`) continue;
      const kind = npc.status === "thinking" ? "dots" : npc.status === "waiting" ? "alert" : npc.status === "error" ? "error" : null;
      if (!kind) continue;
      const bob = Math.round(Math.sin(this.t * 3 + npc.x) * 1.2);
      const w = kind === "dots" ? 13 : 9;
      const x = Math.round(npc.x - w / 2);
      const y = Math.round(npc.y - 34 - lift + bob);
      if (drawStatusArt(ctx, kind, npc.x, y + 12, this.t)) continue;
      ctx.fillStyle = "#261822";
      ctx.fillRect(x, y, w, 10);
      ctx.fillStyle = "#fbf3e0";
      ctx.fillRect(x + 1, y + 1, w - 2, 8);
      ctx.fillStyle = "#261822";
      ctx.fillRect(Math.round(npc.x) - 1, y + 10, 3, 1);
      ctx.fillRect(Math.round(npc.x), y + 11, 1, 1);
      if (kind === "dots") {
        const lit = Math.floor(this.t * 3) % 4;
        for (let i = 0; i < 3; i++) {
          ctx.fillStyle = i < lit ? "#2456d6" : "#c9c2b4";
          ctx.fillRect(x + 3 + i * 3, y + 4, 2, 2);
        }
      } else {
        ctx.fillStyle = kind === "alert" ? "#e0663d" : "#b8323a";
        if (kind === "alert") {
          ctx.fillRect(x + 4, y + 2, 1, 4);
          ctx.fillRect(x + 4, y + 7, 1, 1);
        } else {
          for (let i = 0; i < 5; i++) {
            ctx.fillRect(x + 2 + i, y + 2 + i, 1, 1);
            ctx.fillRect(x + 6 - i, y + 2 + i, 1, 1);
          }
        }
      }
    }
  }

  private drawPrompt(): void {
    const f = this.focus;
    if (!f || this.paused) return;
    const p = this.pointOf(f);
    if (!p) return;
    const { ctx, P } = this;
    const label = this.labelOf(f);
    const thing = f.kind === "thing" ? this.room?.things.find((x) => x.id === f.id) : null;
    const wearer = f.kind === "resident" ? this.npcById.get(f.id)?.sheet : f.kind === "folk" ? this.world.critters.find((c) => c.id === f.id)?.sheet : undefined;
    const lift = thing ? thing.lift + 6 : f.kind === "place" ? 40 : f.kind === "board" ? 38 : f.kind === "peacock" ? (thingOf(PEACOCK, 0) ? 32 : 26) : f.kind === "plot" ? 34 : 30 + (wearer ? this.liftOf(wearer) : 0);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    const size = Math.max(13, P * 4);
    ctx.font = this.font(size);
    ctx.textBaseline = "middle";
    ctx.textAlign = "left";
    const key = this.opts.touch ? "Tap" : "E";
    const keyW = ctx.measureText(key).width + size * 0.9;
    const textW = ctx.measureText(label).width;
    const pad = size * 0.55;
    const h = size * 1.9;
    const w = keyW + textW + pad * 3;
    const bob = this.opts.reducedMotion ? 0 : Math.sin(this.t * 4) * P * 0.6;
    const cx = p.x * P + this.offX;
    const top = Math.round((p.y - lift) * P + this.offY - h + bob);
    const left = Math.round(cx - w / 2);
    ctx.fillStyle = "rgba(38,24,34,0.92)";
    ctx.beginPath();
    ctx.roundRect(left, top, w, h, h * 0.28);
    ctx.fill();
    ctx.fillStyle = "#fbf3e0";
    ctx.beginPath();
    ctx.roundRect(left + pad * 0.7, top + h * 0.18, keyW, h * 0.64, h * 0.16);
    ctx.fill();
    ctx.fillStyle = "#261822";
    ctx.textAlign = "center";
    ctx.fillText(key, left + pad * 0.7 + keyW / 2, top + h / 2 + 1);
    ctx.textAlign = "left";
    ctx.fillStyle = "#fbf3e0";
    ctx.fillText(label, left + pad * 1.7 + keyW, top + h / 2 + 1);
    ctx.fillStyle = "rgba(38,24,34,0.92)";
    ctx.beginPath();
    ctx.moveTo(cx - P * 1.5, top + h);
    ctx.lineTo(cx + P * 1.5, top + h);
    ctx.lineTo(cx, top + h + P * 1.5);
    ctx.fill();
  }
}
