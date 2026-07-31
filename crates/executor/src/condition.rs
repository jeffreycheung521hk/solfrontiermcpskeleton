use claw_types::stage2_watch_rule::Comparison;

/// One basis point expressed in Solend's 18-decimal WAD domain.
pub const BPS_WAD: u128 = 100_000_000_000_000;

/// Compare a full-precision WAD observation with an integer-bps threshold.
///
/// The decision never uses a floor-rounded bps display value. For example,
/// `50 * BPS_WAD + 1` is strictly greater than a 50-bps threshold.
pub fn compare_wad(observed_wad: u128, comparison: Comparison, threshold_bps: u32) -> bool {
    let threshold_wad = u128::from(threshold_bps) * BPS_WAD;
    match comparison {
        Comparison::Lt => observed_wad < threshold_wad,
        Comparison::Lte => observed_wad <= threshold_wad,
        Comparison::Gt => observed_wad > threshold_wad,
        Comparison::Gte => observed_wad >= threshold_wad,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_wad_is_not_lost_to_bps_flooring() {
        let threshold = 50 * BPS_WAD;
        assert!(compare_wad(threshold + 1, Comparison::Gt, 50));
        assert!(!compare_wad(threshold, Comparison::Gt, 50));
        assert!(compare_wad(threshold, Comparison::Gte, 50));
        assert!(compare_wad(threshold - 1, Comparison::Lt, 50));
        assert!(!compare_wad(threshold, Comparison::Lt, 50));
        assert!(compare_wad(threshold, Comparison::Lte, 50));
    }
}
