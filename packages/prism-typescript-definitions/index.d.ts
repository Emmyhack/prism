/** Versioned protocol declarations shared by Prism TypeScript authors. */
export declare const API_VERSION: "1";

export interface RationalTime {
  readonly value: number;
  readonly timescale: number;
}

export interface MoveClipOperation {
  readonly type: "moveClip";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly clipId: string;
  readonly timelineStart: RationalTime;
}

export type Operation = MoveClipOperation;

export interface TransactionRequest {
  readonly requestId: string;
  readonly apiVersion: typeof API_VERSION;
  readonly expectedRevision: number;
  readonly label: string;
  readonly dryRun: boolean;
  readonly operations: readonly Operation[];
}

export declare function rationalTime(value: number, timescale: number): RationalTime;
export declare function moveClip(
  sequenceId: string,
  trackId: string,
  clipId: string,
  timelineStart: RationalTime,
): MoveClipOperation;
export declare function transaction(
  requestId: string,
  expectedRevision: number,
  label: string,
  operations: readonly Operation[],
  dryRun?: boolean,
): TransactionRequest;
