import { placed } from "@/lib/sprite-manifest";
import { context, makeCanvas, rgbOf } from "./pixels";
import { propOf, type ThingImage } from "./sprites";
import type { Trade } from "./types";

type Ctx = CanvasRenderingContext2D;

export const STRIKE_PERIOD: Record<Trade, number> = {
  carpenter: 0.55,
  smith: 0.75,
  alchemist: 0.5,
  wizard: 0.6,
  warrior: 0.85,
};

export const JOB_COLOR: Record<string, string> = {
  task: "#f2c14e",
  review: "#6bc7e8",
  followup: "#c39bff",
  publish: "#7be06b",
};

export const WORK_LIGHT: Partial<Record<Trade, [number, number, number]>> = {
  smith: [255, 150, 70],
  alchemist: [140, 240, 120],
  wizard: [150, 200, 255],
};

const SLIME_COLOR: Record<string, [string, string]> = {
  task: ["#6bd66b", "#3f9e46"],
  review: ["#6bb6e0", "#3a7aa6"],
  followup: ["#c39bff", "#7f5bc4"],
  publish: ["#f2c14e", "#b98a1e"],
};

const RUNES = [
  [0b010, 0b111, 0b010],
  [0b101, 0b010, 0b101],
  [0b111, 0b101, 0b111],
  [0b110, 0b011, 0b110],
];

export function tempo(jobs: number): number {
  return 1 + Math.min(3, Math.max(0, jobs - 1)) * 0.22;
}

export function stationAt(homeX: number, homeY: number): { x: number; y: number } {
  return { x: homeX - 14, y: homeY };
}

export function slimeAt(sx: number, sy: number, i: number, t: number): { x: number; y: number } {
  const hop = Math.abs(Math.sin(t * 3.4 + i * 1.7)) * 2;
  return { x: sx + 2 - i * 9, y: sy + (i % 2 ? -4 : 1) - hop };
}

function px(ctx: Ctx, color: string, x: number, y: number, w = 1, h = 1): void {
  ctx.fillStyle = color;
  ctx.fillRect(Math.round(x), Math.round(y), w, h);
}

export function drawStation(ctx: Ctx, trade: Trade, sx: number, sy: number, t: number, glow: number): void {
  if (trade === "carpenter") {
    px(ctx, "#5a3a1e", sx - 6, sy - 6, 1, 6);
    px(ctx, "#5a3a1e", sx + 5, sy - 6, 1, 6);
    px(ctx, "#6b4423", sx - 4, sy - 5, 1, 5);
    px(ctx, "#6b4423", sx + 3, sy - 5, 1, 5);
    px(ctx, "#8a5a2b", sx - 7, sy - 7, 14, 2);
    px(ctx, "#c8925a", sx - 7, sy - 8, 14, 1);
    px(ctx, "#e0b07a", sx - 4, sy - 10, 9, 2);
    px(ctx, "#b07a45", sx - 4, sy - 9, 9, 1);
    px(ctx, "#7a7f88", sx + 1, sy - 11, 1, 1);
    return;
  }
  if (trade === "smith") {
    px(ctx, "#2b2e36", sx - 3, sy - 3, 7, 3);
    px(ctx, "#3b3f4a", sx - 2, sy - 6, 5, 3);
    px(ctx, "#3b3f4a", sx - 6, sy - 9, 12, 3);
    px(ctx, "#3b3f4a", sx - 8, sy - 8, 2, 1);
    px(ctx, "#6a7080", sx - 6, sy - 9, 12, 1);
    const heat = 0.5 + glow * 0.5;
    ctx.globalAlpha = 0.25 * heat;
    px(ctx, "#ff9a3c", sx - 4, sy - 13, 9, 4);
    ctx.globalAlpha = 1;
    px(ctx, heat > 0.75 ? "#ffe07a" : "#ff9a3c", sx - 2, sy - 11, 5, 2);
    px(ctx, "#ffd45a", sx - 1, sy - 11, 2, 1);
    return;
  }
  if (trade === "alchemist") {
    const flick = Math.floor(t * 12) % 3;
    px(ctx, "#ff9a3c", sx - 3 + flick, sy - 2, 2, 2);
    px(ctx, "#ffd45a", sx + 1 - flick, sy - 2, 1, 2);
    px(ctx, "#ff6a3c", sx - 1, sy - 1, 3, 1);
    px(ctx, "#2a2530", sx - 5, sy - 2, 1, 2);
    px(ctx, "#2a2530", sx + 4, sy - 2, 1, 2);
    px(ctx, "#2a2530", sx - 6, sy - 8, 12, 6);
    px(ctx, "#3d3646", sx - 7, sy - 9, 14, 2);
    px(ctx, "#4d4558", sx - 5, sy - 7, 2, 3);
    const brew = 0.6 + Math.sin(t * 3) * 0.4;
    px(ctx, brew > 0.6 ? "#9bf08b" : "#7be06b", sx - 5, sy - 9, 10, 1);
    px(ctx, "#c8ffb8", sx - 2 + Math.round(Math.sin(t * 2) * 2), sy - 9, 2, 1);
    return;
  }
  if (trade === "wizard") {
    px(ctx, "#5a3a1e", sx - 1, sy - 6, 2, 6);
    px(ctx, "#5a3a1e", sx - 3, sy - 1, 6, 1);
    px(ctx, "#8a5a2b", sx - 5, sy - 9, 10, 3);
    px(ctx, "#6b4423", sx - 5, sy - 7, 10, 1);
    px(ctx, "#fbf3e0", sx - 5, sy - 11, 4, 2);
    px(ctx, "#fbf3e0", sx + 1, sy - 11, 4, 2);
    px(ctx, "#7a4a8a", sx - 1, sy - 11, 2, 2);
    px(ctx, "#c9c2b4", sx - 4, sy - 11, 2, 1);
    px(ctx, "#c9c2b4", sx + 2, sy - 11, 2, 1);
    ctx.globalAlpha = 0.18 + glow * 0.22;
    px(ctx, "#9fdcff", sx - 6, sy - 15, 12, 4);
    ctx.globalAlpha = 1;
    return;
  }
}

export function drawSlime(ctx: Ctx, kind: string, x: number, y: number, t: number, i: number, flash: number): void {
  const [body, shade] = SLIME_COLOR[kind] ?? SLIME_COLOR.task;
  const squash = Math.round(Math.sin(t * 6.8 + i * 1.7));
  const w = 9 + squash;
  const h = 6 - squash;
  const left = Math.round(x - w / 2);
  const top = Math.round(y - h);
  ctx.fillStyle = "rgba(20,24,20,0.25)";
  ctx.fillRect(Math.round(x - 4), Math.round(y) + 1, 8, 2);
  const c = flash > 0 ? "#ffffff" : body;
  px(ctx, c, left + 1, top, w - 2, 1);
  px(ctx, c, left, top + 1, w, h - 1);
  px(ctx, flash > 0 ? "#f0f0f0" : shade, left, top + h - 1, w, 1);
  px(ctx, "#ffffff", left + 2, top + 1, 1, 1);
  px(ctx, "#261822", left + 2, top + 2, 1, 2);
  px(ctx, "#261822", left + w - 4, top + 2, 1, 2);
  if (i === 0 && kind === "review") px(ctx, "#f2c14e", left + 2, top - 1, w - 4, 1);
}

function outlined(ctx: Ctx, color: string, x: number, y: number, w: number, h: number): void {
  px(ctx, "#261822", x - 1, y - 1, w + 2, h + 2);
  px(ctx, color, x, y, w, h);
}

export function drawTool(ctx: Ctx, trade: Trade, x: number, y: number, struck: boolean, t: number): void {
  const hx = Math.round(x) - 5;
  const hy = Math.round(y) - 10;
  if (trade === "carpenter" || trade === "smith") {
    const head = trade === "smith" ? "#5d6370" : "#a9aeb8";
    if (struck) {
      outlined(ctx, "#b07a45", hx - 7, hy, 7, 1);
      outlined(ctx, head, hx - 10, hy - 2, 3, 4);
      px(ctx, "#ffffff", hx - 10, hy - 2, 1, 1);
    } else {
      outlined(ctx, "#b07a45", hx - 3, hy - 7, 1, 7);
      outlined(ctx, head, hx - 6, hy - 10, 6, 3);
      px(ctx, "#ffffff", hx - 6, hy - 10, 1, 1);
    }
    return;
  }
  if (trade === "alchemist") {
    const sway = Math.round(Math.sin(t * 5) * 2);
    outlined(ctx, "#b07a45", hx - 4 + sway, hy - 1, 1, 5);
    outlined(ctx, "#b07a45", hx - 4 + sway, hy - 2, 4, 1);
    return;
  }
  if (trade === "wizard") {
    outlined(ctx, "#7a4a2a", hx - 5, hy - 3, 5, 1);
    const tw = Math.floor(t * 8) % 2;
    outlined(ctx, tw ? "#ffffff" : "#9fdcff", hx - 8, hy - 5, 2, 2);
    if (struck) {
      px(ctx, "#c39bff", hx - 10, hy - 7, 1, 1);
      px(ctx, "#ffffff", hx - 11, hy - 4, 1, 1);
    }
    return;
  }
  if (struck) {
    outlined(ctx, "#d8dde6", hx - 11, hy, 9, 1);
    px(ctx, "#ffffff", hx - 11, hy, 3, 1);
    outlined(ctx, "#c08a1e", hx - 2, hy - 1, 1, 3);
  } else {
    outlined(ctx, "#d8dde6", hx - 3, hy - 11, 1, 9);
    px(ctx, "#ffffff", hx - 3, hy - 11, 1, 3);
    outlined(ctx, "#c08a1e", hx - 4, hy - 2, 3, 1);
  }
}

export function drawRune(ctx: Ctx, glyph: number, x: number, y: number, color: string): void {
  const rows = RUNES[glyph % RUNES.length];
  ctx.fillStyle = color;
  for (let r = 0; r < 3; r++) for (let c = 0; c < 3; c++) if (rows[r] & (4 >> c)) ctx.fillRect(Math.round(x) + c, Math.round(y) + r, 1, 1);
}

export function drawPips(ctx: Ctx, jobs: string[], x: number, y: number, t: number): void {
  const n = Math.min(jobs.length, 5);
  if (!n) return;
  const w = n * 3 + 3;
  const left = Math.round(x - w / 2);
  const top = Math.round(y + Math.sin(t * 2.4) * 0.8);
  px(ctx, "#261822", left, top, w, 5);
  px(ctx, "#fbf3e0", left + 1, top + 1, w - 2, 3);
  px(ctx, "#261822", Math.round(x), top + 5, 1, 1);
  const lit = Math.floor(t * 1.6) % n;
  for (let i = 0; i < n; i++) {
    ctx.globalAlpha = i === lit ? 1 : 0.6;
    px(ctx, JOB_COLOR[jobs[i]] ?? JOB_COLOR.task, left + 2 + i * 3, top + 2, 2, 1);
    ctx.globalAlpha = 1;
    if (i === lit) px(ctx, "#261822", left + 2 + i * 3, top + 3, 2, 1);
  }
  if (jobs.length > n) px(ctx, "#fbf3e0", left + w, top + 2, 1, 1);
}

const ART_FPS = 7.5;
const SLIME_FPS = 12;
const STATION_GLOW_REACH = 6;
const STATION_GLOW_BASE = 0.12;
const STATION_GLOW_GAIN = 0.38;
const ART_SHADOW = "rgba(38,24,34,0.22)";
const DOTS = 3;
const ART_INK = "#411334";
const INK_RGB = [0x41, 0x13, 0x34];
const whitened = new WeakMap<HTMLCanvasElement, HTMLCanvasElement>();
const tints = new WeakMap<HTMLCanvasElement, Map<string, HTMLCanvasElement>>();
const TINT_BASE = 0.9;
const GLOW_EDGE = 0.72;
const GLOWING = ["spark", "flare", "hit"];
const stretched = new WeakMap<HTMLCanvasElement, Map<number, HTMLCanvasElement>>();

function blit(ctx: Ctx, art: ThingImage, x: number, y: number, img: HTMLCanvasElement = art.img): void {
  const d = art.density;
  const [left, top] = placed([art.ax, art.ay], d, x, y);
  ctx.drawImage(img, left, top, img.width / d, img.height / d);
}

function white(img: HTMLCanvasElement): HTMLCanvasElement {
  const hit = whitened.get(img);
  if (hit) return hit;
  const c = makeCanvas(img.width, img.height);
  const g = context(c, { willReadFrequently: true });
  g.drawImage(img, 0, 0);
  const data = g.getImageData(0, 0, c.width, c.height);
  const d = data.data;
  for (let i = 0; i < d.length; i += 4) {
    if (!d[i + 3] || (d[i] === INK_RGB[0] && d[i + 1] === INK_RGB[1] && d[i + 2] === INK_RGB[2])) continue;
    d[i] = d[i + 1] = d[i + 2] = 255;
  }
  g.putImageData(data, 0, 0);
  whitened.set(img, c);
  return c;
}

function widened(art: ThingImage, extra: number): HTMLCanvasElement {
  const tail = art.points.tail;
  if (!tail || extra <= 0) return art.img;
  const sized = stretched.get(art.img) ?? new Map<number, HTMLCanvasElement>();
  stretched.set(art.img, sized);
  const hit = sized.get(extra);
  if (hit) return hit;
  const [x0, x1] = [tail[0][0], tail[1][0]];
  const left = Math.ceil(extra / 2);
  const right = extra - left;
  const { width: w, height: h } = art.img;
  const c = makeCanvas(w + extra, h);
  const g = context(c);
  g.drawImage(art.img, 0, 0, x0, h, 0, 0, x0, h);
  for (let i = 0; i < left; i++) g.drawImage(art.img, x0 - 1, 0, 1, h, x0 + i, 0, 1, h);
  g.drawImage(art.img, x0, 0, x1 - x0, h, x0 + left, 0, x1 - x0, h);
  for (let i = 0; i < right; i++) g.drawImage(art.img, x1, 0, 1, h, x1 + left + i, 0, 1, h);
  g.drawImage(art.img, x1, 0, w - x1, h, x1 + extra, 0, w - x1, h);
  sized.set(extra, c);
  return c;
}

export function slimeArtAt(sx: number, sy: number, i: number): { x: number; y: number } {
  return { x: sx - 2 - i * 13, y: sy + (i % 2 ? -5 : 1) };
}

export function drawStationArt(ctx: Ctx, trade: Trade, sx: number, sy: number, t: number, glow: number): boolean {
  const art = propOf(`station_${trade}`, t * ART_FPS);
  if (!art) return false;
  blit(ctx, art, sx, sy);
  const spot = art.points.glow?.[0];
  const light = WORK_LIGHT[trade];
  if (!spot || !light) return true;
  const [left, top] = placed([art.ax, art.ay], art.density, sx, sy);
  const gx = left + spot[0] / art.density;
  const gy = top + spot[1] / art.density;
  const grad = ctx.createRadialGradient(gx, gy, 0, gx, gy, STATION_GLOW_REACH);
  grad.addColorStop(0, `rgba(${light[0]},${light[1]},${light[2]},${STATION_GLOW_BASE + glow * STATION_GLOW_GAIN})`);
  grad.addColorStop(1, `rgba(${light[0]},${light[1]},${light[2]},0)`);
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  ctx.fillStyle = grad;
  ctx.fillRect(gx - STATION_GLOW_REACH, gy - STATION_GLOW_REACH, STATION_GLOW_REACH * 2, STATION_GLOW_REACH * 2);
  ctx.restore();
  return true;
}

export function drawSlimeArt(ctx: Ctx, kind: string, x: number, y: number, t: number, i: number, flash: number): boolean {
  const art = propOf(`slime_${Object.hasOwn(SLIME_COLOR, kind) ? kind : "task"}`, t * SLIME_FPS + i * 3);
  if (!art) return false;
  ctx.fillStyle = ART_SHADOW;
  ctx.beginPath();
  ctx.ellipse(x, y, art.img.width / art.density * 0.32, 1.6, 0, 0, Math.PI * 2);
  ctx.fill();
  blit(ctx, art, x, y, flash > 0 ? white(art.img) : art.img);
  return true;
}

export function drawToolArt(ctx: Ctx, trade: Trade, x: number, y: number, struck: boolean): boolean {
  const art = propOf(`tool_${trade}`);
  if (!art) return false;
  const d = art.density;
  const [gx, gy] = art.points.grip?.[0] ?? [art.ax, art.ay];
  ctx.save();
  ctx.translate(Math.round(x * d) / d, Math.round(y * d) / d);
  if (struck) ctx.rotate(-Math.PI / 2);
  ctx.drawImage(art.img, -gx / d, -gy / d, art.img.width / d, art.img.height / d);
  ctx.restore();
  return true;
}

export function drawHeldArt(ctx: Ctx, gear: string, x: number, y: number): boolean {
  const art = propOf(`gear_${gear}`);
  if (!art) return false;
  const d = art.density;
  const [gx, gy] = art.points.grip?.[0] ?? [art.ax, art.ay];
  ctx.drawImage(art.img, Math.round(x * d - gx) / d, Math.round(y * d - gy) / d, art.img.width / d, art.img.height / d);
  return true;
}

function tinted(img: HTMLCanvasElement, colour: string, glow: boolean): HTMLCanvasElement {
  const sized = tints.get(img) ?? new Map<string, HTMLCanvasElement>();
  tints.set(img, sized);
  const key = glow ? `${colour}:glow` : colour;
  const hit = sized.get(key);
  if (hit) return hit;
  const to = rgbOf(colour);
  const c = makeCanvas(img.width, img.height);
  const g = context(c, { willReadFrequently: true });
  g.drawImage(img, 0, 0);
  const data = g.getImageData(0, 0, c.width, c.height);
  const d = data.data;
  for (let i = 0; i < d.length; i += 4) {
    if (!d[i + 3]) continue;
    if (d[i] === INK_RGB[0] && d[i + 1] === INK_RGB[1] && d[i + 2] === INK_RGB[2]) {
      if (glow) for (let k = 0; k < 3; k++) d[i + k] = to[k] * GLOW_EDGE;
      continue;
    }
    const v = (d[i] * 0.3 + d[i + 1] * 0.59 + d[i + 2] * 0.11) / 255 / TINT_BASE;
    for (let k = 0; k < 3; k++) d[i + k] = v <= 1 ? to[k] * v : to[k] + (255 - to[k]) * Math.min(1, (v - 1) / (1 / TINT_BASE - 1));
  }
  g.putImageData(data, 0, 0);
  sized.set(key, c);
  return c;
}

export function drawParticleArt(ctx: Ctx, key: string, x: number, y: number, colour: string, alpha = 1, turn = false): boolean {
  const glow = GLOWING.some((g) => key.startsWith(g));
  const art = propOf(`particle_${key}`);
  if (!art) return false;
  const d = art.density;
  const img = /^#[0-9a-f]{6}$/i.test(colour) ? tinted(art.img, colour, glow) : art.img;
  const w = img.width / d;
  const h = img.height / d;
  ctx.globalAlpha = alpha;
  ctx.save();
  ctx.translate(Math.round(x * d) / d, Math.round(y * d) / d);
  if (turn) ctx.rotate(Math.PI / 2);
  ctx.drawImage(img, -Math.round(w * d / 2) / d, -Math.round(h * d / 2) / d, w, h);
  ctx.restore();
  ctx.globalAlpha = 1;
  return true;
}

export function drawBookArt(ctx: Ctx, x: number, y: number): boolean {
  const art = propOf("prop_book");
  if (!art) return false;
  blit(ctx, art, x, y);
  return true;
}

export function drawIconArt(ctx: Ctx, icon: string, x: number, y: number, alpha = 1): boolean {
  const art = propOf(`icon_${icon}`);
  if (!art) return false;
  ctx.globalAlpha = alpha;
  blit(ctx, art, x, y);
  ctx.globalAlpha = 1;
  return true;
}

export function drawPipsArt(ctx: Ctx, jobs: string[], x: number, tipY: number, t: number): boolean {
  const n = Math.min(jobs.length, 5);
  const bubble = propOf("bubble_speech");
  const gem = propOf("pip_task");
  if (!bubble || !gem) return false;
  if (!n) return true;
  const d = bubble.density;
  const step = gem.img.width + 2;
  const more = jobs.length > n ? 4 : 0;
  const extra = Math.max(0, n * step + more + 10 - bubble.img.width);
  const img = widened(bubble, extra);
  const [left, top] = placed([bubble.ax + Math.ceil(extra / 2), bubble.ay], d, x, tipY);
  ctx.drawImage(img, left, top, img.width / d, img.height / d);
  const cy = top + (bubble.points.centre?.[0][1] ?? bubble.img.height / 2) / d;
  const x0 = left + (img.width - n * step + 2 - more) / 2 / d;
  const lit = Math.floor(t * 1.6) % n;
  for (let i = 0; i < n; i++) {
    const art = propOf(`pip_${Object.hasOwn(JOB_COLOR, jobs[i]) ? jobs[i] : "task"}`) ?? gem;
    ctx.globalAlpha = i === lit ? 1 : 0.6;
    blit(ctx, art, x0 + (i * step + art.ax) / d, cy - (i === lit ? 1 / d : 0));
    ctx.globalAlpha = 1;
  }
  if (jobs.length > n) {
    ctx.fillStyle = ART_INK;
    ctx.fillRect(x0 + (n * step) / d, cy, 2 / d, 2 / d);
  }
  return true;
}

export function drawStatusArt(ctx: Ctx, kind: "dots" | "alert" | "error", x: number, tipY: number, t: number): boolean {
  const bubble = propOf("bubble_thought");
  if (!bubble || !propOf(kind === "dots" ? "icon_dot_on" : `icon_${kind}`)) return false;
  const d = bubble.density;
  const [left, top] = placed([bubble.ax, bubble.ay], d, x, tipY);
  ctx.drawImage(bubble.img, left, top, bubble.img.width / d, bubble.img.height / d);
  const [cx, cy] = bubble.points.centre?.[0] ?? [bubble.ax, bubble.img.height / 2];
  const mx = left + cx / d;
  const my = top + cy / d;
  if (kind !== "dots") return drawIconArt(ctx, kind, mx, my);
  const lit = Math.floor(t * 3) % (DOTS + 1);
  for (let i = 0; i < DOTS; i++) drawIconArt(ctx, i < lit ? "dot_on" : "dot_off", mx + (i - 1) * 2.75, my);
  return true;
}
