//! Pure SPL associated-token-account helpers used by Solend plans.
//!
//! Provenance: `crates/gateway/src/integrations/solend/ata.rs` at
//! `7bc42da0d6b52c89a8862a9ed85b98ed5706d9c9`.
//!
//! No RPC, signing, submission, policy, approval, or pending state belongs
//! here. The caller composes the returned unsigned instruction downstream.

use solana_sdk::{instruction::Instruction, pubkey::Pubkey};

pub use spl_associated_token_account::{
    get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};

/// Derive the ATA for `(wallet, mint, token_program)`.
pub fn derive_associated_token_address(
    wallet: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Pubkey {
    get_associated_token_address_with_program_id(wallet, mint, token_program)
}

/// Build an unsigned SPL `CreateIdempotent` ATA instruction.
pub fn build_create_ata_idempotent_instruction(
    funder: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    create_associated_token_account_idempotent(funder, wallet, mint, token_program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    const OWNER: &str = "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L";
    const UNDERLYING_MINT: &str = "E5ndSkaB17Dm7CsD22dvcjfrYSDLCxFcMd6z8ddCk5wp";
    const COLLATERAL_MINT: &str = "7i7dsv8srRcERzd8EtUb6sZJoE4zVG49mH675QkAFJdX";
    const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
    const EXPECTED_SOURCE_ATA: &str = "DSVdfKv8zHQ34riJDra8UNHZynkpk92cavFFLEGGgTt6";
    const EXPECTED_COLLATERAL_ATA: &str = "FErpwhDZsitwzvgJUqMjiNdKCfes3wJRFZXVjkGR2CVt";

    fn key(value: &str) -> Pubkey {
        Pubkey::from_str(value).unwrap()
    }

    #[test]
    fn underlying_ata_matches_independent_vector() {
        assert_eq!(
            derive_associated_token_address(
                &key(OWNER),
                &key(UNDERLYING_MINT),
                &key(TOKEN_PROGRAM)
            )
            .to_string(),
            EXPECTED_SOURCE_ATA
        );
    }

    #[test]
    fn collateral_ata_matches_independent_vector() {
        assert_eq!(
            derive_associated_token_address(
                &key(OWNER),
                &key(COLLATERAL_MINT),
                &key(TOKEN_PROGRAM)
            )
            .to_string(),
            EXPECTED_COLLATERAL_ATA
        );
    }

    #[test]
    fn idempotent_instruction_program_and_tag_are_locked() {
        let ix = build_create_ata_idempotent_instruction(
            &key(OWNER),
            &key(OWNER),
            &key(UNDERLYING_MINT),
            &key(TOKEN_PROGRAM),
        );
        assert_eq!(ix.program_id, spl_associated_token_account::id());
        assert_eq!(ix.data, vec![1]);
    }

    #[test]
    fn idempotent_instruction_account_order_and_flags_are_locked() {
        let funder = key(OWNER);
        let wallet = key("11111111111111111111111111111112");
        let mint = key(UNDERLYING_MINT);
        let token_program = key(TOKEN_PROGRAM);
        let ata = derive_associated_token_address(&wallet, &mint, &token_program);
        let ix = build_create_ata_idempotent_instruction(&funder, &wallet, &mint, &token_program);

        assert_eq!(ix.accounts.len(), 6);
        let expected = [
            (funder, true, true),
            (ata, true, false),
            (wallet, false, false),
            (mint, false, false),
            (solana_sdk::system_program::id(), false, false),
            (token_program, false, false),
        ];
        for (actual, (pubkey, writable, signer)) in ix.accounts.iter().zip(expected) {
            assert_eq!(actual.pubkey, pubkey);
            assert_eq!(actual.is_writable, writable);
            assert_eq!(actual.is_signer, signer);
        }
    }

    #[test]
    fn token_program_participates_in_derivation() {
        let classic = key(TOKEN_PROGRAM);
        let token_2022 = key("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
        assert_ne!(
            derive_associated_token_address(&key(OWNER), &key(UNDERLYING_MINT), &classic),
            derive_associated_token_address(&key(OWNER), &key(UNDERLYING_MINT), &token_2022)
        );
    }

    #[test]
    fn public_helpers_use_only_protocol_inputs() {
        let _derive: fn(&Pubkey, &Pubkey, &Pubkey) -> Pubkey = derive_associated_token_address;
        let _build: fn(&Pubkey, &Pubkey, &Pubkey, &Pubkey) -> Instruction =
            build_create_ata_idempotent_instruction;
    }
}
