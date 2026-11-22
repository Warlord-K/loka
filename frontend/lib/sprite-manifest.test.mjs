import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { accessoryView, bodyFor, mirrored, parseManifest, placed, snapScale, viewOf, walkFrame, withArtOnly, wornLayer } from "./sprite-manifest.ts";
import { canonicalBody, lookFor, speciesFor } from "./looks.ts";

const canonical = canonicalBody;
const innate = (body) => speciesFor(body)?.features ?? [];
const shipped = parseManifest(JSON.parse(readFileSync(new URL("../public/sprites/style-a/manifest.json", import.meta.url), "utf8")));
const agent = (look, persona = "") => ({ slug: "test-agent", persona, look });

const minimal = (extra = {}) => ({
  version: 1,
  atlas: "atlas.png",
  bodies: {
    pip: {
      species: ["person"],
      size: [64, 120],
      anchor: [32, 118],
      features: ["bun"],
      views: {
        front: { slots: { head: [32, 4] }, walk: [[0, 0], [64, 0]] },
        side: { walk: [{ at: [0, 120], slots: { head: [30, 6] } }] },
      },
      swaps: { "#4B837D": "hair", "#335d64": ["hair", -0.3] },
    },
    fox: {
      species: ["fox"],
      size: [64, 120],
      anchor: [32, 118],
      features: ["ears-pointed", "snout"],
      views: { front: { walk: [[0, 240]] }, side: { walk: [[64, 240]] }, back: { walk: [[128, 240]] } },
    },
  },
  accessories: { crown: { slot: "head", size: [36, 24], pivot: [18, 20], views: { front: [0, 360] } } },
  things: { shop: { anchor: [8, 190], frames: [[0, 400, 208, 190]] } },
  ...extra,
});

test("snap rule B keeps device pixels per art pixel on a multiple of 4, ties up", () => {
  const cases = [
    [1440, 900, 1, 4],
    [1440, 900, 2, 8],
    [1280, 720, 2, 8],
    [1100, 700, 2, 8],
    [1919, 1080, 1, 4],
    [1920, 1080, 1, 4],
    [1536, 864, 1.25, 4],
    [1700, 1000, 1, 4],
    [1920, 1152, 1, 8],
    [2560, 1440, 2, 16],
    [1600, 1000, 2, 12],
    [1500, 1000, 2, 8],
    [1760, 1100, 2, 12],
    [390, 844, 3, 4],
    [430, 932, 3, 4],
    [390, 844, 2, 4],
    [1280, 800, 1, 4],
    [2560, 1440, 1, 8],
    [640, 400, 1, 4],
    [5000, 3000, 4, 32],
    [1440, 900, 0, 4],
  ];
  for (const [w, h, dpr, want] of cases) assert.equal(snapScale(w, h, dpr), want, `${w}x${h}@${dpr}`);
});

test("a dpr 3 phone and a dpr 1 laptop see about as many tiles as the classic art", () => {
  const classic = (w, h, dpr) => Math.max(1, Math.round(Math.max(2, Math.floor(Math.min(w / 320, h / 192))) * Math.min(dpr, 2)));
  for (const [w, h, dpr] of [[390, 844, 3], [430, 932, 3], [1440, 900, 1], [1280, 800, 1]]) {
    const ratio = snapScale(w, h, dpr) / classic(w, h, dpr);
    assert.ok(ratio >= 0.67 && ratio <= 1.33, `${w}x${h}@${dpr}: ${ratio}`);
    assert.equal(snapScale(w, h, dpr) % 4, 0);
  }
});

test("snapped zoom stays within 0.8x to 1.33x of the classic scale, dpr capped at 2, once it is 3 or more", () => {
  for (let w = 960; w <= 3840; w += 37) {
    for (const dpr of [1, 1.25, 1.5, 2, 3]) {
      const want = Math.round(Math.max(2, Math.floor(Math.min(w / 320, (w * 0.6) / 192))) * Math.min(dpr, 2));
      if (want < 3) continue;
      const zoom = snapScale(w, w * 0.6, dpr) / want;
      assert.ok(zoom >= 0.79 && zoom <= 1.34, `${w}@${dpr}: ${zoom}`);
    }
  }
});

test("manifest parses with defaults and lowercased colours", () => {
  const m = parseManifest(minimal());
  assert.equal(m.bodies.pip.density, 4);
  assert.equal(m.bodies.pip.sideFaces, "left");
  assert.deepEqual(m.bodies.pip.views.front.idle, [{ at: [0, 0], slots: { head: [32, 4] }, layers: {} }]);
  assert.deepEqual(m.bodies.pip.views.side.walk[0].slots, { head: [30, 6] });
  assert.deepEqual(m.bodies.pip.swaps["#4b837d"], { role: "hair", shade: 0 });
  assert.deepEqual(m.bodies.pip.swaps["#335d64"], { role: "hair", shade: -0.3 });
  assert.equal(m.things.shop.density, 2);
  assert.equal(m.things.shop.sign, null);
  assert.deepEqual(m.things.shop.masks, {});
  assert.deepEqual(m.accessories.crown.sizes[0].front, { at: [0, 360], size: [36, 24], pivot: [18, 20], behind: false });
  assert.equal(m.accessories.crown.atlas, null);
  assert.deepEqual(m.sources, {});
});

test("manifest errors name the bad field", () => {
  const bad = [
    [{ version: 2 }, /version/],
    [{ atlas: "../x.png" }, /atlas/],
    [{ bodies: { a: { ...minimal().bodies.pip, density: 3 } } }, /bodies\.a\.density/],
    [{ bodies: { a: { ...minimal().bodies.pip, anchor: [99, 4] } } }, /bodies\.a\.anchor/],
    [{ bodies: { a: { ...minimal().bodies.pip, views: { front: { walk: [] }, side: { walk: [[0, 0]] } } } } }, /front\.walk/],
    [{ bodies: { a: { ...minimal().bodies.pip, views: { front: { walk: [[0, 0]] } } } } }, /views\.side/],
    [{ bodies: { a: { ...minimal().bodies.pip, swaps: { red: "hair" } } } }, /swaps key red/],
    [{ bodies: { a: { ...minimal().bodies.pip, swaps: { "#ff0000": ["hair", 3] } } } }, /swaps/],
    [{ bodies: { a: { ...minimal().bodies.pip, sideFaces: "up" } } }, /sideFaces/],
    [{ things: { b: { anchor: [0, 0], frames: [[0, 0, 0, 4]] } } }, /things\.b\.frames\[0\]/],
    [{ things: { b: { anchor: [0, 0], frames: [[0, 0, 4, 4]], night: [[0, 0, 4, 4], [0, 0, 4, 4]] } } }, /night/],
  ];
  for (const [patch, re] of bad) assert.throws(() => parseManifest(minimal(patch)), re);
});

test("bodies are picked by species alias only when their baked features match the look and every other accessory has art", () => {
  const m = parseManifest(minimal());
  assert.equal(bodyFor(m, { ...lookFor(agent({ species: "person" })), accessories: ["bun"] }, canonical), "pip");
  assert.equal(bodyFor(m, { ...lookFor(agent({ species: "person" })), accessories: ["bun", "crown"] }, canonical), "pip");
  assert.equal(bodyFor(m, { ...lookFor(agent({ species: "person" })), accessories: ["crown"] }, canonical), null);
  assert.equal(bodyFor(m, { ...lookFor(agent({ species: "person" })), accessories: ["beard"] }, canonical), null);
  assert.equal(bodyFor(m, lookFor(agent({ species: "fox" })), canonical), "fox");
  assert.equal(bodyFor(m, { ...lookFor(agent({ species: "fox" })), accessories: ["ears-pointed", "snout", "crown"] }, canonical), "fox");
  assert.equal(bodyFor(m, lookFor(agent({ species: "octopus" })), canonical), null);
  assert.equal(bodyFor(m, { skin: "#fff", hair: "#000", shirt: "#111", pants: "#222", accessories: [] }, canonical), null);
  assert.equal(bodyFor(m, { ...lookFor(agent({ species: "fox" })), accessories: ["snout"] }, canonical), null);
});

test("one shared base and accessory anchors come only from the manifest", () => {
  const m = parseManifest(minimal({
    accessories: {
      crown: { slot: "head", size: [20, 14], pivot: [10, 12], views: { front: [0, 360] } },
      scarf: { slot: "neck", size: [30, 10], pivot: [15, 2], views: { front: [40, 360], side: [80, 360] } },
    },
  }));
  for (const species of ["person"]) {
    const look = { ...lookFor(agent({ species })), accessories: ["bun", "crown", "scarf"] };
    assert.equal(bodyFor(m, look, canonical), "pip");
  }
  assert.deepEqual(m.accessories.crown.sizes[0].front.pivot, [10, 12]);
  assert.deepEqual(m.accessories.scarf.sizes[0].side.size, [30, 10]);
  assert.deepEqual(m.bodies.pip.views.front.walk[1].slots.head, [32, 4]);
  assert.deepEqual(m.bodies.pip.views.side.walk[0].slots.head, [30, 6]);
});

test("unicorn, cactus and mushroom draw their own species body, with their features innate", () => {
  for (const name of ["unicorn", "cactus", "mushroom"]) {
    const look = lookFor(agent({ species: name }));
    assert.equal(look.body, name);
    assert.equal(bodyFor(shipped, look, canonical, innate), name, name);
    assert.equal(bodyFor(parseManifest(minimal()), look, canonical, innate), null, name);
  }
  assert.equal(lookFor(agent({ species: "toadstool" })).body, "mushroom");
  assert.equal(lookFor(agent({ species: "succulent" })).body, "cactus");
  assert.equal(lookFor(agent({ species: "pegasus" })).body, "unicorn");
});

test("facing picks front, side or back and mirrors the other side", () => {
  const m = parseManifest(minimal());
  assert.deepEqual(viewOf(m.bodies.pip, "down"), { view: "front", mirror: false });
  assert.deepEqual(viewOf(m.bodies.pip, "up"), { view: "front", mirror: false });
  assert.deepEqual(viewOf(m.bodies.fox, "up"), { view: "back", mirror: false });
  assert.deepEqual(viewOf(m.bodies.pip, "left"), { view: "side", mirror: false });
  assert.deepEqual(viewOf(m.bodies.pip, "right"), { view: "side", mirror: true });
  const right = { ...m.bodies.pip, sideFaces: "right" };
  assert.deepEqual(viewOf(right, "left"), { view: "side", mirror: true });
  assert.deepEqual(viewOf(right, "right"), { view: "side", mirror: false });
});

test("walk frames cycle through all 8 and placement snaps to the sprite grid", () => {
  assert.deepEqual([0, 0.125, 0.5, 0.99, 1, 1.25, -0.125].map((c) => walkFrame(c, 8)), [0, 1, 4, 7, 0, 2, 7]);
  assert.equal(walkFrame(Number.NaN, 8), 0);
  assert.equal(walkFrame(0.5, 0), 0);
  assert.deepEqual(placed([33, 119], 4, 100.3, 50.1), [92, 20.25]);
  assert.deepEqual(placed([8, 190], 2, 40, 96), [36, 1]);
});

test("the shipped cast covers every species option, with one human base, a businessman and a kid", () => {
  const bodies = Object.keys(shipped.bodies);
  assert.equal(bodies.length, 30);
  assert.ok(!bodies.some((b) => ["slim", "stocky", "tall"].includes(b)));
  assert.ok(shipped.bodies.human.species.includes("person"));
  assert.deepEqual([shipped.bodies.kid.size, shipped.bodies.kid.anchor], [[56, 100], [27, 99]]);
  assert.deepEqual([shipped.bodies.crab.size, shipped.bodies.crab.anchor], [[166, 110], [83, 109]]);
  for (const [key, body] of Object.entries(shipped.bodies)) {
    assert.equal(body.density, 4, key);
    assert.equal(body.anchor[1], body.size[1] - 1, `${key} stands on the bottom row`);
    assert.ok(body.atlas?.startsWith("cast/bodies/"), key);
    for (const view of ["front", "side", "back"]) assert.equal(body.views[view].walk.length, 8, `${key} ${view}`);
  }
  for (const [key, acc] of Object.entries(shipped.accessories)) {
    assert.deepEqual(Object.keys(acc.sizes).map(Number), [44, 52, 60, 72, 88], key);
  }
  for (const name of ["human", "businessman", "kid", "person", "fox", "crab", "robot", "octopus", "dino", "bee", "panda", "bunny"]) {
    const look = lookFor({ slug: `agent-${name}`, persona: "", look: { species: name } });
    assert.ok(bodyFor(shipped, look, canonical, innate), name);
  }
  const suit = lookFor({ slug: "suit", persona: "", look: { species: "businessman" } });
  assert.equal(bodyFor(shipped, suit, canonical, innate), "businessman");
  const roles = new Set(Object.values(shipped.bodies.businessman.swaps).map((s) => s.role));
  assert.deepEqual([...roles].sort(), ["hair", "shirt", "skin", "suit", "tie"]);
  assert.deepEqual(shipped.sources, {});
});

test("accessories pick the size class nearest the body's head or body class", () => {
  const human = shipped.bodies.human;
  assert.deepEqual(human.classes, { head: 60, body: 44 });
  assert.deepEqual(accessoryView(shipped.accessories.crown, human, "front"), { ...accessoryView(shipped.accessories.crown, { ...human, classes: { head: 60, body: 0 } }, "front") });
  const crown60 = shipped.accessories.crown.sizes[60].front;
  assert.deepEqual(accessoryView(shipped.accessories.crown, human, "front"), crown60);
  assert.deepEqual(accessoryView(shipped.accessories.scarf, human, "front"), shipped.accessories.scarf.sizes[44].front);
  assert.deepEqual(accessoryView(shipped.accessories.crown, { ...human, classes: { head: 66, body: 44 } }, "side"), shipped.accessories.crown.sizes[72].side);
  assert.equal(accessoryView(shipped.accessories.glasses, human, "back"), null);
});

test("the briefcase rides the hand slot, which follows the hand on every walk frame", () => {
  assert.equal(shipped.accessories.briefcase.slot, "hand");
  const cast = JSON.parse(readFileSync(new URL("../sprites/cast/manifest.json", import.meta.url), "utf8"));
  const source = cast.bodies.find((b) => b.name === "businessman");
  const body = shipped.bodies.businessman;
  for (const view of ["front", "side", "back"]) {
    const base = source.accessory_anchors[view].hand;
    assert.deepEqual(body.views[view].idle[0].slots.hand, base);
    body.views[view].walk.forEach((frame, k) => {
      const by = source.hand_walk_offsets[view][k];
      assert.deepEqual(frame.slots.hand, [base[0] + by[0], base[1] + by[1]], `${view} ${k}`);
    });
  }
});

test("the briefcase is hidden on frames marked under and drawn over the body everywhere else", () => {
  const cast = JSON.parse(readFileSync(new URL("../sprites/cast/manifest.json", import.meta.url), "utf8"));
  const source = cast.bodies.find((b) => b.name === "businessman");
  const body = shipped.bodies.businessman;
  for (const view of ["front", "side", "back"]) {
    const cut = accessoryView(shipped.accessories.briefcase, body, view);
    body.views[view].walk.forEach((frame, k) => {
      const want = source.hand_walk_layers[view][k] === "under" ? "hidden" : "over";
      assert.equal(wornLayer(frame, "hand", cut), want, `${view} ${k}`);
    });
    assert.equal(wornLayer(body.views[view].idle[0], "hand", cut), "over", `${view} idle`);
  }
  const back = accessoryView(shipped.accessories.briefcase, body, "back");
  assert.ok(body.views.back.walk.some((f) => wornLayer(f, "hand", back) === "hidden"));
  for (const view of ["front", "side"]) {
    const cut = accessoryView(shipped.accessories.briefcase, body, view);
    assert.ok(body.views[view].walk.every((f) => wornLayer(f, "hand", cut) === "over"), view);
  }
  const cape = { ...back, behind: true };
  assert.equal(wornLayer({ at: [0, 0], slots: {}, layers: {} }, "back", cape), "under");
});

test("a left-facing side view mirrors body and case together and keeps the case layer", () => {
  const body = shipped.bodies.businessman;
  assert.deepEqual(viewOf(body, "left"), { view: "side", mirror: true });
  assert.deepEqual(viewOf(body, "right"), { view: "side", mirror: false });
  const width = body.size[0];
  const frame = body.views.side.walk[2];
  const hand = frame.slots.hand;
  assert.equal(mirrored(hand[0], width), width - 1 - hand[0]);
  assert.equal(mirrored(mirrored(hand[0], width), width), hand[0]);
  assert.equal(mirrored(0, width), width - 1);
  const cut = accessoryView(shipped.accessories.briefcase, body, "side");
  assert.ok(body.views.side.walk.every((f) => wornLayer(f, "hand", cut) === "over"));
});

test("unknown manifest fields never break parsing", () => {
  const pip = minimal().bodies.pip;
  const m = parseManifest(minimal({
    hole_check: { holes_found: 0 },
    proportion_template: { head_to_height: 0.485 },
    sources_url: "https://example.com/x.zip",
    sources: { cast: "https://example.com/c.zip", bad: 4, local: "file:///x" },
    bodies: { pip: { ...pip, hole_check: { frames_checked: 27 }, height_px: 114, walk_stats: {} } },
  }));
  assert.deepEqual(m.sources, { cast: "https://example.com/c.zip" });
  assert.ok(m.bodies.pip);
});

test("things carry signs, points and colour masks", () => {
  const m = parseManifest(minimal({
    things: {
      shop: {
        anchor: [130, 242],
        frames: [[0, 400, 260, 242]],
        sign: [76, 117, 108, 21],
        points: { chimney: [[208, 4]], windows: [[184, 187], [59, 189]] },
        masks: { roof: { at: [524, 400], base: "#C05B47" } },
      },
    },
  }));
  const shop = m.things.shop;
  assert.deepEqual(shop.sign, [76, 117, 108, 21]);
  assert.deepEqual(shop.points.windows, [[184, 187], [59, 189]]);
  assert.deepEqual(shop.masks.roof, { at: [524, 400], base: "#c05b47" });
  const bad = (patch) => minimal({ things: { shop: { anchor: [130, 242], frames: [[0, 400, 260, 242]], ...patch } } });
  assert.throws(() => parseManifest(bad({ points: { chimney: [[300, 4]] } })), /points\.chimney\[0\]/);
  assert.throws(() => parseManifest(bad({ anchor: [300, 242] })), /anchor/);
  assert.throws(() => parseManifest(bad({ masks: { roof: { at: [0, 0], base: "red" } } })), /masks\.roof\.base/);
});

test("the shipped atlas covers every building, place and decoration of the export", () => {
  const names = [
    "shop", "civic_clock", "civic_library", "civic_forge", "civic_post", "civic_market", "civic_observatory",
    "banyan", "portal_arch", "noticeboard", "waysign", "lantern_post", "campfire", "banner",
    "stake", "tree_green", "tree_blossom", "tree_autumn", "tree_pine", "tree_pine_snow", "bush", "bush_flowers",
    "rock", "reeds", "cactus", "ice_rock", "lamp", "bench", "crate", "barrel", "planter", "signpost",
    "palm", "snowman", "mossy_log", "sandstone", "bollard", "boulder",
  ];
  for (const name of names) assert.equal(shipped.things[name]?.density, 2, name);
  assert.equal(Object.keys(shipped.things).length, names.length);
  assert.ok(shipped.things.shop.masks.roof && shipped.things.shop.masks.awning);
  assert.ok(shipped.things.shop.sign && shipped.things.shop.points.chimney);
  const world = JSON.parse(readFileSync(new URL("../sprites/style-a/manifest.json", import.meta.url), "utf8"));
  for (const item of world.items) {
    for (const [name, value] of Object.entries(item.points ?? {})) {
      if (name === "sign") continue;
      const want = value.length === 2 ? [value] : [value.slice(0, 2), [value[0] + value[2], value[1] + value[3]]];
      assert.deepEqual(shipped.things[item.name].points[name], want, `${item.name}.${name}`);
    }
  }
  assert.deepEqual(shipped.things.civic_clock.points.clock_face, [[110, 97]]);
  assert.ok(shipped.things.civic_clock.points.clock_rim[0][1] < 97);
  assert.deepEqual(shipped.things.portal_arch.points.opening, [[40, 30], [118, 121]]);
  for (const name of names) assert.equal(shipped.things[name].points.windows, undefined, name);
  assert.deepEqual(shipped.things.civic_clock.frames[0].slice(2), [220, 254]);
  assert.deepEqual(shipped.things.civic_clock.anchor, [110, 254]);
  assert.deepEqual(shipped.things.civic_market.frames[0].slice(2), [244, 168]);
  assert.deepEqual(shipped.things.civic_market.anchor, [122, 168]);
});

test("no accessory or gear sends a style A agent back to the classic figure", () => {
  const types = readFileSync(new URL("../components/world/types.ts", import.meta.url), "utf8");
  const union = types.slice(types.indexOf("export type Accessory"), types.indexOf(";", types.indexOf("export type Accessory")));
  const every = [...union.matchAll(/"([a-z-]+)"/g)].map((m) => m[1]);
  assert.ok(every.includes("quill") && every.includes("feather"));
  const bodies = Object.values(shipped.bodies).flatMap((b) => b.species);
  for (const body of bodies) {
    for (const extra of every) {
      const look = { body, accessories: [...innate(canonical(body)), extra] };
      const kept = withArtOnly(shipped, look, canonical, innate);
      assert.ok(bodyFor(shipped, kept, canonical, innate), `${body} + ${extra}`);
      if (shipped.accessories[extra]) assert.ok(kept.accessories.includes(extra), `${extra} has art and stays`);
    }
  }
  const plain = { body: "human", accessories: ["glasses"] };
  assert.equal(withArtOnly(shipped, plain, canonical, innate), plain);
});
