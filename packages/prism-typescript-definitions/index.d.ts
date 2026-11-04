/** Versioned protocol declarations shared by Prism TypeScript authors. */
export declare const API_VERSION: "1";

export interface RecipeManifest {
  readonly id: string;
  readonly version: string;
  readonly apiVersion: number;
  readonly language: "python" | "javascript" | "typescript";
  readonly entry: string;
  readonly permissions: readonly string[];
  readonly parameters: Readonly<Record<string, unknown>>;
}

export interface RecipeRequest {
  readonly manifest: RecipeManifest;
  readonly inputs: Readonly<Record<string, unknown>>;
}

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

export interface ImportMediaOperation {
  readonly type: "importMedia";
  readonly id: string;
  readonly sourcePath: string;
}

export interface CreateSequenceOperation {
  readonly type: "createSequence";
  readonly id: string;
  readonly name: string;
}

export interface CreateTrackOperation {
  readonly type: "createTrack";
  readonly sequenceId: string;
  readonly id: string;
  readonly kind: "video" | "audio";
}

export interface SetTrackLockedOperation {
  readonly type: "setTrackLocked";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly locked: boolean;
}

export interface SetTrackVisibleOperation {
  readonly type: "setTrackVisible";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly visible: boolean;
}

export interface SetTrackMutedOperation {
  readonly type: "setTrackMuted";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly muted: boolean;
}

export interface ReorderTrackOperation {
  readonly type: "reorderTrack";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly destinationIndex: number;
}
export interface AddMarkerOperation { readonly type: "addMarker"; readonly sequenceId: string; readonly id: string; readonly time: RationalTime; readonly label: string; }
export interface DeleteMarkerOperation { readonly type: "deleteMarker"; readonly sequenceId: string; readonly markerId: string; }
export interface DuplicateClipOperation { readonly type: "duplicateClip"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; readonly newClipId: string; readonly timelineStart: RationalTime; }
export interface RippleDeleteOperation { readonly type: "rippleDelete"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; }
export interface AddTransitionOperation { readonly type: "addTransition"; readonly sequenceId: string; readonly id: string; readonly outgoingClip: string; readonly incomingClip: string; readonly duration: RationalTime; readonly kind: "dissolve"; }
export interface DeleteTransitionOperation { readonly type: "deleteTransition"; readonly sequenceId: string; readonly transitionId: string; }

export interface InsertClipOperation {
  readonly type: "insertClip";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly id: string;
  readonly mediaId: string;
  readonly timelineStart: RationalTime;
  readonly sourceStart: RationalTime;
  readonly sourceDuration: RationalTime;
}

export interface TrimClipOperation {
  readonly type: "trimClip";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly clipId: string;
  readonly timelineStart: RationalTime;
  readonly sourceStart: RationalTime;
  readonly sourceDuration: RationalTime;
}

export interface SplitClipOperation {
  readonly type: "splitClip";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly clipId: string;
  readonly rightClipId: string;
  readonly timelinePosition: RationalTime;
}

export interface DeleteClipOperation {
  readonly type: "deleteClip";
  readonly sequenceId: string;
  readonly trackId: string;
  readonly clipId: string;
}

export type Operation = ImportMediaOperation | CreateSequenceOperation | CreateTrackOperation | SetTrackLockedOperation | SetTrackVisibleOperation | SetTrackMutedOperation | ReorderTrackOperation | AddMarkerOperation | DeleteMarkerOperation | DuplicateClipOperation | RippleDeleteOperation | AddTransitionOperation | DeleteTransitionOperation | InsertClipOperation | MoveClipOperation | TrimClipOperation | SplitClipOperation | DeleteClipOperation;

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
export declare function setTrackLocked(
  sequenceId: string,
  trackId: string,
  locked: boolean,
): SetTrackLockedOperation;
export declare function setTrackVisible(
  sequenceId: string,
  trackId: string,
  visible: boolean,
): SetTrackVisibleOperation;
export declare function setTrackMuted(
  sequenceId: string,
  trackId: string,
  muted: boolean,
): SetTrackMutedOperation;
export declare function reorderTrack(
  sequenceId: string,
  trackId: string,
  destinationIndex: number,
): ReorderTrackOperation;
export declare function addMarker(sequenceId: string, id: string, time: RationalTime, label: string): AddMarkerOperation;
export declare function deleteMarker(sequenceId: string, markerId: string): DeleteMarkerOperation;
export declare function duplicateClip(sequenceId: string, trackId: string, clipId: string, newClipId: string, timelineStart: RationalTime): DuplicateClipOperation;
export declare function rippleDelete(sequenceId: string, trackId: string, clipId: string): RippleDeleteOperation;
export declare function addTransition(sequenceId: string, id: string, outgoingClip: string, incomingClip: string, duration: RationalTime, kind?: "dissolve"): AddTransitionOperation;
export declare function deleteTransition(sequenceId: string, transitionId: string): DeleteTransitionOperation;
export declare function transaction(
  requestId: string,
  expectedRevision: number,
  label: string,
  operations: readonly Operation[],
  dryRun?: boolean,
): TransactionRequest;

export declare function recipeRequest(
  options: {
    manifest: RecipeManifest;
    inputs?: Readonly<Record<string, unknown>>;
  },
): RecipeRequest;
