# Initial risk register

| Risk | Impact | Mitigation / gate |
|---|---|---|
| Slint lacks pro-editor primitives | High | Timeline/docking/accessibility prototype; Qt fallback |
| Codec patent or license conflict | Critical | Audited build matrix and legal review before shipping |
| FFmpeg unsafe parser compromise | Critical | Minimal adapter, fuzzing, worker isolation where viable |
| CPU/GPU render semantic drift | High | Shared graph semantics and golden parity tests |
| Realtime audio underruns | High | Dedicated allocation-free graph and xrun benchmarks |
| Script sandbox overclaim | Critical | Capability deny-default, OS profiles, document limitations |
| V8 build size/update burden | Medium | Benchmark deno_core versus Boa before runtime lock |
| Project corruption/migration loss | Critical | SQLite transactions, backups, crash/disk-full tests |
| Cross-platform native divergence | High | Adapters and native CI from first implemented workflow |
| Scope overwhelms delivery | High | Acceptance-gated vertical slices; no decorative shells |
