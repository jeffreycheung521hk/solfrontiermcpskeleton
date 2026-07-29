//! Minimal Solend account decoders for the read-only position tool and bounded
//! executor.
//!
//! Adapted from the predecessor repository file
//! `crates/gateway/src/integrations/solend/raw.rs` at commit
//! `61d353a3fa2a35f053d7337b9b51661ad1df88e6`.
//! The on-chain layout is pinned to Solend's `mainnet` branch commit
//! `d04ce00bbf4356c4fd32b3be38eb9760b696bb3e`.
//!
//! Phase 3's reserve prerequisite adds only the predecessor executor's
//! eight-field reserve projection: lending-market identity, liquidity
//! mint/decimals/supply, both oracle accounts, and collateral mint/supply.
//! Deliberately omitted from the predecessor module: reserve liquidity and
//! rate math, deposit-limit/headroom policy, oracle-sentinel interpretation,
//! mapping helpers, and every reserve extension field not consumed by the
//! executor. The decoder preserves the predecessor's exact offsets and
//! fail-closed length/stale-byte checks without widening execution policy.

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

// Deployed mainnet Reserve layout. This is an execution-only projection of
// the 619-byte account, not a complete Reserve representation:
//   9     1    last_update.stale (validated, not returned)
//   10    32   lending_market
//   42    32   liquidity.mint_pubkey
//   74    1    liquidity.mint_decimals
//   75    32   liquidity.supply_pubkey
//   107   32   liquidity.pyth_oracle
//   139   32   liquidity.switchboard_oracle
//   227   32   collateral.mint_pubkey
//   267   32   collateral.supply_pubkey
pub const RESERVE_LEN: usize = 619;

const RES_LAST_UPDATE_STALE_OFF: usize = 9;
const RES_LENDING_MARKET_OFF: usize = 10;
const RES_LIQ_MINT_OFF: usize = 42;
const RES_LIQ_MINT_DECIMALS_OFF: usize = 74;
const RES_LIQ_SUPPLY_OFF: usize = 75;
const RES_LIQ_PYTH_ORACLE_OFF: usize = 107;
const RES_LIQ_SWITCHBOARD_ORACLE_OFF: usize = 139;
const RES_COLL_MINT_OFF: usize = 227;
const RES_COLL_SUPPLY_OFF: usize = 267;

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

/// Execution-only projection of a deployed-mainnet Solend Reserve account.
///
/// Every field is copied verbatim from its on-chain byte range. Rate math,
/// balances, limits, and policy decisions intentionally stay outside this
/// protocol decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolendReserveRaw {
    pub lending_market: Pubkey,
    pub liquidity_mint: Pubkey,
    pub liquidity_mint_decimals: u8,
    pub liquidity_supply: Pubkey,
    pub pyth_oracle: Pubkey,
    pub switchboard_oracle: Pubkey,
    pub collateral_mint: Pubkey,
    pub collateral_supply: Pubkey,
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
    #[error("reserve bytes wrong length: expected {RESERVE_LEN}, got {0}")]
    ReserveWrongSize(usize),
    #[error("reserve stale bit at offset 9 is {0}, expected 0 or 1")]
    ReserveStaleBitInvalid(u8),
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

/// Decode the executor's minimum projection of a deployed-mainnet Solend
/// Reserve account.
pub fn decode_reserve(data: &[u8]) -> Result<SolendReserveRaw, DecodeError> {
    if data.len() != RESERVE_LEN {
        return Err(DecodeError::ReserveWrongSize(data.len()));
    }

    read_bool(data, RES_LAST_UPDATE_STALE_OFF).ok_or(DecodeError::ReserveStaleBitInvalid(
        data[RES_LAST_UPDATE_STALE_OFF],
    ))?;

    Ok(SolendReserveRaw {
        lending_market: read_pubkey(data, RES_LENDING_MARKET_OFF),
        liquidity_mint: read_pubkey(data, RES_LIQ_MINT_OFF),
        liquidity_mint_decimals: data[RES_LIQ_MINT_DECIMALS_OFF],
        liquidity_supply: read_pubkey(data, RES_LIQ_SUPPLY_OFF),
        pyth_oracle: read_pubkey(data, RES_LIQ_PYTH_ORACLE_OFF),
        switchboard_oracle: read_pubkey(data, RES_LIQ_SWITCHBOARD_ORACLE_OFF),
        collateral_mint: read_pubkey(data, RES_COLL_MINT_OFF),
        collateral_supply: read_pubkey(data, RES_COLL_SUPPLY_OFF),
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
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use solana_sdk::instruction::AccountMeta;
    use std::str::FromStr;

    use super::super::{
        amount::UnderlyingAmount,
        ata::derive_associated_token_address,
        deposit::{
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction,
            DepositInstructionInputs,
        },
        refresh::{build_refresh_instructions, RefreshPlanInputs, ReserveRefreshInput},
    };

    // Public account snapshot captured with finalized commitment:
    // reserve: BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw
    // slot: 435_907_990
    // owner: So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo
    // raw-data SHA-256:
    // c7a8a4a7b0f7279aa007239aa80e16fe369dcd1fb15e5527f9a1b2ff0b08a30a
    const MAINNET_RESERVE_FIXTURE_BASE64: &str =
        include_str!("fixtures/main_pool_usdc_reserve_slot_435907990.b64");
    const MAIN_POOL_USDC_RESERVE: &str = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
    const MAIN_POOL_LENDING_MARKET: &str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
    const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const MAIN_POOL_USDC_LIQUIDITY_SUPPLY: &str = "8SheGtsopRUDzdiD6v6BR9a6bqZ9QwywYQY99Fp5meNf";
    const MAIN_POOL_PYTH_ORACLE: &str = "Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX";
    const NULL_SWITCHBOARD_ORACLE: &str = "nu11111111111111111111111111111111111111111";
    const MAIN_POOL_USDC_COLLATERAL_MINT: &str = "993dVFL2uXWYeoXuEBFXR4BijeXdTv4s6BzsCjJZuwqk";
    const MAIN_POOL_USDC_COLLATERAL_SUPPLY: &str = "UtRy8gcEu9fCkDuUrU8EmC7Uc6FZy5NCwttzG7i6nkw";
    const CONTROLLED_WALLET: &str = "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L";
    const CONTROLLED_USDC_ATA: &str = "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3";
    const CONTROLLED_CTOKEN_ATA: &str = "BQazv4UQNFV8t4QGVntELr1ee3bCeTN8AvdPGRutFKn7";
    const PROOF_OBLIGATION: &str = "BdFLjCcP9mCy557vNNGVbTUuvHxXsh8hc6jXzaPra1wN";
    const PROOF_LENDING_MARKET_AUTHORITY: &str = "DdZR6zRFiUt4S5mg7AV1uKB2z1f1WzcNYCaTEEWPAuby";
    const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

    fn key(value: &str) -> Pubkey {
        Pubkey::from_str(value).expect("fixed public key must parse")
    }

    fn mainnet_reserve_fixture() -> Vec<u8> {
        BASE64_STANDARD
            .decode(MAINNET_RESERVE_FIXTURE_BASE64.trim())
            .expect("checked-in mainnet reserve fixture must be valid base64")
    }

    fn synthetic_execution_reserve() -> (Vec<u8>, SolendReserveRaw) {
        let expected = SolendReserveRaw {
            lending_market: Pubkey::new_from_array([0x11; 32]),
            liquidity_mint: Pubkey::new_from_array([0x22; 32]),
            liquidity_mint_decimals: 6,
            liquidity_supply: Pubkey::new_from_array([0x33; 32]),
            pyth_oracle: Pubkey::new_from_array([0x44; 32]),
            switchboard_oracle: Pubkey::new_from_array([0x55; 32]),
            collateral_mint: Pubkey::new_from_array([0x66; 32]),
            collateral_supply: Pubkey::new_from_array([0x77; 32]),
        };
        let mut data = vec![0_u8; RESERVE_LEN];
        data[RES_LAST_UPDATE_STALE_OFF] = 1;
        data[RES_LENDING_MARKET_OFF..RES_LENDING_MARKET_OFF + 32]
            .copy_from_slice(&expected.lending_market.to_bytes());
        data[RES_LIQ_MINT_OFF..RES_LIQ_MINT_OFF + 32]
            .copy_from_slice(&expected.liquidity_mint.to_bytes());
        data[RES_LIQ_MINT_DECIMALS_OFF] = expected.liquidity_mint_decimals;
        data[RES_LIQ_SUPPLY_OFF..RES_LIQ_SUPPLY_OFF + 32]
            .copy_from_slice(&expected.liquidity_supply.to_bytes());
        data[RES_LIQ_PYTH_ORACLE_OFF..RES_LIQ_PYTH_ORACLE_OFF + 32]
            .copy_from_slice(&expected.pyth_oracle.to_bytes());
        data[RES_LIQ_SWITCHBOARD_ORACLE_OFF..RES_LIQ_SWITCHBOARD_ORACLE_OFF + 32]
            .copy_from_slice(&expected.switchboard_oracle.to_bytes());
        data[RES_COLL_MINT_OFF..RES_COLL_MINT_OFF + 32]
            .copy_from_slice(&expected.collateral_mint.to_bytes());
        data[RES_COLL_SUPPLY_OFF..RES_COLL_SUPPLY_OFF + 32]
            .copy_from_slice(&expected.collateral_supply.to_bytes());
        (data, expected)
    }

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

    #[test]
    fn reserve_wrong_size_rejected() {
        let long = vec![0_u8; RESERVE_LEN + 1];
        assert_eq!(
            decode_reserve(&long),
            Err(DecodeError::ReserveWrongSize(RESERVE_LEN + 1))
        );
    }

    #[test]
    fn execution_projection_offsets_roundtrip() {
        let (data, expected) = synthetic_execution_reserve();
        assert_eq!(decode_reserve(&data), Ok(expected));
    }

    #[test]
    fn reserve_invalid_stale_bit_fails_closed() {
        let (mut data, _) = synthetic_execution_reserve();
        data[RES_LAST_UPDATE_STALE_OFF] = 2;
        assert_eq!(
            decode_reserve(&data),
            Err(DecodeError::ReserveStaleBitInvalid(2))
        );
    }

    #[test]
    fn mainnet_reserve_fixture_decodes_expected_execution_fields() {
        let data = mainnet_reserve_fixture();
        let reserve = decode_reserve(&data).expect("mainnet fixture must decode");

        assert_eq!(data.len(), RESERVE_LEN);
        assert_eq!(reserve.lending_market, key(MAIN_POOL_LENDING_MARKET));
        assert_eq!(reserve.liquidity_mint, key(USDC_MINT));
        assert_eq!(reserve.liquidity_mint_decimals, 6);
        assert_eq!(
            reserve.liquidity_supply,
            key(MAIN_POOL_USDC_LIQUIDITY_SUPPLY)
        );
        assert_eq!(reserve.pyth_oracle, key(MAIN_POOL_PYTH_ORACLE));
        assert_eq!(reserve.switchboard_oracle, key(NULL_SWITCHBOARD_ORACLE));
        assert_eq!(reserve.collateral_mint, key(MAIN_POOL_USDC_COLLATERAL_MINT));
        assert_eq!(
            reserve.collateral_supply,
            key(MAIN_POOL_USDC_COLLATERAL_SUPPLY)
        );
    }

    #[test]
    fn null_switchboard_pubkey_is_preserved_verbatim() {
        let reserve =
            decode_reserve(&mainnet_reserve_fixture()).expect("mainnet fixture must decode");
        assert_eq!(reserve.switchboard_oracle, key(NULL_SWITCHBOARD_ORACLE));
    }

    #[test]
    fn proof_transaction_fields_build_exact_refresh_and_deposit() {
        let reserve_pubkey = key(MAIN_POOL_USDC_RESERVE);
        let controlled_wallet = key(CONTROLLED_WALLET);
        let source_liquidity = key(CONTROLLED_USDC_ATA);
        let user_collateral = key(CONTROLLED_CTOKEN_ATA);
        let obligation = key(PROOF_OBLIGATION);
        let solend_program_id = key(SOLEND_PROGRAM_ID_BS58);
        let token_program = key(TOKEN_PROGRAM);
        let reserve =
            decode_reserve(&mainnet_reserve_fixture()).expect("mainnet fixture must decode");

        assert_eq!(
            derive_associated_token_address(
                &controlled_wallet,
                &reserve.liquidity_mint,
                &token_program,
            ),
            source_liquidity
        );
        assert_eq!(
            derive_associated_token_address(
                &controlled_wallet,
                &reserve.collateral_mint,
                &token_program,
            ),
            user_collateral
        );

        let refresh = build_refresh_instructions(RefreshPlanInputs {
            solend_program_id,
            reserves: vec![ReserveRefreshInput {
                reserve_pubkey,
                pyth_oracle: reserve.pyth_oracle,
                switchboard_oracle: reserve.switchboard_oracle,
            }],
            obligation: None,
        });
        assert_eq!(refresh.instructions.len(), 1);
        assert_eq!(refresh.instructions[0].program_id, solend_program_id);
        assert_eq!(refresh.instructions[0].data, vec![3]);
        assert_eq!(
            refresh.instructions[0].accounts,
            vec![
                AccountMeta::new(reserve_pubkey, false),
                AccountMeta::new_readonly(reserve.pyth_oracle, false),
                AccountMeta::new_readonly(reserve.switchboard_oracle, false),
                AccountMeta::new_readonly(solana_sdk::sysvar::clock::id(), false),
            ]
        );

        let deposit = build_deposit_reserve_liquidity_and_obligation_collateral_instruction(
            DepositInstructionInputs {
                solend_program_id,
                amount: UnderlyingAmount::new(500_000),
                source_liquidity,
                user_collateral,
                reserve: reserve_pubkey,
                reserve_liquidity_supply: reserve.liquidity_supply,
                reserve_collateral_mint: reserve.collateral_mint,
                lending_market: reserve.lending_market,
                destination_deposit_collateral: reserve.collateral_supply,
                obligation,
                obligation_owner: controlled_wallet,
                pyth_oracle: reserve.pyth_oracle,
                switchboard_oracle: reserve.switchboard_oracle,
                user_transfer_authority: controlled_wallet,
            },
        )
        .expect("proof deposit must build");
        assert_eq!(deposit.program_id, solend_program_id);
        assert_eq!(deposit.data, hex_bytes("0e20a1070000000000"));
        assert_eq!(
            deposit.accounts,
            vec![
                AccountMeta::new(source_liquidity, false),
                AccountMeta::new(user_collateral, false),
                AccountMeta::new(reserve_pubkey, false),
                AccountMeta::new(reserve.liquidity_supply, false),
                AccountMeta::new(reserve.collateral_mint, false),
                AccountMeta::new_readonly(reserve.lending_market, false),
                AccountMeta::new_readonly(key(PROOF_LENDING_MARKET_AUTHORITY), false),
                AccountMeta::new(reserve.collateral_supply, false),
                AccountMeta::new(obligation, false),
                AccountMeta::new(controlled_wallet, true),
                AccountMeta::new_readonly(reserve.pyth_oracle, false),
                AccountMeta::new_readonly(reserve.switchboard_oracle, false),
                AccountMeta::new_readonly(controlled_wallet, true),
                AccountMeta::new_readonly(token_program, false),
            ]
        );
    }

    fn hex_bytes(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).expect("hex test vector must be ASCII");
                u8::from_str_radix(text, 16).expect("hex test vector must parse")
            })
            .collect()
    }
}
