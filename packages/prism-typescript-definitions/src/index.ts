export const API_VERSION = "1" as const;

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

export interface RationalTime { readonly value: number; readonly timescale: number; }
export type Operation =
  | ImportMediaOperation | CreateSequenceOperation | CreateTrackOperation | SetTrackLockedOperation | SetTrackVisibleOperation | SetTrackMutedOperation | ReorderTrackOperation | InsertClipOperation
  | MoveClipOperation | SlipClipOperation | TrimClipOperation | SplitClipOperation | DeleteClipOperation | AddMarkerOperation | DeleteMarkerOperation | DuplicateClipOperation | RippleDeleteOperation | AddTransitionOperation | DeleteTransitionOperation;
export interface AddMarkerOperation { readonly type: "addMarker"; readonly sequenceId: string; readonly id: string; readonly time: RationalTime; readonly label: string; }
export interface DeleteMarkerOperation { readonly type: "deleteMarker"; readonly sequenceId: string; readonly markerId: string; }
export interface DuplicateClipOperation { readonly type: "duplicateClip"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; readonly newClipId: string; readonly timelineStart: RationalTime; }
export interface RippleDeleteOperation { readonly type: "rippleDelete"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; }
export interface AddTransitionOperation { readonly type: "addTransition"; readonly sequenceId: string; readonly id: string; readonly outgoingClip: string; readonly incomingClip: string; readonly duration: RationalTime; readonly kind: "dissolve"; }
export interface DeleteTransitionOperation { readonly type: "deleteTransition"; readonly sequenceId: string; readonly transitionId: string; }
export interface ImportMediaOperation { readonly type: "importMedia"; readonly id: string; readonly sourcePath: string; }
export interface CreateSequenceOperation { readonly type: "createSequence"; readonly id: string; readonly name: string; }
export interface CreateTrackOperation { readonly type: "createTrack"; readonly sequenceId: string; readonly id: string; readonly kind: "video" | "audio"; }
export interface SetTrackLockedOperation { readonly type: "setTrackLocked"; readonly sequenceId: string; readonly trackId: string; readonly locked: boolean; }
export interface SetTrackVisibleOperation { readonly type: "setTrackVisible"; readonly sequenceId: string; readonly trackId: string; readonly visible: boolean; }
export interface SetTrackMutedOperation { readonly type: "setTrackMuted"; readonly sequenceId: string; readonly trackId: string; readonly muted: boolean; }
export interface ReorderTrackOperation { readonly type: "reorderTrack"; readonly sequenceId: string; readonly trackId: string; readonly destinationIndex: number; }
export interface InsertClipOperation { readonly type: "insertClip"; readonly sequenceId: string; readonly trackId: string; readonly id: string; readonly mediaId: string; readonly timelineStart: RationalTime; readonly sourceStart: RationalTime; readonly sourceDuration: RationalTime; }
export interface MoveClipOperation { readonly type: "moveClip"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; readonly timelineStart: RationalTime; }
export interface SlipClipOperation { readonly type: "slipClip"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; readonly sourceDelta: RationalTime; }
export interface TrimClipOperation { readonly type: "trimClip"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; readonly timelineStart: RationalTime; readonly sourceStart: RationalTime; readonly sourceDuration: RationalTime; }
export interface SplitClipOperation { readonly type: "splitClip"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; readonly rightClipId: string; readonly timelinePosition: RationalTime; }
export interface DeleteClipOperation { readonly type: "deleteClip"; readonly sequenceId: string; readonly trackId: string; readonly clipId: string; }
export interface TransactionRequest { readonly requestId: string; readonly apiVersion: typeof API_VERSION; readonly expectedRevision: number; readonly label: string; readonly dryRun: boolean; readonly operations: readonly Operation[]; }

export function recipeRequest(options: { manifest: RecipeManifest; inputs?: Readonly<Record<string, unknown>> }): RecipeRequest {
  if (!options?.manifest || typeof options.manifest !== "object") throw new TypeError("manifest must be an object");
  return { manifest: options.manifest, inputs: options.inputs ?? {} };
}

export function rationalTime(value: number, timescale: number): RationalTime {
  if (!Number.isInteger(value) || !Number.isInteger(timescale) || timescale <= 0) throw new TypeError("invalid rational time");
  return { value, timescale };
}
export function importMedia(id: string, sourcePath: string): ImportMediaOperation { return { type: "importMedia", id, sourcePath }; }
export function createSequence(id: string, name: string): CreateSequenceOperation { return { type: "createSequence", id, name }; }
export function createTrack(sequenceId: string, id: string, kind: "video" | "audio"): CreateTrackOperation { return { type: "createTrack", sequenceId, id, kind }; }
export function setTrackLocked(sequenceId: string, trackId: string, locked: boolean): SetTrackLockedOperation {
  if (typeof locked !== "boolean") throw new TypeError("locked must be a boolean");
  return { type: "setTrackLocked", sequenceId, trackId, locked };
}
export function setTrackVisible(sequenceId: string, trackId: string, visible: boolean): SetTrackVisibleOperation {
  if (typeof visible !== "boolean") throw new TypeError("visible must be a boolean");
  return { type: "setTrackVisible", sequenceId, trackId, visible };
}
export function setTrackMuted(sequenceId: string, trackId: string, muted: boolean): SetTrackMutedOperation {
  if (typeof muted !== "boolean") throw new TypeError("muted must be a boolean");
  return { type: "setTrackMuted", sequenceId, trackId, muted };
}
export function reorderTrack(sequenceId: string, trackId: string, destinationIndex: number): ReorderTrackOperation {
  if (!Number.isInteger(destinationIndex) || destinationIndex < 0) throw new TypeError("destinationIndex must be a nonnegative integer");
  return { type: "reorderTrack", sequenceId, trackId, destinationIndex };
}
export function addMarker(sequenceId: string, id: string, time: RationalTime, label: string): AddMarkerOperation {
  if (!label.trim()) throw new TypeError("label must not be empty");
  return { type: "addMarker", sequenceId, id, time, label };
}
export function deleteMarker(sequenceId: string, markerId: string): DeleteMarkerOperation { return { type: "deleteMarker", sequenceId, markerId }; }
export function duplicateClip(sequenceId: string, trackId: string, clipId: string, newClipId: string, timelineStart: RationalTime): DuplicateClipOperation {
  return { type: "duplicateClip", sequenceId, trackId, clipId, newClipId, timelineStart };
}
export function rippleDelete(sequenceId: string, trackId: string, clipId: string): RippleDeleteOperation { return { type: "rippleDelete", sequenceId, trackId, clipId }; }
export function addTransition(sequenceId: string, id: string, outgoingClip: string, incomingClip: string, duration: RationalTime, kind: "dissolve" = "dissolve"): AddTransitionOperation { return { type: "addTransition", sequenceId, id, outgoingClip, incomingClip, duration, kind }; }
export function deleteTransition(sequenceId: string, transitionId: string): DeleteTransitionOperation { return { type: "deleteTransition", sequenceId, transitionId }; }
export function insertClip(sequenceId: string, trackId: string, id: string, mediaId: string, timelineStart: RationalTime, sourceStart: RationalTime, sourceDuration: RationalTime): InsertClipOperation {
  return { type: "insertClip", sequenceId, trackId, id, mediaId, timelineStart, sourceStart, sourceDuration };
}
export function moveClip(sequenceId: string, trackId: string, clipId: string, timelineStart: RationalTime): MoveClipOperation { return { type: "moveClip", sequenceId, trackId, clipId, timelineStart }; }
export function slipClip(sequenceId: string, trackId: string, clipId: string, sourceDelta: RationalTime): SlipClipOperation { return { type: "slipClip", sequenceId, trackId, clipId, sourceDelta }; }
export function trimClip(sequenceId: string, trackId: string, clipId: string, timelineStart: RationalTime, sourceStart: RationalTime, sourceDuration: RationalTime): TrimClipOperation {
  return { type: "trimClip", sequenceId, trackId, clipId, timelineStart, sourceStart, sourceDuration };
}
export function splitClip(sequenceId: string, trackId: string, clipId: string, rightClipId: string, timelinePosition: RationalTime): SplitClipOperation {
  return { type: "splitClip", sequenceId, trackId, clipId, rightClipId, timelinePosition };
}
export function deleteClip(sequenceId: string, trackId: string, clipId: string): DeleteClipOperation { return { type: "deleteClip", sequenceId, trackId, clipId }; }
export function transaction(requestId: string, expectedRevision: number, label: string, operations: readonly Operation[], dryRun = true): TransactionRequest {
  if (!label.trim()) throw new TypeError("label must not be empty");
  if (!Number.isInteger(expectedRevision) || expectedRevision < 0) throw new TypeError("expectedRevision must be a nonnegative integer");
  if (!Array.isArray(operations)) throw new TypeError("operations must be an array");
  if (typeof dryRun !== "boolean") throw new TypeError("dryRun must be a boolean");
  return { requestId, apiVersion: API_VERSION, expectedRevision, label, dryRun, operations };
}
