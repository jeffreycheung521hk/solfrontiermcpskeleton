//! Jupiter swap-build wire types.
//!
//! Provenance: `crates/gateway/src/integrations/jupiter.rs` at
//! `d423ca170b4cbbfc8dc3e48ae95cb78ba6e7564d`.
//!
//! HTTP transport is deliberately absent. These types only encode/decode the
//! payload consumed by the pure unsigned transaction assembler.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::quote::SwapQuoteResponse;

fn null_or_map<'de, D>(deserializer: D) -> Result<HashMap<String, Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<HashMap<String, Vec<String>>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapBuildRequest {
    pub quote_response: SwapQuoteResponse,
    pub user_public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap_and_unwrap_sol: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_compute_unit_limit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prioritization_fee_lamports: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapBuildResponse {
    #[serde(default)]
    pub compute_budget_instructions: Vec<JupiterInstruction>,
    #[serde(default)]
    pub setup_instructions: Vec<JupiterInstruction>,
    pub swap_instruction: JupiterInstruction,
    #[serde(default)]
    pub cleanup_instruction: Option<JupiterInstruction>,
    #[serde(default)]
    pub other_instructions: Vec<JupiterInstruction>,
    #[serde(default)]
    pub address_lookup_table_addresses: Vec<String>,
    #[serde(default, deserialize_with = "null_or_map")]
    pub addresses_by_lookup_table_address: HashMap<String, Vec<String>>,
    pub blockhash_with_metadata: BlockhashWithMetadata,
    #[serde(default)]
    pub token_ledger_instruction: Option<JupiterInstruction>,
}

impl SwapBuildResponse {
    pub fn total_instruction_count(&self) -> usize {
        self.compute_budget_instructions.len()
            + self.setup_instructions.len()
            + 1
            + usize::from(self.cleanup_instruction.is_some())
            + self.other_instructions.len()
    }

    pub fn all_program_ids(&self) -> Vec<&str> {
        let mut ids = Vec::new();
        let instructions = self
            .compute_budget_instructions
            .iter()
            .chain(self.setup_instructions.iter())
            .chain(std::iter::once(&self.swap_instruction))
            .chain(self.cleanup_instruction.iter())
            .chain(self.other_instructions.iter());
        for instruction in instructions {
            if !ids.contains(&instruction.program_id.as_str()) {
                ids.push(instruction.program_id.as_str());
            }
        }
        ids
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JupiterInstruction {
    pub program_id: String,
    pub accounts: Vec<JupiterAccountMeta>,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JupiterAccountMeta {
    pub pubkey: String,
    pub is_signer: bool,
    pub is_writable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockhashWithMetadata {
    #[serde(deserialize_with = "blockhash_string_or_bytes")]
    pub blockhash: String,
    pub last_valid_block_height: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<serde_json::Value>,
}

fn blockhash_string_or_bytes<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        String(String),
        Bytes(Vec<u8>),
    }

    match Wire::deserialize(deserializer)? {
        Wire::String(value) => Ok(value),
        Wire::Bytes(bytes) => {
            if bytes.len() != 32 {
                return Err(D::Error::custom(format!(
                    "blockhash byte array must be 32 bytes, got {}",
                    bytes.len()
                )));
            }
            Ok(bs58::encode(bytes).into_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BLOCKHASH: &str = "GHtXQBpokKZYzitZ9bHnDZbUXZwqfsFQ2NUjRmiaV5Hf";
    const JUPITER_PROGRAM: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";

    fn wire_response(
        addresses: serde_json::Value,
        blockhash: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "computeBudgetInstructions": [],
            "setupInstructions": [],
            "swapInstruction": {
                "programId": JUPITER_PROGRAM,
                "accounts": [{
                    "pubkey": "11111111111111111111111111111111",
                    "isSigner": false,
                    "isWritable": true
                }],
                "data": "AQ=="
            },
            "cleanupInstruction": null,
            "otherInstructions": [],
            "addressLookupTableAddresses": [],
            "addressesByLookupTableAddress": addresses,
            "blockhashWithMetadata": {
                "blockhash": blockhash,
                "lastValidBlockHeight": 999
            }
        })
    }

    #[test]
    fn live_camel_case_shape_and_explicit_null_alt_map_decode() {
        let response: SwapBuildResponse =
            serde_json::from_value(wire_response(serde_json::Value::Null, json!(BLOCKHASH)))
                .unwrap();
        assert_eq!(response.swap_instruction.program_id, JUPITER_PROGRAM);
        assert!(response.addresses_by_lookup_table_address.is_empty());
        assert_eq!(response.blockhash_with_metadata.blockhash, BLOCKHASH);
    }

    #[test]
    fn absent_alt_map_defaults_to_empty() {
        let mut value = wire_response(json!({}), json!(BLOCKHASH));
        value
            .as_object_mut()
            .unwrap()
            .remove("addressesByLookupTableAddress");
        let response: SwapBuildResponse = serde_json::from_value(value).unwrap();
        assert!(response.addresses_by_lookup_table_address.is_empty());
    }

    #[test]
    fn blockhash_accepts_exactly_thirty_two_wire_bytes() {
        let bytes: Vec<u8> = (0..32).collect();
        let expected = bs58::encode(&bytes).into_string();
        let response: SwapBuildResponse =
            serde_json::from_value(wire_response(json!({}), json!(bytes))).unwrap();
        assert_eq!(response.blockhash_with_metadata.blockhash, expected);

        let error =
            serde_json::from_value::<SwapBuildResponse>(wire_response(json!({}), json!([1, 2, 3])))
                .unwrap_err();
        assert!(error.to_string().contains("must be 32 bytes"));
    }

    #[test]
    fn helpers_preserve_count_and_deduplicate_program_ids() {
        let mut response: SwapBuildResponse =
            serde_json::from_value(wire_response(json!({}), json!(BLOCKHASH))).unwrap();
        response.compute_budget_instructions = vec![response.swap_instruction.clone()];
        response.cleanup_instruction = Some(response.swap_instruction.clone());
        assert_eq!(response.total_instruction_count(), 3);
        assert_eq!(response.all_program_ids(), vec![JUPITER_PROGRAM]);
    }

    #[test]
    fn response_round_trips_through_json() {
        let response: SwapBuildResponse =
            serde_json::from_value(wire_response(json!({}), json!(BLOCKHASH))).unwrap();
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: SwapBuildResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.swap_instruction.program_id, JUPITER_PROGRAM);
        assert_eq!(decoded.blockhash_with_metadata.blockhash, BLOCKHASH);
    }
}
