//! Pure Solend `InitObligation` and obligation-account builders.
//!
//! Provenance: `crates/gateway/src/integrations/solend/init_obligation.rs`
//! at `7bc42da0d6b52c89a8862a9ed85b98ed5706d9c9`.
//! Protocol layout source: `solendprotocol/solana-program-library` mainnet
//! commit `d04ce00bbf4356c4fd32b3be38eb9760b696bb3e`.
//!
//! Rent lookup, transaction assembly, signing, and broadcast remain caller
//! responsibilities.

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_instruction, sysvar,
};

use super::{deposit::SPL_TOKEN_PROGRAM_ID_BS58, raw::OBLIGATION_LEN};

const SOLEND_IX_TAG_INIT_OBLIGATION: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitObligationInputs {
    pub solend_program_id: Pubkey,
    pub obligation: Pubkey,
    pub lending_market: Pubkey,
    pub obligation_owner: Pubkey,
}

/// Build an unsigned Solend `InitObligation` instruction.
pub fn build_init_obligation_instruction(inputs: InitObligationInputs) -> Instruction {
    let spl_token_program_id = SPL_TOKEN_PROGRAM_ID_BS58
        .parse()
        .expect("SPL Token program id is a well-known constant");

    Instruction {
        program_id: inputs.solend_program_id,
        accounts: vec![
            AccountMeta::new(inputs.obligation, false),
            AccountMeta::new_readonly(inputs.lending_market, false),
            AccountMeta::new_readonly(inputs.obligation_owner, true),
            AccountMeta::new_readonly(sysvar::rent::id(), false),
            AccountMeta::new_readonly(spl_token_program_id, false),
        ],
        data: vec![SOLEND_IX_TAG_INIT_OBLIGATION],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateObligationAccountInputs {
    pub funder: Pubkey,
    pub new_obligation: Pubkey,
    pub rent_exempt_lamports: u64,
    pub solend_program_id: Pubkey,
}

/// Build the System Program instruction that allocates a Solend obligation.
pub fn build_create_obligation_account_instruction(
    inputs: CreateObligationAccountInputs,
) -> Instruction {
    system_instruction::create_account(
        &inputs.funder,
        &inputs.new_obligation,
        inputs.rent_exempt_lamports,
        OBLIGATION_LEN as u64,
        &inputs.solend_program_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn solend_program() -> Pubkey {
        Pubkey::from_str("So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo").unwrap()
    }

    fn init_inputs() -> InitObligationInputs {
        InitObligationInputs {
            solend_program_id: solend_program(),
            obligation: Pubkey::new_unique(),
            lending_market: Pubkey::new_unique(),
            obligation_owner: Pubkey::new_unique(),
        }
    }

    #[test]
    fn init_tag_program_and_account_layout_match_mainnet() {
        let inputs = init_inputs();
        let ix = build_init_obligation_instruction(inputs);
        assert_eq!(ix.program_id, inputs.solend_program_id);
        assert_eq!(ix.data, vec![6]);
        assert_eq!(ix.accounts.len(), 5);

        let token_program: Pubkey = SPL_TOKEN_PROGRAM_ID_BS58.parse().unwrap();
        let expected = [
            (inputs.obligation, true, false),
            (inputs.lending_market, false, false),
            (inputs.obligation_owner, false, true),
            (sysvar::rent::id(), false, false),
            (token_program, false, false),
        ];
        for (actual, (pubkey, writable, signer)) in ix.accounts.iter().zip(expected) {
            assert_eq!(actual.pubkey, pubkey);
            assert_eq!(actual.is_writable, writable);
            assert_eq!(actual.is_signer, signer);
        }
    }

    #[test]
    fn init_obligation_does_not_include_clock_sysvar() {
        let ix = build_init_obligation_instruction(init_inputs());
        assert!(ix
            .accounts
            .iter()
            .all(|account| account.pubkey != sysvar::clock::id()));
    }

    fn create_inputs() -> CreateObligationAccountInputs {
        CreateObligationAccountInputs {
            funder: Pubkey::new_unique(),
            new_obligation: Pubkey::new_unique(),
            rent_exempt_lamports: 9_938_880,
            solend_program_id: solend_program(),
        }
    }

    #[test]
    fn create_account_program_and_signer_layout_are_locked() {
        let inputs = create_inputs();
        let ix = build_create_obligation_account_instruction(inputs);
        assert_eq!(ix.program_id, solana_sdk::system_program::id());
        assert_eq!(ix.accounts.len(), 2);
        assert_eq!(ix.accounts[0].pubkey, inputs.funder);
        assert!(ix.accounts[0].is_writable && ix.accounts[0].is_signer);
        assert_eq!(ix.accounts[1].pubkey, inputs.new_obligation);
        assert!(ix.accounts[1].is_writable && ix.accounts[1].is_signer);
    }

    #[test]
    fn create_account_payload_encodes_lamports_space_and_owner() {
        let inputs = create_inputs();
        let ix = build_create_obligation_account_instruction(inputs);
        assert_eq!(u32::from_le_bytes(ix.data[0..4].try_into().unwrap()), 0);
        assert_eq!(
            u64::from_le_bytes(ix.data[4..12].try_into().unwrap()),
            inputs.rent_exempt_lamports
        );
        assert_eq!(
            u64::from_le_bytes(ix.data[12..20].try_into().unwrap()),
            OBLIGATION_LEN as u64
        );
        let owner: [u8; 32] = ix.data[20..52].try_into().unwrap();
        assert_eq!(Pubkey::new_from_array(owner), inputs.solend_program_id);
    }

    #[test]
    fn caller_supplies_rent_without_hidden_lookup() {
        let mut first = create_inputs();
        let mut second = first;
        first.rent_exempt_lamports = 1_000_000;
        second.rent_exempt_lamports = 2_000_000;
        let first_ix = build_create_obligation_account_instruction(first);
        let second_ix = build_create_obligation_account_instruction(second);
        assert_eq!(
            u64::from_le_bytes(first_ix.data[4..12].try_into().unwrap()),
            1_000_000
        );
        assert_eq!(
            u64::from_le_bytes(second_ix.data[4..12].try_into().unwrap()),
            2_000_000
        );
        assert_eq!(first_ix.data[12..], second_ix.data[12..]);
    }

    #[test]
    fn builders_accept_only_explicit_protocol_inputs() {
        let _init: fn(InitObligationInputs) -> Instruction = build_init_obligation_instruction;
        let _create: fn(CreateObligationAccountInputs) -> Instruction =
            build_create_obligation_account_instruction;
    }
}
