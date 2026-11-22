import type { LokaLook } from "./types";
import { context, makeCanvas, mirror, mix, outline, shade } from "./pixels";
import { rememberLook } from "./sprites";

export type Dir = "down" | "up" | "left" | "right";
export type CharacterSheet = Record<Dir, HTMLCanvasElement[]>;

const PAD = 4;
const W = 12;

const HEAD_DOWN = [
  "....HHHH....",
  "..HHHHHHHH..",
  ".HHHHHHHHHH.",
  ".HHSSSSSSHH.",
  ".HSSSSSSSSH.",
  ".HSESSSSESH.",
  "..rSSSSSSr..",
  "...sSSSSs...",
];

const HEAD_UP = [
  "....HHHH....",
  "..HHHHHHHH..",
  ".HHHHHHHHHH.",
  ".HHHHHHHHHH.",
  ".HHHHHHHHHH.",
  ".hHHHHHHHHh.",
  "..hHHHHHHh..",
  "...sSSSSs...",
];

const HEAD_SIDE = [
  "....HHHH....",
  "...HHHHHHH..",
  "..HHHHHHHHH.",
  "..SSSHHHHHH.",
  ".SSSSSSHHHH.",
  ".SESSSSHHHH.",
  "..SrSSSShh..",
  "...sSSSs....",
];

const BODY_FRONT = [
  "..CCCCCCCC..",
  ".CCCCCCCCCC.",
  ".CCcCCCCcCC.",
  ".SccCCCCccS.",
];

const BODY_SIDE = [
  "....CCCCC...",
  "...CCCCCCC..",
  "...CCcCCCC..",
  "...ccScccc..",
];

const LEGS_FRONT = {
  idle: ["..PPPPPPPP..", "..PPP..PPP..", "..BBB..BBB..", "............"],
  a: ["..PPPPPPPP..", "..PPP..PPP..", "..BBB..PPP..", ".......BBB.."],
  b: ["..PPPPPPPP..", "..PPP..PPP..", "..PPP..BBB..", "..BBB......."],
};

const LEGS_SIDE = {
  idle: ["...PPPPPP...", "....PPPP....", "...BBBBB....", "............"],
  a: ["...PPPPPP...", "..PP...PP...", ".BBB...BBB..", "............"],
};

const TENTACLES_FRONT = {
  idle: ["..SSSSSSSS..", ".SS.SSSS.SS.", ".S..S..S..S.", "S...s..s...S"],
  a: ["..SSSSSSSS..", ".SS.SSSS.SS.", "S...S..S..S.", "s..s....s..S"],
  b: ["..SSSSSSSS..", ".SS.SSSS.SS.", ".S..S..S...S", "S..s....s..s"],
};

const TENTACLES_SIDE = {
  idle: ["...SSSSSS...", "..S.SSSS.S..", ".S..S..S..S.", "S...s...s..s"],
  a: ["...SSSSSS...", "..S.SSSS.S..", "S...S..S...S", "s..s...s...."],
  b: ["...SSSSSS...", "..S.SSSS.S..", "S...S..S...S", "s..s...s...."],
};

const GHOST_FRONT = {
  idle: [".SSSSSSSSSS.", ".SSSSSSSSSS.", ".S.SS.SS.SS.", "............"],
  a: [".SSSSSSSSSS.", ".SSSSSSSSSS.", ".SS.SS.SS.S.", "............"],
  b: [".SSSSSSSSSS.", ".SSSSSSSSSS.", ".S.SS.SS.SS.", "............"],
};

const GHOST_SIDE = {
  idle: ["...SSSSSSS..", "...SSSSSSSS.", "...S.SS.SS.S", "............"],
  a: ["...SSSSSSS..", "...SSSSSSSS.", "....SS.SS.SS", "............"],
  b: ["...SSSSSSS..", "...SSSSSSSS.", "....SS.SS.SS", "............"],
};

type Step = "idle" | "a" | "b";

type Grid = string[][];

function walkPhase(g: Grid, dir: "down" | "up" | "side"): Step {
  if (dir === "side") return g[13 + PAD].join("") === LEGS_SIDE.a[1] ? "a" : "idle";
  const row = g[14 + PAD].join("");
  if (row === LEGS_FRONT.a[2]) return "a";
  if (row === LEGS_FRONT.b[2]) return "b";
  return "idle";
}

function build(head: string[], body: string[], legs: string[]): Grid {
  const rows = [...Array(PAD).fill(".".repeat(W)), ...head, ...body, ...legs];
  return rows.map((r) => r.split(""));
}

function set(g: Grid, x: number, y: number, c: string): void {
  if (y >= 0 && y < g.length && x >= 0 && x < W) g[y][x] = c;
}

function replaceRow(g: Grid, row: number, from: string[], to: string, x0 = 0, x1 = W): void {
  const y = row + PAD;
  for (let x = x0; x < x1; x++) if (from.includes(g[y][x])) g[y][x] = to;
}

function accessorize(g: Grid, look: LokaLook, dir: "down" | "up" | "side"): void {
  const acc = look.accessories ?? [];
  if (acc.includes("beard")) {
    if (dir === "down") {
      replaceRow(g, 6, ["S", "r"], "H", 2, 10);
      replaceRow(g, 7, ["S", "s"], "H", 3, 9);
    } else if (dir === "side") {
      replaceRow(g, 6, ["S", "r"], "H", 1, 7);
      replaceRow(g, 7, ["S", "s"], "H", 3, 7);
    }
  }
  if (acc.includes("scarf")) {
    replaceRow(g, 7, ["S", "s", "H"], "A");
    replaceRow(g, 8, ["C"], "A");
  }
  if (acc.includes("apron")) {
    if (dir === "down") for (let r = 9; r <= 11; r++) replaceRow(g, r, ["C", "c"], "W", 4, 8);
    if (dir === "side") for (let r = 9; r <= 11; r++) replaceRow(g, r, ["C", "c"], "W", 3, 5);
  }
  if (acc.includes("briefcase")) {
    const x = dir === "down" ? 10 : dir === "up" ? 0 : 4;
    for (const [dx, dy] of [[0, 12], [1, 12], [0, 13], [1, 13]]) set(g, x + dx, dy + PAD, "B");
  }
  if (acc.includes("glasses")) {
    if (dir === "down") {
      for (const x of [2, 4, 5, 6, 7, 9]) set(g, x, 5 + PAD, "G");
    } else if (dir === "side") {
      set(g, 1, 5 + PAD, "G");
      set(g, 3, 5 + PAD, "G");
      set(g, 4, 5 + PAD, "G");
    }
  }
  if (acc.includes("headband")) replaceRow(g, 2, ["H", "h"], "A");
  if (acc.includes("cap")) {
    replaceRow(g, 0, ["H", "h"], "A");
    replaceRow(g, 1, ["H", "h"], "A");
    replaceRow(g, 2, ["H", "h"], "a");
    if (dir === "down") for (let x = 2; x < 10; x++) set(g, x, 3 + PAD, "a");
    if (dir === "side") for (let x = 0; x < 5; x++) set(g, x, 3 + PAD, "a");
  }
  if (acc.includes("beanie")) {
    replaceRow(g, 0, ["H", "h"], "A");
    replaceRow(g, 1, ["H", "h"], "A");
    replaceRow(g, 2, ["H", "h", "S"], "a");
    set(g, 5, PAD - 1, "W");
    set(g, 6, PAD - 1, "W");
  }
  if (acc.includes("bun")) {
    const x = dir === "side" ? 7 : 5;
    for (const [dx, dy] of [[0, -1], [1, -1], [0, -2], [1, -2]]) set(g, x + dx, PAD + dy, "H");
  }
  const ear = (pts: [number, number][], c: string) => {
    for (const [x, y] of pts) set(g, x, PAD + y, c);
  };
  if (acc.includes("ears-pointed")) {
    if (dir === "side") ear([[6, -2], [6, -1], [7, -1]], "H");
    else ear([[2, -2], [2, -1], [3, -1], [3, 0], [9, -2], [9, -1], [8, -1], [8, 0]], "H");
  }
  if (acc.includes("ears-round")) {
    if (dir === "side") ear([[6, -1], [7, -1], [6, 0], [7, 0]], "h");
    else ear([[2, -1], [3, -1], [2, 0], [3, 0], [8, -1], [9, -1], [8, 0], [9, 0]], "h");
  }
  if (acc.includes("ears-long")) {
    if (dir === "side") ear([[6, -3], [6, -2], [6, -1], [7, -1]], "H");
    else ear([[3, -3], [3, -2], [3, -1], [3, 0], [8, -3], [8, -2], [8, -1], [8, 0]], "H");
  }
  if (acc.includes("mask") && dir !== "up") {
    if (dir === "down") {
      replaceRow(g, 5, ["S"], "M", 1, 11);
      replaceRow(g, 4, ["S"], "M", 2, 4);
      replaceRow(g, 4, ["S"], "M", 8, 10);
    } else replaceRow(g, 5, ["S"], "M", 1, 7);
  }
  if (acc.includes("snout") && dir !== "up") {
    if (dir === "down") {
      replaceRow(g, 6, ["S", "r"], "W", 4, 8);
      replaceRow(g, 7, ["S", "s"], "W", 4, 8);
      set(g, 5, 6 + PAD, "E");
      set(g, 6, 6 + PAD, "E");
    } else {
      replaceRow(g, 6, ["S", "r", "."], "W", 0, 3);
      set(g, 0, 6 + PAD, "E");
    }
  }
  if (acc.includes("beak") && dir !== "up") {
    if (dir === "down") {
      set(g, 5, 6 + PAD, "K");
      set(g, 6, 6 + PAD, "K");
      set(g, 5, 7 + PAD, "O");
      set(g, 6, 7 + PAD, "O");
    } else {
      set(g, 0, 6 + PAD, "K");
      set(g, 1, 6 + PAD, "K");
      set(g, 1, 7 + PAD, "O");
    }
  }
  if (acc.includes("whiskers") && dir !== "up") {
    if (dir === "down") for (const x of [0, 1, 10, 11]) set(g, x, 6 + PAD, "G");
    else for (const x of [0, 1]) set(g, x, 7 + PAD, "G");
  }
  if (acc.includes("horns")) {
    replaceRow(g, 0, ["H", "h"], "Y");
    replaceRow(g, 1, ["H", "h"], "Y");
    replaceRow(g, 2, ["H", "h"], "V");
    if (dir === "side") ear([[8, 0], [9, -1], [9, -2]], "W");
    else ear([[1, 1], [1, 0], [0, -1], [10, 1], [10, 0], [11, -1]], "W");
  }
  if (acc.includes("hardhat")) {
    replaceRow(g, 0, ["H", "h"], "U");
    replaceRow(g, 1, ["H", "h"], "U");
    for (let x = dir === "side" ? 0 : 1; x < (dir === "side" ? 10 : 11); x++) set(g, x, 2 + PAD, "u");
  }
  if (acc.includes("wizard")) {
    replaceRow(g, 0, ["H", "h"], "A");
    ear([[5, -4], [6, -4], [4, -3], [5, -3], [6, -3], [7, -3], [4, -2], [5, -2], [6, -2], [7, -2], [3, -1], [4, -1], [5, -1], [6, -1], [7, -1], [8, -1]], "A");
    set(g, 5, PAD - 2, "y");
    for (let x = 1; x < 11; x++) set(g, x, 1 + PAD, "a");
  }
  if (acc.includes("sword")) {
    if (dir === "side") {
      for (let r = 6; r <= 10; r++) set(g, 4, r + PAD, "X");
      set(g, 3, 11 + PAD, "V");
      set(g, 4, 11 + PAD, "V");
      set(g, 5, 11 + PAD, "V");
    } else if (dir === "down") {
      for (let r = 6; r <= 10; r++) set(g, 11, r + PAD, "X");
      set(g, 10, 11 + PAD, "V");
      set(g, 11, 11 + PAD, "V");
    } else {
      for (let r = 6; r <= 10; r++) set(g, 0, r + PAD, "X");
      set(g, 0, 11 + PAD, "V");
      set(g, 1, 11 + PAD, "V");
    }
  }
  if (acc.includes("lantern")) {
    if (dir === "side") {
      set(g, 4, 12 + PAD, "y");
      set(g, 4, 13 + PAD, "V");
    } else if (dir === "down") {
      set(g, 11, 12 + PAD, "y");
      set(g, 11, 13 + PAD, "V");
    }
  }
  if (acc.includes("cloak")) {
    const x0 = dir === "up" ? 1 : dir === "side" ? 5 : 1;
    const x1 = dir === "up" ? 11 : dir === "side" ? 9 : 3;
    for (let r = 8; r <= 12; r++) {
      replaceRow(g, r, ["C", "c", "P"], r % 2 ? "b" : "Q", x0, x1);
      if (dir === "down") replaceRow(g, r, ["C", "c", "P"], r % 2 ? "b" : "Q", 9, 11);
    }
    if (dir === "up") set(g, 6, 10 + PAD, "y");
  }
  if (acc.includes("mortarboard")) {
    replaceRow(g, 0, ["H", "h"], "M");
    replaceRow(g, 1, ["H", "h"], "M");
    for (let x = dir === "side" ? 0 : 1; x < (dir === "side" ? 10 : 11); x++) set(g, x, PAD - 1, "M");
    set(g, dir === "side" ? 1 : 10, PAD, "y");
    set(g, dir === "side" ? 1 : 10, PAD + 1, "y");
  }
  if (acc.includes("quill")) {
    const x = dir === "side" ? 4 : dir === "down" ? 11 : 0;
    set(g, x, 7 + PAD, "W");
    for (let r = 8; r <= 10; r++) set(g, x, r + PAD, "y");
    set(g, x, 11 + PAD, "V");
  }
  const step = walkPhase(g, dir);
  const rows = (lines: string[], from: number) => {
    lines.forEach((line, i) => {
      for (let x = 0; x < W; x++) set(g, x, from + i + PAD, line[x]);
    });
  };
  if (acc.includes("ghost-tail")) {
    for (let r = 8; r <= 11; r++) {
      replaceRow(g, r, ["C"], "S");
      replaceRow(g, r, ["c"], "s");
    }
    const tails = dir === "side" ? GHOST_SIDE : GHOST_FRONT;
    rows(tails[step], 12);
  }
  if (acc.includes("tentacles")) {
    const tails = dir === "side" ? TENTACLES_SIDE : TENTACLES_FRONT;
    rows(tails[step], 12);
  }
  if (acc.includes("stripes")) for (const r of [9, 11]) replaceRow(g, r, ["C", "c"], "M");
  if (acc.includes("belly") && dir === "down") for (let r = 8; r <= 11; r++) replaceRow(g, r, ["C", "c"], "W", 4, 8);
  if (acc.includes("claws")) {
    if (dir === "side") ear([[1, 10], [2, 10], [1, 12], [2, 12], [2, 11]], "S");
    else ear([[0, 10], [0, 12], [1, 12], [11, 10], [11, 12], [10, 12]], "S");
  }
  if (acc.includes("wings") || acc.includes("bug-wings")) {
    const c = acc.includes("wings") ? "h" : "X";
    if (dir === "side") ear([[9, 8], [10, 7], [11, 6], [10, 8], [11, 7], [10, 9]], c);
    else ear([[0, 7], [1, 7], [0, 8], [1, 8], [0, 9], [11, 7], [10, 7], [11, 8], [10, 8], [11, 9]], c);
  }
  if (acc.includes("spikes")) {
    if (dir === "side") {
      ear([[6, -1], [8, 0], [10, 1], [11, 3]], "h");
      ear([[10, 11], [10, 12], [11, 12]], "S");
    } else ear([[4, -1], [5, -2], [5, -1], [6, -1], [6, -2], [7, -1]], "h");
  }
  if (acc.includes("fin")) {
    if (dir === "side") ear([[5, -1], [6, -1], [7, -1], [6, -2], [7, -2], [7, -3]], "h");
    else ear([[5, -1], [6, -1], [5, -2], [6, -2], [6, -3]], "h");
  }
  if (acc.includes("teeth") && dir !== "up") {
    if (dir === "side") {
      ear([[1, 6], [2, 6]], "W");
      set(g, 3, 6 + PAD, "E");
    } else for (let x = 3; x <= 8; x++) set(g, x, 6 + PAD, x === 4 || x === 7 ? "E" : "W");
  }
  if (acc.includes("frog-eyes")) {
    if (dir === "side") {
      ear([[3, -1], [4, -1], [3, -2], [4, -2]], "W");
      set(g, 3, PAD - 1, "E");
      ear([[1, 6], [2, 6]], "s");
    } else {
      const c = dir === "up" ? "S" : "W";
      ear([[2, -1], [3, -1], [2, 0], [3, 0], [8, -1], [9, -1], [8, 0], [9, 0]], c);
      if (dir === "down") {
        set(g, 3, PAD, "E");
        set(g, 8, PAD, "E");
        replaceRow(g, 6, ["S", "r"], "s", 3, 9);
      }
    }
  }
  if (acc.includes("eyestalks")) {
    if (dir === "side") {
      ear([[3, 0], [3, -1]], "S");
      set(g, 2, PAD - 2, "E");
      set(g, 3, PAD - 2, "W");
    } else {
      ear([[3, 0], [3, -1], [8, 0], [8, -1]], "S");
      ear([[2, -2], [9, -2]], "W");
      ear([[3, -2], [8, -2]], dir === "up" ? "W" : "E");
    }
  }
  if (acc.includes("gills")) {
    if (dir === "side") ear([[11, 2], [11, 4], [10, 6]], "R");
    else ear([[0, 2], [0, 4], [1, 6], [11, 2], [11, 4], [10, 6]], "R");
  }
  if (acc.includes("visor") && dir !== "up") {
    if (dir === "side") {
      replaceRow(g, 5, ["S", "E"], "M", 1, 7);
      set(g, 2, 5 + PAD, "Z");
    } else {
      replaceRow(g, 5, ["S", "E"], "M", 2, 10);
      set(g, 3, 5 + PAD, "Z");
      set(g, 8, 5 + PAD, "Z");
    }
  }
  if (acc.includes("antenna")) {
    const x = dir === "side" ? 6 : 5;
    ear([[x, -1], [x, -2]], "V");
    set(g, x, PAD - 3, "y");
  }
  if (acc.includes("antennae")) {
    if (dir === "side") ear([[5, -1], [4, -2], [3, -3], [7, -1], [8, -2], [9, -3]], "M");
    else ear([[4, -1], [3, -2], [2, -3], [7, -1], [8, -2], [9, -3]], "M");
  }
  if (acc.includes("cape")) {
    if (dir === "down") {
      for (let r = 9; r <= 12; r++) {
        replaceRow(g, r, ["C", "c", "P", "."], "a", 1, 2);
        replaceRow(g, r, ["C", "c", "P", "."], "a", 10, 11);
      }
      for (const r of [11, 12]) {
        replaceRow(g, r, ["."], "a", 0, 1);
        replaceRow(g, r, ["."], "a", 11, 12);
      }
    } else if (dir === "up") {
      for (let r = 8; r <= 13; r++) {
        replaceRow(g, r, ["C", "c", "P", "S", "."], "A", 1, 11);
        replaceRow(g, r, ["A"], "a", 3, 4);
        replaceRow(g, r, ["A"], "a", 8, 9);
      }
    } else for (let r = 8; r <= 13; r++) replaceRow(g, r, ["C", "c", "P", "."], r % 2 ? "a" : "A", 8, 11);
  }
  if (acc.includes("headphones")) {
    if (dir === "side") {
      ear([[7, 3], [7, 2], [7, 1], [7, 0], [6, -1], [5, -1]], "M");
      ear([[7, 4], [8, 4], [7, 5], [8, 5]], "A");
    } else {
      ear([[1, 3], [1, 2], [2, 1], [3, 0], [4, -1], [5, -1], [6, -1], [7, -1], [8, 0], [9, 1], [10, 2], [10, 3]], "M");
      ear([[0, 4], [0, 5], [1, 4], [1, 5], [10, 4], [10, 5], [11, 4], [11, 5]], "A");
    }
  }
  if (acc.includes("horn")) {
    if (dir === "side") ear([[3, -1], [2, -1], [2, -2], [1, -3]], "U");
    else ear([[5, -1], [6, -1], [5, -2], [6, -2], [6, -3]], "U");
    if (dir === "side") set(g, 2, PAD - 1, "u");
    else set(g, 6, PAD - 2, "u");
  }
  if (acc.includes("cactus")) {
    for (let r = 0; r <= 7; r++) {
      replaceRow(g, r, ["H"], "S");
      replaceRow(g, r, ["h"], "s");
    }
    for (let r = 8; r <= 11; r++) {
      replaceRow(g, r, ["C"], "S");
      replaceRow(g, r, ["c"], "s");
    }
    for (let r = 12; r <= 15; r++) replaceRow(g, r, ["P", "B", "S", "s"], ".");
    const x0 = dir === "side" ? 3 : 2;
    const x1 = dir === "side" ? 9 : 10;
    for (let x = x0 - 1; x < x1 + 1; x++) set(g, x, 12 + PAD, "K");
    for (let r = 13; r <= 14; r++) for (let x = x0; x < x1; x++) set(g, x, r + PAD, "O");
    if (dir === "side") ear([[2, 8], [1, 8], [1, 7], [1, 6]], "S");
    else ear([[0, 8], [0, 7], [0, 6], [1, 9], [11, 8], [11, 7], [11, 6], [10, 9]], "S");
    ear([[3, 1], [8, 2], [4, 9], [8, 10]], "W");
    ear([[5, -1], [6, -1]], "R");
    set(g, 5, PAD - 2, "y");
  }
  if (acc.includes("mushroom-cap")) {
    for (let x = 3; x < 9; x++) set(g, x, PAD - 2, "H");
    for (let x = 1; x < 11; x++) set(g, x, PAD - 1, "H");
    for (const r of [0, 1]) for (let x = 0; x < W; x++) set(g, x, r + PAD, "H");
    for (let x = 1; x < 11; x++) set(g, x, 2 + PAD, "h");
    if (dir !== "side") ear([[4, -1], [8, 0], [2, 1], [6, 1]], "W");
    else ear([[3, -1], [7, 0], [9, 1], [5, 1]], "W");
  }
  if (acc.includes("crown")) {
    replaceRow(g, 0, ["H", "h", "."], "U", 3, 9);
    ear([[3, -1], [5, -1], [6, -1], [8, -1]], "U");
    if (dir !== "up") ear([[5, 0], [6, 0]], "R");
  }
  if (acc.includes("feather")) {
    const pts: [number, number, string][] = [
      [9, 4, "k"], [9, 3, "k"], [9, 2, "g"], [10, 2, "g"], [10, 3, "g"],
      [8, 1, "g"], [9, 1, "y"], [10, 1, "b"], [11, 1, "g"], [9, 0, "g"], [10, 0, "g"],
    ];
    for (const [x, y, c] of pts) set(g, x, y, c);
  }
}

function palette(look: LokaLook): Record<string, string> {
  const accent = look.accent ?? look.shirt;
  return {
    H: look.hair,
    h: shade(look.hair, -0.25),
    S: look.skin,
    s: shade(look.skin, -0.16),
    r: mix(look.skin, "#ff7a7a", 0.32),
    E: "#2a1d2e",
    C: look.shirt,
    c: shade(look.shirt, -0.22),
    P: look.pants,
    B: "#3a2a26",
    A: accent,
    a: shade(accent, -0.25),
    W: "#f6f1e7",
    G: "#2a1d2e",
    g: "#1f9d6b",
    k: "#155e4a",
    b: "#2456d6",
    y: "#e8b64a",
    M: "#2b2730",
    K: "#f39c34",
    O: "#c46a1c",
    Y: "#a7b0b8",
    V: "#5f6b75",
    U: "#f2c230",
    u: "#c99a1a",
    X: "#e6edf3",
    Q: "#1b2a5e",
    Z: "#5ee0e6",
    R: "#e8508a",
  };
}

function paint(g: Grid, pal: Record<string, string>): HTMLCanvasElement {
  const c = makeCanvas(W, g.length);
  const ctx = context(c);
  for (let y = 0; y < g.length; y++) {
    for (let x = 0; x < W; x++) {
      const ch = g[y][x];
      if (ch === ".") continue;
      ctx.fillStyle = pal[ch] ?? "#ff00ff";
      ctx.fillRect(x, y, 1, 1);
    }
  }
  return outline(c, [38, 24, 34]);
}

function swingArms(body: string[], side: "left" | "right"): string[] {
  const rows = body.map((r) => r.split(""));
  const x = side === "left" ? 1 : 10;
  rows[3][x] = "c";
  rows[2][x] = "S";
  return rows.map((r) => r.join(""));
}

function frame(look: LokaLook, dir: "down" | "up" | "side", legs: string[], body: string[]): HTMLCanvasElement {
  const head = dir === "down" ? HEAD_DOWN : dir === "up" ? HEAD_UP : HEAD_SIDE;
  const g = build(head, body, legs);
  accessorize(g, look, dir);
  return paint(g, palette(look));
}

export function buildSheet(look: LokaLook): CharacterSheet {
  const front = (legs: string[], body: string[], d: "down" | "up") => frame(look, d, legs, body);
  const down = [
    front(LEGS_FRONT.idle, BODY_FRONT, "down"),
    front(LEGS_FRONT.a, swingArms(BODY_FRONT, "left"), "down"),
    front(LEGS_FRONT.idle, BODY_FRONT, "down"),
    front(LEGS_FRONT.b, swingArms(BODY_FRONT, "right"), "down"),
  ];
  const up = [
    front(LEGS_FRONT.idle, BODY_FRONT, "up"),
    front(LEGS_FRONT.a, swingArms(BODY_FRONT, "right"), "up"),
    front(LEGS_FRONT.idle, BODY_FRONT, "up"),
    front(LEGS_FRONT.b, swingArms(BODY_FRONT, "left"), "up"),
  ];
  const sideIdle = frame(look, "side", LEGS_SIDE.idle, BODY_SIDE);
  const sideStep = frame(look, "side", LEGS_SIDE.a, BODY_SIDE);
  const left = [sideIdle, sideStep, sideIdle, sideStep];
  const right = left.map(mirror);
  const sheet = { down, up, left, right };
  rememberLook(sheet, look);
  return sheet;
}

export function portraitUrl(look: LokaLook, scale = 6): string {
  const src = frame(look, "down", LEGS_FRONT.idle, BODY_FRONT);
  const crop = 17;
  const out = makeCanvas(src.width * scale, crop * scale);
  const ctx = context(out);
  ctx.drawImage(src, 0, 0, src.width, crop, 0, 0, src.width * scale, crop * scale);
  return out.toDataURL("image/png");
}
