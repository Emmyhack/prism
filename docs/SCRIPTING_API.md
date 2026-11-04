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
