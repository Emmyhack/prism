/** Versioned, protocol-first JavaScript authoring helpers for Prism. */
export const API_VERSION = "1";

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

export function transaction({ requestId, expectedRevision, label, operations, dryRun = true }) {
  if (!label?.trim()) throw new TypeError("label must not be empty");
  if (!Number.isInteger(expectedRevision) || expectedRevision < 0) {
    throw new TypeError("expectedRevision must be a nonnegative integer");
  }
  return {
    requestId,
    apiVersion: API_VERSION,
    expectedRevision,
    label,
    dryRun,
    operations,
  };
}
