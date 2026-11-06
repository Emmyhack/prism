/** Versioned, protocol-first JavaScript authoring helpers for Prism. */
export const API_VERSION = "1";

/** Builds the shared recipe invocation envelope; host validation is separate. */
export function recipeRequest({ manifest, inputs = {} }) {
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)) {
    throw new TypeError("manifest must be an object");
  }
  if (!inputs || typeof inputs !== "object" || Array.isArray(inputs)) {
    throw new TypeError("inputs must be an object");
  }
  return { manifest, inputs };
}

export function importMedia({ id, sourcePath }) {
  return { type: "importMedia", id, sourcePath };
}

export function setMediaProxy({ mediaId, proxyPath }) {
  if (proxyPath !== null && typeof proxyPath !== "string") throw new TypeError("proxyPath must be a string or null");
  if (typeof proxyPath === "string" && !proxyPath.trim()) throw new TypeError("proxyPath must not be empty");
  return { type: "setMediaProxy", mediaId, proxyPath };
}

export function createSequence({ id, name }) {
  return { type: "createSequence", id, name };
}

export function createTrack({ sequenceId, id, kind }) {
  if (kind !== "video" && kind !== "audio") throw new TypeError("kind must be video or audio");
  return { type: "createTrack", sequenceId, id, kind };
}

export function setTrackLocked({ sequenceId, trackId, locked }) {
  if (typeof locked !== "boolean") throw new TypeError("locked must be a boolean");
  return { type: "setTrackLocked", sequenceId, trackId, locked };
}

export function setTrackVisible({ sequenceId, trackId, visible }) {
  if (typeof visible !== "boolean") throw new TypeError("visible must be a boolean");
  return { type: "setTrackVisible", sequenceId, trackId, visible };
}

export function setTrackMuted({ sequenceId, trackId, muted }) {
  if (typeof muted !== "boolean") throw new TypeError("muted must be a boolean");
  return { type: "setTrackMuted", sequenceId, trackId, muted };
}

export function reorderTrack({ sequenceId, trackId, destinationIndex }) {
  if (!Number.isInteger(destinationIndex) || destinationIndex < 0) {
    throw new TypeError("destinationIndex must be a nonnegative integer");
  }
  return { type: "reorderTrack", sequenceId, trackId, destinationIndex };
}

export function addMarker({ sequenceId, id, time, label }) {
  if (!(time instanceof RationalTime)) throw new TypeError("time must be RationalTime");
  if (!label?.trim()) throw new TypeError("label must not be empty");
  return { type: "addMarker", sequenceId, id, time: time.toJSON(), label };
}

export function deleteMarker({ sequenceId, markerId }) {
  return { type: "deleteMarker", sequenceId, markerId };
}

export function duplicateClip({ sequenceId, trackId, clipId, newClipId, timelineStart }) {
  if (!(timelineStart instanceof RationalTime)) throw new TypeError("timelineStart must be RationalTime");
  return { type: "duplicateClip", sequenceId, trackId, clipId, newClipId, timelineStart: timelineStart.toJSON() };
}

export function rippleDelete({ sequenceId, trackId, clipId }) {
  return { type: "rippleDelete", sequenceId, trackId, clipId };
}

export function addTransition({ sequenceId, id, outgoingClip, incomingClip, duration, kind = "dissolve" }) {
  if (!(duration instanceof RationalTime)) throw new TypeError("duration must be RationalTime");
  if (kind !== "dissolve") throw new TypeError("kind must be dissolve");
  return { type: "addTransition", sequenceId, id, outgoingClip, incomingClip, duration: duration.toJSON(), kind };
}

export function deleteTransition({ sequenceId, transitionId }) {
  return { type: "deleteTransition", sequenceId, transitionId };
}

export function insertClip({ sequenceId, trackId, id, mediaId, timelineStart, sourceStart, sourceDuration }) {
  for (const time of [timelineStart, sourceStart, sourceDuration]) {
    if (!(time instanceof RationalTime)) throw new TypeError("clip times must be RationalTime");
  }
  return {
    type: "insertClip", sequenceId, trackId, id, mediaId,
    timelineStart: timelineStart.toJSON(), sourceStart: sourceStart.toJSON(),
    sourceDuration: sourceDuration.toJSON(),
  };
}

export class RationalTime {
  constructor(value, timescale) {
    if (!Number.isInteger(value) || !Number.isInteger(timescale) || timescale <= 0) {
      throw new TypeError("value must be an integer and timescale must be greater than zero");
    }
    this.value = value;
    this.timescale = timescale;
  }

  toJSON() {
    return { value: this.value, timescale: this.timescale };
  }
}

export function moveClip({ sequenceId, trackId, clipId, timelineStart }) {
  if (!(timelineStart instanceof RationalTime)) {
    throw new TypeError("timelineStart must be a RationalTime");
  }
  return {
    type: "moveClip",
    sequenceId,
    trackId,
    clipId,
    timelineStart: timelineStart.toJSON(),
  };
}

export function slipClip({ sequenceId, trackId, clipId, sourceDelta }) {
  if (!(sourceDelta instanceof RationalTime)) throw new TypeError("sourceDelta must be RationalTime");
  return { type: "slipClip", sequenceId, trackId, clipId, sourceDelta: sourceDelta.toJSON() };
}

export function rollEdit({ sequenceId, trackId, leftClipId, rightClipId, delta }) {
  if (!(delta instanceof RationalTime)) throw new TypeError("delta must be RationalTime");
  return { type: "rollEdit", sequenceId, trackId, leftClipId, rightClipId, delta: delta.toJSON() };
}

export function slideEdit({ sequenceId, trackId, previousClipId, clipId, nextClipId, delta }) {
  if (!(delta instanceof RationalTime)) throw new TypeError("delta must be RationalTime");
  return { type: "slideEdit", sequenceId, trackId, previousClipId, clipId, nextClipId, delta: delta.toJSON() };
}

export function trimClip({ sequenceId, trackId, clipId, timelineStart, sourceStart, sourceDuration }) {
  for (const time of [timelineStart, sourceStart, sourceDuration]) {
    if (!(time instanceof RationalTime)) throw new TypeError("clip times must be RationalTime");
  }
  return {
    type: "trimClip", sequenceId, trackId, clipId,
    timelineStart: timelineStart.toJSON(), sourceStart: sourceStart.toJSON(),
    sourceDuration: sourceDuration.toJSON(),
  };
}

export function splitClip({ sequenceId, trackId, clipId, rightClipId, timelinePosition }) {
  if (!(timelinePosition instanceof RationalTime)) throw new TypeError("timelinePosition must be a RationalTime");
  return {
    type: "splitClip", sequenceId, trackId, clipId, rightClipId,
    timelinePosition: timelinePosition.toJSON(),
  };
}

export function deleteClip({ sequenceId, trackId, clipId }) {
  return { type: "deleteClip", sequenceId, trackId, clipId };
}

export function transaction({ requestId, expectedRevision, label, operations, dryRun = true }) {
  if (!label?.trim()) throw new TypeError("label must not be empty");
  if (!Number.isInteger(expectedRevision) || expectedRevision < 0) {
    throw new TypeError("expectedRevision must be a nonnegative integer");
  }
  if (!Array.isArray(operations)) throw new TypeError("operations must be an array");
  if (typeof dryRun !== "boolean") throw new TypeError("dryRun must be a boolean");
  return {
    requestId,
    apiVersion: API_VERSION,
    expectedRevision,
    label,
    dryRun,
    operations,
  };
}
