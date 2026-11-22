import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(import.meta.url);
const repo = join(dirname(here), "..", "..");
const ROOTS = ["frontend", "desktop", "ios", "agent", "shared", "api", "docs", "skills"];
const SKIP_DIRS = new Set(["node_modules", ".next", "target", "binaries", ".git", "build", "DerivedData"]);
const TEXT = new Set([".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".css", ".swift", ".rs", ".txt", ".md", ".mdx", ".html", ".svg", ".py", ".yaml", ".yml", ".plist", ".sh"]);
const SPARKLE_EMOJI = String.fromCodePoint(0x2728);
const BANNED = [
  { name: "sparkle icon", pattern: new RegExp(["spar", "kle"].join(""), "i") },
  { name: "sparkles emoji", pattern: new RegExp(SPARKLE_EMOJI, "u") },
  { name: "magic wand with sparkles", pattern: new RegExp(["Magic", "Wand"].join(""), "") },
  { name: "SF Symbol wand with stars", pattern: new RegExp(["wand", "and", "stars"].join("\\."), "") },
];

function* files(dir) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) yield* files(path);
    } else if (entry.isFile() && TEXT.has(extname(entry.name)) && path !== here) {
      yield path;
    }
  }
}

test("no sparkles icon or emoji anywhere in the app, prompts or docs", () => {
  const found = [];
  for (const root of ROOTS) {
    for (const path of files(join(repo, root))) {
      const lines = readFileSync(path, "utf8").split("\n");
      lines.forEach((line, i) => {
        for (const { name, pattern } of BANNED) {
          if (pattern.test(line)) found.push(`${relative(repo, path)}:${i + 1} ${name}`);
        }
      });
    }
  }
  assert.deepEqual(found, [], "Pick an icon from MeaningIcon (frontend/lib/meaning-icons.ts) or MeaningSymbol (ios/App/MeaningSymbol.swift) that says what the thing means.");
});
