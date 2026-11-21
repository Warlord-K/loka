import { test } from "node:test";
import assert from "node:assert/strict";
import { LOOK_ACCESSORIES, SPECIES_OPTIONS, declaredAccessories, gearAccessories, lookFor, speciesFor, unknownAccessories, withGear } from "./looks.ts";

const agent = (look, persona = "") => ({ slug: "test-agent", persona, look });

test("every listed species except person draws a species", () => {
  for (const name of SPECIES_OPTIONS) {
    if (name === "person") assert.equal(speciesFor(name), undefined);
    else assert.ok(speciesFor(name), name);
  }
});

test("octopus and bunny have their own features", () => {
  assert.ok(lookFor(agent({ species: "octopus" })).accessories.includes("tentacles"));
  assert.ok(lookFor(agent({ species: "bunny" })).accessories.includes("ears-long"));
  assert.ok(lookFor(agent({ species: "rabbit" })).accessories.includes("ears-long"));
  assert.ok(lookFor(agent({ species: "squid" })).accessories.includes("tentacles"));
});

test("unknown species fall back to a person", () => {
  const person = lookFor(agent({ species: "person" }));
  const unknown = lookFor(agent({ species: "zorblax" }));
  assert.deepEqual(unknown, person);
  assert.equal(speciesFor("zorblax"), undefined);
});

test("persona words match whole words only", () => {
  assert.equal(speciesFor("", "handles communication for the team"), undefined);
  assert.equal(speciesFor("", "a pirate who loves maps"), undefined);
  assert.ok(speciesFor("", "a friendly fox who writes docs"));
  assert.ok(speciesFor("", "loves cats"));
});

test("declared accessories keep known names and one hat", () => {
  assert.deepEqual(declaredAccessories("Wizard hat, glasses, laser"), ["wizard", "glasses"]);
  assert.deepEqual(declaredAccessories(["crown", 3, "cape"]), ["crown", "cape"]);
  const look = lookFor(agent({ species: "person", accessories: ["crown", "beanie", "headphones"] }));
  assert.deepEqual(look.accessories, ["crown", "headphones"]);
  for (const name of LOOK_ACCESSORIES) assert.ok(lookFor(agent({ accessories: [name] })).accessories.includes(name), name);
});

test("android and spirit only count when declared", () => {
  assert.equal(speciesFor("", "an android engineer with team spirit"), undefined);
  assert.ok(speciesFor("android").features.includes("antenna"));
  assert.ok(speciesFor("spirit").features.includes("ghost-tail"));
  assert.ok(speciesFor("", "a friendly robot").features.includes("antenna"));
});

test("declared species match whole words and simple plurals", () => {
  for (const name of ["beetle", "caterpillar", "dragonfly", "rattlesnake"]) assert.equal(speciesFor(name), undefined, name);
  assert.ok(speciesFor("wolves").features.includes("snout"));
  assert.ok(speciesFor("foxes").features.includes("snout"));
  assert.ok(speciesFor("bunnies").features.includes("ears-long"));
});

test("gear words match whole words", () => {
  assert.deepEqual(gearAccessories("walking, baking and an escape plan"), []);
  assert.deepEqual(gearAccessories("king with a cape"), ["crown", "cape"]);
  assert.deepEqual(gearAccessories("a hard hat"), ["hardhat"]);
  assert.deepEqual(gearAccessories("a wizard hat"), ["wizard"]);
});

test("gear hats replace a chosen hat", () => {
  assert.deepEqual(withGear(["beanie", "glasses"], ["crown", "cape"]), ["glasses", "crown", "cape"]);
  assert.deepEqual(withGear(["beanie"], ["sword"]), ["beanie", "sword"]);
});

test("unknown accessories survive a save", () => {
  assert.deepEqual(unknownAccessories(["glasses", "monocle", "jetpack"]), ["monocle", "jetpack"]);
});

test("declared ducks keep their old colours unless a palette is set", () => {
  assert.equal(lookFor(agent({ species: "duck" })).skin, "#57b65a");
  assert.notEqual(lookFor(agent({ species: "duck", palette: "sunny yellow" })).skin, "#57b65a");
  assert.equal(lookFor(agent({ species: "penguin" })).hair, "#d8433a");
});

test("accessory aliases count as known and resolve to their id", () => {
  assert.deepEqual(unknownAccessories(["Wizard hat", "goggles", "hat", "tiara", "monocle"]), ["monocle"]);
  const saved = ["Wizard hat", "goggles"];
  const kept = [...declaredAccessories(saved).filter((a) => a !== "wizard"), "crown", ...unknownAccessories(saved)];
  assert.deepEqual(kept, ["glasses", "crown"]);
});

test("ducks and penguins from persona or look words keep their old colours", () => {
  assert.equal(lookFor(agent({}, "a cheerful duck who reviews code")).skin, "#57b65a");
  assert.equal(lookFor(agent({}, "a calm penguin")).hair, "#d8433a");
  assert.equal(lookFor(agent({ archetype: "penguin" })).skin, "#57b65a");
  assert.notEqual(lookFor(agent({ archetype: "penguin", palette: "icy blue" })).skin, "#57b65a");
});
