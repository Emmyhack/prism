# ADR-006: Python execution

Status: Proposed. Run Python in supervised worker processes speaking the shared
protocol. This provides lifecycle and crash isolation and avoids embedding the
GIL in Studio, but is not itself a security sandbox. Environments are explicit,
pinned, and compatibility-checked; arbitrary native dependencies are denied by
default. Add OS isolation, resource limits, cancellation escalation, audit logs,
and clear trust levels before running untrusted code.
