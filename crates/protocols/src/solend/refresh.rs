//! Pure Solend refresh-instruction builder.
//!
//! Provenance: `crates/gateway/src/integrations/solend/refresh.rs` at
//! `8d5969797e7ef61fdbb3fdee5fdc90914cba0073`.
//!
//! The returned plan contains unsigned instructions only. RPC refresh
//! submission, finality, refetching, and policy evaluation stay outside this
//! crate.

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    sysvar,
};

const SOLEND_IX_TAG_REFRESH_RESERVE: u8 = 3;
const SOLEND_IX_TAG_REFRESH_OBLIGATION: u8 = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReserveRefreshInput {
    pub reserve_pubkey: Pubkey,
    pub pyth_oracle: Pubkey,
    pub switchboard_oracle: Pubkey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationRefreshInput {
    pub obligation_pubkey: Pubkey,
    /// Deposit reserves followed by borrow reserves, in on-chain order.
    pub referenced_reserves_in_order: Vec<Pubkey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshPlanInputs {
    pub solend_program_id: Pubkey,
    pub reserves: Vec<ReserveRefreshInput>,
    pub obligation: Option<ObligationRefreshInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshPlan {
    pub instructions: Vec<Instruction>,
    pub reserves_refreshed: Vec<Pubkey>,
    pub obligation_refreshed: Option<Pubkey>,
}

/// Build `RefreshReserve` instructions followed by an optional
/// `RefreshObligation` instruction.
pub fn build_refresh_instructions(inputs: RefreshPlanInputs) -> RefreshPlan {
    let RefreshPlanInputs {
        solend_program_id,
        reserves,
        obligation,
    } = inputs;

    let mut instructions = Vec::with_capacity(reserves.len() + 1);
    let mut reserves_refreshed = Vec::with_capacity(reserves.len());

    for reserve in &reserves {
        instructions.push(Instruction {
            program_id: solend_program_id,
            accounts: vec![
                AccountMeta::new(reserve.reserve_pubkey, false),
                AccountMeta::new_readonly(reserve.pyth_oracle, false),
                AccountMeta::new_readonly(reserve.switchboard_oracle, false),
                AccountMeta::new_readonly(sysvar::clock::id(), false),
            ],
            data: vec![SOLEND_IX_TAG_REFRESH_RESERVE],
        });
        reserves_refreshed.push(reserve.reserve_pubkey);
    }

    let obligation_refreshed = obligation
        .as_ref()
        .map(|obligation| obligation.obligation_pubkey);
    if let Some(obligation) = obligation {
        // The deployed mainnet handler reads Clock via `Clock::get()`; Clock
        // must not occupy an account slot here. Referenced reserves are
        // writable because the processor may pack attribution updates back.
        let mut accounts = Vec::with_capacity(1 + obligation.referenced_reserves_in_order.len());
        accounts.push(AccountMeta::new(obligation.obligation_pubkey, false));
        for reserve in obligation.referenced_reserves_in_order {
            accounts.push(AccountMeta::new(reserve, false));
        }
        instructions.push(Instruction {
            program_id: solend_program_id,
            accounts,
            data: vec![SOLEND_IX_TAG_REFRESH_OBLIGATION],
        });
    }

    RefreshPlan {
        instructions,
        reserves_refreshed,
        obligation_refreshed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn solend_program() -> Pubkey {
        Pubkey::from_str("So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo").unwrap()
    }

    fn reserve_input(pubkey: Pubkey) -> ReserveRefreshInput {
        ReserveRefreshInput {
            reserve_pubkey: pubkey,
            pyth_oracle: Pubkey::new_unique(),
            switchboard_oracle: Pubkey::new_unique(),
        }
    }

    #[test]
    fn first_deposit_emits_only_reserve_refreshes() {
        let reserve = Pubkey::new_unique();
        let plan = build_refresh_instructions(RefreshPlanInputs {
            solend_program_id: solend_program(),
            reserves: vec![reserve_input(reserve)],
            obligation: None,
        });
        assert_eq!(plan.instructions.len(), 1);
        assert_eq!(plan.reserves_refreshed, vec![reserve]);
        assert_eq!(plan.obligation_refreshed, None);
        let ix = &plan.instructions[0];
        assert_eq!(ix.data, vec![3]);
        assert_eq!(ix.accounts.len(), 4);
        assert_eq!(ix.accounts[0].pubkey, reserve);
        assert!(ix.accounts[0].is_writable);
        assert_eq!(ix.accounts[3].pubkey, sysvar::clock::id());
    }

    #[test]
    fn existing_obligation_is_refreshed_after_all_reserves() {
        let reserve_a = Pubkey::new_unique();
        let reserve_b = Pubkey::new_unique();
        let obligation = Pubkey::new_unique();
        let plan = build_refresh_instructions(RefreshPlanInputs {
            solend_program_id: solend_program(),
            reserves: vec![reserve_input(reserve_a), reserve_input(reserve_b)],
            obligation: Some(ObligationRefreshInput {
                obligation_pubkey: obligation,
                referenced_reserves_in_order: vec![reserve_a, reserve_b],
            }),
        });
        assert_eq!(plan.instructions.len(), 3);
        assert_eq!(plan.instructions[0].data, vec![3]);
        assert_eq!(plan.instructions[1].data, vec![3]);
        assert_eq!(plan.instructions[2].data, vec![7]);
        assert_eq!(plan.obligation_refreshed, Some(obligation));
    }

    #[test]
    fn empty_inputs_emit_empty_plan() {
        let plan = build_refresh_instructions(RefreshPlanInputs {
            solend_program_id: solend_program(),
            reserves: vec![],
            obligation: None,
        });
        assert!(plan.instructions.is_empty());
        assert!(plan.reserves_refreshed.is_empty());
        assert_eq!(plan.obligation_refreshed, None);
    }

    #[test]
    fn obligation_accounts_are_obligation_then_writable_reserves_without_clock() {
        let obligation = Pubkey::new_unique();
        let reserves: Vec<Pubkey> = (0..5).map(|_| Pubkey::new_unique()).collect();
        let plan = build_refresh_instructions(RefreshPlanInputs {
            solend_program_id: solend_program(),
            reserves: vec![],
            obligation: Some(ObligationRefreshInput {
                obligation_pubkey: obligation,
                referenced_reserves_in_order: reserves.clone(),
            }),
        });
        let ix = &plan.instructions[0];
        assert_eq!(ix.accounts.len(), 1 + reserves.len());
        assert_eq!(ix.accounts[0].pubkey, obligation);
        assert!(ix.accounts[0].is_writable);
        for (actual, expected) in ix.accounts[1..].iter().zip(&reserves) {
            assert_eq!(actual.pubkey, *expected);
            assert!(actual.is_writable);
            assert!(!actual.is_signer);
        }
        assert!(ix
            .accounts
            .iter()
            .all(|account| account.pubkey != sysvar::clock::id()));
    }

    #[test]
    fn hckrv5jo_usdc_reserve_is_at_slot_one() {
        let obligation = Pubkey::from_str("HcKrv5Jo5f6qvzSGhJVYTNSqwKudRizn6fxbjPW7M8SV").unwrap();
        let usdc_reserve =
            Pubkey::from_str("BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw").unwrap();
        let plan = build_refresh_instructions(RefreshPlanInputs {
            solend_program_id: solend_program(),
            reserves: vec![],
            obligation: Some(ObligationRefreshInput {
                obligation_pubkey: obligation,
                referenced_reserves_in_order: vec![usdc_reserve],
            }),
        });
        assert_eq!(plan.instructions[0].accounts.len(), 2);
        assert_eq!(plan.instructions[0].accounts[0].pubkey, obligation);
        assert_eq!(plan.instructions[0].accounts[1].pubkey, usdc_reserve);
        assert!(plan.instructions[0].accounts[1].is_writable);
    }

    #[test]
    fn public_builder_uses_only_explicit_protocol_inputs() {
        let _builder: fn(RefreshPlanInputs) -> RefreshPlan = build_refresh_instructions;
    }
}
