//! Pure Solend combined deposit instruction builder.
//!
//! Provenance: `crates/gateway/src/integrations/solend/deposit.rs` at
//! `1ebb6329f9e0cd05d88705e6435346d0ab935939`.
//! Protocol layout source: `solendprotocol/solana-program-library` mainnet
//! commit `d04ce00bbf4356c4fd32b3be38eb9760b696bb3e`.
//!
//! This module builds one unsigned instruction. Refresh, policy, ATA
//! existence checks, signing, and submission are intentionally outside it.

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::amount::UnderlyingAmount;

const SOLEND_IX_TAG_DEPOSIT_RESERVE_LIQUIDITY_AND_OBLIGATION_COLLATERAL: u8 = 14;

pub(crate) const SPL_TOKEN_PROGRAM_ID_BS58: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepositInstructionInputs {
    pub solend_program_id: Pubkey,
    pub amount: UnderlyingAmount,
    pub source_liquidity: Pubkey,
    pub user_collateral: Pubkey,
    pub reserve: Pubkey,
    pub reserve_liquidity_supply: Pubkey,
    pub reserve_collateral_mint: Pubkey,
    pub lending_market: Pubkey,
    pub destination_deposit_collateral: Pubkey,
    pub obligation: Pubkey,
    pub obligation_owner: Pubkey,
    pub pyth_oracle: Pubkey,
    pub switchboard_oracle: Pubkey,
    pub user_transfer_authority: Pubkey,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DepositBuildError {
    #[error("deposit amount must be greater than zero")]
    ZeroAmount,
}

pub fn build_deposit_reserve_liquidity_and_obligation_collateral_instruction(
    inputs: DepositInstructionInputs,
) -> Result<Instruction, DepositBuildError> {
    if inputs.amount.raw() == 0 {
        return Err(DepositBuildError::ZeroAmount);
    }

    let (lending_market_authority, _) = Pubkey::find_program_address(
        &[&inputs.lending_market.to_bytes()],
        &inputs.solend_program_id,
    );
    let spl_token_program_id = SPL_TOKEN_PROGRAM_ID_BS58
        .parse()
        .expect("SPL Token program id is a well-known constant");

    let mut data = Vec::with_capacity(9);
    data.push(SOLEND_IX_TAG_DEPOSIT_RESERVE_LIQUIDITY_AND_OBLIGATION_COLLATERAL);
    data.extend_from_slice(&inputs.amount.raw().to_le_bytes());

    Ok(Instruction {
        program_id: inputs.solend_program_id,
        accounts: vec![
            AccountMeta::new(inputs.source_liquidity, false),
            AccountMeta::new(inputs.user_collateral, false),
            AccountMeta::new(inputs.reserve, false),
            AccountMeta::new(inputs.reserve_liquidity_supply, false),
            AccountMeta::new(inputs.reserve_collateral_mint, false),
            AccountMeta::new_readonly(inputs.lending_market, false),
            AccountMeta::new_readonly(lending_market_authority, false),
            AccountMeta::new(inputs.destination_deposit_collateral, false),
            AccountMeta::new(inputs.obligation, false),
            AccountMeta::new(inputs.obligation_owner, true),
            AccountMeta::new_readonly(inputs.pyth_oracle, false),
            AccountMeta::new_readonly(inputs.switchboard_oracle, false),
            AccountMeta::new_readonly(inputs.user_transfer_authority, true),
            AccountMeta::new_readonly(spl_token_program_id, false),
        ],
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn solend_program() -> Pubkey {
        Pubkey::from_str("So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo").unwrap()
    }

    fn inputs() -> DepositInstructionInputs {
        DepositInstructionInputs {
            solend_program_id: solend_program(),
            amount: UnderlyingAmount::new(1_000),
            source_liquidity: Pubkey::new_unique(),
            user_collateral: Pubkey::new_unique(),
            reserve: Pubkey::new_unique(),
            reserve_liquidity_supply: Pubkey::new_unique(),
            reserve_collateral_mint: Pubkey::new_unique(),
            lending_market: Pubkey::new_unique(),
            destination_deposit_collateral: Pubkey::new_unique(),
            obligation: Pubkey::new_unique(),
            obligation_owner: Pubkey::new_unique(),
            pyth_oracle: Pubkey::new_unique(),
            switchboard_oracle: Pubkey::new_unique(),
            user_transfer_authority: Pubkey::new_unique(),
        }
    }

    #[test]
    fn program_tag_and_amount_encoding_are_locked() {
        let mut input = inputs();
        input.amount = UnderlyingAmount::new(0x0123_4567_89ab_cdef);
        let ix =
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction(input).unwrap();
        assert_eq!(ix.program_id, solend_program());
        assert_eq!(ix.data[0], 14);
        assert_eq!(
            &ix.data[1..],
            &[0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]
        );
    }

    #[test]
    fn account_metas_match_mainnet_order_and_flags() {
        let input = inputs();
        let expected_pda = Pubkey::find_program_address(
            &[&input.lending_market.to_bytes()],
            &input.solend_program_id,
        )
        .0;
        let expected = [
            (input.source_liquidity, true, false),
            (input.user_collateral, true, false),
            (input.reserve, true, false),
            (input.reserve_liquidity_supply, true, false),
            (input.reserve_collateral_mint, true, false),
            (input.lending_market, false, false),
            (expected_pda, false, false),
            (input.destination_deposit_collateral, true, false),
            (input.obligation, true, false),
            (input.obligation_owner, true, true),
            (input.pyth_oracle, false, false),
            (input.switchboard_oracle, false, false),
            (input.user_transfer_authority, false, true),
            (
                Pubkey::from_str(SPL_TOKEN_PROGRAM_ID_BS58).unwrap(),
                false,
                false,
            ),
        ];
        let ix =
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction(input).unwrap();
        assert_eq!(ix.accounts.len(), 14);
        for (actual, (pubkey, writable, signer)) in ix.accounts.iter().zip(expected) {
            assert_eq!(actual.pubkey, pubkey);
            assert_eq!(actual.is_writable, writable);
            assert_eq!(actual.is_signer, signer);
        }
    }

    #[test]
    fn zero_amount_is_rejected() {
        let mut input = inputs();
        input.amount = UnderlyingAmount::ZERO;
        assert_eq!(
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction(input)
                .unwrap_err(),
            DepositBuildError::ZeroAmount
        );
    }

    #[test]
    fn reserve_and_oracles_are_caller_supplied() {
        let reserve = Pubkey::new_unique();
        let pyth = Pubkey::new_unique();
        let mut input = inputs();
        input.reserve = reserve;
        input.pyth_oracle = pyth;
        let ix =
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction(input).unwrap();
        assert_eq!(ix.accounts[2].pubkey, reserve);
        assert_eq!(ix.accounts[10].pubkey, pyth);
    }

    #[test]
    fn lending_market_authority_is_derived_from_market() {
        let mut first = inputs();
        let mut second = inputs();
        first.lending_market = Pubkey::new_unique();
        second.lending_market = Pubkey::new_unique();
        let first_ix =
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction(first).unwrap();
        let second_ix =
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction(second).unwrap();
        assert_ne!(first_ix.accounts[6].pubkey, second_ix.accounts[6].pubkey);
    }

    #[test]
    fn builder_signature_is_protocol_only() {
        let _builder: fn(DepositInstructionInputs) -> Result<Instruction, DepositBuildError> =
            build_deposit_reserve_liquidity_and_obligation_collateral_instruction;
    }
}
