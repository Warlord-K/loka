import type { Sprite } from "./art";
import { buildSheet } from "./characters";
import type { LokaLook } from "./types";
import { context, disc, ellipse, makeCanvas, rect, rng, shade, type Ctx } from "./pixels";

const T = 16;

export type FurnitureKind =
  | "shelf"
  | "desk"
  | "terminal"
  | "machine"
  | "clock"
  | "counter"
  | "mailbox"
  | "telescope"
  | "anvil"
  | "workbench"
  | "bell"
  | "scale"
  | "lamp"
  | "chest"
  | "plant"
  | "rug"
  | "bed"
  | "hearth"
  | "chart"
  | "drum"
  | "doorway"
  | "mentor";

export type FurnitureState = "on" | "busy" | "off" | "alarm";

export interface Furniture {
  id: string;
  kind: FurnitureKind;
  x: number;
  y: number;
  label?: string;
  colors?: string[];
  state?: FurnitureState;
  tint?: string;
  look?: LokaLook;
}

export interface RoomSpec {
  id: string;
  title: string;
  w: number;
  h: number;
  floor: "wood" | "stone" | "tile";
  wall: string;
  furniture: Furniture[];
  exitTo?: string;
}

export interface RoomThing {
  id: string;
  label: string;
  x: number;
  y: number;
  lift: number;
  goals: number[];
}

export interface RoomGlow {
  x: number;
  y: number;
  color: [number, number, number];
  pulse: boolean;
}

export interface Room {
  spec: RoomSpec;
  w: number;
  h: number;
  solid: Uint8Array;
  ground: HTMLCanvasElement;
  objects: { x: number; y: number; sprite: Sprite }[];
  things: RoomThing[];
  glows: RoomGlow[];
  entry: { x: number; y: number };
  door: number;
}

const FOOT: Record<FurnitureKind, [number, number]> = {
  shelf: [3, 1],
  desk: [2, 1],
  terminal: [2, 1],
  machine: [1, 1],
  clock: [1, 1],
  counter: [3, 1],
  mailbox: [3, 1],
  telescope: [2, 2],
  anvil: [1, 1],
  workbench: [2, 1],
  bell: [1, 1],
  scale: [1, 1],
  lamp: [1, 1],
  chest: [1, 1],
  plant: [1, 1],
  rug: [3, 2],
  bed: [2, 2],
  hearth: [2, 1],
  chart: [2, 1],
  drum: [1, 1],
  doorway: [2, 1],
  mentor: [1, 1],
};

const STATE_COLOR: Record<FurnitureState, string> = {
  on: "#5fe08a",
  busy: "#ffc94a",
  off: "#4a4048",
  alarm: "#ff5a4a",
};

function sprite(w: number, h: number, draw: (ctx: Ctx, w: number, h: number) => void): Sprite {
  const c = makeCanvas(w, h);
  const ctx = context(c);
  draw(ctx, w, h);
  return { img: c, ax: 0, ay: h };
}

function legs(ctx: Ctx, w: number, h: number, top: number, color: string): void {
  rect(ctx, 2, top, 2, h - top - 1, color);
  rect(ctx, w - 4, top, 2, h - top - 1, color);
}

function drawFurniture(f: Furniture, fw: number): Sprite {
  const W = fw * T;
  const colors = f.colors ?? [];
  const led = STATE_COLOR[f.state ?? "on"];
  switch (f.kind) {
    case "shelf":
      return sprite(W, 36, (ctx, w, h) => {
        ellipse(ctx, w / 2, h - 2, w / 2 - 1, 1, "rgba(0,0,0,0.25)");
        rect(ctx, 1, 0, w - 2, h - 2, "#5a3c25");
        rect(ctx, 3, 2, w - 6, h - 6, "#3a2618");
        for (let row = 0; row < 3; row++) {
          const y = 2 + row * 10;
          rect(ctx, 3, y + 9, w - 6, 2, "#7a5234");
          let x = 4;
          for (let i = row; i < colors.length && x < w - 8; i += 3) {
            const bw = 3 + ((i * 7) % 2);
            const bh = 7 + ((i * 5) % 2);
            rect(ctx, x, y + 9 - bh, bw, bh, colors[i]);
            rect(ctx, x, y + 9 - bh + 2, bw, 1, shade(colors[i], 0.3));
            x += bw + 1;
          }
          if (!colors.length && row === 1) for (let k = 0; k < 4; k++) rect(ctx, 8 + k * 9, y + 7, 3, 1, "#6a5a4a");
        }
      });
    case "desk":
      return sprite(W, 22, (ctx, w, h) => {
        legs(ctx, w, h, 10, "#5a3c25");
        rect(ctx, 0, 6, w, 5, "#8a5a33");
        rect(ctx, 0, 6, w, 1, "#a8703f");
        rect(ctx, 6, 2, 10, 5, "#fbf3e0");
        rect(ctx, 7, 3, 7, 1, "#b3a58c");
        rect(ctx, 7, 5, 5, 1, "#b3a58c");
        rect(ctx, w - 10, 1, 2, 6, f.tint ?? "#3f6fb0");
        rect(ctx, w - 7, 4, 3, 3, "#e8c16a");
      });
    case "terminal":
      return sprite(W, 26, (ctx, w, h) => {
        legs(ctx, w, h, 14, "#3a3a44");
        rect(ctx, 0, 11, w, 4, "#5a5a68");
        rect(ctx, 8, 0, 16, 11, "#22242c");
        rect(ctx, 9, 1, 14, 8, f.state === "off" ? "#2e3038" : "#123a2a");
        if (f.state !== "off") {
          rect(ctx, 10, 3, 6, 1, "#5fe08a");
          rect(ctx, 10, 5, 9, 1, "#5fe08a");
          rect(ctx, 10, 7, 3, 1, "#5fe08a");
        }
        rect(ctx, 14, 9, 4, 2, "#3a3a44");
        rect(ctx, 6, 12, 20, 2, "#8a8a98");
      });
    case "machine":
      return sprite(W, 26, (ctx, w, h) => {
        ellipse(ctx, w / 2, h - 2, 7, 1, "rgba(0,0,0,0.3)");
        rect(ctx, 2, 4, w - 4, h - 6, f.tint ?? "#5f6a7a");
        rect(ctx, 2, 4, w - 4, 2, shade(f.tint ?? "#5f6a7a", 0.25));
        rect(ctx, 4, 8, w - 8, 7, "#1d2028");
        for (let i = 0; i < 3; i++) rect(ctx, 5 + i * 2, 10 + (i % 2), 1, 3, f.state === "off" ? "#3a3a44" : led);
        rect(ctx, w - 6, 17, 2, 2, led);
        rect(ctx, 4, 20, w - 8, 1, "#3a3f4a");
        rect(ctx, 6, 0, 1, 4, "#8a8a98");
      });
    case "clock":
      return sprite(W, 38, (ctx, w, h) => {
        rect(ctx, 3, 4, 10, h - 5, "#6e4a2e");
        rect(ctx, 2, 2, 12, 3, "#8a5a33");
        disc(ctx, 8, 10, 4, "#f8f1df");
        rect(ctx, 8, 7, 1, 3, "#3a2a20");
        rect(ctx, 8, 10, 3, 1, "#3a2a20");
        rect(ctx, 5, 17, 6, 14, "#3a2618");
        rect(ctx, 7, 19, 2, 8, "#c9a24a");
        disc(ctx, 8, 27, 2, "#e8c16a");
      });
    case "counter":
      return sprite(W, 24, (ctx, w, h) => {
        rect(ctx, 0, 6, w, h - 7, "#8a5a33");
        rect(ctx, 0, 4, w, 3, "#a8703f");
        for (let x = 6; x < w; x += 10) rect(ctx, x, 9, 1, h - 12, "#6e4a2e");
        colors.slice(0, 6).forEach((c, i) => rect(ctx, 4 + i * 7, 0, 5, 4, c));
      });
    case "mailbox":
      return sprite(W, 34, (ctx, w, h) => {
        rect(ctx, 1, 0, w - 2, h - 2, "#6e4a2e");
        for (let r = 0; r < 4; r++) {
          for (let c = 0; c < 6; c++) {
            const x = 3 + c * 7;
            const y = 2 + r * 7;
            rect(ctx, x, y, 6, 6, "#3a2618");
            const n = r * 6 + c;
            if (n < colors.length) {
              rect(ctx, x + 1, y + 2, 4, 3, colors[n]);
              rect(ctx, x + 1, y + 2, 4, 1, "#ffffff");
            }
          }
        }
      });
    case "telescope":
      return sprite(W, 40, (ctx, w, h) => {
        ellipse(ctx, w / 2, h - 4, 12, 2, "rgba(0,0,0,0.3)");
        rect(ctx, 14, 20, 4, h - 22, "#4a4e5a");
        rect(ctx, 8, h - 6, 16, 3, "#4a4e5a");
        for (let i = 0; i < 9; i++) rect(ctx, 6 + i * 2, 22 - i * 2, 6, 4, i > 6 ? "#d8b65a" : "#7a86a6");
        rect(ctx, 22, 2, 6, 6, "#d8b65a");
      });
    case "anvil":
      return sprite(W, 20, (ctx, w, h) => {
        rect(ctx, 4, 12, 8, h - 13, "#6e4a2e");
        rect(ctx, 1, 5, 14, 4, "#5a5c66");
        rect(ctx, 4, 9, 8, 3, "#4a4c56");
        rect(ctx, 0, 5, 3, 2, "#5a5c66");
        rect(ctx, 1, 5, 14, 1, "#8a8c98");
      });
    case "workbench":
      return sprite(W, 24, (ctx, w, h) => {
        legs(ctx, w, h, 12, "#5a3c25");
        rect(ctx, 0, 8, w, 5, "#a8703f");
        rect(ctx, 0, 8, w, 1, "#c9925a");
        rect(ctx, 4, 4, 8, 4, "#7a86a6");
        rect(ctx, 16, 2, 2, 7, "#5a3c25");
        rect(ctx, 14, 1, 6, 3, "#8a8c98");
        rect(ctx, w - 9, 5, 6, 3, "#e8b64a");
      });
    case "bell":
      return sprite(W, 28, (ctx, w, h) => {
        rect(ctx, 7, 6, 2, h - 7, "#6e4a2e");
        rect(ctx, 3, h - 3, 10, 2, "#6e4a2e");
        rect(ctx, 2, 2, 12, 2, "#5a3c25");
        rect(ctx, 5, 4, 6, 6, f.tint ?? "#e8c16a");
        rect(ctx, 4, 9, 8, 2, f.tint ?? "#e8c16a");
        rect(ctx, 7, 11, 2, 1, "#8a6a2a");
      });
    case "scale":
      return sprite(W, 26, (ctx, w, h) => {
        rect(ctx, 7, 6, 2, h - 8, "#c9a24a");
        rect(ctx, 4, h - 3, 8, 2, "#8a6a2a");
        rect(ctx, 1, 5, 14, 1, "#c9a24a");
        rect(ctx, 0, 9, 5, 2, "#c9a24a");
        rect(ctx, 11, 7, 5, 2, "#c9a24a");
        rect(ctx, 1, 7, 3, 2, "#e8c16a");
        rect(ctx, 12, 5, 3, 2, "#e8c16a");
      });
    case "lamp":
      return sprite(W, 30, (ctx, w, h) => {
        rect(ctx, 7, 8, 2, h - 9, "#3a2a20");
        rect(ctx, 4, h - 3, 8, 2, "#3a2a20");
        rect(ctx, 4, 1, 8, 8, "#3a2a20");
        rect(ctx, 5, 2, 6, 6, f.state === "alarm" ? "#ff5a4a" : f.state === "off" ? "#5a4a3a" : "#ffd77a");
      });
    case "chest":
      return sprite(W, 16, (ctx, w, h) => {
        rect(ctx, 1, 4, 14, h - 5, "#8a5a33");
        rect(ctx, 1, 2, 14, 4, "#a8703f");
        rect(ctx, 1, 6, 14, 1, "#5a3c25");
        rect(ctx, 7, 6, 2, 3, "#e8c16a");
      });
    case "plant":
      return sprite(W, 24, (ctx, w, h) => {
        rect(ctx, 4, h - 8, 8, 7, "#b8603a");
        rect(ctx, 3, h - 9, 10, 2, "#c8704a");
        disc(ctx, 8, 9, 6, "#2f7a3e");
        disc(ctx, 5, 12, 3, "#3d8a48");
        disc(ctx, 11, 11, 3, "#57a852");
      });
    case "rug":
      return sprite(W, 2 * T, (ctx, w, h) => {
        rect(ctx, 2, 3, w - 4, h - 6, f.tint ?? "#8f3f35");
        rect(ctx, 4, 5, w - 8, h - 10, shade(f.tint ?? "#8f3f35", 0.2));
        rect(ctx, 8, 9, w - 16, h - 18, f.tint ?? "#8f3f35");
      });
    case "bed":
      return sprite(W, 2 * T + 6, (ctx, w, h) => {
        rect(ctx, 1, 0, w - 2, 8, "#6e4a2e");
        rect(ctx, 2, 6, w - 4, h - 8, "#f6f1e7");
        rect(ctx, 4, 8, w - 8, 6, "#ffffff");
        rect(ctx, 2, 16, w - 4, h - 18, f.tint ?? "#3f6fb0");
        rect(ctx, 2, 16, w - 4, 2, shade(f.tint ?? "#3f6fb0", 0.25));
      });
    case "hearth":
      return sprite(W, 30, (ctx, w, h) => {
        rect(ctx, 0, 0, w, h - 1, "#8e8279");
        rect(ctx, 2, 2, w - 4, 3, "#a79d90");
        rect(ctx, 6, 10, w - 12, h - 11, "#2a1d18");
        rect(ctx, 9, h - 8, w - 18, 4, "#ff9a3c");
        rect(ctx, 11, h - 11, w - 22, 4, "#ffd45a");
      });
    case "chart":
      return sprite(W, 30, (ctx, w, h) => {
        rect(ctx, 4, 18, 2, h - 19, "#5a3c25");
        rect(ctx, w - 6, 18, 2, h - 19, "#5a3c25");
        rect(ctx, 0, 0, w, 20, "#5a3c25");
        rect(ctx, 1, 1, w - 2, 18, "#1d2448");
        const r = rng(f.id.length * 97 + 13);
        for (let i = 0; i < 14; i++) rect(ctx, 2 + Math.floor(r() * (w - 4)), 2 + Math.floor(r() * 15), 1, 1, i % 4 ? "#fbf3e0" : "#ffd45a");
        rect(ctx, 6, 8, 8, 1, "#7ee0d2");
        rect(ctx, 14, 8, 1, 5, "#7ee0d2");
        rect(ctx, 14, 12, 9, 1, "#7ee0d2");
      });
    case "doorway":
      return sprite(W, 34, (ctx, w, h) => {
        rect(ctx, 1, 2, w - 2, h - 2, "#4a3226");
        rect(ctx, 3, 6, w - 6, h - 6, "#2a1d18");
        rect(ctx, 4, 7, w - 8, h - 8, "#7a5234");
        rect(ctx, 5, 8, w - 10, 1, "#8f6440");
        rect(ctx, w - 8, h / 2 + 3, 2, 2, "#e8b64a");
        rect(ctx, 2, 0, w - 4, 6, f.tint ?? "#e8b64a");
        rect(ctx, 4, 2, w - 8, 2, "#fbf3e0");
      });
    case "mentor":
      return sprite(W, 26, (ctx, w, h) => {
        ellipse(ctx, w / 2, h - 2, 5, 1, "rgba(0,0,0,0.25)");
        const img = buildSheet(f.look ?? { skin: "#f1c7a1", hair: "#2b1d16", shirt: "#4b3a8c", pants: "#35506e" }).down[0];
        ctx.drawImage(img, Math.round(w / 2 - 7), h - 22);
      });
    case "drum":
      return sprite(W, 20, (ctx, w, h) => {
        ellipse(ctx, 8, h - 3, 6, 2, "#6e4a2e");
        rect(ctx, 2, 6, 12, h - 9, "#b8513a");
        ellipse(ctx, 8, 6, 6, 2, "#f6f1e7");
        for (let x = 3; x < 14; x += 3) rect(ctx, x, 9, 1, h - 12, "#e8c16a");
      });
  }
}

function paintRoom(spec: RoomSpec, door: number): HTMLCanvasElement {
  const { w, h } = spec;
  const c = makeCanvas(w * T, h * T);
  const ctx = context(c);
  const r = rng(spec.id.length * 131 + w * 17 + h);
  rect(ctx, 0, 0, w * T, h * T, "#1b1410");
  for (let y = 2; y < h - 1; y++) {
    for (let x = 1; x < w - 1; x++) {
      const px = x * T;
      const py = y * T;
      if (spec.floor === "wood") {
        rect(ctx, px, py, T, T, (y + Math.floor(x / 3)) % 2 ? "#b07a48" : "#a8703f");
        rect(ctx, px, py + T - 1, T, 1, "#7a5234");
        if ((x + y) % 3 === 0) rect(ctx, px + 15, py, 1, T, "#8a5a33");
        rect(ctx, px + Math.floor(r() * 14), py + 4 + Math.floor(r() * 8), 2, 1, "#c08a58");
      } else if (spec.floor === "stone") {
        rect(ctx, px, py, T, T, (x + y) % 2 ? "#9a9488" : "#a59f92");
        rect(ctx, px, py, T, 1, "#857f74");
        rect(ctx, px, py, 1, T, "#857f74");
        if (r() < 0.3) rect(ctx, px + 3 + Math.floor(r() * 9), py + 3 + Math.floor(r() * 9), 2, 1, "#8a847a");
      } else {
        rect(ctx, px, py, T, T, (x + y) % 2 ? "#e8e0cc" : "#5f6a7a");
        rect(ctx, px, py, T, 1, "rgba(0,0,0,0.12)");
      }
    }
  }
  const wall = spec.wall;
  rect(ctx, T, 0, (w - 2) * T, 2 * T, wall);
  for (let x = T; x < (w - 1) * T; x += 8) rect(ctx, x, 0, 1, 2 * T - 6, shade(wall, -0.08));
  rect(ctx, T, 2 * T - 8, (w - 2) * T, 6, shade(wall, -0.25));
  rect(ctx, T, 2 * T - 8, (w - 2) * T, 1, shade(wall, 0.15));
  rect(ctx, T, 2 * T - 2, (w - 2) * T, 2, "rgba(0,0,0,0.3)");
  for (let wx = 3; wx < w - 3; wx += 5) {
    rect(ctx, wx * T + 2, 4, 12, 12, "#5a3c25");
    rect(ctx, wx * T + 3, 5, 10, 10, "#9fd3e6");
    rect(ctx, wx * T + 8, 5, 1, 10, "#5a3c25");
    rect(ctx, wx * T + 3, 10, 10, 1, "#5a3c25");
    rect(ctx, wx * T + 4, 6, 2, 1, "#d4eef6");
  }
  rect(ctx, 0, 0, T, h * T, shade(wall, -0.45));
  rect(ctx, T - 2, 2 * T, 2, (h - 3) * T, "rgba(0,0,0,0.25)");
  rect(ctx, (w - 1) * T, 0, T, h * T, shade(wall, -0.45));
  rect(ctx, (w - 1) * T, 2 * T, 2, (h - 3) * T, "rgba(0,0,0,0.25)");
  rect(ctx, 0, (h - 1) * T, w * T, T, shade(wall, -0.45));
  rect(ctx, 0, (h - 1) * T, w * T, 2, shade(wall, -0.2));
  rect(ctx, door * T - 2, (h - 1) * T, T + 4, T, "#2a1d18");
  rect(ctx, door * T, (h - 1) * T, T, T, "#6e4a2e");
  rect(ctx, door * T + 1, (h - 1) * T + 2, T - 2, T - 4, "#c9a24a");
  for (let i = 0; i < 4; i++) rect(ctx, door * T + 2 + i * 3, (h - 1) * T + 4, 2, 8, "#a8803a");
  return c;
}

export function buildRoom(spec: RoomSpec): Room {
  const w = Math.max(7, spec.w);
  const h = Math.max(7, spec.h);
  const fixed = { ...spec, w, h };
  const solid = new Uint8Array(w * h);
  const door = Math.floor(w / 2);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      if (y < 2 || x === 0 || x === w - 1 || (y === h - 1 && x !== door)) solid[y * w + x] = 1;
    }
  }
  const objects: Room["objects"] = [];
  const things: RoomThing[] = [];
  const glows: RoomGlow[] = [];
  const placed = fixed.furniture.filter((f) => f.x >= 1 && f.y >= 2 && f.x + FOOT[f.kind][0] <= w - 1 && f.y + FOOT[f.kind][1] <= h - 1);
  for (const f of placed) {
    const [fw, fh] = FOOT[f.kind];
    if (f.kind !== "rug") for (let y = f.y; y < f.y + fh; y++) for (let x = f.x; x < f.x + fw; x++) solid[y * w + x] = 1;
  }
  for (const f of placed) {
    const [fw, fh] = FOOT[f.kind];
    const s = drawFurniture(f, fw);
    const base = (f.y + fh) * T;
    objects.push({ x: f.x * T, y: f.kind === "rug" ? f.y * T + 1 : base - 1, sprite: s });
    if ((f.kind === "machine" || f.kind === "terminal") && f.state && f.state !== "off") {
      glows.push({ x: f.x * T + fw * 8, y: base - 14, color: f.state === "busy" ? [255, 200, 90] : [110, 230, 150], pulse: f.state === "busy" });
    }
    if (f.kind === "lamp" && f.state === "alarm") glows.push({ x: f.x * T + 8, y: base - 24, color: [255, 80, 70], pulse: true });
    if (f.kind === "hearth") glows.push({ x: f.x * T + 16, y: base - 8, color: [255, 160, 80], pulse: true });
    if (!f.label) continue;
    const goals: number[] = [];
    for (let x = f.x; x < f.x + fw; x++) {
      const y = f.y + fh;
      if (y < h - 1 && !solid[y * w + x]) goals.push(y * w + x);
    }
    if (!goals.length) {
      for (const [x, y] of [[f.x - 1, f.y], [f.x + fw, f.y], [f.x - 1, f.y + fh - 1], [f.x + fw, f.y + fh - 1]]) {
        if (x > 0 && x < w - 1 && y > 1 && y < h - 1 && !solid[y * w + x]) goals.push(y * w + x);
      }
    }
    things.push({ id: f.id, label: f.label, x: f.x * T + (fw * T) / 2, y: base + 6, lift: s.img.height + 4, goals });
  }
  return {
    spec: fixed,
    w,
    h,
    solid,
    ground: paintRoom(fixed, door),
    objects,
    things,
    glows,
    entry: { x: door * T + 8, y: (h - 2) * T + 11 },
    door: (h - 1) * w + door,
  };
}
