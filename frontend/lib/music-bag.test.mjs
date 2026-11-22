import { test } from "node:test";
import assert from "node:assert/strict";
import { drawFromBag, EMPTY_BAG, freshLap, markPlayed, nextInOrder, pickNext, playerErrorReason, readBag } from "./music-bag.ts";

const ids = (n) => Array.from({ length: n }, (_, i) => `song${String(i).padStart(7, "0")}`);

function seeded(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

function draws(list, count, random, start = EMPTY_BAG) {
  let bag = start;
  const out = [];
  for (let i = 0; i < count; i++) {
    const next = drawFromBag(bag, list, random);
    out.push(next.id);
    bag = next.bag;
  }
  return { out, bag };
}

test("no song plays twice before every song has played", () => {
  const list = ids(45);
  const { out } = draws(list, 45 * 4, seeded(7));
  for (let lap = 0; lap < 4; lap++) {
    const chunk = out.slice(lap * 45, lap * 45 + 45);
    assert.equal(new Set(chunk).size, 45);
  }
});

test("a new lap never starts with the song that just ended", () => {
  const list = ids(3);
  for (let seed = 1; seed < 200; seed++) {
    const { out } = draws(list, 30, seeded(seed));
    for (let i = 1; i < out.length; i++) assert.notEqual(out[i], out[i - 1]);
  }
});

test("a saved bag picks up where it left off after a reload", () => {
  const list = ids(20);
  const first = draws(list, 7, seeded(3));
  const saved = readBag(JSON.parse(JSON.stringify(first.bag)));
  const rest = draws(list, 13, seeded(99), saved);
  assert.equal(new Set([...first.out, ...rest.out]).size, 20);
  assert.deepEqual(rest.out, first.bag.left);
});

test("two reloads in a row start with different songs", () => {
  const list = ids(40);
  const a = draws(list, 1, seeded(5));
  const b = draws(list, 1, seeded(11), readBag(JSON.parse(JSON.stringify(a.bag))));
  assert.notEqual(a.out[0], b.out[0]);
});

test("a changed playlist keeps the songs not played yet and adds new ones", () => {
  const list = ids(10);
  const { out, bag } = draws(list, 4, seeded(2));
  const removed = bag.left[0];
  const changed = [...list.filter((id) => id !== removed), "newSong0001", "newSong0002"];
  const rest = draws(changed, changed.length - 4, seeded(8), bag);
  assert.equal(new Set(rest.out).size, rest.out.length);
  assert.ok(!rest.out.includes(removed));
  assert.ok(rest.out.includes("newSong0001") && rest.out.includes("newSong0002"));
  for (const id of out) assert.ok(!rest.out.includes(id));
  assert.deepEqual(new Set([...out, ...rest.out]), new Set(changed));
});

test("picking a song from the list counts it as played for this lap", () => {
  const list = ids(8);
  const bag = markPlayed(EMPTY_BAG, list, list[5], seeded(1));
  const { out } = draws(list, 7, seeded(4), bag);
  assert.ok(!out.includes(list[5]));
  assert.equal(new Set(out).size, 7);
});

test("in order mode the list continues after the last song and wraps", () => {
  const list = ids(3);
  assert.equal(nextInOrder(list, null), list[0]);
  assert.equal(nextInOrder(list, list[1]), list[2]);
  assert.equal(nextInOrder(list, list[2]), list[0]);
  assert.equal(nextInOrder(list, "goneSong001"), list[0]);
  assert.equal(nextInOrder([], null), null);
});

test("an empty list draws nothing and fresh laps of one song still work", () => {
  assert.equal(drawFromBag(EMPTY_BAG, [], seeded(1)).id, null);
  assert.deepEqual(freshLap(["onlySong001"], "onlySong001", seeded(1)), ["onlySong001"]);
});

test("stored bags are checked before use", () => {
  assert.deepEqual(readBag(null), EMPTY_BAG);
  assert.deepEqual(readBag({ left: ["ok_song_001", "bad id", 4, "ok_song_001"], played: "x", last: "<script>" }), { left: ["ok_song_001"], played: [], last: null });
});

function skipping(list, broken, shuffle, count, random) {
  let bag = EMPTY_BAG;
  const out = [];
  for (let i = 0; i < count; i++) {
    const next = pickNext(bag, list, broken, shuffle, random);
    out.push(next.id);
    bag = next.bag;
  }
  return out;
}

test("songs that failed to play are skipped in shuffle and in order", () => {
  const list = ids(10);
  const broken = new Set([list[0], list[3], list[4]]);
  for (const shuffle of [true, false]) {
    const out = skipping(list, broken, shuffle, 14, seeded(6));
    for (const id of out) assert.ok(!broken.has(id), `${id} played after failing`);
    assert.equal(new Set(out.slice(0, 7)).size, 7);
  }
  assert.deepEqual(skipping(list, broken, false, 3, seeded(1)), [list[1], list[2], list[5]]);
});

test("a song that fails mid-lap does not break the rest of the lap", () => {
  const list = ids(6);
  const random = seeded(12);
  const broken = new Set();
  let bag = EMPTY_BAG;
  const played = [];
  for (let i = 0; i < 5; i++) {
    const next = pickNext(bag, list, broken, true, random);
    bag = next.bag;
    if (i === 1) broken.add(next.id);
    else played.push(next.id);
  }
  assert.equal(new Set(played).size, 4);
  assert.equal(bag.left.length, 1);
});

test("once every song has failed nothing more is picked, so the player stops instead of looping", () => {
  const list = ids(5);
  const broken = new Set(list);
  for (const shuffle of [true, false]) assert.equal(pickNext(EMPTY_BAG, list, broken, shuffle, seeded(1)).id, null);
});

test("player errors read as plain reasons", () => {
  assert.match(playerErrorReason(150), /outside YouTube/);
  assert.match(playerErrorReason(101), /outside YouTube/);
  assert.match(playerErrorReason(100), /removed or is private/);
  assert.equal(playerErrorReason(999), "player error 999");
});

test("only the removed, refused and invalid codes mark a song broken, a player hiccup only skips it", async () => {
  const { errorEffect } = await import("./music-bag.ts");
  for (const code of [2, 100, 101, 150]) assert.equal(errorEffect(code), "broken");
  for (const code of [5, 153, 999]) assert.equal(errorEffect(code), "skip");
});

test("a late error from the previous song never counts against the song that replaced it", async () => {
  const { errorBelongs } = await import("./music-bag.ts");
  assert.equal(errorBelongs({ requested: "songB000000", current: "songB000000", urlId: "songA000000", stateSinceLoad: false }), false);
  assert.equal(errorBelongs({ requested: "songB000000", current: "songB000000", urlId: null, stateSinceLoad: false }), false);
  assert.equal(errorBelongs({ requested: "songA000000", current: "songB000000", urlId: "songA000000", stateSinceLoad: true }), false);
  assert.equal(errorBelongs({ requested: null, current: "songB000000", urlId: null, stateSinceLoad: true }), false);
  assert.equal(errorBelongs({ requested: "songB000000", current: "songB000000", urlId: "songB000000", stateSinceLoad: false }), true);
  assert.equal(errorBelongs({ requested: "songB000000", current: "songB000000", urlId: null, stateSinceLoad: true }), true);
});

test("skipping stops once every playable song failed in a row", async () => {
  const { skipsExhausted } = await import("./music-bag.ts");
  assert.equal(skipsExhausted(new Set(["a", "b"]), ["a", "b", "c"]), false);
  assert.equal(skipsExhausted(new Set(["a", "b", "c"]), ["a", "b", "c"]), true);
  assert.equal(skipsExhausted(new Set(["a"]), []), true);
});

test("a broken song counts once, so the songs left are all tried before giving up", async () => {
  const { skipsExhausted } = await import("./music-bag.ts");
  const songs = ["a", "b", "c"];
  const broken = new Set();
  const failed = new Set();
  const playable = () => songs.filter((id) => !broken.has(id));
  for (const id of ["a", "b"]) {
    broken.add(id);
    failed.add(id);
    failed.add(id);
    assert.equal(skipsExhausted(failed, playable()), false);
  }
  broken.add("c");
  failed.add("c");
  assert.equal(skipsExhausted(failed, playable()), true);
});

test("a song that fails without being broken stays playable and still ends the run once all have failed", async () => {
  const { skipsExhausted } = await import("./music-bag.ts");
  const failed = new Set(["a"]);
  assert.equal(skipsExhausted(failed, ["a", "b"]), false);
  failed.add("a");
  assert.equal(skipsExhausted(failed, ["a", "b"]), false);
  failed.add("b");
  assert.equal(skipsExhausted(failed, ["a", "b"]), true);
});
