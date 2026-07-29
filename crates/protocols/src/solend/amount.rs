//! Unit-tagged amount types used by the pure Solend instruction builders.
//!
//! Provenance: `crates/gateway/src/lending/types.rs` at
//! `5f4accee9e22346f1aed1409a34e3e4a0265dc66`.
//!
//! These wrappers are copied rather than replaced with bare integers because
//! confusing underlying-token units with collateral-token units must remain a
//! compile-time error.

/// Base units of an SPL mint (for example, USDC micro-units).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnderlyingAmount(u64);

impl UnderlyingAmount {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Protocol collateral-token (c-token) base units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollateralTokenAmount(u64);

impl CollateralTokenAmount {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_tags_preserve_raw_values() {
        assert_eq!(UnderlyingAmount::new(42).raw(), 42);
        assert_eq!(CollateralTokenAmount::new(84).raw(), 84);
        assert_eq!(UnderlyingAmount::ZERO.raw(), 0);
        assert_eq!(CollateralTokenAmount::ZERO.raw(), 0);
    }
}
