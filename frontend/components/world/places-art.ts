import type { Sprite } from "./art";
import { context, disc, ellipse, makeCanvas, rect, shade, type Ctx } from "./pixels";
import { rememberThing } from "./sprites";

export type CivicId = "clock" | "library" | "forge" | "post" | "market" | "observatory";

export interface Point {
  x: number;
  y: number;
}

export interface CivicSprite {
  day: Sprite;
  night: Sprite;
  sign: { x: number; y: number; w: number };
  windows: Point[];
  chimney: Point | null;
  anchors: Record<string, Point>;
}

const SHADOW = "rgba(20,30,20,0.22)";
const WOOD = "#6e4a2e";
const DARK_WOOD = "#4a2f1d";

function door(ctx: Ctx, top: number, base: number, lit: boolean, color = "#8a5a33"): void {
  rect(ctx, 43, top, 18, base - top, DARK_WOOD);
  rect(ctx, 45, top + 2, 14, base - top - 2, lit ? shade(color, 0.12) : color);
  rect(ctx, 46, top + 1, 12, 1, lit ? shade(color, 0.12) : color);
  rect(ctx, 49, top + 2, 1, base - top - 2, WOOD);
  rect(ctx, 54, top + 2, 1, base - top - 2, WOOD);
  rect(ctx, 56, top + Math.round((base - top) / 2), 2, 2, "#e8c16a");
  if (lit) rect(ctx, 47, top + 3, 10, 3, "#ffd77a");
  rect(ctx, 41, base, 22, 2, "#a79d90");
}

function windowPane(ctx: Ctx, x: number, y: number, w: number, h: number, lit: boolean, frame = "#5a3c25"): void {
  rect(ctx, x, y, w, h, frame);
  rect(ctx, x + 1, y + 1, w - 2, h - 2, lit ? "#ffd77a" : "#9fd3e6");
  rect(ctx, x + 2, y + 2, 2, 1, lit ? "#fff0b8" : "#d4eef6");
  rect(ctx, x + Math.floor(w / 2), y + 1, 1, h - 2, frame);
}

function plaque(ctx: Ctx, cx: number, y: number, w: number, color = "#ecd7a8"): void {
  rect(ctx, cx - w / 2 - 1, y - 1, w + 2, 12, "#5a3c25");
  rect(ctx, cx - w / 2, y, w, 10, color);
  rect(ctx, cx - w / 2, y + 9, w, 1, shade(color, -0.12));
}

function stones(ctx: Ctx, x: number, y: number, w: number, h: number, base: string): void {
  rect(ctx, x, y, w, h, base);
  for (let yy = y + 4; yy < y + h; yy += 5) {
    rect(ctx, x, yy, w, 1, shade(base, -0.1));
    for (let xx = x + ((yy / 5) % 2 ? 3 : 7); xx < x + w; xx += 9) rect(ctx, xx, yy - 4, 1, 4, shade(base, -0.08));
  }
}

function make(w: number, h: number, draw: (ctx: Ctx, lit: boolean) => void): { day: Sprite; night: Sprite } {
  const one = (lit: boolean): Sprite => {
    const c = makeCanvas(w, h);
    draw(context(c), lit);
    return { img: c, ax: 4, ay: h };
  };
  return { day: one(false), night: one(true) };
}

function library(): CivicSprite {
  const H = 100;
  const sprite = make(104, H, (ctx, lit) => {
    ellipse(ctx, 52, H - 3, 48, 2, SHADOW);
    stones(ctx, 8, 46, 88, 46, "#e6dcc6");
    const roof = "#3f5f8f";
    for (let y = 14; y <= 46; y++) {
      const t = (y - 14) / 32;
      const half = Math.round(6 + t * 46);
      rect(ctx, 52 - half, y, half * 2, 1, (y - 14) % 4 === 3 ? shade(roof, -0.2) : roof);
    }
    for (let y = 22; y <= 42; y++) {
      const half = Math.round(((y - 22) / 20) * 30);
      rect(ctx, 52 - half, y, half * 2, 1, "#e9e1cf");
    }
    rect(ctx, 2, 46, 100, 3, shade(roof, -0.35));
    for (const cx of [14, 28, 72, 86]) {
      rect(ctx, cx, 50, 6, 38, "#f6f0e2");
      rect(ctx, cx + 4, 50, 2, 38, "#d9cfba");
      rect(ctx, cx - 1, 49, 8, 2, "#d9cfba");
      rect(ctx, cx - 1, 87, 8, 2, "#d9cfba");
    }
    windowPane(ctx, 20, 58, 8, 22, lit);
    windowPane(ctx, 78, 58, 8, 22, lit);
    rect(ctx, 34, 56, 36, 2, "#d9cfba");
    for (let i = 0; i < 6; i++) rect(ctx, 36 + i * 6, 52, 4, 3, ["#b8513a", "#3f6fb0", "#2f7a5a", "#e8b64a", "#6c4f9e", "#b8513a"][i]);
    door(ctx, 70, 92, lit, "#5f3f86");
    rect(ctx, 6, 92, 92, 4, "#cfc4ae");
    rect(ctx, 10, 96, 84, 2, "#b9ad95");
  });
  return { ...sprite, sign: { x: 52, y: 34, w: 30 }, windows: [{ x: 24, y: 69 }, { x: 82, y: 69 }], chimney: null, anchors: { glow: { x: 52, y: 60 } } };
}

function clockTower(): CivicSprite {
  const H = 124;
  const sprite = make(104, H, (ctx, lit) => {
    ellipse(ctx, 52, H - 3, 46, 2, SHADOW);
    stones(ctx, 12, 80, 80, 40, "#d9cbb0");
    rect(ctx, 8, 76, 88, 5, "#7a4f3c");
    rect(ctx, 8, 76, 88, 1, "#9b6a52");
    stones(ctx, 32, 22, 40, 56, "#cdbd9f");
    rect(ctx, 32, 22, 3, 56, shade("#cdbd9f", -0.18));
    rect(ctx, 69, 22, 3, 56, shade("#cdbd9f", -0.18));
    const roof = "#2f7a5a";
    for (let y = 0; y <= 22; y++) {
      const half = Math.round(2 + (y / 22) * 22);
      rect(ctx, 52 - half, y, half * 2, 1, y % 4 === 3 ? shade(roof, -0.22) : roof);
    }
    rect(ctx, 51, 0, 2, 3, "#e8c16a");
    rect(ctx, 28, 22, 48, 3, shade(roof, -0.35));
    disc(ctx, 52, 44, 13, "#5a3c25");
    disc(ctx, 52, 44, 12, "#f8f1df");
    for (let i = 0; i < 12; i++) {
      const a = (i / 12) * Math.PI * 2;
      rect(ctx, Math.round(52 + Math.cos(a) * 10), Math.round(44 + Math.sin(a) * 10), 1, 1, "#7a5a3a");
    }
    rect(ctx, 44, 62, 16, 10, "#3a2a20");
    rect(ctx, 46, 63, 12, 8, lit ? "#e9b75a" : "#5a4a3a");
    rect(ctx, 50, 64, 4, 5, "#c9a24a");
    windowPane(ctx, 20, 88, 14, 12, lit);
    windowPane(ctx, 70, 88, 14, 12, lit);
    plaque(ctx, 52, 82, 28);
    door(ctx, 98, 120, lit, "#7a4f2e");
    rect(ctx, 10, 120, 84, 2, "#a79d90");
  });
  return { ...sprite, sign: { x: 52, y: 87, w: 28 }, windows: [{ x: 27, y: 94 }, { x: 77, y: 94 }], chimney: null, anchors: { clock: { x: 52, y: 44 } } };
}

function forge(): CivicSprite {
  const H = 96;
  const sprite = make(104, H, (ctx, lit) => {
    ellipse(ctx, 52, H - 3, 48, 2, SHADOW);
    stones(ctx, 8, 3, 16, 46, "#7a6a60");
    rect(ctx, 7, 1, 18, 4, "#4f4540");
    stones(ctx, 8, 44, 88, 48, "#8e8279");
    const roof = "#4a4e5a";
    for (let y = 22; y <= 46; y++) {
      const t = (y - 22) / 24;
      rect(ctx, Math.round(30 - t * 26), y, Math.round(44 + t * 52), 1, (y - 22) % 4 === 3 ? shade(roof, -0.25) : roof);
    }
    rect(ctx, 2, 46, 100, 3, "#2e3038");
    rect(ctx, 66, 58, 26, 30, "#2a1d18");
    rect(ctx, 68, 60, 22, 26, "#3a2620");
    rect(ctx, 70, 74, 18, 12, lit ? "#ff9a3c" : "#d4622a");
    rect(ctx, 72, 76, 14, 6, lit ? "#ffd45a" : "#f29a3a");
    rect(ctx, 66, 56, 26, 2, "#4f4540");
    windowPane(ctx, 16, 60, 16, 14, lit, "#3a2a20");
    rect(ctx, 14, 82, 20, 6, "#4f4f58");
    rect(ctx, 18, 78, 12, 4, "#5f6070");
    rect(ctx, 22, 76, 4, 2, "#7a7c8a");
    plaque(ctx, 50, 49, 26);
    door(ctx, 68, 92, lit, "#5a3c25");
    rect(ctx, 8, 92, 88, 3, "#6f675d");
  });
  return { ...sprite, sign: { x: 50, y: 54, w: 26 }, windows: [{ x: 24, y: 67 }, { x: 79, y: 80 }], chimney: { x: 16, y: 0 }, anchors: { hearth: { x: 79, y: 80 } } };
}

function postOffice(): CivicSprite {
  const H = 96;
  const sprite = make(104, H, (ctx, lit) => {
    ellipse(ctx, 50, H - 3, 46, 2, SHADOW);
    rect(ctx, 6, 46, 84, 46, "#eef0f4");
    for (let y = 50; y < 92; y += 6) rect(ctx, 6, y, 84, 1, "#d8dce6");
    rect(ctx, 6, 46, 3, 46, WOOD);
    rect(ctx, 87, 46, 3, 46, WOOD);
    const roof = "#c0443a";
    for (let y = 12; y <= 48; y++) {
      const t = (y - 12) / 36;
      const left = Math.round(28 - t * 26);
      const right = Math.round(70 + t * 24);
      rect(ctx, left, y, right - left, 1, (y - 12) % 5 === 4 ? shade(roof, -0.22) : y % 10 < 5 ? roof : shade(roof, 0.08));
    }
    rect(ctx, 0, 48, 96, 2, shade(roof, -0.4));
    plaque(ctx, 48, 31, 28);
    rect(ctx, 26, 54, 44, 3, "#2456d6");
    rect(ctx, 26, 57, 44, 1, "#f2c14e");
    windowPane(ctx, 14, 62, 20, 16, lit);
    windowPane(ctx, 62, 62, 20, 16, lit);
    rect(ctx, 66, 80, 12, 9, "#2456d6");
    rect(ctx, 67, 79, 10, 1, "#2456d6");
    rect(ctx, 68, 82, 8, 1, "#0e1e4a");
    rect(ctx, 70, 89, 4, 3, "#2a3550");
    door(ctx, 72, 92, lit, "#3f6fb0");
    rect(ctx, 96, 8, 2, 86, "#8d8478");
    rect(ctx, 95, 6, 4, 3, "#e8c16a");
    rect(ctx, 94, 92, 6, 2, "#6f675d");
    rect(ctx, 4, 92, 88, 2, "#a79d90");
  });
  return {
    ...sprite,
    sign: { x: 48, y: 36, w: 28 },
    windows: [{ x: 24, y: 70 }, { x: 72, y: 70 }],
    chimney: null,
    anchors: { flagTop: { x: 98, y: 10 }, flagLow: { x: 98, y: 58 } },
  };
}

function market(): CivicSprite {
  const H = 92;
  const sprite = make(104, H, (ctx, lit) => {
    ellipse(ctx, 52, H - 3, 48, 2, SHADOW);
    rect(ctx, 10, 40, 4, 48, WOOD);
    rect(ctx, 90, 40, 4, 48, WOOD);
    rect(ctx, 49, 40, 4, 22, WOOD);
    rect(ctx, 10, 50, 84, 34, "#f0e2c2");
    for (let y = 54; y < 84; y += 6) rect(ctx, 10, y, 84, 1, "#dcc9a2");
    for (let i = 0; i < 15; i++) {
      const x = 4 + i * 6.5;
      const col = i % 2 ? "#fbf3e0" : "#2f9a8a";
      rect(ctx, x, 30, 7, 14, col);
      rect(ctx, x + 1, 44, 5, 2, col);
      rect(ctx, x + 2, 46, 3, 1, col);
    }
    rect(ctx, 4, 28, 98, 3, "#1f6a5e");
    rect(ctx, 6, 18, 92, 11, "#7a4f2e");
    rect(ctx, 8, 20, 88, 7, "#ecd7a8");
    rect(ctx, 14, 66, 26, 6, "#8a5a33");
    for (let i = 0; i < 6; i++) disc(ctx, 17 + i * 4, 64, 2, ["#e85d40", "#f2c14e", "#7bc74d", "#e85d40", "#f29a3a", "#7bc74d"][i]);
    rect(ctx, 64, 66, 26, 6, "#8a5a33");
    for (let i = 0; i < 6; i++) disc(ctx, 67 + i * 4, 64, 2, ["#9b7be0", "#e8b64a", "#e85d75", "#fbf3e0", "#9b7be0", "#e8b64a"][i]);
    windowPane(ctx, 16, 74, 18, 8, lit);
    windowPane(ctx, 70, 74, 18, 8, lit);
    door(ctx, 66, 88, lit, "#8a5a33");
    rect(ctx, 6, 88, 92, 2, "#a79d90");
  });
  return { ...sprite, sign: { x: 52, y: 23, w: 40 }, windows: [{ x: 25, y: 78 }, { x: 79, y: 78 }], chimney: null, anchors: { coins: { x: 30, y: 84 } } };
}

function observatory(): CivicSprite {
  const H = 100;
  const sprite = make(104, H, (ctx, lit) => {
    ellipse(ctx, 52, H - 3, 46, 2, SHADOW);
    stones(ctx, 12, 52, 80, 42, "#dfe3ea");
    rect(ctx, 10, 50, 84, 3, "#8a94a6");
    for (let y = 14; y <= 50; y++) {
      const dy = (50 - y) / 36;
      const half = Math.round(38 * Math.sqrt(Math.max(0, 1 - dy * dy)));
      rect(ctx, 52 - half, y, half * 2, 1, y < 30 ? "#c5cbd8" : "#aeb6c6");
      rect(ctx, 52 - half, y, 2, 1, "#8a94a6");
    }
    rect(ctx, 46, 15, 12, 35, "#2a3150");
    for (let y = 18; y < 50; y += 5) rect(ctx, 46, y, 12, 1, "#3a4468");
    rect(ctx, 54, 10, 6, 14, "#5f6a86");
    rect(ctx, 56, 4, 8, 8, "#7a86a6");
    rect(ctx, 62, 2, 4, 6, "#d8b65a");
    windowPane(ctx, 22, 62, 14, 14, lit, "#3f4a66");
    windowPane(ctx, 68, 62, 14, 14, lit, "#3f4a66");
    plaque(ctx, 52, 57, 26, "#e9eef6");
    door(ctx, 72, 94, lit, "#3f4a66");
    rect(ctx, 8, 94, 88, 2, "#a79d90");
  });
  return { ...sprite, sign: { x: 52, y: 62, w: 26 }, windows: [{ x: 29, y: 69 }, { x: 75, y: 69 }], chimney: null, anchors: { lens: { x: 64, y: 4 } } };
}

const MAKERS: Record<CivicId, () => CivicSprite> = {
  clock: clockTower,
  library,
  forge,
  post: postOffice,
  market,
  observatory,
};

export function makeCivic(id: CivicId): CivicSprite {
  const civic = MAKERS[id]();
  rememberThing(civic.day, `civic_${id}`);
  rememberThing(civic.night, `civic_${id}`, {}, true);
  return civic;
}
