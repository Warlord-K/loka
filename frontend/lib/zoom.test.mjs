import { test } from "node:test";
import assert from "node:assert/strict";
import { clampView, crisp, isFiniteView, fitScale, fitView, isZoomed, panBy, toggleZoom, wheelFactor, zoomAt, zoomLimits, zoomTo } from "./zoom.ts";

const stage = { w: 800, h: 600 };
const photo = { w: 4000, h: 3000 };
const sprite = { w: 32, h: 48 };

const close = (a, b) => assert.ok(Math.abs(a - b) < 1e-9, `${a} is not ${b}`);

test("large images fit inside the stage without upscaling", () => {
  close(fitScale(photo, stage), 0.2);
  assert.equal(fitScale({ w: 600, h: 400 }, stage), 1);
});

test("small pixel art fits at a whole-number upscale", () => {
  assert.equal(fitScale(sprite, stage), 4);
  assert.equal(fitScale({ w: 250, h: 100 }, stage), 3);
  assert.equal(fitScale({ w: 10, h: 10 }, { w: 0, h: 0 }), 1);
});

test("zoom stays between fit and the cap", () => {
  const { min, max } = zoomLimits(photo, stage);
  close(min, 0.2);
  assert.equal(max, 8);
  assert.equal(zoomLimits(sprite, stage).max, 16);
  close(zoomAt(fitView(photo, stage), 0.01, { x: 0, y: 0 }, photo, stage).scale, 0.2);
  assert.equal(zoomAt(fitView(photo, stage), 100, { x: 0, y: 0 }, photo, stage).scale, 8);
});

test("zooming keeps the point under the cursor in place", () => {
  const start = { scale: 1, x: 0, y: 0 };
  const at = { x: 120, y: -80 };
  const next = zoomAt(start, 2, at, photo, stage);
  close((at.x - start.x) / start.scale, (at.x - next.x) / next.scale);
  close((at.y - start.y) / start.scale, (at.y - next.y) / next.scale);
});

test("pinch moves the picked point with the fingers", () => {
  const start = { scale: 1, x: 0, y: 0 };
  const next = zoomTo(start, 2, { x: 10, y: 10 }, { x: 40, y: 20 }, photo, stage);
  close((10 - start.x) / start.scale, (40 - next.x) / next.scale);
});

test("panning is clamped so the image never leaves the stage", () => {
  const zoomed = { scale: 1, x: 0, y: 0 };
  const far = panBy(zoomed, 99999, -99999, photo, stage);
  assert.equal(far.x, (4000 - 800) / 2);
  assert.equal(far.y, -(3000 - 600) / 2);
  const fit = clampView({ scale: 0.2, x: 300, y: 300 }, photo, stage);
  assert.equal(fit.x, 0);
  assert.equal(fit.y, 0);
});

test("double click toggles between fit and a closer look", () => {
  const fit = fitView(photo, stage);
  const near = toggleZoom(fit, { x: 0, y: 0 }, photo, stage);
  assert.equal(near.scale, 1);
  assert.ok(isZoomed(near, photo, stage));
  assert.deepEqual(toggleZoom(near, { x: 0, y: 0 }, photo, stage), fit);
  assert.equal(toggleZoom(fitView(sprite, stage), { x: 0, y: 0 }, sprite, stage).scale, 8);
});

test("wheel and pinch deltas become bounded zoom factors", () => {
  assert.ok(wheelFactor(-100, 0, false) > 1);
  assert.ok(wheelFactor(100, 0, false) < 1);
  assert.ok(wheelFactor(-10, 0, true) > wheelFactor(-10, 0, false));
  close(wheelFactor(-1000, 0, false), wheelFactor(-120, 0, false));
  close(wheelFactor(-3, 1, false), wheelFactor(-48, 0, false));
});

test("pixels stay crisp for small art and past actual size", () => {
  assert.ok(crisp({ scale: 1, x: 0, y: 0 }, sprite));
  assert.ok(!crisp({ scale: 0.5, x: 0, y: 0 }, photo));
  assert.ok(crisp({ scale: 2, x: 0, y: 0 }, photo));
});

test("non-finite input keeps the last good view", () => {
  const good = { scale: 1, x: 40, y: -20 };
  assert.deepEqual(zoomAt(good, Number.NaN, { x: 0, y: 0 }, photo, stage), good);
  assert.deepEqual(zoomAt(good, 2, { x: Number.NaN, y: Number.NaN }, photo, stage), good);
  assert.deepEqual(zoomTo(good, 2, { x: 0, y: 0 }, { x: undefined, y: 0 }, photo, stage), good);
  assert.deepEqual(panBy(good, Number.NaN, 0, photo, stage), good);
  assert.deepEqual(panBy(good, Infinity, 0, photo, stage), good);
  assert.deepEqual(clampView({ scale: Number.NaN, x: 0, y: 0 }, photo, stage, good), good);
  assert.deepEqual(clampView({ scale: 2, x: Number.NaN, y: 0 }, photo, stage), fitView(photo, stage));
  assert.ok(!isFiniteView({ scale: 0, x: 0, y: 0 }));
  assert.ok(isFiniteView(good));
});
