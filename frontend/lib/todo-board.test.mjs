import { test } from "node:test";
import assert from "node:assert/strict";
import { MOVE_FAILED, moveFailureNote, placeOpenTodo } from "./todo-board.ts";

const card = (id, priority = 2) => ({ id, priority });
const lanes = () => ({
  todo: [card("a", 0), card("b"), card("c", 3)],
  progress: [card("d"), card("e")],
});

test("dropping above a higher-priority card places it there and raises it just enough", () => {
  const l = lanes();
  assert.deepEqual(placeOpenTodo(l, l.todo[2], "todo", "b"), {
    priority: 2,
    order: ["a", "c", "b", "d", "e"],
  });
});

test("moving across lanes keeps the other lane in order", () => {
  const l = lanes();
  assert.deepEqual(placeOpenTodo(l, l.todo[0], "progress", "e"), {
    priority: 2,
    order: ["b", "c", "d", "a", "e"],
  });
});

test("dropping on empty column space puts the card last, even when it outranks the last card", () => {
  const l = lanes();
  const placed = placeOpenTodo(l, l.todo[0], "todo", null);
  assert.deepEqual(placed.order, ["b", "c", "a", "d", "e"]);
  assert.equal(placed.priority, 3);
});

test("a card that already ranks lowest keeps its priority at the end", () => {
  const l = { todo: [card("a", 0)], progress: [] };
  assert.deepEqual(placeOpenTodo(l, card("z", 3), "todo", null), {
    priority: 3,
    order: ["a", "z"],
  });
});

test("an empty lane keeps the card's priority", () => {
  const l = { todo: [card("a")], progress: [] };
  assert.deepEqual(placeOpenTodo(l, l.todo[0], "progress", null), {
    priority: 2,
    order: ["a"],
  });
});

test("an unknown drop target counts as the end of the lane", () => {
  const l = lanes();
  assert.deepEqual(placeOpenTodo(l, l.progress[0], "progress", "gone").order, [
    "a",
    "b",
    "c",
    "e",
    "d",
  ]);
});

test("a card moved to another column's empty space keeps its priority", () => {
  const l = { todo: [card("a", 0)], progress: [card("d", 3)] };
  assert.deepEqual(placeOpenTodo(l, l.todo[0], "progress", null), {
    priority: 0,
    order: ["d", "a"],
  });
});

test("a card coming back from Done to a column's empty space keeps its priority", () => {
  const l = { todo: [card("a", 3)], progress: [] };
  assert.deepEqual(placeOpenTodo(l, card("z", 1), "todo", null), {
    priority: 1,
    order: ["a", "z"],
  });
});

test("one step down inside a priority group keeps the card's priority", () => {
  const l = { todo: [card("a", 0), card("b", 0), card("c", 2)], progress: [] };
  assert.deepEqual(placeOpenTodo(l, l.todo[0], "todo", "c"), {
    priority: 0,
    order: ["b", "a", "c"],
  });
});

test("one step down across a priority boundary moves only as far as the neighbours need", () => {
  const l = {
    todo: [card("a", 0), card("b", 1), card("c", 2), card("d", 3)],
    progress: [],
  };
  assert.deepEqual(placeOpenTodo(l, l.todo[1], "todo", "d"), {
    priority: 2,
    order: ["a", "c", "b", "d"],
  });
});

test("dropping onto a card in a lower group keeps a priority that still fits", () => {
  const l = { todo: [card("a", 1), card("b", 1), card("c", 3)], progress: [] };
  assert.deepEqual(placeOpenTodo(l, l.todo[0], "todo", "c"), {
    priority: 1,
    order: ["b", "a", "c"],
  });
});

test("a move that only partly saved says what stayed instead of claiming it went back", () => {
  assert.equal(moveFailureNote({ column: false, priority: false }, "In progress"), MOVE_FAILED);
  assert.equal(moveFailureNote({ column: true, priority: false }, "In progress"), "Moved to In progress, but its place in the list did not save, so it may sit in a different spot.");
  assert.equal(moveFailureNote({ column: true, priority: true }, "To do"), "Moved to To do, but its place in the list did not save, so it may sit in a different spot.");
  assert.equal(moveFailureNote({ column: false, priority: true }, "To do"), "Could not save the new order, so the list shows the last order that saved.");
});
