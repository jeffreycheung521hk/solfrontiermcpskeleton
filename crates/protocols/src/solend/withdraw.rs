//! Pure Solend combined withdraw-and-redeem instruction builder.
//!
//! Provenance: `crates/gateway/src/integrations/solend/withdraw.rs` at
//! `8d5969797e7ef61fdbb3fdee5fdc90914cba0073`.
//! Protocol layout source: `solendprotocol/solana-program-library` mainnet
//! commit `d04ce00bbf4356c4fd32b3be38eb9760b696bb3e`.
//!
//! The mainnet-verified layout has 12 base accounts followed by every
//! obligation deposit reserve, writable and in exact on-chain order. It has no
//! Clock account; the program reads Clock through a syscall.

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::{amount::CollateralTokenAmount, deposit::SPL_TOKEN_PROGRAM_ID_BS58};

const SOLEND_IX_TAG_WITHDRAW_OBLIGATION_COLLATERAL_AND_REDEEM_RESERVE_COLLATERAL: u8 = 15;

pub const WITHDRAW_ALL_COLLATERAL_AMOUNT_SENTINEL_RAW: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawInstructionInputs {
    pub solend_program_id: Pubkey,
    pub collateral_amount: CollateralTokenAmount,
    pub source_collateral: Pubkey,
    pub destination_collateral: Pubkey,
    pub reserve: Pubkey,
    pub obligation: Pubkey,
    pub lending_market: Pubkey,
    pub destination_liquidity: Pubkey,
    pub reserve_collateral_mint: Pubkey,
    pub reserve_liquidity_supply: Pubkey,
    pub obligation_owner: Pubkey,
    pub user_transfer_authority: Pubkey,
    pub deposit_reserves_in_order: Vec<Pubkey>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WithdrawBuildError {
    #[error("withdraw collateral amount must be greater than zero")]
    ZeroAmount,
}

pub fn build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction(
    inputs: WithdrawInstructionInputs,
) -> Result<Instruction, WithdrawBuildError> {
    if inputs.collateral_amount.raw() == 0 {
        return Err(WithdrawBuildError::ZeroAmount);
    }

    let (lending_market_authority, _) = Pubkey::find_program_address(
        &[&inputs.lending_market.to_bytes()],
        &inputs.solend_program_id,
    );
    let spl_token_program_id = SPL_TOKEN_PROGRAM_ID_BS58
        .parse()
        .expect("SPL Token program id is a well-known constant");

    let mut data = Vec::with_capacity(9);
    data.push(SOLEND_IX_TAG_WITHDRAW_OBLIGATION_COLLATERAL_AND_REDEEM_RESERVE_COLLATERAL);
    data.extend_from_slice(&inputs.collateral_amount.raw().to_le_bytes());

    let mut accounts = Vec::with_capacity(12 + inputs.deposit_reserves_in_order.len());
    accounts.push(AccountMeta::new(inputs.source_collateral, false));
    accounts.push(AccountMeta::new(inputs.destination_collateral, false));
    accounts.push(AccountMeta::new(inputs.reserve, false));
    accounts.push(AccountMeta::new(inputs.obligation, false));
    accounts.push(AccountMeta::new(inputs.lending_market, false));
    accounts.push(AccountMeta::new_readonly(lending_market_authority, false));
    accounts.push(AccountMeta::new(inputs.destination_liquidity, false));
    accounts.push(AccountMeta::new(inputs.reserve_collateral_mint, false));
    accounts.push(AccountMeta::new(inputs.reserve_liquidity_supply, false));
    accounts.push(AccountMeta::new_readonly(inputs.obligation_owner, true));
    accounts.push(AccountMeta::new_readonly(
        inputs.user_transfer_authority,
        true,
    ));
    accounts.push(AccountMeta::new_readonly(spl_token_program_id, false));
    for reserve in inputs.deposit_reserves_in_order {
        accounts.push(AccountMeta::new(reserve, false));
    }

    Ok(Instruction {
        program_id: inputs.solend_program_id,
        accounts,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::sysvar;
    use std::str::FromStr;

    fn key(value: &str) -> Pubkey {
        Pubkey::from_str(value).unwrap()
    }

    fn inputs() -> WithdrawInstructionInputs {
        let reserve = Pubkey::new_unique();
        WithdrawInstructionInputs {
            solend_program_id: key("So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"),
            collateral_amount: CollateralTokenAmount::new(1_000),
            source_collateral: Pubkey::new_unique(),
            destination_collateral: Pubkey::new_unique(),
            reserve,
            obligation: Pubkey::new_unique(),
            lending_market: Pubkey::new_unique(),
            destination_liquidity: Pubkey::new_unique(),
            reserve_collateral_mint: Pubkey::new_unique(),
            reserve_liquidity_supply: Pubkey::new_unique(),
            obligation_owner: Pubkey::new_unique(),
            user_transfer_authority: Pubkey::new_unique(),
            deposit_reserves_in_order: vec![reserve],
        }
    }

    #[test]
    fn program_tag_amount_and_mainnet_base_layout_are_locked() {
        let mut input = inputs();
        input.collateral_amount = CollateralTokenAmount::new(0x0123_4567_89ab_cdef);
        let market_authority = Pubkey::find_program_address(
            &[&input.lending_market.to_bytes()],
            &input.solend_program_id,
        )
        .0;
        let expected = [
            (input.source_collateral, true, false),
            (input.destination_collateral, true, false),
            (input.reserve, true, false),
            (input.obligation, true, false),
            (input.lending_market, true, false),
            (market_authority, false, false),
            (input.destination_liquidity, true, false),
            (input.reserve_collateral_mint, true, false),
            (input.reserve_liquidity_supply, true, false),
            (input.obligation_owner, false, true),
            (input.user_transfer_authority, false, true),
            (key(SPL_TOKEN_PROGRAM_ID_BS58), false, false),
            (input.deposit_reserves_in_order[0], true, false),
        ];
        let ix =
            build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction(input)
                .unwrap();
        assert_eq!(ix.data[0], 15);
        assert_eq!(
            &ix.data[1..],
            &[0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]
        );
        assert_eq!(ix.accounts.len(), 13);
        for (actual, (pubkey, writable, signer)) in ix.accounts.iter().zip(expected) {
            assert_eq!(actual.pubkey, pubkey);
            assert_eq!(actual.is_writable, writable);
            assert_eq!(actual.is_signer, signer);
        }
    }

    #[test]
    fn deposit_reserves_are_writable_and_preserve_exact_order() {
        let reserves: Vec<Pubkey> = (0..5).map(|_| Pubkey::new_unique()).collect();
        let mut input = inputs();
        input.deposit_reserves_in_order = reserves.clone();
        let ix =
            build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction(input)
                .unwrap();
        assert_eq!(ix.accounts.len(), 12 + reserves.len());
        for (actual, expected) in ix.accounts[12..].iter().zip(reserves) {
            assert_eq!(actual.pubkey, expected);
            assert!(actual.is_writable);
            assert!(!actual.is_signer);
        }
    }

    #[test]
    fn no_clock_token_program_at_eleven_and_only_two_signers() {
        let ix = build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction(
            inputs(),
        )
        .unwrap();
        assert!(ix
            .accounts
            .iter()
            .all(|account| account.pubkey != sysvar::clock::id()));
        assert_eq!(ix.accounts[11].pubkey, key(SPL_TOKEN_PROGRAM_ID_BS58));
        let signers: Vec<usize> = ix
            .accounts
            .iter()
            .enumerate()
            .filter_map(|(index, account)| account.is_signer.then_some(index))
            .collect();
        assert_eq!(signers, vec![9, 10]);
    }

    #[test]
    fn zero_rejected_and_withdraw_all_sentinel_preserved() {
        let mut zero = inputs();
        zero.collateral_amount = CollateralTokenAmount::ZERO;
        assert_eq!(
            build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction(zero)
                .unwrap_err(),
            WithdrawBuildError::ZeroAmount
        );

        let mut all = inputs();
        all.collateral_amount =
            CollateralTokenAmount::new(WITHDRAW_ALL_COLLATERAL_AMOUNT_SENTINEL_RAW);
        let ix =
            build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction(all)
                .unwrap();
        assert_eq!(&ix.data[1..], &[0xff; 8]);
    }

    #[test]
    fn hckrv5jo_fixture_keeps_reserve_at_slots_two_and_twelve() {
        let reserve = key("BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw");
        let mut input = inputs();
        input.reserve = reserve;
        input.obligation = key("HcKrv5Jo5f6qvzSGhJVYTNSqwKudRizn6fxbjPW7M8SV");
        input.deposit_reserves_in_order = vec![reserve];
        let ix =
            build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction(input)
                .unwrap();
        assert_eq!(ix.accounts[2].pubkey, reserve);
        assert_eq!(ix.accounts[12].pubkey, reserve);
        assert!(ix.accounts[4].is_writable);
        assert!(ix.accounts[12].is_writable);
    }

    #[test]
    fn builder_signature_is_protocol_only() {
        let _builder: fn(WithdrawInstructionInputs) -> Result<Instruction, WithdrawBuildError> =
            build_withdraw_obligation_collateral_and_redeem_reserve_collateral_instruction;
    }
}
