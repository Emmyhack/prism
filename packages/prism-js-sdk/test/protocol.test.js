import assert from "node:assert/strict";
import test from "node:test";
import { RationalTime, moveClip, transaction } from "../src/index.js";

test("emits the shared exact-time transaction shape", () => {
  const request = transaction({
    requestId: "js-test",
    expectedRevision: 4,
    label: "Move opening clip",
    operations: [moveClip({
      sequenceId: "11111111-1111-4111-8111-111111111111",
      trackId: "22222222-2222-4222-8222-222222222222",
      clipId: "33333333-3333-4333-8333-333333333333",
      timelineStart: new RationalTime(1, 8),
    })],
  });
  assert.equal(request.apiVersion, "1");
  assert.deepEqual(request.operations[0].timelineStart, { value: 1, timescale: 8 });
});

test("rejects invalid timebases", () => {
  assert.throws(() => new RationalTime(1, 0), TypeError);
});
