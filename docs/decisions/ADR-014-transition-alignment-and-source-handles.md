# ADR-014: Transition alignment and source handles

Status: Accepted

## Context

Prism previously persisted only a dissolve duration between adjacent clips. Studio
drew the window centered on the cut, but export could not determine or validate
the hidden source ranges needed to render it. Defining dissolves only inside the
visible ranges would contradict normal editing semantics and make preview and
export disagree.

## Decision

Persist one of three typed alignments: `start_at_cut`, `centered`, or
`end_at_cut`. Existing transitions migrate to `centered`.

The duration is the complete dissolve window. Handle requirements are derived:

| Alignment | Before cut | After cut | Incoming preroll | Outgoing postroll |
|---|---:|---:|---:|---:|
| Start at cut | 0 | duration | 0 | duration |
| Centered | duration / 2 | duration / 2 | duration / 2 | duration / 2 |
| End at cut | duration | 0 | duration | 0 |

Exact rational division defines centered placement; no floating-point time is
introduced. Project and command validation reject a transition when its visible
spans or required incoming preroll are unavailable. Export additionally probes
the source duration before encoding to reject missing outgoing postroll.

## Consequences

SQLite storage version 8 adds the alignment column with a centered default.
Commands, JSON protocol, Python, JavaScript and TypeScript representations carry
the same alignment. The sequence exporter renders the derived source windows,
so the transition definition is shared rather than reconstructed differently by
Studio and export.
