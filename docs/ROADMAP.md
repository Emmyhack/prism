# Roadmap

1. Phase 0: architecture, workspace, exact time, commands, protocol, risk gates.
2. Phase 1: real ingest, basic timeline, preview, persistence, undo, MP4 export.
3. Phase 2: professional timeline, proxies, caches, transitions, audio mixing.
4. Phase 3: Python/JavaScript/TypeScript runtimes, SDKs, Script UI, recipes.
5. Phase 4: validated color, audio, compositing, multicam, retiming.
6. Phase 5: capability-based plugins and extension distribution.
7. Phase 6: optional intelligent tools issuing ordinary reviewed commands.
8. Phase 7: collaboration only after local workflows are reliable.

Command/protocol architecture is built from Phase 0; Phase 3 now has bounded
Python, JavaScript, and TypeScript process paths, reviewed runtime bundles,
permission decisions, recipe execution, SDK parity, transaction debugging, and
a native Studio scripting surface. These feed transactions through the same
host and command engine rather than bypassing visual editing semantics.
Recipe and plugin manifest validation now provide the declarative safety boundary;
runtime loading and distribution remain later milestones.
Content-addressed cache keys and bounded age/size eviction now provide the
storage boundary for future thumbnails, waveforms, and proxies; cache workers
and derived-media generation remain pending.
