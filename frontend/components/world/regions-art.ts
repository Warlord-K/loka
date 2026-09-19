import type { Sprite } from "./art";
import { context, ellipse, makeCanvas, outline, rect, shade } from "./pixels";

const SHADOW = "rgba(20,24,20,0.28)";
const VEILS = ["#7ee0d2", "#c39bff", "#ffd45a", "#9fdcff", "#ff9ab0", "#9bf08b"];

export function makeArch(seed: number): Sprite {
  const c = makeCanvas(64, 46);
  const ctx = context(c);
  const veil = VEILS[seed % VEILS.length];
  ellipse(ctx, 32, 44, 28, 2, SHADOW);
  for (let y = 12; y < 44; y++) rect(ctx, 16, y, 32, 1, y % 3 ? veil : shade(veil, 0.25));
  ctx.globalAlpha = 0.5;
  rect(ctx, 19, 14, 26, 30, "#ffffff");
  ctx.globalAlpha = 1;
  for (const x of [3, 49]) {
    rect(ctx, x, 9, 12, 35, "#8d8a99");
    rect(ctx, x + 1, 9, 3, 35, "#aaa7b6");
    rect(ctx, x + 10, 9, 2, 35, "#6f6c7c");
    for (let y = 15; y < 44; y += 7) rect(ctx, x, y, 12, 1, "#6f6c7c");
    rect(ctx, x - 1, 40, 14, 4, "#6f6c7c");
  }
  for (let x = 2; x < 62; x++) {
    const top = 2 + Math.round(Math.abs(x - 31.5) * 0.16);
    rect(ctx, x, top, 1, 9, x % 6 === 0 ? "#6f6c7c" : "#8d8a99");
    rect(ctx, x, top, 1, 2, "#b9b6c4");
  }
  rect(ctx, 29, 3, 6, 5, veil);
  rect(ctx, 31, 4, 2, 3, "#ffffff");
  return { img: outline(c, [38, 24, 34]), ax: 33, ay: 45 };
}

export function makeWaySign(): Sprite {
  const c = makeCanvas(26, 30);
  const ctx = context(c);
  ellipse(ctx, 13, 28, 5, 1, SHADOW);
  rect(ctx, 12, 6, 2, 22, "#6e4a2e");
  rect(ctx, 2, 4, 20, 7, "#6e4a2e");
  rect(ctx, 3, 5, 18, 5, "#ecd7a8");
  rect(ctx, 21, 6, 3, 3, "#6e4a2e");
  rect(ctx, 21, 7, 2, 1, "#ecd7a8");
  rect(ctx, 4, 13, 18, 6, "#6e4a2e");
  rect(ctx, 5, 14, 16, 4, "#d9c08c");
  rect(ctx, 2, 15, 3, 2, "#6e4a2e");
  return { img: c, ax: 13, ay: 28 };
}

export function makeCactus(r: () => number): Sprite {
  const c = makeCanvas(16, 22);
  const ctx = context(c);
  ellipse(ctx, 8, 20, 5, 1, SHADOW);
  const g = "#4f8f4a";
  const d = "#3b6f39";
  rect(ctx, 6, 3, 4, 18, g);
  rect(ctx, 9, 3, 1, 18, d);
  rect(ctx, 6, 2, 3, 1, g);
  const armY = 7 + Math.floor(r() * 4);
  rect(ctx, 2, armY, 4, 3, g);
  rect(ctx, 2, armY - 4, 3, 5, g);
  rect(ctx, 10, armY + 2, 4, 3, g);
  rect(ctx, 11, armY - 2, 3, 5, d);
  for (let y = 5; y < 19; y += 3) rect(ctx, 7, y, 1, 1, "#d8e6b8");
  if (r() < 0.4) rect(ctx, 7, 1, 2, 2, "#ff9ab0");
  return { img: outline(c, [38, 24, 34]), ax: 8, ay: 20 };
}

export function makeIceRock(r: () => number): Sprite {
  const c = makeCanvas(14, 12);
  const ctx = context(c);
  ellipse(ctx, 7, 10, 6, 1, SHADOW);
  const h = 6 + Math.floor(r() * 3);
  rect(ctx, 2, 10 - h, 10, h, "#bfe3f2");
  rect(ctx, 3, 10 - h, 4, 2, "#ffffff");
  rect(ctx, 9, 10 - h + 1, 3, h - 1, "#8fc3dc");
  return { img: outline(c, [38, 24, 34]), ax: 7, ay: 10 };
}

export function frost(sprite: Sprite): Sprite {
  const src = sprite.img;
  const c = makeCanvas(src.width, src.height);
  const ctx = context(c);
  ctx.drawImage(src, 0, 0);
  const data = ctx.getImageData(0, 0, c.width, c.height);
  const d = data.data;
  const w = c.width;
  for (let y = 1; y < c.height; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      const above = ((y - 1) * w + x) * 4;
      if (d[i + 3] > 200 && d[above + 3] > 200 && (y < 2 || d[((y - 2) * w + x) * 4 + 3] < 40) && d[i + 1] > d[i]) {
        d[i] = 246;
        d[i + 1] = 250;
        d[i + 2] = 255;
      }
    }
  }
  ctx.putImageData(data, 0, 0);
  return { img: c, ax: sprite.ax, ay: sprite.ay };
}

export function makeLantern(): Sprite {
  const c = makeCanvas(10, 26);
  const ctx = context(c);
  ellipse(ctx, 5, 24, 4, 1, SHADOW);
  rect(ctx, 4, 8, 2, 16, "#4b3a2e");
  rect(ctx, 1, 2, 8, 7, "#4b3a2e");
  rect(ctx, 2, 3, 6, 5, "#ffd45a");
  rect(ctx, 3, 4, 2, 2, "#fff4c2");
  rect(ctx, 0, 1, 10, 1, "#2f241c");
  return { img: c, ax: 5, ay: 24 };
}

export function makeCampfire(): Sprite {
  const c = makeCanvas(16, 14);
  const ctx = context(c);
  ellipse(ctx, 8, 12, 7, 1, SHADOW);
  rect(ctx, 2, 10, 12, 2, "#6e4a2e");
  rect(ctx, 4, 9, 8, 1, "#8a5a33");
  rect(ctx, 5, 4, 6, 5, "#f39c34");
  rect(ctx, 6, 2, 4, 4, "#ffd45a");
  rect(ctx, 7, 1, 2, 2, "#fff4c2");
  return { img: c, ax: 8, ay: 12 };
}

export function makeBanner(color: string): Sprite {
  const c = makeCanvas(12, 30);
  const ctx = context(c);
  ellipse(ctx, 3, 28, 3, 1, SHADOW);
  rect(ctx, 2, 1, 2, 27, "#5f4a3a");
  rect(ctx, 4, 2, 7, 12, color);
  rect(ctx, 4, 14, 3, 3, color);
  rect(ctx, 8, 14, 3, 3, color);
  rect(ctx, 6, 5, 3, 3, "#fbf3e0");
  return { img: outline(c, [38, 24, 34]), ax: 3, ay: 28 };
}
