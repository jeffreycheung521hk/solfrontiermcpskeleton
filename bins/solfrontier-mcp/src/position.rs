//! Read-only Solend obligation discovery and MCP response mapping.
//!
//! The reader seam and bounded `getProgramAccounts` strategy are adapted from
//! predecessor path `crates/gateway/src/tools/get_solend_position.rs` at
//! commit `7f677120d1de0baa2b4503b343b9809df5e29be0`.

use std::str::FromStr;

use claw_solana_core::{rpc::EndpointConfig, RpcPool, RpcPoolConfig};
use serde_json::{json, Value};
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};

use crate::solend_raw::{
    decode_obligation, SolendObligationRaw, OBLIGATION_LEN, OBLIGATION_OWNER_OFFSET,
    SOLEND_PROGRAM_ID_BS58,
};

const RPC_ENDPOINT_LABEL: &str = "configured-read-rpc";
const STATUS_OK: &str = "ok";
const STATUS_NO_POSITION: &str = "no_position";
const STATUS_RPC_ERROR: &str = "rpc_error";
const STATUS_DECODE_ERROR: &str = "decode_error";
const STATUS_CONFIG_MISSING: &str = "config_missing";

const ESTIMATE_UNAVAILABLE_REASON: &str = "Phase 1 reports deposited cToken units exactly; \
the underlying-token/USDC estimate requires reserve exchange-rate decoding, so no estimate is \
invented.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PositionReadError {
    #[error("Solana RPC pool is unavailable")]
    PoolUnavailable,
    #[error("Solana RPC request failed")]
    RequestFailed,
}

/// Narrow read-only seam. Production uses [`RpcSolendPositionReader`];
/// tests inject an offline mock.
#[allow(async_fn_in_trait)]
pub(crate) trait SolendPositionReader: Send + Sync {
    async fn find_obligations_for_owner(
        &self,
        owner_wallet: &Pubkey,
    ) -> Result<Vec<(Pubkey, Vec<u8>)>, PositionReadError>;
}

/// Production reader backed by the shared core `RpcPool`.
///
/// The full endpoint remains private inside `RpcPool`. Its label is fixed and
/// raw RPC errors are discarded at this boundary because provider errors may
/// embed an API-key-bearing request URL.
#[derive(Clone)]
pub(crate) struct RpcSolendPositionReader {
    rpc_pool: RpcPool,
    program_id: Pubkey,
}

impl RpcSolendPositionReader {
    fn new(rpc_url: String) -> Self {
        let rpc_pool = RpcPool::new(RpcPoolConfig {
            endpoints: vec![EndpointConfig {
                url: rpc_url,
                label: RPC_ENDPOINT_LABEL.to_owned(),
                is_write_endpoint: false,
            }],
            ..RpcPoolConfig::default()
        });
        Self {
            rpc_pool,
            program_id: Pubkey::from_str(SOLEND_PROGRAM_ID_BS58)
                .expect("hard-coded Solend program id must parse"),
        }
    }
}

impl SolendPositionReader for RpcSolendPositionReader {
    async fn find_obligations_for_owner(
        &self,
        owner_wallet: &Pubkey,
    ) -> Result<Vec<(Pubkey, Vec<u8>)>, PositionReadError> {
        let client = self.rpc_pool.read_client().map_err(|_| {
            tracing::warn!(
                operation = "getProgramAccounts",
                rpc_endpoint = RPC_ENDPOINT_LABEL,
                "Solana RPC pool unavailable"
            );
            PositionReadError::PoolUnavailable
        })?;
        let config = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(OBLIGATION_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                    OBLIGATION_OWNER_OFFSET,
                    owner_wallet.to_bytes().to_vec(),
                )),
            ]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                data_slice: None,
                min_context_slot: None,
            },
            with_context: Some(false),
            sort_results: None,
        };

        match client
            .get_program_accounts_with_config(&self.program_id, config)
            .await
        {
            Ok(accounts) => {
                self.rpc_pool.record_success(&client.url());
                Ok(accounts
                    .into_iter()
                    .map(|(pubkey, account)| (pubkey, account.data))
                    .collect())
            }
            Err(_) => {
                self.rpc_pool.record_failure(&client.url());
                tracing::warn!(
                    operation = "getProgramAccounts",
                    rpc_endpoint = RPC_ENDPOINT_LABEL,
                    "Solana RPC request failed"
                );
                Err(PositionReadError::RequestFailed)
            }
        }
    }
}

/// Build the production reader only when a non-empty, Unicode endpoint exists.
///
/// Missing, blank, or non-Unicode configuration deliberately leaves the
/// server running so the other MCP tools remain available.
pub(crate) fn configured_reader_from_env() -> Option<RpcSolendPositionReader> {
    let rpc_url = std::env::var_os("SOLFRONTIER_RPC_URL")?
        .into_string()
        .ok()?;
    let rpc_url = rpc_url.trim();
    if rpc_url.is_empty() {
        return None;
    }
    Some(RpcSolendPositionReader::new(rpc_url.to_owned()))
}

pub(crate) async fn query_position<R: SolendPositionReader>(
    reader: Option<&R>,
    wallet: &str,
) -> Value {
    let Some(reader) = reader else {
        return json!({
            "status": STATUS_CONFIG_MISSING,
            "reason": "SOLFRONTIER_RPC_URL is not set",
            "setup": "Set SOLFRONTIER_RPC_URL in the server environment and restart solfrontier-mcp; never commit the endpoint.",
        });
    };

    let owner = match Pubkey::from_str(wallet) {
        Ok(owner) => owner,
        Err(_) => {
            return json!({
                "status": STATUS_DECODE_ERROR,
                "reason": "wallet must be a valid base58 Solana public key",
            });
        }
    };

    let raw_obligations = match reader.find_obligations_for_owner(&owner).await {
        Ok(obligations) => obligations,
        Err(error) => {
            return json!({
                "status": STATUS_RPC_ERROR,
                "phase": "find_obligations_for_owner",
                "reason": error.to_string(),
            });
        }
    };
    if raw_obligations.is_empty() {
        return json!({
            "status": STATUS_NO_POSITION,
            "wallet": wallet,
            "network": "mainnet",
            "protocol": "Solend/Save",
            "program_id": SOLEND_PROGRAM_ID_BS58,
            "obligations": [],
        });
    }

    let mut obligations = Vec::with_capacity(raw_obligations.len());
    let mut decode_errors = Vec::new();
    for (obligation_pubkey, account_data) in raw_obligations {
        match decode_obligation(&account_data) {
            Ok(obligation) if obligation.owner == owner => {
                obligations.push(obligation_json(&obligation_pubkey, &obligation));
            }
            Ok(_) => decode_errors.push(format!(
                "{obligation_pubkey}: decoded owner does not match requested wallet"
            )),
            Err(error) => decode_errors.push(format!("{obligation_pubkey}: {error}")),
        }
    }

    if obligations.is_empty() {
        return json!({
            "status": STATUS_DECODE_ERROR,
            "reason": "all returned obligation accounts failed validation",
            "decode_errors": decode_errors,
        });
    }

    json!({
        "status": STATUS_OK,
        "wallet": wallet,
        "network": "mainnet",
        "protocol": "Solend/Save",
        "program_id": SOLEND_PROGRAM_ID_BS58,
        "obligation_count": obligations.len(),
        "obligations": obligations,
        "decode_warnings": decode_errors,
    })
}

fn obligation_json(obligation_pubkey: &Pubkey, obligation: &SolendObligationRaw) -> Value {
    let deposits: Vec<Value> = obligation
        .deposits
        .iter()
        .map(|deposit| {
            json!({
                "reserve": deposit.deposit_reserve.to_string(),
                "deposited_amount": deposit.deposited_amount.to_string(),
                "supplied_usdc_estimate_raw": Value::Null,
                "supplied_usdc_estimate_ui": Value::Null,
                "estimate_unavailable_reason": ESTIMATE_UNAVAILABLE_REASON,
            })
        })
        .collect();

    json!({
        "obligation_pubkey": obligation_pubkey.to_string(),
        "lending_market": obligation.lending_market.to_string(),
        "deposits": deposits,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::solend_raw::one_deposit_test_fixture;

    type MockReadResult = Result<Vec<(Pubkey, Vec<u8>)>, PositionReadError>;

    struct MockReader {
        result: Mutex<MockReadResult>,
        owners: Mutex<Vec<Pubkey>>,
    }

    impl MockReader {
        fn returning(result: MockReadResult) -> Self {
            Self {
                result: Mutex::new(result),
                owners: Mutex::new(Vec::new()),
            }
        }
    }

    impl SolendPositionReader for MockReader {
        async fn find_obligations_for_owner(
            &self,
            owner_wallet: &Pubkey,
        ) -> Result<Vec<(Pubkey, Vec<u8>)>, PositionReadError> {
            self.owners.lock().expect("owners lock").push(*owner_wallet);
            self.result.lock().expect("result lock").clone()
        }
    }

    fn assert_payload_has_no_sensitive_fields(response: &Value) {
        let serialized = response.to_string();
        let forbidden = [
            "Keypair".to_owned(),
            ["priv", "ate_key"].concat(),
            ["secret", "_"].concat(),
            ["tx", "_bytes"].concat(),
            ["sign", "ed_bytes"].concat(),
            ["approval", "_request_id"].concat(),
            ["signing", "_request_id"].concat(),
        ];
        for needle in forbidden {
            assert!(
                !serialized.contains(&needle),
                "position payload must not contain `{needle}`"
            );
        }
    }

    #[tokio::test]
    async fn config_missing_is_normal_json_without_calling_rpc() {
        let response = query_position(None::<&MockReader>, "not-even-parsed").await;

        assert_eq!(response["status"], STATUS_CONFIG_MISSING);
        assert!(response["setup"]
            .as_str()
            .expect("setup")
            .contains("SOLFRONTIER_RPC_URL"));
    }

    #[tokio::test]
    async fn no_position_maps_empty_reader_result() {
        let owner = Pubkey::new_from_array([0x24; 32]);
        let reader = MockReader::returning(Ok(Vec::new()));

        let response = query_position(Some(&reader), &owner.to_string()).await;

        assert_eq!(response["status"], STATUS_NO_POSITION);
        assert_eq!(response["obligations"], json!([]));
        assert_eq!(
            reader.owners.lock().expect("owners lock").as_slice(),
            &[owner]
        );
    }

    #[tokio::test]
    async fn rpc_error_uses_fixed_safe_reason() {
        let owner = Pubkey::new_from_array([0x25; 32]);
        let reader = MockReader::returning(Err(PositionReadError::RequestFailed));

        let response = query_position(Some(&reader), &owner.to_string()).await;
        let serialized = response.to_string();

        assert_eq!(response["status"], STATUS_RPC_ERROR);
        assert_eq!(response["reason"], "Solana RPC request failed");
        assert!(!serialized.contains("http://"));
        assert!(!serialized.contains("https://"));
        assert!(!serialized.contains("api-key"));
    }

    #[tokio::test]
    async fn fixed_obligation_maps_to_ok_with_exact_raw_deposit() {
        let fixture = one_deposit_test_fixture();
        let obligation_pubkey = Pubkey::new_from_array([0x26; 32]);
        let reader = MockReader::returning(Ok(vec![(obligation_pubkey, fixture.data.clone())]));

        let response = query_position(Some(&reader), &fixture.owner.to_string()).await;

        assert_eq!(response["status"], STATUS_OK);
        assert_eq!(response["obligation_count"], 1);
        let obligation = &response["obligations"][0];
        assert_eq!(
            obligation["obligation_pubkey"],
            obligation_pubkey.to_string()
        );
        assert_eq!(
            obligation["lending_market"],
            fixture.lending_market.to_string()
        );
        let deposit = &obligation["deposits"][0];
        assert_eq!(deposit["reserve"], fixture.deposit_reserve.to_string());
        assert_eq!(
            deposit["deposited_amount"],
            fixture.deposited_amount.to_string()
        );
        assert_eq!(deposit["supplied_usdc_estimate_raw"], Value::Null);
        assert_eq!(deposit["supplied_usdc_estimate_ui"], Value::Null);
        assert!(deposit["estimate_unavailable_reason"]
            .as_str()
            .expect("estimate reason")
            .contains("no estimate is invented"));
        assert_payload_has_no_sensitive_fields(&response);
    }

    #[tokio::test]
    async fn malformed_obligation_maps_to_decode_error() {
        let owner = Pubkey::new_from_array([0x27; 32]);
        let reader = MockReader::returning(Ok(vec![(
            Pubkey::new_from_array([0x28; 32]),
            vec![0_u8; 100],
        )]));

        let response = query_position(Some(&reader), &owner.to_string()).await;

        assert_eq!(response["status"], STATUS_DECODE_ERROR);
        assert!(response["decode_errors"][0]
            .as_str()
            .expect("decode error")
            .contains("expected 1300"));
    }

    #[test]
    fn module_has_no_write_or_signing_path() {
        const SOURCE: &str = include_str!("position.rs");
        let forbidden = [
            ["write_", "client("].concat(),
            [".send_", "transaction("].concat(),
            [".send_", "transaction_with_config("].concat(),
            [".send_raw_", "transaction("].concat(),
            [".send_raw_", "transaction_with_config("].concat(),
            [".send_and_confirm_", "transaction("].concat(),
            [".send_and_confirm_", "transaction_with_spinner("].concat(),
            [".confirm_", "transaction("].concat(),
            ["Transaction::", "new_signed_with_payer("].concat(),
            ["Versioned", "Transaction"].concat(),
            ["Message", "V0"].concat(),
            ["AddressLookup", "Table"].concat(),
            [".simulate_", "transaction("].concat(),
            ["Keypair", "::new("].concat(),
            ["create_signing_", "handoff("].concat(),
            ["ApprovalRequest", "::new("].concat(),
            ["submit_signed_solend_", "transaction("].concat(),
            ["sign", "ed_bytes"].concat(),
            ["private", "_key"].concat(),
            ["tx", "_bytes"].concat(),
        ];

        for needle in forbidden {
            assert!(
                !SOURCE.contains(&needle),
                "position module must not contain `{needle}`"
            );
        }
    }
}
