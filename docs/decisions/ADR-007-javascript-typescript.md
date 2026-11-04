# ADR-007: JavaScript and TypeScript execution

Status: Proposed. Use `deno_core`/V8 in supervised workers for modern JS,
Promises, inspector support, termination, and heap constraints. Expose only
Prism ops; do not provide ambient Deno, Node, filesystem, process, or network
APIs. Compile TypeScript with a pinned compiler before execution and retain
source maps. A custom loader accepts SDK and approved local modules only; remote
imports are denied. Boa remains a fallback if binary size and V8 maintenance
outweigh compatibility after benchmarks.
