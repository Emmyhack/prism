# Milestone P0.2 integrity slice

Date: 2026-11-04. Status: Implemented and tested; not production ready.

## Outcome

The authoritative project model now owns integrity validation instead of relying
on command or storage adapters to happen to construct valid state. Validation
covers project and sequence names, map-key identity, source paths, globally
unique track/clip IDs, existing media references, nonnegative sequence/source
starts, positive durations, ordered clips, overlap, and exact-time overflow.

Both successful command staging and persistence restoration run the validator.
Invalid staged state is discarded before publication. Invalid restored state is
rejected before becoming a `Project`.

The first proptest suite generates rational values and timebases and verifies
exact addition is commutative and reversible by subtraction.

## Verification

- Formatting passes.
- Strict workspace Clippy passes with warnings denied.
- 25 workspace tests pass, including generated property cases.

## Remaining limitations

The validator does not yet cover effects, nested sequences, linked A/V, speed
maps, or other model types that have not been introduced. Property testing must
expand from exact time into generated command sequences and persistence round
trips. Media duration bounds cannot be checked before truthful media probing is
available.

## Next milestone

Begin `prism-media` with explicit runtime capability detection and an ffprobe
metadata adapter. Use actual redistributable fixtures, distinguish VFR evidence
from nominal frame rate, and do not promote Homebrew's development FFmpeg build
to a distributable product dependency.
