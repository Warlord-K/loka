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

function callsQuestFolderWorkspace(line) {
  if (!/\bworkspaces?\b/i.test(line)) return false;
  if (/\bquest workspaces?\b/i.test(line) || /^Workspace:\s/i.test(line)) return true;
  if (/\bworkspaces?\s+(?:cleanup|storage)\b/i.test(line) || /\bworkspaces?\s+or\s+caches\b/i.test(line)) return true;
  if (/\bworkspaces?\b.{0,40}\b(?:temp(?:orary)?|caches?)\b.{0,40}\b(?:folders?|directories|paths?|storage|cleanup|disk)\b/i.test(line)) return true;
  return [...line.matchAll(/\b(?:quests?|runs?|checkouts?|worktrees?)\b.{0,50}\bworkspaces?\b/gi)].some((match) => {
    const nearby = line.slice(Math.max(0, match.index - 40), match.index + match[0].length + 80);
    return /\b(?:folders?|directories|paths?|storage|cleanup|clean up|delet(?:e|ed|es|ing)|remov(?:e|ed|es|ing)|disk)\b/i.test(nearby);
  });
}

test("the live canvas room is called Workspaces, never by its old working name", () => {
  const found = copyLines()
    .filter(({ line }) => /\bStudio\b/.test(line.replace(/\bMac Studio\b/g, "")))
    .map(({ at }) => at);
  assert.deepEqual(found, [], "Say Workspaces (or a workspace) for the canvas room with your agents.");
});

test("live boards and Academy practice use Workspace in product copy", () => {
  const surfaces = [
    "frontend/components/world/canvas-embed.tsx",
    "frontend/components/world/loka-ui.tsx",
    "frontend/components/world/practice-view.tsx",
    "agent/src/world/practice_tools.rs",
    "agent/src/world/loka_tools.rs",
    "shared/src/features.rs",
    "shared/src/practice.rs",
    "api/src/routes/reports.rs",
    "api/src/routes/voice.rs",
  ];
  const sources = new Map(surfaces.map((path) => [path, shipped(path, readFileSync(join(repo, path), "utf8"))]));
  const oldCopy = /\b(?:practice canvases?|canvas lessons?|opening the canvas|reload the canvas|canvas owner|browser on a canvas|attempts on this canvas|from a canvas|only canvases show|canvas's current coach|canvas width|outside the canvas|strokes? on (?:a )?[^.]* canvas)\b/i;
  const found = [...sources].flatMap(([path, source]) =>
    source
      .split("\n")
      .map((line, i) => ({ at: `${path}:${i + 1}`, line }))
      .filter(({ line }) => oldCopy.test(line))
      .map(({ at, line }) => `${at} ${line.trim().slice(0, 120)}`),
  );
  assert.deepEqual(found, [], "Use workspace for product-facing boards and Academy practice.");
  assert.match(sources.get("frontend/components/world/canvas-embed.tsx"), /Reload the workspace to watch this/);
  assert.match(sources.get("frontend/components/world/loka-ui.tsx"), /Practice workspace/);
  assert.match(sources.get("agent/src/world/practice_tools.rs"), /Grade a practice workspace lesson attempt/);
  assert.match(sources.get("agent/src/world/loka_tools.rs"), /or canvas \(shown as a practice workspace/);
  assert.match(sources.get("frontend/components/world/loka-ui.tsx"), /card\.type === "canvas"/);
  assert.match(sources.get("api/src/routes/reports.rs"), /only workspaces show who is looking/);
  assert.match(sources.get("api/src/routes/voice.rs"), /started this call from a workspace/);
});

test("a quest's own folder is called its quest folder, so it never reads like the Workspaces feature", () => {
  const oldQuestFolderCopy = [
    "The quest workspace is ready.",
    "Closed quests' workspaces are deleted after an hour.",
    "Workspace: /tmp/quest-42",
    "Clear workspaces or caches.",
    "Workspaces and temp folders take disk space.",
  ];
  const productCopy = [
    "Quote this workspace in chat.",
    "Reports and workspaces link to quests and to-dos.",
    "The agent that made this workspace gets your mark.",
    "No workspace mentions that phrase.",
    "Workspaces (shared boards) keep notes together.",
  ];
  assert.deepEqual(oldQuestFolderCopy.filter((line) => !callsQuestFolderWorkspace(line)), []);
  assert.deepEqual(productCopy.filter(callsQuestFolderWorkspace), []);
  const found = copyLines()
    .filter(({ line }) => callsQuestFolderWorkspace(line))
    .map(({ at, line }) => `${at} ${line.trim().slice(0, 100)}`);
  assert.deepEqual(found, [], "Write quest folder (or folder) for a quest's checkout and temp files.");
});

test("the guard reads every agent tool description and how-to file", () => {
  const tools = toolFiles();
  assert.ok(tools.includes("agent/src/world/mcp.rs") && tools.includes("agent/src/world/mcp_control.rs") && tools.includes("agent/src/world/canvas_tools.rs"));
  assert.ok(tools.length >= 8);
});
