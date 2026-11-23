import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(import.meta.url);
const repo = join(dirname(here), "..", "..");
const SKIP_DIRS = new Set(["node_modules", ".next", "target", "binaries", ".git", "build", "DerivedData", "gen", ".claude"]);
const CODE = new Set([".rs", ".ts", ".tsx", ".mjs", ".js", ".swift", ".css"]);
const EM_DASH = String.fromCodePoint(0x2014);
const DIRECTIVE = /^\/\/\s*(@ts-expect-error|@ts-ignore|@ts-nocheck|eslint-disable[\w-]*|swiftlint:\w+)(\s+[\w@/,-]+)*$/;

function* files(dir, extensions) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) yield* files(path, extensions);
    } else if (entry.isFile() && extensions.has(extname(entry.name)) && path !== here) {
      yield path;
    }
  }
}

function linesOf(roots, extensions, skip = () => false) {
  const out = [];
  for (const root of roots) {
    for (const path of files(join(repo, root), extensions)) {
      if (skip(path)) continue;
      readFileSync(path, "utf8")
        .split("\n")
        .forEach((text, i) => out.push({ where: `${relative(repo, path)}:${i + 1}`, path, text }));
    }
  }
  return out;
}

function hasTrailingComment(text) {
  const match = text.match(/\s\/\/(\s|$)/);
  if (!match) return false;
  const before = text.slice(0, match.index);
  const quotes = (before.match(/(?<!\\)"/g) ?? []).length;
  const ticks = (before.match(/`/g) ?? []).length;
  return quotes % 2 === 0 && ticks % 2 === 0;
}

function isComment({ text }) {
  const trimmed = text.trim();
  if (DIRECTIVE.test(trimmed)) return false;
  if (/^(\/\/|\/\*|\*\/)/.test(trimmed)) return true;
  return hasTrailingComment(text) && !DIRECTIVE.test(trimmed.slice(trimmed.indexOf("//")));
}

test("source code carries no comments", () => {
  const lines = linesOf(
    ["agent", "shared", "api", "frontend", "desktop", "ios"],
    CODE,
    (path) => path.endsWith(".generated.ts") || path.endsWith(".d.ts"),
  );
  const found = lines.filter(isComment).map(({ where, text }) => `${where} ${text.trim().slice(0, 70)}`);
  assert.deepEqual(found, [], "Write no comments. Rename or restructure so the code reads on its own.");
});

test("text people or agents read has no em dashes", () => {
  const surfaces = [
    { roots: ["frontend/app", "frontend/components", "frontend/lib"], extensions: new Set([".ts", ".tsx"]), skip: (p) => p.endsWith(".d.ts") },
    { roots: ["ios/App", "ios/Shared", "ios/Watch", "ios/Notify"], extensions: new Set([".swift"]) },
    { roots: ["desktop/src"], extensions: new Set([".ts", ".tsx", ".js", ".html"]) },
    { roots: ["agent/templates", "skills", "docs"], extensions: new Set([".txt", ".md"]) },
  ];
  const found = surfaces
    .flatMap(({ roots, extensions, skip }) => linesOf(roots, extensions, skip))
    .filter(({ text }) => text.includes(EM_DASH))
    .map(({ where }) => where);
  assert.deepEqual(found, [], "Use a hyphen with spaces, a comma or two sentences instead of an em dash.");
});

test("nothing pulses or blinks", () => {
  const found = linesOf(["frontend/app", "frontend/components"], new Set([".tsx", ".ts", ".css"]))
    .filter(({ text }) => /animate-(pulse|ping)\b|@keyframes\s+(pulse|blink|ping)\b/.test(text))
    .map(({ where }) => where);
  assert.deepEqual(found, [], "Live things use Shimmer text, never pulsing or blinking dots.");
});

test("migration numbers are unique", () => {
  const names = readdirSync(join(repo, "migrations")).filter((name) => name.endsWith(".sql"));
  const numbers = names.map((name) => name.match(/^(\d{3})_[a-z0-9_]+\.sql$/)?.[1]);
  assert.ok(!numbers.includes(undefined), "migration files are named NNN_name.sql");
  assert.deepEqual(numbers.filter((n, i) => numbers.indexOf(n) !== i), [], "two migrations share a number");
});
