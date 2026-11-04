# ADR-009: Color management

Status: Proposed. Use explicit input, working, display, and output transforms.
Evaluate OpenColorIO for ACES/interchange workflows while implementing required
linear-light compositing and GPU transforms consistently. Preserve source color
metadata and HDR mastering data. Gate on numerical comparisons against published
reference transforms, GPU/CPU parity, scopes, and calibrated display limitations.
