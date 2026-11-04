# Scripting API proposal

Python, JavaScript, and TypeScript SDKs are projections of one versioned schema.
They expose equivalent objects and errors while preserving language idioms. The
wire boundary uses JSON initially for debuggability; MessagePack may be added
only after profiling. Protocol DTOs convert to strongly typed Rust commands.

Every transaction carries API version, request ID, expected project revision,
label, dry-run intent, ordered operations, and capability context. Time values
are exact numerator/timescale pairs rather than floating-point seconds. Responses
carry either typed result or structured error
with stable code, message, optional field path, and diagnostics. Mutation batches
run in one command transaction. Dry-run returns planned affected IDs without
committing. Filesystem/network side effects are separate, explicitly authorized
operations and cannot be rolled back with project state.

JavaScript is provisionally a supervised `deno_core` worker with a custom module
loader and no ambient Deno/Node APIs. TypeScript compiles before execution using
a pinned compiler; remote imports are denied. Python is a supervised isolated
worker and SDK client; virtual environments are explicit and capability-scoped.
No runtime is described as fully sandboxed.

## Executable protocol example

The checked-in `examples/script-transaction-dry-run.json` fixture demonstrates
the version-1 transaction envelope. It can be submitted with:

```sh
cargo run -p prism-cli -- script-transaction \
  --project ./edit.prism \
  --request ./examples/script-transaction-dry-run.json
```

The `expectedRevision` must match the loaded project. Set `dryRun` to `false`
only after reviewing the validated operation; committed requests are persisted
through the same command engine used by the visual editor.

The initial Python SDK slice in `packages/prism-python-sdk` builds this envelope
with exact `RationalTime` values. It is a protocol authoring layer; execution,
permissions, and worker isolation remain Prism application responsibilities.

The JavaScript SDK slice in `packages/prism-js-sdk` exposes equivalent
`RationalTime`, `moveClip`, and `transaction` helpers as an ESM package. Both
SDKs intentionally emit the same protocol objects rather than maintaining
separate editing semantics.

The TypeScript module and declarations in `packages/prism-typescript-definitions` expose
the same operation and transaction types, including literal API-version and
operation discriminants for editor autocomplete and compiler diagnostics. Its
typed authoring helpers cover media import, sequence/track creation, clip insert,
track locking/reordering, visibility/mute state, move, trim, split, and delete
operations. The CLI TypeScript path accepts an explicit `tsc` executable, uses
fixed NodeNext/ES2022 compiler settings, and runs the compiled module through
the same bounded Node worker; the checked-in module remains a pure protocol
builder for editor tooling.

Recipe requests now pass through the same Rust scripting boundary: manifest and
input validation produces an execution plan without starting a worker, and
declared parameter defaults are materialized into the plan for every language.
Runtime
startup, capability enforcement, and command execution remain separate stages.
The Python, JavaScript, and TypeScript SDKs expose equivalent recipe-request
builders that emit the same `{ manifest, inputs }` envelope; they do not grant
permissions or bypass host validation.
Worker hosts must call the authorization-aware planning path with an explicit
permission grant set; manifest declarations alone never grant access.
The Rust host boundary also enforces serialized-payload and operation-count
limits, supports cancellation before mutation, and uses one dry-run/commit path
for all command execution. This is host supervision, not a claim of OS-level
sandboxing or a completed Python/JavaScript runtime.

The `run-python-script` and `run-javascript-script` CLI paths now supervise
explicit Python or Node executables,
injects the current project revision as `PRISM_PROJECT_REVISION`, bounds wall
clock and captured output, and parses the script's stdout as one transaction
envelope before invoking `ScriptWorker`. A non-zero exit, timeout, cancellation,
oversized stream, malformed JSON response, stale revision, or command validation
failure prevents project persistence. This is intentionally a trusted-process
boundary; OS-level sandboxing remains future work. The optional `--permissions` sidecar contains
`requested` and `granted` capability arrays; the host rejects missing grants
before spawning and derives `project.read`, `project.write`, and
`media.import` requirements again from the decoded transaction before mutation.
Timeline markers are exposed through shared `addMarker` and `deleteMarker` operations. Marker times use exact rational values, and the core validates labels, ordering, and unique IDs.

Clip duplication is exposed through `duplicateClip`; it reuses the original
media/source range while requiring a new clip ID and exact destination time.
`rippleDelete` removes a clip and shifts later clips on that track left by the
removed duration using checked rational-time arithmetic.

Transitions use `addTransition` and `deleteTransition`. The current supported
kind is `dissolve`; the command layer requires adjacent video clips and a
positive duration that fits both source ranges.
