import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_QUOTED, quoteItems, readAbout } from "./canvas-quote.ts";

const report = { id: "abcdef12-0000-0000-0000-000000000000", title: "Checkout redesign" };

test("several selected items become one quote with every item in its source", () => {
  const quote = quoteItems(report, [
    { id: "cart", kind: "Note", label: "Cart" },
    { id: "pay", kind: "Image", label: "" },
  ]);
  assert.deepEqual(quote, {
    text: "- Note: Cart\n- Image",
    label: '2 items on workspace "Checkout redesign"',
    source: `report:${report.id}#cart+pay`,
  });
  assert.deepEqual(quoteItems(report, [{ id: "cart", kind: "Note", label: "Cart" }]), {
    text: "Note: Cart",
    label: 'Note on workspace "Checkout redesign"',
    source: `report:${report.id}#cart`,
  });
  assert.equal(quoteItems(report, []), null);
});

test("a quote never names more items than the server accepts", () => {
  const many = Array.from({ length: MAX_QUOTED + 5 }, (_, i) => ({ id: `n${i}`, kind: "Note", label: `Item ${i}` }));
  const quote = quoteItems(report, many);
  assert.equal(quote.source.split("#")[1].split("+").length, MAX_QUOTED);
  assert.ok(quote.label.startsWith(`${MAX_QUOTED} items`));
});

test("only a canvas reference can ride along with a call", () => {
  assert.equal(readAbout({ about: "report:abcdef12" }), "report:abcdef12");
  assert.equal(readAbout({ about: "report:abcdef12#n1" }), null);
  assert.equal(readAbout({ about: "quest:abcdef12" }), null);
  assert.equal(readAbout({ about: 3 }), null);
  assert.equal(readAbout(null), null);
});
