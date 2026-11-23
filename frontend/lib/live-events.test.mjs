import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const read = (path) => readFileSync(join(root, path), "utf8");

function rustConst(file, name) {
  const found = read(file).match(new RegExp(`pub const ${name}: &str = "([^"]+)";`));
  assert.ok(found, `${name} in ${file}`);
  return found[1];
}

function rustKinds() {
  const list = read("shared/src/signals.rs").match(/pub const KINDS: \[&str; \d+\] = \[([^\]]+)\];/);
  assert.ok(list, "KINDS in shared/src/signals.rs");
  const consts = { "crate::typing::EVENT": rustConst("shared/src/typing.rs", "EVENT"), "crate::canvas::PRESENCE_EVENT": rustConst("shared/src/canvas.rs", "PRESENCE_EVENT") };
  return list[1].split(",").map((part) => part.trim()).filter(Boolean).map((part) => consts[part] ?? JSON.parse(part));
}

function listed(file, pattern) {
  const found = read(file).match(pattern);
  assert.ok(found, `live event list in ${file}`);
  return [...found[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

test("web and server agree on which events are live only", () => {
  const server = rustKinds().sort();
  assert.deepEqual(listed("frontend/lib/world.ts", /LIVE_EVENTS: ReadonlySet<string> = new Set\(\[([^\]]+)\]\)/).sort(), server);
  assert.ok(server.includes("agent.typing") && server.includes("presence.canvas"));
});

const TEMPLATE_HEADINGS = ["Quest folder:", "Workspace:", "Your brief:", "Focus especially on", "Address every finding", "[follow-up from", "pulled you aside", "Task: "];
const SOURCES = ["frontend/components", "frontend/app", "frontend/lib"];

function files(dir) {
  return readdirSync(join(root, dir), { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return entry.name === "node_modules" ? [] : files(path);
    return [".ts", ".tsx", ".swift"].includes(extname(entry.name)) && !entry.name.includes(".test.") ? [path] : [];
  });
}

test("no client reads what was asked out of a run's prompt text", () => {
  const offenders = SOURCES.flatMap(files).flatMap((path) => {
    const text = read(path);
    return TEMPLATE_HEADINGS.filter((heading) => text.includes(heading) && /prompt/i.test(text.slice(Math.max(0, text.indexOf(heading) - 300), text.indexOf(heading) + 300))).map((heading) => `${relative(root, join(root, path))}: ${heading}`);
  });
  assert.deepEqual(offenders, []);
});

function rustSources(dir) {
  return readdirSync(join(root, dir), { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return rustSources(path);
    return extname(entry.name) === ".rs" ? [path] : [];
  });
}

test("runs are copied only through clone_run, which always carries what was asked", () => {
  const copies = ["shared/src", "agent/src", "api/src"].flatMap(rustSources).flatMap((path) => {
    const text = read(path);
    const shipped = text.replace(/\n#\[cfg\(test\)\]\n(pub )?mod [\s\S]*?\n}\n/g, (block) => "\n".repeat(block.split("\n").length - 1));
    return [...shipped.matchAll(/INSERT INTO session_runs[^"]*?FROM session_runs/g)].map((m) => `${path}:${shipped.slice(0, m.index).split("\n").length}`);
  });
  assert.equal(copies.length, 1, `only clone_run may copy a run: ${copies.join(", ")}`);
  assert.match(copies[0], /^shared\/src\/world\.rs:/);
  const clone = read("shared/src/world.rs").match(/pub async fn clone_run[\s\S]*?\n}\n/);
  assert.ok(clone && /request\)/.test(clone[0]) && /ELSE request END/.test(clone[0]), "clone_run copies request");
});

test("agent tools and voice calls read activity only through agent_feed, which drops quiet notes", () => {
  const readers = [...rustSources("agent/src/world"), "api/src/routes/voice.rs"].flatMap((path) => {
    const shipped = read(path).replace(/\n#\[cfg\(test\)\]\n(pub )?mod [\s\S]*?\n}\n/g, "\n");
    return /\.recent_events(_in|_of)?\(/.test(shipped) ? [path] : [];
  });
  assert.deepEqual(readers, []);
  assert.match(read("shared/src/world.rs"), /pub async fn agent_feed[\s\S]*?payload->>'quiet', ''\) <> 'true'/);
});
