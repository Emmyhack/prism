# Security model

## Trust boundaries

Media, project packages, scripts, recipes, plugins, dependencies, and network
responses are untrusted. The project command engine is the integrity boundary.
Native codec libraries are high-risk FFI and run in dedicated workers where
practical. Unsafe Rust is forbidden workspace-wide until an exception is
documented, narrowly scoped, and reviewed.

Scripts declare capabilities (`project.read`, `project.write`, `media.read`,
`media.import`, `filesystem.read`, `filesystem.write`, `network.access`,
`export.create`, `plugin.invoke`). Paths are canonicalized and checked against
granted roots after symlink resolution. Network, process, native module, and
remote package access default to denied. Time, memory, output, and command-count
budgets are enforced by supervisors where the OS/runtime permits.

Python and JavaScript workers provide crash containment, not a verified security
sandbox. OS-specific profiles are required: macOS sandbox profiles/seatbelt
evaluation, Windows AppContainer/restricted tokens, and Linux namespaces plus
seccomp/landlock evaluation. Limitations must be displayed to users.

Threat-model test cases include path traversal, symlink escape, decompression
bombs, malformed codec input, cyclic project graphs, transaction exhaustion,
infinite script loops, dependency confusion, plugin crashes, and disk-full save.
