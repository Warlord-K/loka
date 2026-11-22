import { test } from "node:test";
import assert from "node:assert/strict";
import { defaultDevice, devicePickerShown } from "./run-device.ts";

test("the browser keeps the agent's own device", () => {
  assert.equal(defaultDevice({ devices: ["mac", "cloud"], home: "cloud", appRunner: null }), "cloud");
});

test("the desktop app picks the Mac it runs on when that Mac is one of your online devices", () => {
  assert.equal(defaultDevice({ devices: ["mac", "cloud"], home: "cloud", appRunner: "mac" }), "mac");
});

test("the desktop app falls back to the agent's device when this Mac is offline, someone else's or unpaired", () => {
  assert.equal(defaultDevice({ devices: ["cloud"], home: "cloud", appRunner: "mac" }), "cloud");
  assert.equal(defaultDevice({ devices: ["cloud", "other"], home: "cloud", appRunner: null }), "cloud");
});

test("a home device that is not online falls back to the first online one, and no devices gives none", () => {
  assert.equal(defaultDevice({ devices: ["a", "b"], home: "gone", appRunner: null }), "a");
  assert.equal(defaultDevice({ devices: [], home: "cloud", appRunner: "mac" }), null);
});

test("the picker shows with a choice to make or when the default is not the agent's device", () => {
  assert.equal(devicePickerShown({ devices: ["cloud"], chosen: "cloud", home: "cloud" }), false);
  assert.equal(devicePickerShown({ devices: ["mac", "cloud"], chosen: "cloud", home: "cloud" }), true);
  assert.equal(devicePickerShown({ devices: ["mac"], chosen: "mac", home: "cloud" }), true);
  assert.equal(devicePickerShown({ devices: [], chosen: null, home: "cloud" }), false);
});

test("the confirm waits until both the device list and this Mac are known", async () => {
  const { devicesLoading } = await import("./run-device.ts");
  assert.equal(devicesLoading({ needed: true, devices: null, appRunner: { id: null } }), true);
  assert.equal(devicesLoading({ needed: true, devices: [], appRunner: null }), true);
  assert.equal(devicesLoading({ needed: true, devices: [], appRunner: { id: null } }), false);
  assert.equal(devicesLoading({ needed: false, devices: null, appRunner: null }), false);
});
