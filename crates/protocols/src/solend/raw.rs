//! Minimal Solend obligation decoder for the Phase 1 read-only position tool.
//!
//! Adapted from the predecessor repository file
//! `crates/gateway/src/integrations/solend/raw.rs` at commit
//! `61d353a3fa2a35f053d7337b9b51661ad1df88e6`.
//! The on-chain layout is pinned to Solend's `mainnet` branch commit
//! `d04ce00bbf4356c4fd32b3be38eb9760b696bb3e`.
//!
//! Deliberately omitted from the predecessor module: the reserve layout and
//! decoder, oracle-sentinel handling, reserve liquidity/rate math, mapping
//! helpers, and reserve fixtures. The Phase 1 slice only discovers and decodes
//! obligation accounts; Phase 3 moved that protocol-specific subset here
//! without widening its behavior.

use solana_sdk::pubkey::Pubkey;

/// Solend token-lending program id on mainnet.
pub const SOLEND_PROGRAM_ID_BS58: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";

// Deployed mainnet Obligation layout:
//   0     1    version
//   1     8    last_update.slot
//   9     1    last_update.stale
//   10    32   lending_market
//   42    32   owner
//   74    64   value fields (not consumed here)
//   138   50   mainnet extension fields
//   188   14   true zero padding
//   202   1    deposits_len
//   203   1    borrows_len
//   204   1096 deposits (88 bytes each), then borrows (112 bytes each)
pub const OBLIGATION_LEN: usize = 1300;
pub const OBLIGATION_OWNER_OFFSET: usize = 42;

const OBLIGATION_COLLATERAL_LEN: usize = 88;
const OBLIGATION_LIQUIDITY_LEN: usize = 112;

const OBL_VERSION_OFF: usize = 0;
const OBL_LAST_UPDATE_SLOT_OFF: usize = 1;
const OBL_LAST_UPDATE_STALE_OFF: usize = 9;
const OBL_LENDING_MARKET_OFF: usize = 10;

const OBL_BORROWED_VALUE_UPPER_BOUND_OFF: usize = 138;
const OBL_BORROWING_ISOLATED_ASSET_OFF: usize = 154;
const OBL_SUPER_UNHEALTHY_BORROW_VALUE_OFF: usize = 155;
const OBL_UNWEIGHTED_BORROWED_VALUE_OFF: usize = 171;
const OBL_CLOSEABLE_OFF: usize = 187;

const OBL_PADDING_OFF: usize = 188;
const OBL_PADDING_LEN: usize = 14;
const OBL_DEPOSITS_LEN_OFF: usize = 202;
const OBL_BORROWS_LEN_OFF: usize = 203;
const OBL_DATA_FLAT_OFF: usize = 204;
const OBL_DATA_FLAT_LEN: usize = OBLIGATION_LEN - OBL_DATA_FLAT_OFF;

const OBL_COLL_DEPOSIT_RESERVE_OFF: usize = 0;
const OBL_COLL_DEPOSITED_AMOUNT_OFF: usize = 32;
const OBL_LIQ_BORROW_RESERVE_OFF: usize = 0;
const OBL_LIQ_BORROWED_AMOUNT_WADS_OFF: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolendObligationRaw {
    pub version: u8,
    pub last_update_slot: u64,
    pub last_update_stale: bool,
    pub lending_market: Pubkey,
    pub owner: Pubkey,
    pub deposits: Vec<SolendObligationCollateralRaw>,
    pub borrows: Vec<SolendObligationLiquidityRaw>,
    pub borrowed_value_upper_bound_wads: u128,
    pub borrowing_isolated_asset: bool,
    pub super_unhealthy_borrow_value_wads: u128,
    pub unweighted_borrowed_value_wads: u128,
    pub closeable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolendObligationCollateralRaw {
    pub deposit_reserve: Pubkey,
    /// Raw cToken units from the obligation account.
    pub deposited_amount: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolendObligationLiquidityRaw {
    pub borrow_reserve: Pubkey,
    /// Wad-scaled raw amount. This Phase 1 tool does not expose an estimate.
    pub borrowed_amount_wads: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("obligation bytes wrong length: expected {OBLIGATION_LEN}, got {0}")]
    WrongSize(usize),
    #[error("obligation padding at bytes 188..202 is not all zero")]
    PaddingNonZero,
    #[error(
        "obligation arrays overflow data_flat: deposits_len={deposits}, borrows_len={borrows}"
    )]
    ArrayOverflow { deposits: u8, borrows: u8 },
    #[error("obligation stale bit at offset 9 is {0}, expected 0 or 1")]
    StaleBitInvalid(u8),
    #[error("obligation bool at offset {offset} is {value}, expected 0 or 1 (field: {field})")]
    BoolInvalid {
        offset: usize,
        value: u8,
        field: &'static str,
    },
}

fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn read_u128_le(data: &[u8], offset: usize) -> u128 {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&data[offset..offset + 16]);
    u128::from_le_bytes(bytes)
}

fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&data[offset..offset + 32]);
    Pubkey::new_from_array(bytes)
}

fn read_bool(data: &[u8], offset: usize) -> Option<bool> {
    match data[offset] {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Decode the read-model subset of a deployed-mainnet Solend obligation.
pub fn decode_obligation(data: &[u8]) -> Result<SolendObligationRaw, DecodeError> {
    if data.len() != OBLIGATION_LEN {
        return Err(DecodeError::WrongSize(data.len()));
    }

    if data[OBL_PADDING_OFF..OBL_PADDING_OFF + OBL_PADDING_LEN]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(DecodeError::PaddingNonZero);
    }

    let last_update_stale = read_bool(data, OBL_LAST_UPDATE_STALE_OFF).ok_or(
        DecodeError::StaleBitInvalid(data[OBL_LAST_UPDATE_STALE_OFF]),
    )?;
    let borrowing_isolated_asset =
        read_bool(data, OBL_BORROWING_ISOLATED_ASSET_OFF).ok_or(DecodeError::BoolInvalid {
            offset: OBL_BORROWING_ISOLATED_ASSET_OFF,
            value: data[OBL_BORROWING_ISOLATED_ASSET_OFF],
            field: "borrowing_isolated_asset",
        })?;
    let closeable = read_bool(data, OBL_CLOSEABLE_OFF).ok_or(DecodeError::BoolInvalid {
        offset: OBL_CLOSEABLE_OFF,
        value: data[OBL_CLOSEABLE_OFF],
        field: "closeable",
    })?;

    let deposits_len = data[OBL_DEPOSITS_LEN_OFF];
    let borrows_len = data[OBL_BORROWS_LEN_OFF];
    let total_array_bytes = usize::from(deposits_len) * OBLIGATION_COLLATERAL_LEN
        + usize::from(borrows_len) * OBLIGATION_LIQUIDITY_LEN;
    if total_array_bytes > OBL_DATA_FLAT_LEN {
        return Err(DecodeError::ArrayOverflow {
            deposits: deposits_len,
            borrows: borrows_len,
        });
    }

    let mut deposits = Vec::with_capacity(usize::from(deposits_len));
    for index in 0..usize::from(deposits_len) {
        let base = OBL_DATA_FLAT_OFF + index * OBLIGATION_COLLATERAL_LEN;
        deposits.push(SolendObligationCollateralRaw {
            deposit_reserve: read_pubkey(data, base + OBL_COLL_DEPOSIT_RESERVE_OFF),
            deposited_amount: read_u64_le(data, base + OBL_COLL_DEPOSITED_AMOUNT_OFF),
        });
    }

    let borrows_start = OBL_DATA_FLAT_OFF + usize::from(deposits_len) * OBLIGATION_COLLATERAL_LEN;
    let mut borrows = Vec::with_capacity(usize::from(borrows_len));
    for index in 0..usize::from(borrows_len) {
        let base = borrows_start + index * OBLIGATION_LIQUIDITY_LEN;
        borrows.push(SolendObligationLiquidityRaw {
            borrow_reserve: read_pubkey(data, base + OBL_LIQ_BORROW_RESERVE_OFF),
            borrowed_amount_wads: read_u128_le(data, base + OBL_LIQ_BORROWED_AMOUNT_WADS_OFF),
        });
    }

    Ok(SolendObligationRaw {
        version: data[OBL_VERSION_OFF],
        last_update_slot: read_u64_le(data, OBL_LAST_UPDATE_SLOT_OFF),
        last_update_stale,
        lending_market: read_pubkey(data, OBL_LENDING_MARKET_OFF),
        owner: read_pubkey(data, OBLIGATION_OWNER_OFFSET),
        deposits,
        borrows,
        borrowed_value_upper_bound_wads: read_u128_le(data, OBL_BORROWED_VALUE_UPPER_BOUND_OFF),
        borrowing_isolated_asset,
        super_unhealthy_borrow_value_wads: read_u128_le(data, OBL_SUPER_UNHEALTHY_BORROW_VALUE_OFF),
        unweighted_borrowed_value_wads: read_u128_le(data, OBL_UNWEIGHTED_BORROWED_VALUE_OFF),
        closeable,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub struct ObligationTestFixture {
    pub data: Vec<u8>,
    pub owner: Pubkey,
    pub lending_market: Pubkey,
    pub deposit_reserve: Pubkey,
    pub deposited_amount: u64,
}

/// Fixed one-deposit fixture matching the deployed mainnet account shape seen
/// by the predecessor's Slice 3G regression test.
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub fn one_deposit_test_fixture() -> ObligationTestFixture {
    const MAIN_POOL_LENDING_MARKET: &str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
    const MAIN_POOL_USDC_RESERVE: &str = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
    const LAST_UPDATE_SLOT: u64 = 415_083_795;
    const DEPOSITED_AMOUNT: u64 = 772;

    let owner = Pubkey::new_from_array([0x42; 32]);
    let lending_market: Pubkey = MAIN_POOL_LENDING_MARKET.parse().expect("fixture market");
    let deposit_reserve: Pubkey = MAIN_POOL_USDC_RESERVE.parse().expect("fixture reserve");
    let mut data = vec![0_u8; OBLIGATION_LEN];
    data[OBL_VERSION_OFF] = 1;
    data[OBL_LAST_UPDATE_SLOT_OFF..OBL_LAST_UPDATE_SLOT_OFF + 8]
        .copy_from_slice(&LAST_UPDATE_SLOT.to_le_bytes());
    data[OBL_LAST_UPDATE_STALE_OFF] = 1;
    data[OBL_LENDING_MARKET_OFF..OBL_LENDING_MARKET_OFF + 32]
        .copy_from_slice(&lending_market.to_bytes());
    data[OBLIGATION_OWNER_OFFSET..OBLIGATION_OWNER_OFFSET + 32].copy_from_slice(&owner.to_bytes());
    data[OBL_DEPOSITS_LEN_OFF] = 1;
    data[OBL_BORROWS_LEN_OFF] = 0;
    data[OBL_DATA_FLAT_OFF..OBL_DATA_FLAT_OFF + 32].copy_from_slice(&deposit_reserve.to_bytes());
    data[OBL_DATA_FLAT_OFF + OBL_COLL_DEPOSITED_AMOUNT_OFF
        ..OBL_DATA_FLAT_OFF + OBL_COLL_DEPOSITED_AMOUNT_OFF + 8]
        .copy_from_slice(&DEPOSITED_AMOUNT.to_le_bytes());

    ObligationTestFixture {
        data,
        owner,
        lending_market,
        deposit_reserve,
        deposited_amount: DEPOSITED_AMOUNT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_mainnet_shape_obligation_decodes() {
        let fixture = one_deposit_test_fixture();
        let obligation = decode_obligation(&fixture.data).expect("fixture must decode");

        assert_eq!(fixture.data.len(), OBLIGATION_LEN);
        assert_eq!(obligation.version, 1);
        assert_eq!(obligation.last_update_slot, 415_083_795);
        assert!(obligation.last_update_stale);
        assert_eq!(obligation.owner, fixture.owner);
        assert_eq!(obligation.lending_market, fixture.lending_market);
        assert_eq!(obligation.deposits.len(), 1);
        assert_eq!(
            obligation.deposits[0].deposit_reserve,
            fixture.deposit_reserve
        );
        assert_eq!(
            obligation.deposits[0].deposited_amount,
            fixture.deposited_amount
        );
        assert!(obligation.borrows.is_empty());
    }

    #[test]
    fn wrong_size_and_nonzero_true_padding_fail_closed() {
        assert_eq!(
            decode_obligation(&vec![0_u8; OBLIGATION_LEN - 1]),
            Err(DecodeError::WrongSize(OBLIGATION_LEN - 1))
        );

        let mut fixture = one_deposit_test_fixture();
        fixture.data[OBL_PADDING_OFF + OBL_PADDING_LEN - 1] = 1;
        assert_eq!(
            decode_obligation(&fixture.data),
            Err(DecodeError::PaddingNonZero)
        );
    }

    #[test]
    fn invalid_bool_and_array_overflow_fail_closed() {
        let mut fixture = one_deposit_test_fixture();
        fixture.data[OBL_CLOSEABLE_OFF] = 2;
        assert_eq!(
            decode_obligation(&fixture.data),
            Err(DecodeError::BoolInvalid {
                offset: OBL_CLOSEABLE_OFF,
                value: 2,
                field: "closeable",
            })
        );

        let mut fixture = one_deposit_test_fixture();
        fixture.data[OBL_DEPOSITS_LEN_OFF] = 15;
        fixture.data[OBL_BORROWS_LEN_OFF] = 5;
        assert_eq!(
            decode_obligation(&fixture.data),
            Err(DecodeError::ArrayOverflow {
                deposits: 15,
                borrows: 5,
            })
        );
    }
}
