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
