# ADR-010: Audio processing

Status: Proposed. Build a pull-based, fixed-block audio graph on a dedicated
realtime thread. Callbacks perform no allocation, blocking locks, file I/O, or
logging. Control changes cross bounded queues and parameter ramps prevent clicks.
Use native device adapters per platform through an abstraction. Gate on xrun,
latency, resampling, sync, routing, loudness, and reference-signal tests.
