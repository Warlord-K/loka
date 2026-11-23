import assert from "node:assert/strict";
import test from "node:test";

import {
  attachmentKeysAfterQuestSend,
  cardAfterSettlement,
  draftAfterQuestSend,
  interruptAfterQuestSend,
  questCardKey,
  retainedQuestCard,
  visibleQuestCard,
} from "./quest-room-state.ts";

test("clarification results only settle their own task and card", () => {
  const cardA = { id: "a" };
  const cardB = { id: "b" };
  const settled = new Set([questCardKey({ taskId: "one", cardId: "a" })]);

  assert.equal(visibleQuestCard("one", "one", cardA, cardB, settled), cardB);
  assert.equal(visibleQuestCard("one", "one", cardA, null, settled), null);
  assert.equal(visibleQuestCard("one", "one", cardB, cardA, settled), cardB);
  assert.equal(visibleQuestCard("two", "one", cardB, cardA, settled), cardA);
  assert.equal(visibleQuestCard("one", "one", null, cardB, settled), null);
  assert.deepEqual(cardAfterSettlement({ taskId: "one", cardId: "b" }, { taskId: "one", cardId: "a" }), {
    taskId: "one",
    cardId: "b",
  });
  assert.equal(cardAfterSettlement({ taskId: "one", cardId: "a" }, { taskId: "one", cardId: "a" }), null);

  const heldA = { taskId: "one", card: cardA };
  assert.equal(retainedQuestCard("one", "one", null, heldA, settled), heldA);
  assert.equal(retainedQuestCard("one", "one", cardA, heldA, settled), heldA);
  assert.deepEqual(retainedQuestCard("one", "one", cardB, heldA, settled), { taskId: "one", card: cardB });
  assert.equal(retainedQuestCard("two", "two", null, heldA, settled), null);
});

test("an ordinary send clears only the captured draft and attachments", () => {
  const submission = {
    taskId: "one",
    draft: "ordinary message",
    attachmentKeys: ["sent-file"],
    interrupt: true,
  };

  assert.equal(draftAfterQuestSend(submission, "one", "ordinary message"), "");
  assert.equal(draftAfterQuestSend(submission, "one", "edited while sending"), "edited while sending");
  assert.deepEqual(attachmentKeysAfterQuestSend(submission, "one"), ["sent-file"]);
  assert.equal(interruptAfterQuestSend(submission, "one", true), false);
  assert.equal(draftAfterQuestSend(submission, "two", "another quest"), "another quest");
  assert.deepEqual(attachmentKeysAfterQuestSend(submission, "two"), []);
  assert.equal(interruptAfterQuestSend(submission, "two", true), true);
});
