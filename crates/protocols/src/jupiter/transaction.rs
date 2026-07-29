//! Pure V0 transaction assembly from a Jupiter build response.
//!
//! Provenance: `crates/gateway/src/integrations/jupiter_tx.rs` at
//! `d423ca170b4cbbfc8dc3e48ae95cb78ba6e7564d`.
//!
//! Instruction order is canonical:
//! compute budget, setup, swap, optional cleanup, then other instructions.
//! The result has placeholder signatures and is not signed or broadcast here.

use std::{collections::HashMap, str::FromStr};

use solana_sdk::{
    address_lookup_table::AddressLookupTableAccount,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    signature::Signature,
    transaction::VersionedTransaction,
};
use thiserror::Error;

use super::build::{JupiterAccountMeta, JupiterInstruction, SwapBuildResponse};

#[derive(Debug, Error)]
pub enum AssembleError {
    #[error("invalid program id '{0}': {1}")]
    InvalidProgramId(String, String),
    #[error("invalid account pubkey '{0}': {1}")]
    InvalidAccountPubkey(String, String),
    #[error("invalid lookup table address '{0}': {1}")]
    InvalidLookupTableAddress(String, String),
    #[error("invalid base64 data for instruction program {0}: {1}")]
    InvalidInstructionData(String, String),
    #[error("invalid blockhash '{0}': {1}")]
    InvalidBlockhash(String, String),
    #[error("failed to compile V0 message: {0}")]
    MessageCompile(String),
}

pub fn assemble_v0_transaction(
    build: &SwapBuildResponse,
    payer: &Pubkey,
) -> Result<VersionedTransaction, AssembleError> {
    let lookup_tables = convert_lookup_tables(&build.addresses_by_lookup_table_address)?;
    assemble_v0_transaction_with_resolved_alts(build, payer, &lookup_tables)
}

pub fn assemble_v0_transaction_with_resolved_alts(
    build: &SwapBuildResponse,
    payer: &Pubkey,
    lookup_tables: &[AddressLookupTableAccount],
) -> Result<VersionedTransaction, AssembleError> {
    let instructions = ordered_instructions(build)
        .into_iter()
        .map(convert_instruction)
        .collect::<Result<Vec<_>, _>>()?;
    let blockhash = Hash::from_str(&build.blockhash_with_metadata.blockhash).map_err(|error| {
        AssembleError::InvalidBlockhash(
            build.blockhash_with_metadata.blockhash.clone(),
            error.to_string(),
        )
    })?;
    let message = v0::Message::try_compile(payer, &instructions, lookup_tables, blockhash)
        .map_err(|error| AssembleError::MessageCompile(error.to_string()))?;
    let signatures = vec![Signature::default(); message.header.num_required_signatures as usize];
    Ok(VersionedTransaction {
        signatures,
        message: VersionedMessage::V0(message),
    })
}

pub fn ordered_instructions(build: &SwapBuildResponse) -> Vec<&JupiterInstruction> {
    let mut instructions = Vec::with_capacity(build.total_instruction_count());
    instructions.extend(&build.compute_budget_instructions);
    instructions.extend(&build.setup_instructions);
    instructions.push(&build.swap_instruction);
    if let Some(cleanup) = &build.cleanup_instruction {
        instructions.push(cleanup);
    }
    instructions.extend(&build.other_instructions);
    instructions
}

fn convert_instruction(instruction: &JupiterInstruction) -> Result<Instruction, AssembleError> {
    let program_id = Pubkey::from_str(&instruction.program_id).map_err(|error| {
        AssembleError::InvalidProgramId(instruction.program_id.clone(), error.to_string())
    })?;
    let accounts = instruction
        .accounts
        .iter()
        .map(convert_account_meta)
        .collect::<Result<Vec<_>, _>>()?;
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD
        .decode(&instruction.data)
        .map_err(|error| {
            AssembleError::InvalidInstructionData(instruction.program_id.clone(), error.to_string())
        })?;
    Ok(Instruction {
        program_id,
        accounts,
        data,
    })
}

fn convert_account_meta(account: &JupiterAccountMeta) -> Result<AccountMeta, AssembleError> {
    let pubkey = Pubkey::from_str(&account.pubkey).map_err(|error| {
        AssembleError::InvalidAccountPubkey(account.pubkey.clone(), error.to_string())
    })?;
    Ok(AccountMeta {
        pubkey,
        is_signer: account.is_signer,
        is_writable: account.is_writable,
    })
}

pub(crate) fn convert_lookup_tables(
    table_map: &HashMap<String, Vec<String>>,
) -> Result<Vec<AddressLookupTableAccount>, AssembleError> {
    let mut entries: Vec<_> = table_map.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    entries
        .into_iter()
        .map(|(table_address, addresses)| {
            let key = Pubkey::from_str(table_address).map_err(|error| {
                AssembleError::InvalidLookupTableAddress(table_address.clone(), error.to_string())
            })?;
            let addresses = addresses
                .iter()
                .map(|address| {
                    Pubkey::from_str(address).map_err(|error| {
                        AssembleError::InvalidAccountPubkey(address.clone(), error.to_string())
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(AddressLookupTableAccount { key, addresses })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jupiter::build::BlockhashWithMetadata;
    use base64::Engine;

    const PAYER: &str = "FhU4P7HWvLCk4W7E6VuH6GttYeHJekdPSHF9xXmVnWwy";
    const BLOCKHASH: &str = "GHtXQBpokKZYzitZ9bHnDZbUXZwqfsFQ2NUjRmiaV5Hf";
    const JUPITER: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
    const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
    const SYSTEM: &str = "11111111111111111111111111111111";
    const COMPUTE: &str = "ComputeBudget111111111111111111111111111111";
    const ALT: &str = "4yTMfaDAUMeaoBnhXnshdbw2Z35DX9CU5HBsPufxeqp5";

    fn instruction(program: &str, payload: &[u8]) -> JupiterInstruction {
        JupiterInstruction {
            program_id: program.to_owned(),
            accounts: vec![JupiterAccountMeta {
                pubkey: PAYER.to_owned(),
                is_signer: true,
                is_writable: true,
            }],
            data: base64::engine::general_purpose::STANDARD.encode(payload),
        }
    }

    fn response() -> SwapBuildResponse {
        SwapBuildResponse {
            compute_budget_instructions: vec![
                instruction(COMPUTE, b"CB1"),
                instruction(COMPUTE, b"CB2"),
            ],
            setup_instructions: vec![
                instruction(SYSTEM, b"SETUP1"),
                instruction(TOKEN, b"SETUP2"),
            ],
            swap_instruction: instruction(JUPITER, b"SWAP"),
            cleanup_instruction: Some(instruction(TOKEN, b"CLEANUP")),
            other_instructions: vec![instruction(SYSTEM, b"OTHER1")],
            address_lookup_table_addresses: vec![ALT.to_owned()],
            addresses_by_lookup_table_address: HashMap::from([(
                ALT.to_owned(),
                vec![TOKEN.to_owned(), JUPITER.to_owned()],
            )]),
            blockhash_with_metadata: BlockhashWithMetadata {
                blockhash: BLOCKHASH.to_owned(),
                last_valid_block_height: 100_000,
                fetched_at: None,
            },
            token_ledger_instruction: None,
        }
    }

    #[test]
    fn assembles_unsigned_v0_with_payer_and_blockhash() {
        let payer = Pubkey::from_str(PAYER).unwrap();
        let transaction = assemble_v0_transaction(&response(), &payer).unwrap();
        let VersionedMessage::V0(message) = &transaction.message else {
            panic!("expected V0 message");
        };
        assert_eq!(message.recent_blockhash, Hash::from_str(BLOCKHASH).unwrap());
        assert_eq!(message.account_keys[0], payer);
        assert_eq!(
            transaction.signatures.len(),
            message.header.num_required_signatures as usize
        );
        assert!(transaction
            .signatures
            .iter()
            .all(|signature| signature == &Signature::default()));
    }

    #[test]
    fn canonical_instruction_order_is_locked() {
        let build = response();
        let payloads = ordered_instructions(&build)
            .into_iter()
            .map(|instruction| {
                base64::engine::general_purpose::STANDARD
                    .decode(&instruction.data)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            payloads,
            [
                b"CB1".as_slice(),
                b"CB2",
                b"SETUP1",
                b"SETUP2",
                b"SWAP",
                b"CLEANUP",
                b"OTHER1"
            ]
        );
    }

    #[test]
    fn order_without_cleanup_keeps_swap_before_other() {
        let mut build = response();
        build.cleanup_instruction = None;
        let ordered = ordered_instructions(&build);
        let payload = |index: usize| {
            base64::engine::general_purpose::STANDARD
                .decode(&ordered[index].data)
                .unwrap()
        };
        assert_eq!(payload(4), b"SWAP");
        assert_eq!(payload(5), b"OTHER1");
    }

    #[test]
    fn lookup_tables_parse_addresses_and_sort_keys() {
        let map = HashMap::from([
            (
                "BPFLoaderUpgradeab1e11111111111111111111111".to_owned(),
                vec![SYSTEM.to_owned()],
            ),
            (ALT.to_owned(), vec![TOKEN.to_owned(), JUPITER.to_owned()]),
        ]);
        let tables = convert_lookup_tables(&map).unwrap();
        assert_eq!(tables.len(), 2);
        assert!(tables[0].key.to_string() < tables[1].key.to_string());
        assert_eq!(
            tables
                .iter()
                .map(|table| table.addresses.len())
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn empty_lookup_tables_still_assemble() {
        let payer = Pubkey::from_str(PAYER).unwrap();
        let mut build = response();
        build.addresses_by_lookup_table_address.clear();
        let transaction = assemble_v0_transaction(&build, &payer).unwrap();
        let VersionedMessage::V0(message) = transaction.message else {
            panic!("expected V0 message");
        };
        assert!(message.address_table_lookups.is_empty());
    }

    #[test]
    fn malformed_program_blockhash_and_data_fail_closed() {
        let payer = Pubkey::from_str(PAYER).unwrap();

        let mut invalid_program = response();
        invalid_program.swap_instruction.program_id = "invalid".to_owned();
        assert!(matches!(
            assemble_v0_transaction(&invalid_program, &payer),
            Err(AssembleError::InvalidProgramId(..))
        ));

        let mut invalid_blockhash = response();
        invalid_blockhash.blockhash_with_metadata.blockhash = "invalid".to_owned();
        assert!(matches!(
            assemble_v0_transaction(&invalid_blockhash, &payer),
            Err(AssembleError::InvalidBlockhash(..))
        ));

        let mut invalid_data = response();
        invalid_data.swap_instruction.data = "###".to_owned();
        assert!(matches!(
            assemble_v0_transaction(&invalid_data, &payer),
            Err(AssembleError::InvalidInstructionData(..))
        ));
    }

    #[test]
    fn assembly_is_deterministic_for_equal_inputs() {
        let payer = Pubkey::from_str(PAYER).unwrap();
        let first = assemble_v0_transaction(&response(), &payer).unwrap();
        let second = assemble_v0_transaction(&response(), &payer).unwrap();
        assert_eq!(
            bincode::serialize(&first).unwrap(),
            bincode::serialize(&second).unwrap()
        );
    }
}
