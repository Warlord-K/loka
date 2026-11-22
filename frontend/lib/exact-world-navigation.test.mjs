import assert from "node:assert/strict";
import { test } from "node:test";
import { openExactDestination, revealExactMap } from "./exact-world-navigation.ts";

const home = "home";
const away = "away";
const region = (id) => ({ region: { id, name: id } });
const item = (id, kind, region_id = null) => ({ id, kind, region_id, name: id });
const reader = (row) => ({
  home,
  team: async () => ({ id: "team" }),
  item: async () => row,
  region: async (id) => region(id.toLowerCase()),
});

const destinations = [
  { kind: "team", id: "team" },
  { kind: "list", id: "checklist", row: item("checklist", "list") },
  { kind: "list", id: "reminder", row: item("reminder", "timer") },
  { kind: "region", id: home, regionId: home },
  { kind: "region", id: away, regionId: away },
  { kind: "place", id: "building", row: item("building", "building"), regionId: home },
  { kind: "place", id: "npc", row: item("npc", "npc", away), regionId: away },
];

const startingViews = [
  { name: "desktop baseline", current: home },
  { name: "room interior", current: home, room: "room" },
  { name: "saved Quick Chat", current: home, panel: "quick" },
  { name: "open report and side chat", current: home, panel: "report" },
  { name: "Quests tab in an already-mounted Board", current: home, panel: "quests" },
  { name: "another region", current: away },
  { name: "mobile interior", current: home, room: "room", panel: "chat" },
];

for (const destination of destinations) {
  for (const start of startingViews) {
    test(`${destination.id} from ${start.name}`, async () => {
      const steps = [];
      let current = start.current;
      let room = start.room;
      let panel = start.panel;
      const engine = {
        currentPlace: () => room,
        leaveRoom(instant) {
          assert.equal(instant, true);
          steps.push("leave");
          room = null;
        },
      };
      const reveal = (place) => steps.push(place?.id ?? "outdoors");
      const travel = (target) => {
        steps.push(`travel:${target.regionId}`);
        current = target.regionId;
        room = null;
        revealExactMap(target, current, engine, travel, reveal);
      };
      await openExactDestination(
        { kind: destination.kind, id: destination.id.toUpperCase() },
        reader(destination.row),
        () => true,
        {
          closePanels: () => { steps.push("close"); panel = null; },
          updateLink: (kind, id) => steps.push(`${kind}:${id}`),
          showTeam: () => { steps.push("teams"); panel = "teams"; },
          showList: () => { steps.push("lists"); panel = "lists"; },
          showMap: (target) => revealExactMap(target, current, engine, travel, reveal),
          unavailable: () => assert.fail("valid destination"),
        },
      );
      const expected = ["close", `${destination.kind}:${destination.id}`];
      if (destination.regionId) {
        if (destination.regionId !== start.current) expected.push(`travel:${destination.regionId}`);
        else if (start.room) expected.push("leave");
        expected.push(destination.row?.id ?? "outdoors");
        assert.equal(panel, null);
      } else {
        expected.push(destination.kind === "team" ? "teams" : "lists");
        assert.equal(room, start.room);
      }
      assert.deepEqual(steps, expected);
    });
  }
}

const unexpectedNavigation = {
  closePanels: () => assert.fail("closed the current panel"),
  updateLink: () => assert.fail("changed the current URL"),
  showTeam: () => assert.fail("opened Teams"),
  showList: () => assert.fail("opened Lists"),
  showMap: () => assert.fail("changed the map"),
  unavailable: () => assert.fail("announced a superseded result"),
};

test("a panel that refuses to close keeps its view and URL", async () => {
  await openExactDestination({ kind: "team", id: "team" }, reader(), () => true, {
    ...unexpectedNavigation,
    closePanels: () => false,
  });
});

for (const destination of destinations) {
  test(`a superseded ${destination.id} response keeps the current view`, async () => {
    let current = true;
    const pending = openExactDestination(
      destination,
      reader(destination.row),
      () => current,
      unexpectedNavigation,
    );
    current = false;
    await pending;
  });
}

test("a list response held until after opening a report cannot navigate", async () => {
  let selection = "list";
  const list = Promise.withResolvers();
  const pending = openExactDestination(
    { kind: "list", id: "checklist" },
    { ...reader(), item: () => list.promise },
    () => selection === "list",
    unexpectedNavigation,
  );
  selection = "report";
  list.resolve(item("checklist", "list"));
  await pending;
});

for (const [link, read] of [
  [{ kind: "team", id: "hidden" }, { ...reader(), team: async () => { throw new Error("404"); } }],
  [{ kind: "list", id: "wrong-kind" }, reader(item("wrong-kind", "building"))],
  [{ kind: "place", id: "wrong-kind" }, reader(item("wrong-kind", "list"))],
  [{ kind: "place", id: "closed-road" }, { ...reader(item("closed-road", "building", away)), region: async () => { throw new Error("404"); } }],
  [{ kind: "region", id: "hidden" }, { ...reader(), region: async () => { throw new Error("404"); } }],
]) {
  test(`${link.kind} validation failure keeps the current view and URL`, async () => {
    let unavailable = 0;
    await openExactDestination(link, read, () => true, {
      ...unexpectedNavigation,
      unavailable: () => { unavailable += 1; },
    });
    assert.equal(unavailable, 1);
  });
}
