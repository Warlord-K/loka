import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(import.meta.url);
const repo = join(dirname(here), "..", "..");
const SKIP_DIRS = new Set(["node_modules", ".next", "target", "build", "DerivedData", "fixtures"]);
const TEXT = new Set([".ts", ".tsx", ".swift", ".md", ".txt", ".rs"]);
const COPY_DIRS = ["frontend/components", "frontend/app", "frontend/lib", "ios/App", "ios/Shared", "skills", "agent/templates"];
const COPY_FILES = ["shared/src/features.rs", "shared/src/cleanup.rs", "shared/src/readiness.rs", "shared/src/onboarding.rs", "agent/src/world/prompts.rs"];
const TOOL_FILE = /^(mcp.*|.*_tools|tool_catalog)\.rs$/;
const toolFiles = () => readdirSync(join(repo, "agent/src/world")).filter((name) => TOOL_FILE.test(name)).map((name) => `agent/src/world/${name}`);

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
    } else if (entry.isFile() && TEXT.has(extname(entry.name)) && !entry.name.includes(".test.") && path !== here) {
      yield path;
    }
  }
}

function shipped(path, text) {
  if (extname(path) !== ".rs") return text;
  const tests = text.indexOf("\n#[cfg(test)]");
  return tests < 0 ? text : text.slice(0, tests);
}

function copyLines() {
  const paths = [...COPY_DIRS.flatMap((dir) => [...files(join(repo, dir))]), ...[...COPY_FILES, ...toolFiles()].map((file) => join(repo, file))];
  return paths.flatMap((path) =>
    shipped(path, readFileSync(path, "utf8"))
      .split("\n")
      .map((line, i) => ({ at: `${relative(repo, path)}:${i + 1}`, line })),
  );
}

test("the live canvas room is called Workspaces, never by its old working name", () => {
  const found = copyLines()
    .filter(({ line }) => /\bStudio\b/.test(line.replace(/\bMac Studio\b/g, "")))
    .map(({ at }) => at);
  assert.deepEqual(found, [], "Say Workspaces (or a workspace) for the canvas room with your agents.");
});

test("a quest's own folder is called its quest folder, so it never reads like the Workspaces feature", () => {
  const OLD = [/\bquest workspaces?\b/i, /\b(this|own|same|saved|no) workspace\b/i, /^Workspace: /, /\bworkspaces \(/i, /\bworkspaces cleanup\b/i, /\bworkspaces (and|or) temp folders\b/i, /\bclosed quests?'? workspaces\b/i, /"workspaces or caches"/];
  const found = copyLines()
    .filter(({ line }) => OLD.some((pattern) => pattern.test(line)))
    .map(({ at, line }) => `${at} ${line.trim().slice(0, 100)}`);
  assert.deepEqual(found, [], "Write quest folder (or folder) for a quest's checkout and temp files.");
});

test("the guard reads every agent tool description and how-to file", () => {
  const tools = toolFiles();
  assert.ok(tools.includes("agent/src/world/mcp.rs") && tools.includes("agent/src/world/mcp_control.rs") && tools.includes("agent/src/world/canvas_tools.rs"));
  assert.ok(tools.length >= 8);
});
