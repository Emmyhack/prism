//! Shared test fixtures and assertions for Prism crates.

/// Returns the deterministic seed reserved for baseline fixtures.
#[must_use]
pub const fn baseline_seed() -> u64 {
    0x0050_5249_534D
}
