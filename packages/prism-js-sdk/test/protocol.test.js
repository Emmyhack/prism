import assert from "node:assert/strict";
import test from "node:test";
import { RationalTime, addMarker, addTransition, createSequence, createTrack, deleteClip, deleteMarker, deleteTransition, duplicateClip, importMedia, insertClip, moveClip, recipeRequest, reorderTrack, rippleDelete, rollEdit, setTrackLocked, setTrackMuted, setTrackVisible, slipClip, splitClip, transaction, trimClip } from "../src/index.js";

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

test("rejects invalid transaction envelope fields", () => {
  assert.throws(() => transaction({ requestId: "x", expectedRevision: -1, label: "x", operations: [] }), /expectedRevision/);
  assert.throws(() => transaction({ requestId: "x", expectedRevision: 0, label: "x", operations: {} }), /operations/);
  assert.throws(() => transaction({ requestId: "x", expectedRevision: 0, label: "x", operations: [], dryRun: "yes" }), /dryRun/);
});

test("rejects invalid timebases", () => {
  assert.throws(() => new RationalTime(1, 0), TypeError);
});

test("emits the shared recipe request shape", () => {
  assert.deepEqual(
    recipeRequest({
      manifest: { id: "prism.example.recipe", apiVersion: 1 },
      inputs: { clipCount: 5 },
    }),
    {
      manifest: { id: "prism.example.recipe", apiVersion: 1 },
      inputs: { clipCount: 5 },
    },
  );
});

test("emits the remaining core clip operations", () => {
  const time = new RationalTime(1, 8);
  assert.equal(trimClip({ sequenceId: "s", trackId: "t", clipId: "c", timelineStart: time, sourceStart: time, sourceDuration: time }).type, "trimClip");
  assert.equal(splitClip({ sequenceId: "s", trackId: "t", clipId: "c", rightClipId: "r", timelinePosition: time }).type, "splitClip");
  assert.equal(deleteClip({ sequenceId: "s", trackId: "t", clipId: "c" }).type, "deleteClip");
  assert.deepEqual(slipClip({ sequenceId: "s", trackId: "t", clipId: "c", sourceDelta: new RationalTime(1, 2) }).sourceDelta, { value: 1, timescale: 2 });
  assert.deepEqual(rollEdit({ sequenceId: "s", trackId: "t", leftClipId: "a", rightClipId: "b", delta: new RationalTime(1, 2) }).delta, { value: 1, timescale: 2 });
});

test("emits project-building operations", () => {
  const time = new RationalTime(0, 1);
  assert.equal(importMedia({ id: "m", sourcePath: "source.mp4" }).type, "importMedia");
  assert.equal(createSequence({ id: "s", name: "Main" }).type, "createSequence");
  assert.equal(createTrack({ sequenceId: "s", id: "t", kind: "video" }).kind, "video");
  assert.equal(insertClip({ sequenceId: "s", trackId: "t", id: "c", mediaId: "m", timelineStart: time, sourceStart: time, sourceDuration: new RationalTime(1, 1) }).type, "insertClip");
});

test("emits track lock operations", () => {
  assert.deepEqual(
    setTrackLocked({ sequenceId: "s", trackId: "t", locked: true }),
    { type: "setTrackLocked", sequenceId: "s", trackId: "t", locked: true },
  );
});

test("emits track reorder operations", () => {
  assert.deepEqual(
    reorderTrack({ sequenceId: "s", trackId: "t", destinationIndex: 2 }),
    { type: "reorderTrack", sequenceId: "s", trackId: "t", destinationIndex: 2 },
  );
});

test("emits track visibility and mute operations", () => {
  assert.deepEqual(setTrackVisible({ sequenceId: "s", trackId: "t", visible: false }), { type: "setTrackVisible", sequenceId: "s", trackId: "t", visible: false });
  assert.deepEqual(setTrackMuted({ sequenceId: "s", trackId: "t", muted: true }), { type: "setTrackMuted", sequenceId: "s", trackId: "t", muted: true });
});

test("emits marker operations", () => {
  const marker = addMarker({ sequenceId: "s", id: "m", time: new RationalTime(3, 2), label: "Beat" });
  assert.deepEqual(marker, { type: "addMarker", sequenceId: "s", id: "m", time: { value: 3, timescale: 2 }, label: "Beat" });
  assert.deepEqual(deleteMarker({ sequenceId: "s", markerId: "m" }), { type: "deleteMarker", sequenceId: "s", markerId: "m" });
});

test("emits duplicate clip operation", () => {
  assert.equal(duplicateClip({ sequenceId: "s", trackId: "t", clipId: "c", newClipId: "d", timelineStart: new RationalTime(4, 1) }).type, "duplicateClip");
});

test("emits ripple delete operation", () => {
  assert.deepEqual(rippleDelete({ sequenceId: "s", trackId: "t", clipId: "c" }), { type: "rippleDelete", sequenceId: "s", trackId: "t", clipId: "c" });
});

test("emits transition operations", () => {
  assert.equal(addTransition({ sequenceId: "s", id: "x", outgoingClip: "a", incomingClip: "b", duration: new RationalTime(1, 2) }).type, "addTransition");
  assert.deepEqual(deleteTransition({ sequenceId: "s", transitionId: "x" }), { type: "deleteTransition", sequenceId: "s", transitionId: "x" });
});
