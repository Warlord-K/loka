import type { LokaShopStyle } from "./types";
import { context, disc, ellipse, makeCanvas, outline, rect, shade, type Ctx } from "./pixels";

export interface Sprite {
  img: HTMLCanvasElement;
  ax: number;
  ay: number;
}

export type TreeKind = "green" | "blossom" | "autumn" | "pine";

const CANOPY: Record<Exclude<TreeKind, "pine">, string[]> = {
  green: ["#2d6a3e", "#3d8a48", "#57a852", "#86c965"],
  blossom: ["#a8506f", "#d47a9c", "#eca3bd", "#fbd3e0"],
  autumn: ["#9c4a22", "#c8672b", "#e39440", "#f4c064"],
};

const SHADOW = "rgba(20,30,20,0.22)";

function speckle(ctx: Ctx, r: () => number, x: number, y: number, w: number, h: number, color: string, n: number): void {
  const img = ctx.getImageData(x, y, w, h).data;
  ctx.fillStyle = color;
  for (let i = 0; i < n; i++) {
    const px = Math.floor(r() * w);
    const py = Math.floor(r() * h);
    if (img[(py * w + px) * 4 + 3] > 0) ctx.fillRect(x + px, y + py, 1, 1);
  }
}

export function makeTree(kind: TreeKind, r: () => number): Sprite {
  if (kind === "pine") return makePine(r);
  const [dark, mid, light, hi] = CANOPY[kind];
  const c = makeCanvas(34, 46);
  const ctx = context(c, { willReadFrequently: true });
  ellipse(ctx, 17, 43, 12, 2, SHADOW);
  rect(ctx, 14, 28, 6, 15, "#6b4527");
  rect(ctx, 17, 28, 3, 15, "#553520");
  rect(ctx, 12, 41, 10, 2, "#553520");
  const j = () => Math.round((r() - 0.5) * 2);
  disc(ctx, 17 + j(), 19, 12, dark);
  disc(ctx, 9, 25 + j(), 7, dark);
  disc(ctx, 25, 25 + j(), 7, dark);
  disc(ctx, 17, 28, 8, dark);
  disc(ctx, 16, 17, 11, mid);
  disc(ctx, 9, 23, 6, mid);
  disc(ctx, 24, 23, 6, mid);
  disc(ctx, 13, 14, 7, light);
  disc(ctx, 21, 16, 5, light);
  disc(ctx, 8, 20, 3, light);
  disc(ctx, 11, 10, 2, hi);
  disc(ctx, 18, 11, 2, hi);
  disc(ctx, 23, 15, 1, hi);
  speckle(ctx, r, 2, 4, 30, 30, dark, 26);
  speckle(ctx, r, 2, 4, 30, 24, hi, 10);
  if (kind === "blossom") speckle(ctx, r, 2, 4, 30, 30, "#ffffff", 8);
  if (kind === "autumn") speckle(ctx, r, 2, 4, 30, 30, "#f9dd8a", 8);
  return { img: c, ax: 17, ay: 43 };
}

function makePine(r: () => number): Sprite {
  const c = makeCanvas(30, 50);
  const ctx = context(c, { willReadFrequently: true });
  ellipse(ctx, 15, 47, 10, 2, SHADOW);
  rect(ctx, 13, 38, 4, 9, "#5a3a22");
  const tiers = [
    { y: 26, h: 14, w: 13 },
    { y: 16, h: 14, w: 11 },
    { y: 6, h: 13, w: 8 },
  ];
  for (const t of tiers) {
    for (let i = 0; i < t.h; i++) {
      const hw = Math.round((i / t.h) * t.w) + 1;
      rect(ctx, 15 - hw, t.y + i, hw * 2, 1, "#1f5238");
      rect(ctx, 15 - hw, t.y + i, hw, 1, "#2c6e47");
      if (i % 4 === 3) rect(ctx, 15 - hw, t.y + i, hw * 2, 1, "#1a4630");
    }
  }
  rect(ctx, 14, 3, 2, 4, "#2c6e47");
  speckle(ctx, r, 2, 4, 26, 36, "#4a9a62", 18);
  return { img: c, ax: 15, ay: 47 };
}

export function makeBush(r: () => number, flowers: string | null): Sprite {
  const c = makeCanvas(20, 15);
  const ctx = context(c, { willReadFrequently: true });
  ellipse(ctx, 10, 13, 8, 1, SHADOW);
  disc(ctx, 6, 9, 5, "#2f6f3c");
  disc(ctx, 14, 9, 5, "#2f6f3c");
  disc(ctx, 10, 7, 6, "#3f8a48");
  disc(ctx, 8, 5, 3, "#57a852");
  disc(ctx, 13, 6, 2, "#6fbb5c");
  if (flowers) speckle(ctx, r, 2, 1, 16, 11, flowers, 7);
  return { img: c, ax: 10, ay: 13 };
}

export function makeRock(r: () => number): Sprite {
  const c = makeCanvas(14, 10);
  const ctx = context(c);
  ellipse(ctx, 7, 8, 6, 1, SHADOW);
  ellipse(ctx, 7, 5, 5, 3, "#8b8f98");
  ellipse(ctx, 6, 4, 4, 2, "#a9adb5");
  rect(ctx, 4, 3, 2, 1, "#c8ccd2");
  if (r() > 0.5) rect(ctx, 8, 6, 3, 1, "#6d9a55");
  return { img: c, ax: 7, ay: 8 };
}

export function makeReeds(r: () => number): Sprite {
  const c = makeCanvas(12, 16);
  const ctx = context(c);
  for (let i = 0; i < 5; i++) {
    const x = 1 + Math.floor(r() * 10);
    const h = 7 + Math.floor(r() * 7);
    rect(ctx, x, 15 - h, 1, h, i % 2 ? "#4d8a3f" : "#3b7034");
    if (i % 2 === 0) rect(ctx, x, 15 - h, 1, 3, "#7a4a2a");
  }
  return { img: c, ax: 6, ay: 15 };
}

export function makeLamp(lit: boolean): Sprite {
  const c = makeCanvas(12, 30);
  const ctx = context(c);
  ellipse(ctx, 6, 28, 4, 1, SHADOW);
  rect(ctx, 5, 9, 2, 19, "#3b3440");
  rect(ctx, 3, 26, 6, 3, "#2f2a33");
  rect(ctx, 1, 1, 10, 2, "#2f2a33");
  rect(ctx, 5, 0, 2, 1, "#2f2a33");
  rect(ctx, 2, 3, 8, 7, "#3b3440");
  rect(ctx, 3, 4, 6, 5, lit ? "#ffd77a" : "#efe2b8");
  rect(ctx, 4, 5, 2, 2, lit ? "#fff4c8" : "#fbf5e0");
  rect(ctx, 2, 10, 8, 1, "#2f2a33");
  return { img: c, ax: 6, ay: 28 };
}

export function makeBench(): Sprite {
  const c = makeCanvas(30, 16);
  const ctx = context(c);
  ellipse(ctx, 15, 14, 13, 1, SHADOW);
  rect(ctx, 2, 1, 26, 3, "#8a5a33");
  rect(ctx, 2, 1, 26, 1, "#a8703f");
  rect(ctx, 2, 6, 26, 4, "#9a6a3c");
  rect(ctx, 2, 6, 26, 1, "#b58050");
  rect(ctx, 4, 4, 2, 10, "#4a2f1d");
  rect(ctx, 24, 4, 2, 10, "#4a2f1d");
  return { img: c, ax: 15, ay: 14 };
}

export function makeCrate(): Sprite {
  const c = makeCanvas(16, 16);
  const ctx = context(c);
  ellipse(ctx, 8, 14, 7, 1, SHADOW);
  rect(ctx, 1, 2, 14, 12, "#9a6a3c");
  rect(ctx, 1, 2, 14, 1, "#b58050");
  rect(ctx, 1, 2, 1, 12, "#6e4a2e");
  rect(ctx, 14, 2, 1, 12, "#6e4a2e");
  rect(ctx, 1, 13, 14, 1, "#6e4a2e");
  for (let i = 0; i < 10; i++) {
    rect(ctx, 3 + i, 4 + i, 1, 1, "#6e4a2e");
    rect(ctx, 12 - i, 4 + i, 1, 1, "#6e4a2e");
  }
  rect(ctx, 3, 0, 4, 3, "#e8b64a");
  rect(ctx, 8, 0, 4, 3, "#d9534f");
  rect(ctx, 4, 0, 1, 1, "#57a852");
  return { img: c, ax: 8, ay: 14 };
}

export function makeBarrel(): Sprite {
  const c = makeCanvas(14, 16);
  const ctx = context(c);
  ellipse(ctx, 7, 14, 6, 1, SHADOW);
  rect(ctx, 2, 2, 10, 12, "#8a5a33");
  rect(ctx, 1, 4, 12, 8, "#8a5a33");
  rect(ctx, 3, 2, 3, 12, "#a8703f");
  rect(ctx, 1, 4, 12, 1, "#4a3a36");
  rect(ctx, 1, 10, 12, 1, "#4a3a36");
  rect(ctx, 2, 1, 10, 2, "#6e4a2e");
  return { img: c, ax: 7, ay: 14 };
}

export function makePlanter(flower: string): Sprite {
  const c = makeCanvas(16, 18);
  const ctx = context(c);
  ellipse(ctx, 8, 16, 6, 1, SHADOW);
  rect(ctx, 3, 9, 10, 7, "#c46a45");
  rect(ctx, 2, 8, 12, 2, "#d98a5a");
  rect(ctx, 3, 14, 10, 2, "#9c4f33");
  disc(ctx, 8, 6, 5, "#3f8a48");
  disc(ctx, 6, 5, 2, "#57a852");
  for (const [x, y] of [[5, 3], [9, 2], [11, 5], [7, 6], [4, 7]]) {
    rect(ctx, x, y, 2, 2, flower);
    rect(ctx, x, y, 1, 1, "#fff6d8");
  }
  return { img: c, ax: 8, ay: 16 };
}

export function makeSignpost(): Sprite {
  const c = makeCanvas(24, 26);
  const ctx = context(c);
  ellipse(ctx, 12, 24, 5, 1, SHADOW);
  rect(ctx, 11, 10, 2, 14, "#6e4a2e");
  rect(ctx, 1, 1, 22, 11, "#6e4a2e");
  rect(ctx, 2, 2, 20, 9, "#ecd7a8");
  rect(ctx, 2, 10, 20, 1, "#d3b981");
  rect(ctx, 2, 2, 1, 1, "#6e4a2e");
  rect(ctx, 21, 2, 1, 1, "#6e4a2e");
  return { img: c, ax: 12, ay: 24 };
}

export function makeStake(): Sprite {
  const c = makeCanvas(5, 11);
  const ctx = context(c);
  rect(ctx, 1, 1, 3, 9, "#8a5a33");
  rect(ctx, 1, 1, 3, 1, "#a8703f");
  rect(ctx, 2, 0, 1, 1, "#8a5a33");
  rect(ctx, 0, 3, 5, 1, "#e0c68e");
  return { img: c, ax: 2, ay: 10 };
}

export function makeNoticeboard(): Sprite {
  const c = makeCanvas(34, 30);
  const ctx = context(c);
  ellipse(ctx, 17, 28, 14, 1, SHADOW);
  rect(ctx, 4, 6, 3, 22, "#5a3c25");
  rect(ctx, 27, 6, 3, 22, "#5a3c25");
  rect(ctx, 0, 1, 34, 4, "#8f3f35");
  rect(ctx, 0, 1, 34, 1, "#b0574b");
  rect(ctx, 2, 5, 30, 16, "#6e4a2e");
  rect(ctx, 3, 6, 28, 14, "#b58050");
  rect(ctx, 5, 8, 7, 8, "#f6f1e7");
  rect(ctx, 6, 10, 5, 1, "#b3a58c");
  rect(ctx, 6, 12, 4, 1, "#b3a58c");
  rect(ctx, 14, 7, 8, 6, "#fbe39a");
  rect(ctx, 15, 9, 6, 1, "#b39a52");
  rect(ctx, 24, 9, 5, 9, "#cfe8f0");
  rect(ctx, 25, 11, 3, 1, "#7a9fb0");
  rect(ctx, 14, 14, 7, 5, "#f3c6d2");
  rect(ctx, 8, 8, 1, 1, "#d9534f");
  rect(ctx, 17, 7, 1, 1, "#2e6fdb");
  return { img: c, ax: 17, ay: 28 };
}

export interface BuildingSprite {
  day: Sprite;
  night: Sprite;
  sign: { x: number; y: number; w: number };
  windows: { x: number; y: number }[];
  chimney: { x: number; y: number };
}

function drawBuilding(ctx: Ctx, s: LokaShopStyle, lit: boolean): void {
  const roof = s.roof;
  const trim = shade(roof, -0.4);
  ellipse(ctx, 52, 93, 48, 2, SHADOW);
  rect(ctx, 79, 2, 10, 28, "#9b6a52");
  rect(ctx, 78, 1, 12, 3, "#7a4f3c");
  for (let y = 6; y < 28; y += 4) rect(ctx, 80 + ((y / 4) % 2) * 4, y, 4, 1, "#85563f");
  rect(ctx, 8, 48, 88, 46, s.wall);
  rect(ctx, 8, 84, 88, 6, shade(s.wall, -0.07));
  rect(ctx, 8, 48, 3, 46, "#6e4a2e");
  rect(ctx, 93, 48, 3, 46, "#6e4a2e");
  rect(ctx, 38, 60, 2, 34, "#6e4a2e");
  rect(ctx, 64, 60, 2, 34, "#6e4a2e");
  rect(ctx, 8, 89, 88, 7, "#8d8478");
  for (let x = 8; x < 96; x += 7) rect(ctx, x, 92, 1, 4, "#6f675d");
  rect(ctx, 8, 89, 88, 1, "#a79d90");
  for (let y = 8; y <= 52; y++) {
    const t = (y - 8) / 44;
    const left = Math.round(30 - t * 29);
    const right = Math.round(74 + t * 29);
    const row = Math.floor((y - 8) / 5);
    const base = row % 2 ? shade(roof, 0.06) : roof;
    rect(ctx, left, y, right - left, 1, (y - 8) % 5 === 4 ? shade(roof, -0.22) : base);
    if ((y - 8) % 5 !== 4) {
      for (let x = left + ((row % 2) * 3); x < right; x += 6) rect(ctx, x, y, 1, 1, shade(roof, -0.14));
    }
    rect(ctx, left, y, 1, 1, trim);
    rect(ctx, right - 1, y, 1, 1, trim);
  }
  rect(ctx, 30, 6, 44, 3, shade(roof, 0.18));
  rect(ctx, 30, 6, 44, 1, trim);
  rect(ctx, 0, 52, 104, 2, trim);
  rect(ctx, 8, 54, 88, 3, "rgba(40,20,20,0.22)");
  rect(ctx, 29, 55, 46, 12, "#5a3c25");
  rect(ctx, 30, 56, 44, 10, "#ecd7a8");
  rect(ctx, 30, 65, 44, 1, "#d3b981");
  rect(ctx, 31, 57, 1, 1, "#8a6a45");
  rect(ctx, 72, 57, 1, 1, "#8a6a45");
  const [a0, a1] = s.awning;
  for (let i = 0; i < 15; i++) {
    const x = 8 + i * 6;
    const col = i % 2 ? a1 : a0;
    rect(ctx, x, 68, 6, 7, col);
    rect(ctx, x + 1, 75, 4, 1, col);
    rect(ctx, x + 2, 76, 2, 1, col);
  }
  rect(ctx, 8, 67, 88, 1, shade(a1, -0.35));
  rect(ctx, 8, 77, 88, 2, "rgba(40,20,20,0.16)");
  const glass = lit ? "#ffd77a" : "#9fd3e6";
  const glint = lit ? "#fff0b8" : "#d4eef6";
  for (const wx of [16, 70]) {
    rect(ctx, wx, 79, 18, 13, "#5a3c25");
    rect(ctx, wx + 1, 80, 16, 11, glass);
    rect(ctx, wx + 2, 81, 3, 1, glint);
    rect(ctx, wx + 2, 82, 1, 2, glint);
    rect(ctx, wx + 8, 80, 2, 11, "#5a3c25");
    rect(ctx, wx + 1, 85, 16, 1, "#5a3c25");
    rect(ctx, wx - 1, 91, 20, 3, "#7a4f2e");
    for (let f = 0; f < 5; f++) {
      rect(ctx, wx + 1 + f * 4, 89, 2, 2, ["#e85d75", "#f2c14e", "#ffffff", "#e85d75", "#9b7be0"][f]);
      rect(ctx, wx + f * 4, 90, 1, 1, "#57a852");
    }
  }
  rect(ctx, 43, 76, 18, 18, "#4a2f1d");
  rect(ctx, 44, 75, 16, 1, "#4a2f1d");
  rect(ctx, 45, 77, 14, 17, lit ? "#9a6a3c" : "#8a5a33");
  rect(ctx, 46, 76, 12, 1, lit ? "#9a6a3c" : "#8a5a33");
  rect(ctx, 49, 77, 1, 17, "#6e4a2e");
  rect(ctx, 54, 77, 1, 17, "#6e4a2e");
  rect(ctx, 56, 85, 2, 2, "#e8c16a");
  if (lit) rect(ctx, 47, 78, 10, 3, "#ffd77a");
  rect(ctx, 41, 94, 22, 2, "#a79d90");
}

export function makeBuilding(style: LokaShopStyle): BuildingSprite {
  const make = (lit: boolean): Sprite => {
    const c = makeCanvas(104, 96);
    drawBuilding(context(c), style, lit);
    return { img: c, ax: 4, ay: 96 };
  };
  return {
    day: make(false),
    night: make(true),
    sign: { x: 52, y: 61, w: 42 },
    windows: [
      { x: 25, y: 85 },
      { x: 79, y: 85 },
    ],
    chimney: { x: 84, y: 1 },
  };
}

export const FAIRY_COLORS = ["#ffd86b", "#ff8fa3", "#7ee0d2", "#b7a0ff"];

export interface BanyanSprite extends Sprite {
  bulbs: { x: number; y: number; color: string }[];
}

export function makeBanyan(r: () => number): BanyanSprite {
  const c = makeCanvas(120, 104);
  const ctx = context(c, { willReadFrequently: true });
  ellipse(ctx, 60, 98, 44, 5, "rgba(20,30,20,0.25)");
  const roots = [18, 26, 34, 84, 92, 100, 40, 78];
  for (const x of roots) {
    const len = 26 + Math.floor(r() * 26);
    rect(ctx, x, 46, 2, len, "#6b4a30");
    rect(ctx, x, 46, 1, len, "#7d5a3a");
  }
  rect(ctx, 48, 50, 24, 46, "#6b4527");
  rect(ctx, 42, 88, 36, 8, "#6b4527");
  rect(ctx, 38, 93, 44, 4, "#5a3a22");
  rect(ctx, 60, 50, 12, 46, "#553520");
  for (let i = 0; i < 7; i++) rect(ctx, 50 + i * 3, 56 + ((i * 7) % 20), 1, 14, "#4a2e1b");
  const dark = "#24573a";
  const mid = "#2f7043";
  const light = "#44904f";
  const hi = "#6ab35e";
  const blobs: [number, number, number][] = [
    [22, 40, 16], [42, 32, 20], [62, 28, 23], [82, 32, 20], [100, 40, 15],
    [32, 48, 12], [88, 48, 12], [60, 44, 16],
  ];
  for (const [x, y, rad] of blobs) disc(ctx, x, y, rad, dark);
  for (const [x, y, rad] of blobs) disc(ctx, x - 1, y - 3, rad - 3, mid);
  for (const [x, y, rad] of blobs) disc(ctx, x - 4, y - 7, Math.max(2, rad - 10), light);
  for (const [x, y] of [[40, 16], [60, 12], [80, 17], [24, 30], [98, 30]]) disc(ctx, x, y, 3, hi);
  speckle(ctx, r, 4, 4, 112, 56, dark, 90);
  speckle(ctx, r, 4, 4, 112, 40, hi, 30);
  const bulbs: { x: number; y: number; color: string }[] = [];
  const n = 18;
  for (let i = 0; i < n; i++) {
    const t = i / (n - 1);
    const x = Math.round(14 + t * 92);
    const y = Math.round(44 + Math.sin(t * Math.PI * 3) * 3 + Math.sin(t * Math.PI) * 6);
    const color = FAIRY_COLORS[i % FAIRY_COLORS.length];
    rect(ctx, x, y - 1, 1, 1, "#2c2a2a");
    rect(ctx, x, y, 2, 2, color);
    bulbs.push({ x: x + 1, y: y + 1, color });
  }
  return { img: c, ax: 60, ay: 96, bulbs };
}

export function makeFan(): HTMLCanvasElement {
  const R = 22;
  const c = makeCanvas(R * 2 + 1, R + 2);
  const ctx = context(c);
  const cx = R;
  const cy = R;
  const spokes = 11;
  for (let y = 0; y <= R; y++) {
    for (let x = 0; x <= R * 2; x++) {
      const dx = x - cx;
      const dy = cy - y;
      const d = Math.sqrt(dx * dx + dy * dy);
      if (d > R || dy < 0) continue;
      const ang = Math.atan2(dy, dx);
      const s = (ang / Math.PI) * (spokes - 1);
      const near = Math.abs(s - Math.round(s));
      let color = d > R - 2 ? "#6fd08c" : near < 0.12 ? "#15664a" : Math.round(s) % 2 ? "#1f8a5a" : "#23995f";
      const eyeD = Math.hypot(near * (d * Math.PI / (spokes - 1)), d - R * 0.74);
      if (eyeD < 1.6) color = "#1b3fb8";
      else if (eyeD < 2.6) color = "#1fb3a0";
      else if (eyeD < 3.4) color = "#e8b64a";
      const innerD = Math.hypot(near * (d * Math.PI / (spokes - 1)), d - R * 0.45);
      if (Math.round(s) % 2 === 0 && innerD < 1.4) color = "#1b3fb8";
      else if (Math.round(s) % 2 === 0 && innerD < 2.3) color = "#e8b64a";
      rect(ctx, x, y, 1, 1, color);
    }
  }
  return c;
}

export interface PeacockSprite {
  walk: HTMLCanvasElement[];
  displaying: HTMLCanvasElement[];
  fan: HTMLCanvasElement;
  ax: number;
  ay: number;
}

function drawPeacockBody(ctx: Ctx, tail: boolean, step: number): void {
  if (tail) {
    const rows: [number, number, number][] = [
      [15, 7, 13],
      [16, 4, 13],
      [17, 1, 12],
      [18, 0, 11],
      [19, 2, 8],
    ];
    for (const [y, x0, x1] of rows) rect(ctx, x0, y, x1 - x0, 1, "#1f8a5a");
    rect(ctx, 4, 16, 8, 1, "#2fae6a");
    rect(ctx, 1, 18, 3, 1, "#2fae6a");
    for (const [x, y] of [[3, 17], [7, 16], [6, 18], [10, 17]]) {
      rect(ctx, x, y, 1, 1, "#1b3fb8");
      rect(ctx, x + 1, y, 1, 1, "#e8b64a");
    }
  }
  const legA = step ? 1 : 0;
  rect(ctx, 14, 19, 1, 3 - legA, "#9a6a3c");
  rect(ctx, 16, 19, 1, 2 + legA, "#9a6a3c");
  rect(ctx, 13, 21 - legA, 2, 1, "#9a6a3c");
  rect(ctx, 16, 20 + legA, 2, 1, "#9a6a3c");
  ellipse(ctx, 15, 15, 5, 4, "#1a4fb8");
  ellipse(ctx, 15, 14, 4, 3, "#1e5bd8");
  rect(ctx, 11, 13, 4, 4, "#c79a5a");
  rect(ctx, 11, 14, 4, 1, "#9a6f3c");
  rect(ctx, 11, 16, 4, 1, "#9a6f3c");
  rect(ctx, 17, 6, 3, 8, "#2466e8");
  rect(ctx, 19, 7, 1, 6, "#3b82f6");
  rect(ctx, 18, 3, 4, 4, "#2466e8");
  rect(ctx, 20, 4, 1, 1, "#ffffff");
  rect(ctx, 21, 4, 1, 1, "#10131c");
  rect(ctx, 19, 5, 2, 1, "#ffffff");
  rect(ctx, 22, 5, 1, 1, "#e8b64a");
  rect(ctx, 18, 1, 1, 2, "#2466e8");
  rect(ctx, 20, 0, 1, 3, "#2466e8");
  rect(ctx, 21, 1, 1, 2, "#2466e8");
  rect(ctx, 18, 0, 1, 1, "#7ee0d2");
  rect(ctx, 20, 0, 1, 1, "#7ee0d2");
  rect(ctx, 21, 0, 1, 1, "#7ee0d2");
}

export function makePeacock(): PeacockSprite {
  const frame = (tail: boolean, step: number) => {
    const c = makeCanvas(26, 23);
    drawPeacockBody(context(c), tail, step);
    return outline(c, [22, 28, 44]);
  };
  return {
    walk: [frame(true, 0), frame(true, 1)],
    displaying: [frame(false, 0), frame(false, 1)],
    fan: outline(makeFan(), [22, 28, 44]),
    ax: 16,
    ay: 23,
  };
}
