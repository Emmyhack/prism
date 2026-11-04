# Performance baselines

No measurements exist yet. Baselines must record machine, OS, build profile,
media fingerprints, codec build, GPU/driver, warm/cold cache, and repetition
count. Initial budgets are hypotheses, not achieved results:

| Operation | Phase 1 target |
|---|---:|
| In-memory single clip command p95 | < 2 ms |
| Timeline viewport input response p95 | < 16 ms |
| Warm cached 1080p frame seek p95 | < 50 ms |
| Script bridge overhead per batched command p95 | < 1 ms |

Representative fixtures will include short 1080p, long interview, mixed-rate,
4K multitrack, large library, effects-heavy, and long export projects.
