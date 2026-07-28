//! Phase 2 funding confirmation: a read-only chain proof followed by two
//! state-store compare-and-swap transitions.
//!
//! Adapted from predecessor
//! `crates/gateway/src/stage2_w5h_funding_confirm.rs` at commit
//! `1dea569d5dd9a944ee4a418208d633c5174fd474`.
//!
//! The predecessor's production RPC request used `commitment=confirmed`
//! despite naming the stored transaction slot `funding_finalized_slot`. This
//! module preserves that wire behavior exactly: both `confirmed` and
//! `finalized` observations satisfy the gate, and the top-level transaction
//! slot is persisted in the legacy-named column.
//!
//! This MCP boundary intentionally closes two predecessor security gaps:
//! validation happens before either status transition, and the registered
//! funder must be the signer whose registered ATA loses the exact amount.
//! Failed validation never calls `mark_funding_invalid_if_submitted`; it leaves
//! `funding_required` untouched. If the second CAS is interrupted after the
//! first succeeds, retrying the same signature revalidates the transaction and
//! resumes `funding_submitted -> budget_reserved`.

use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use claw_solana_core::{rpc::EndpointConfig, RpcPool, RpcPoolConfig};
use claw_state_store::{
    Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository, StoreError, StoredWatchRule,
    W5hFundingIntent, W5hIntentStatus,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use solana_client::rpc_request::RpcRequest;
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use spl_associated_token_account::get_associated_token_address;

use crate::finalize::{hex_lower, USDC_MINT_BS58};

const RPC_ENDPOINT_LABEL: &str = "configured-funding-rpc";
const MEMO_PROGRAM_ID_BS58: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
const TOKEN_PROGRAM_ID_BS58: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const USDC_DECIMALS: u8 = 6;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfirmFundingParams {
    /// Finalized intent UUID (the 16-byte rule id rendered as 32 lower hex).
    pub(crate) intent_id: String,
    /// Phantom-submitted Solana transaction signature.
    pub(crate) tx_signature: String,
}

pub(crate) trait FundingClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SystemFundingClock;

impl FundingClock for SystemFundingClock {
    fn now_ms(&self) -> i64 {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_millis();
        i64::try_from(millis).unwrap_or(i64::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmationLevel {
    Confirmed,
    Finalized,
}

impl ConfirmationLevel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedTokenBalance {
    pub(crate) account: String,
    pub(crate) mint: String,
    pub(crate) owner: String,
    pub(crate) amount_raw: u64,
    pub(crate) decimals: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedTransferChecked {
    pub(crate) source: String,
    pub(crate) mint: String,
    pub(crate) destination: String,
    pub(crate) authority: String,
    pub(crate) amount_raw: u64,
    pub(crate) decimals: u8,
}

/// Minimal, normalized proof consumed by the pure verifier.
///
/// Production constructs this from `getTransaction(jsonParsed)`; tests build
/// it directly, keeping every confirmation test offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedFundingTransaction {
    pub(crate) slot: u64,
    /// RPC `blockTime`, converted from seconds to milliseconds when present.
    /// This is the closest available on-chain arrival clock for comparison
    /// with the funding row's absolute millisecond deadline.
    pub(crate) block_time_ms: Option<i64>,
    pub(crate) confirmation: ConfirmationLevel,
    pub(crate) succeeded: bool,
    pub(crate) signer_pubkeys: Vec<String>,
    pub(crate) memos: Vec<String>,
    pub(crate) transfer_checked: Vec<ObservedTransferChecked>,
    pub(crate) pre_token_balances: Vec<ObservedTokenBalance>,
    pub(crate) post_token_balances: Vec<ObservedTokenBalance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FundingTransactionRead {
    Pending,
    Confirmed(ObservedFundingTransaction),
}

/// Fixed error classes only. Provider URLs, query strings, response bodies,
/// and raw client errors are discarded at the RPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FundingReadError {
    #[error("Solana RPC pool is unavailable")]
    PoolUnavailable,
    #[error("Solana signature-status request failed")]
    StatusRequestFailed,
    #[error("Solana transaction request failed")]
    TransactionRequestFailed,
    #[error("Solana transaction response could not be safely decoded")]
    ResponseDecodeFailed,
}

impl FundingReadError {
    const fn code(self) -> &'static str {
        match self {
            Self::PoolUnavailable => "rpc_pool_unavailable",
            Self::StatusRequestFailed => "signature_status_request_failed",
            Self::TransactionRequestFailed => "transaction_request_failed",
            Self::ResponseDecodeFailed => "transaction_response_decode_failed",
        }
    }
}

/// Narrow, read-only seam. Production uses [`RpcFundingTransactionReader`];
/// tests inject deterministic normalized observations.
#[allow(async_fn_in_trait)]
pub(crate) trait FundingTransactionReader: Send + Sync {
    async fn read_funding_transaction(
        &self,
        signature: &str,
    ) -> Result<FundingTransactionRead, FundingReadError>;
}

/// Production reader backed by the shared core [`RpcPool`].
///
/// It performs only `getSignatureStatuses` and `getTransaction`; there is no
/// signing, simulation, or broadcast path.
#[derive(Clone)]
pub(crate) struct RpcFundingTransactionReader {
    rpc_pool: RpcPool,
}

impl RpcFundingTransactionReader {
    fn new(rpc_url: String) -> Self {
        Self {
            rpc_pool: RpcPool::new(RpcPoolConfig {
                endpoints: vec![EndpointConfig {
                    url: rpc_url,
                    label: RPC_ENDPOINT_LABEL.to_owned(),
                    is_write_endpoint: false,
                }],
                ..RpcPoolConfig::default()
            }),
        }
    }
}

pub(crate) fn configured_funding_reader_from_env() -> Option<RpcFundingTransactionReader> {
    let rpc_url = std::env::var_os("SOLFRONTIER_RPC_URL")?
        .into_string()
        .ok()?;
    let rpc_url = rpc_url.trim();
    if rpc_url.is_empty() {
        return None;
    }
    Some(RpcFundingTransactionReader::new(rpc_url.to_owned()))
}

impl FundingTransactionReader for RpcFundingTransactionReader {
    async fn read_funding_transaction(
        &self,
        signature: &str,
    ) -> Result<FundingTransactionRead, FundingReadError> {
        let client = self.rpc_pool.read_client().map_err(|_| {
            tracing::warn!(
                operation = "confirm funding",
                rpc_endpoint = RPC_ENDPOINT_LABEL,
                error_class = "rpc_pool_unavailable",
                "Solana RPC pool unavailable"
            );
            FundingReadError::PoolUnavailable
        })?;

        let status_response = client
            .send::<Value>(
                RpcRequest::GetSignatureStatuses,
                json!([[signature], {"searchTransactionHistory": true}]),
            )
            .await;
        let status_response = match status_response {
            Ok(response) => {
                self.rpc_pool.record_success(&client.url());
                response
            }
            Err(_) => {
                self.rpc_pool.record_failure(&client.url());
                tracing::warn!(
                    operation = "getSignatureStatuses",
                    rpc_endpoint = RPC_ENDPOINT_LABEL,
                    error_class = "signature_status_request_failed",
                    "Solana RPC request failed"
                );
                return Err(FundingReadError::StatusRequestFailed);
            }
        };

        let Some(confirmation) = parse_confirmation_level(&status_response)? else {
            return Ok(FundingTransactionRead::Pending);
        };

        // Preserve the predecessor's actual commitment. The legacy column
        // name says "finalized", but confirmed is the compatibility boundary.
        let tx_response = client
            .send::<Value>(
                RpcRequest::GetTransaction,
                json!([
                    signature,
                    {
                        "encoding": "jsonParsed",
                        "commitment": "confirmed",
                        "maxSupportedTransactionVersion": 0
                    }
                ]),
            )
            .await;
        let tx_response = match tx_response {
            Ok(response) => {
                self.rpc_pool.record_success(&client.url());
                response
            }
            Err(_) => {
                self.rpc_pool.record_failure(&client.url());
                tracing::warn!(
                    operation = "getTransaction",
                    rpc_endpoint = RPC_ENDPOINT_LABEL,
                    error_class = "transaction_request_failed",
                    "Solana RPC request failed"
                );
                return Err(FundingReadError::TransactionRequestFailed);
            }
        };
        if tx_response.is_null() {
            return Ok(FundingTransactionRead::Pending);
        }

        let observed = parse_json_parsed_transaction(&tx_response, confirmation)?;
        Ok(FundingTransactionRead::Confirmed(observed))
    }
}

fn parse_confirmation_level(
    response: &Value,
) -> Result<Option<ConfirmationLevel>, FundingReadError> {
    let status = response
        .get("value")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .ok_or(FundingReadError::ResponseDecodeFailed)?;
    if status.is_null() {
        return Ok(None);
    }
    match status.get("confirmationStatus").and_then(Value::as_str) {
        Some("confirmed") => Ok(Some(ConfirmationLevel::Confirmed)),
        Some("finalized") => Ok(Some(ConfirmationLevel::Finalized)),
        Some("processed") => Ok(None),
        Some(_) => Err(FundingReadError::ResponseDecodeFailed),
        None => match status.get("confirmations") {
            // Compatibility with older RPC nodes: a null confirmation count
            // means rooted/finalized, while more than one confirmation
            // satisfies the SDK's confirmed fallback.
            Some(Value::Null) => Ok(Some(ConfirmationLevel::Finalized)),
            Some(value) if value.as_u64().is_some_and(|count| count > 1) => {
                Ok(Some(ConfirmationLevel::Confirmed))
            }
            Some(_) | None => Ok(None),
        },
    }
}

fn parse_json_parsed_transaction(
    result: &Value,
    confirmation: ConfirmationLevel,
) -> Result<ObservedFundingTransaction, FundingReadError> {
    let slot = result
        .get("slot")
        .and_then(Value::as_u64)
        .ok_or(FundingReadError::ResponseDecodeFailed)?;
    let block_time_ms = match result.get("blockTime") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|seconds| *seconds >= 0)
                .and_then(|seconds| seconds.checked_mul(1_000))
                .ok_or(FundingReadError::ResponseDecodeFailed)?,
        ),
    };
    let transaction = result
        .get("transaction")
        .ok_or(FundingReadError::ResponseDecodeFailed)?;
    let message = transaction
        .get("message")
        .ok_or(FundingReadError::ResponseDecodeFailed)?;
    let meta = result
        .get("meta")
        .ok_or(FundingReadError::ResponseDecodeFailed)?;

    let (account_keys, signer_pubkeys) = parse_account_keys(message, meta)?;
    let (memos, transfer_checked) = parse_outer_instructions(message)?;
    let pre_token_balances = parse_token_balances(meta.get("preTokenBalances"), &account_keys)?;
    let post_token_balances = parse_token_balances(meta.get("postTokenBalances"), &account_keys)?;

    Ok(ObservedFundingTransaction {
        slot,
        block_time_ms,
        confirmation,
        succeeded: meta.get("err").is_some_and(Value::is_null),
        signer_pubkeys,
        memos,
        transfer_checked,
        pre_token_balances,
        post_token_balances,
    })
}

fn parse_account_keys(
    message: &Value,
    meta: &Value,
) -> Result<(Vec<String>, Vec<String>), FundingReadError> {
    let values = message
        .get("accountKeys")
        .and_then(Value::as_array)
        .ok_or(FundingReadError::ResponseDecodeFailed)?;
    let required_signatures = message
        .get("header")
        .and_then(|header| header.get("numRequiredSignatures"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let mut keys = Vec::with_capacity(values.len());
    let mut signers = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let (pubkey, signer) = if let Some(pubkey) = value.as_str() {
            (pubkey, index < required_signatures)
        } else {
            (
                value
                    .get("pubkey")
                    .and_then(Value::as_str)
                    .ok_or(FundingReadError::ResponseDecodeFailed)?,
                value
                    .get("signer")
                    .and_then(Value::as_bool)
                    .ok_or(FundingReadError::ResponseDecodeFailed)?,
            )
        };
        keys.push(pubkey.to_owned());
        if signer {
            signers.push(pubkey.to_owned());
        }
    }

    // `jsonParsed` normally expands ALT-loaded keys into accountKeys. Keep a
    // conservative fallback for nodes that return the separate raw shape.
    if values.iter().all(Value::is_string) {
        if let Some(loaded) = meta.get("loadedAddresses") {
            for field in ["writable", "readonly"] {
                if let Some(entries) = loaded.get(field).and_then(Value::as_array) {
                    for entry in entries {
                        let key = entry
                            .as_str()
                            .ok_or(FundingReadError::ResponseDecodeFailed)?;
                        keys.push(key.to_owned());
                    }
                }
            }
        }
    }
    Ok((keys, signers))
}

fn parse_outer_instructions(
    message: &Value,
) -> Result<(Vec<String>, Vec<ObservedTransferChecked>), FundingReadError> {
    let instructions = message
        .get("instructions")
        .and_then(Value::as_array)
        .ok_or(FundingReadError::ResponseDecodeFailed)?;
    let mut memos = Vec::new();
    let mut transfers = Vec::new();

    for instruction in instructions {
        let Some(program_id) = instruction.get("programId").and_then(Value::as_str) else {
            // A raw instruction cannot safely prove either required shape.
            continue;
        };
        if program_id == MEMO_PROGRAM_ID_BS58 {
            if let Some(memo) = instruction.get("parsed").and_then(Value::as_str) {
                memos.push(memo.to_owned());
            }
            continue;
        }
        if program_id != TOKEN_PROGRAM_ID_BS58 {
            continue;
        }
        let Some(parsed) = instruction.get("parsed") else {
            continue;
        };
        if parsed.get("type").and_then(Value::as_str) != Some("transferChecked") {
            continue;
        }
        let info = parsed
            .get("info")
            .ok_or(FundingReadError::ResponseDecodeFailed)?;
        let token_amount = info
            .get("tokenAmount")
            .ok_or(FundingReadError::ResponseDecodeFailed)?;
        let amount_raw = token_amount
            .get("amount")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(FundingReadError::ResponseDecodeFailed)?;
        let decimals = token_amount
            .get("decimals")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or(FundingReadError::ResponseDecodeFailed)?;
        transfers.push(ObservedTransferChecked {
            source: required_string(info, "source")?,
            mint: required_string(info, "mint")?,
            destination: required_string(info, "destination")?,
            authority: required_string(info, "authority")?,
            amount_raw,
            decimals,
        });
    }
    Ok((memos, transfers))
}

fn required_string(value: &Value, field: &str) -> Result<String, FundingReadError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(FundingReadError::ResponseDecodeFailed)
}

fn parse_token_balances(
    balances: Option<&Value>,
    account_keys: &[String],
) -> Result<Vec<ObservedTokenBalance>, FundingReadError> {
    let Some(balances) = balances.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    balances
        .iter()
        .map(|balance| {
            let account_index = balance
                .get("accountIndex")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(FundingReadError::ResponseDecodeFailed)?;
            let account = account_keys
                .get(account_index)
                .cloned()
                .ok_or(FundingReadError::ResponseDecodeFailed)?;
            let token_amount = balance
                .get("uiTokenAmount")
                .ok_or(FundingReadError::ResponseDecodeFailed)?;
            let amount_raw = token_amount
                .get("amount")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(FundingReadError::ResponseDecodeFailed)?;
            let decimals = token_amount
                .get("decimals")
                .and_then(Value::as_u64)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(FundingReadError::ResponseDecodeFailed)?;
            Ok(ObservedTokenBalance {
                account,
                mint: required_string(balance, "mint")?,
                owner: required_string(balance, "owner")?,
                amount_raw,
                decimals,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerificationError {
    OnChainFailure,
    MemoMismatch,
    FunderNotSigner,
    TransferMissing,
    WrongMint,
    SourceAtaMismatch,
    ReceivingAtaMismatch,
    TransferAuthorityMismatch,
    TransferAmountMismatch,
    TokenDecimalsMismatch,
    SourceBalanceMissing,
    ReceivingBalanceMissing,
    SourceOwnerMismatch,
    ReceivingOwnerMismatch,
    SourceDeltaMismatch,
    ReceivingDeltaMismatch,
    StoredAtaMismatch,
}

impl VerificationError {
    const fn code(self) -> &'static str {
        match self {
            Self::OnChainFailure => "on_chain_failure",
            Self::MemoMismatch => "memo_mismatch",
            Self::FunderNotSigner => "funder_not_signer",
            Self::TransferMissing => "transfer_checked_missing",
            Self::WrongMint => "wrong_mint",
            Self::SourceAtaMismatch => "source_ata_mismatch",
            Self::ReceivingAtaMismatch => "receiving_ata_mismatch",
            Self::TransferAuthorityMismatch => "transfer_authority_mismatch",
            Self::TransferAmountMismatch => "transfer_amount_mismatch",
            Self::TokenDecimalsMismatch => "token_decimals_mismatch",
            Self::SourceBalanceMissing => "source_balance_missing",
            Self::ReceivingBalanceMissing => "receiving_balance_missing",
            Self::SourceOwnerMismatch => "source_owner_mismatch",
            Self::ReceivingOwnerMismatch => "receiving_owner_mismatch",
            Self::SourceDeltaMismatch => "source_delta_mismatch",
            Self::ReceivingDeltaMismatch => "receiving_delta_mismatch",
            Self::StoredAtaMismatch => "stored_ata_mismatch",
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::OnChainFailure => "the transaction failed on-chain",
            Self::MemoMismatch => "the exact canonical funding memo is missing",
            Self::FunderNotSigner => "the registered user wallet did not sign the transaction",
            Self::TransferMissing => "no TransferChecked instruction was found",
            Self::WrongMint => "the transfer or token balance mint is not mainnet USDC",
            Self::SourceAtaMismatch => "TransferChecked did not debit the registered user ATA",
            Self::ReceivingAtaMismatch => {
                "TransferChecked did not credit the registered controlled ATA"
            }
            Self::TransferAuthorityMismatch => {
                "TransferChecked authority is not the registered user wallet"
            }
            Self::TransferAmountMismatch => "TransferChecked amount does not exactly match intent",
            Self::TokenDecimalsMismatch => "TransferChecked or token balance decimals are not 6",
            Self::SourceBalanceMissing => "the registered user ATA balance proof is incomplete",
            Self::ReceivingBalanceMissing => {
                "the registered controlled ATA balance proof is incomplete"
            }
            Self::SourceOwnerMismatch => "the source ATA is not owned by the registered user",
            Self::ReceivingOwnerMismatch => {
                "the receiving ATA is not owned by the controlled wallet"
            }
            Self::SourceDeltaMismatch => {
                "the registered user ATA did not decrease by the exact amount"
            }
            Self::ReceivingDeltaMismatch => {
                "the controlled ATA did not increase by the exact amount"
            }
            Self::StoredAtaMismatch => {
                "stored token accounts are not the owners' canonical USDC ATAs"
            }
        }
    }
}

/// Pure validation. Nothing in this function can mutate the database.
fn verify_funding_transaction(
    tx: &ObservedFundingTransaction,
    intent: &W5hFundingIntent,
) -> Result<(), VerificationError> {
    if !tx.succeeded {
        return Err(VerificationError::OnChainFailure);
    }

    let expected_memo = format!(
        "claw:w5h:{}:{}",
        intent.intent_id, intent.canonical_rule_hash_hex
    );
    if !tx.memos.iter().any(|memo| memo == &expected_memo) {
        return Err(VerificationError::MemoMismatch);
    }
    if !tx
        .signer_pubkeys
        .iter()
        .any(|signer| signer == &intent.user_wallet)
    {
        return Err(VerificationError::FunderNotSigner);
    }

    validate_stored_atas(intent)?;

    let Some(transfer) = tx.transfer_checked.iter().find(|transfer| {
        transfer.source == intent.user_usdc_ata
            && transfer.destination == intent.controlled_usdc_ata
            && transfer.mint == USDC_MINT_BS58
            && transfer.authority == intent.user_wallet
            && transfer.amount_raw == intent.amount_raw
            && transfer.decimals == USDC_DECIMALS
    }) else {
        return Err(diagnose_transfer_mismatch(
            tx.transfer_checked.first(),
            intent,
        ));
    };
    debug_assert_eq!(transfer.amount_raw, intent.amount_raw);

    let source_pre = find_balance(&tx.pre_token_balances, &intent.user_usdc_ata)
        .ok_or(VerificationError::SourceBalanceMissing)?;
    let source_post = find_balance(&tx.post_token_balances, &intent.user_usdc_ata)
        .ok_or(VerificationError::SourceBalanceMissing)?;
    validate_balance_identity(source_pre, &intent.user_wallet, true)?;
    validate_balance_identity(source_post, &intent.user_wallet, true)?;
    let source_delta = source_pre
        .amount_raw
        .checked_sub(source_post.amount_raw)
        .ok_or(VerificationError::SourceDeltaMismatch)?;
    if source_delta != intent.amount_raw {
        return Err(VerificationError::SourceDeltaMismatch);
    }

    let receiving_pre = find_balance(&tx.pre_token_balances, &intent.controlled_usdc_ata);
    let receiving_post = find_balance(&tx.post_token_balances, &intent.controlled_usdc_ata)
        .ok_or(VerificationError::ReceivingBalanceMissing)?;
    if let Some(receiving_pre) = receiving_pre {
        validate_balance_identity(receiving_pre, &intent.controlled_wallet, false)?;
    }
    validate_balance_identity(receiving_post, &intent.controlled_wallet, false)?;
    let pre_amount = receiving_pre.map_or(0, |balance| balance.amount_raw);
    let receiving_delta = receiving_post
        .amount_raw
        .checked_sub(pre_amount)
        .ok_or(VerificationError::ReceivingDeltaMismatch)?;
    if receiving_delta != intent.amount_raw {
        return Err(VerificationError::ReceivingDeltaMismatch);
    }

    Ok(())
}

fn validate_stored_atas(intent: &W5hFundingIntent) -> Result<(), VerificationError> {
    let mint =
        Pubkey::from_str(USDC_MINT_BS58).map_err(|_| VerificationError::StoredAtaMismatch)?;
    let user =
        Pubkey::from_str(&intent.user_wallet).map_err(|_| VerificationError::StoredAtaMismatch)?;
    let controlled = Pubkey::from_str(&intent.controlled_wallet)
        .map_err(|_| VerificationError::StoredAtaMismatch)?;
    if get_associated_token_address(&user, &mint).to_string() != intent.user_usdc_ata
        || get_associated_token_address(&controlled, &mint).to_string()
            != intent.controlled_usdc_ata
    {
        return Err(VerificationError::StoredAtaMismatch);
    }
    Ok(())
}

fn diagnose_transfer_mismatch(
    transfer: Option<&ObservedTransferChecked>,
    intent: &W5hFundingIntent,
) -> VerificationError {
    let Some(transfer) = transfer else {
        return VerificationError::TransferMissing;
    };
    if transfer.mint != USDC_MINT_BS58 {
        VerificationError::WrongMint
    } else if transfer.source != intent.user_usdc_ata {
        VerificationError::SourceAtaMismatch
    } else if transfer.destination != intent.controlled_usdc_ata {
        VerificationError::ReceivingAtaMismatch
    } else if transfer.authority != intent.user_wallet {
        VerificationError::TransferAuthorityMismatch
    } else if transfer.amount_raw != intent.amount_raw {
        VerificationError::TransferAmountMismatch
    } else if transfer.decimals != USDC_DECIMALS {
        VerificationError::TokenDecimalsMismatch
    } else {
        VerificationError::TransferMissing
    }
}

fn find_balance<'a>(
    balances: &'a [ObservedTokenBalance],
    account: &str,
) -> Option<&'a ObservedTokenBalance> {
    balances.iter().find(|balance| balance.account == account)
}

fn validate_balance_identity(
    balance: &ObservedTokenBalance,
    expected_owner: &str,
    source: bool,
) -> Result<(), VerificationError> {
    if balance.mint != USDC_MINT_BS58 {
        return Err(VerificationError::WrongMint);
    }
    if balance.decimals != USDC_DECIMALS {
        return Err(VerificationError::TokenDecimalsMismatch);
    }
    if balance.owner != expected_owner {
        return Err(if source {
            VerificationError::SourceOwnerMismatch
        } else {
            VerificationError::ReceivingOwnerMismatch
        });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfirmFundingInternalError {
    #[error("state-store operation failed")]
    StateStore(#[source] StoreError),
}

impl From<StoreError> for ConfirmFundingInternalError {
    fn from(error: StoreError) -> Self {
        Self::StateStore(error)
    }
}

/// Verify an already user-signed funding transaction and, only after every
/// proof passes, advance `funding_required -> funding_submitted ->
/// budget_reserved`.
pub(crate) async fn confirm_funding_json<R, C>(
    params: &ConfirmFundingParams,
    reader: Option<&R>,
    funding_intents: &Stage2W5hFundingIntentRepository,
    watch_rules: &Stage2WatchRuleRepository,
    clock: &C,
) -> Result<Value, ConfirmFundingInternalError>
where
    R: FundingTransactionReader,
    C: FundingClock,
{
    let intent_id = params.intent_id.trim();
    let tx_signature = params.tx_signature.trim();
    if decode_rule_id(intent_id).is_none() {
        return Ok(json!({
            "status": "invalid_input",
            "reason": "intent_id must be exactly 32 lowercase hexadecimal characters",
        }));
    }
    if Signature::from_str(tx_signature).is_err() {
        return Ok(json!({
            "status": "invalid_input",
            "reason": "tx_signature must be a valid base58 Solana signature",
        }));
    }

    let Some(intent) = funding_intents.get(intent_id).await? else {
        return Ok(json!({
            "status": "not_found",
            "intent_id": intent_id,
        }));
    };
    let rule_id = decode_rule_id(intent_id).expect("validated above");
    let Some(stored_rule) = watch_rules.get(&rule_id).await? else {
        return Ok(json!({
            "status": "rule_unavailable",
            "intent_id": intent_id,
            "reason": "the authoritative WatchRule is missing; funding was not accepted",
            "database_writes": 0,
        }));
    };
    if !watch_rule_matches_intent(&stored_rule, &intent) {
        return Ok(json!({
            "status": "intent_integrity_error",
            "intent_id": intent_id,
            "reason": "the authoritative WatchRule and funding row do not have the same canonical identity",
            "database_writes": 0,
        }));
    }

    let already_reserved = match intent.status {
        W5hIntentStatus::BudgetReserved
            if intent.funding_signature.as_deref() == Some(tx_signature) =>
        {
            true
        }
        W5hIntentStatus::FundingRequired => false,
        W5hIntentStatus::FundingSubmitted
            if intent.funding_signature.as_deref() == Some(tx_signature) =>
        {
            false
        }
        W5hIntentStatus::FundingSubmitted | W5hIntentStatus::BudgetReserved => {
            return Ok(json!({
                "status": "signature_conflict",
                "intent_id": intent_id,
                "current_status": intent.status.as_str(),
                "reason": "this intent is already bound to a different funding signature",
                "database_writes": 0,
            }));
        }
        _ => {
            return Ok(json!({
                "status": "state_conflict",
                "intent_id": intent_id,
                "current_status": intent.status.as_str(),
                "reason": "this lifecycle state cannot accept funding confirmation",
                "database_writes": 0,
            }));
        }
    };

    let Some(reader) = reader else {
        return Ok(json!({
            "status": "config_missing",
            "intent_id": intent_id,
            "reason": "SOLFRONTIER_RPC_URL is not set",
            "setup": "Set SOLFRONTIER_RPC_URL in the server environment, restart, then retry confirm_funding",
            "database_writes": 0,
        }));
    };

    let observation = match reader.read_funding_transaction(tx_signature).await {
        Ok(FundingTransactionRead::Pending) => {
            return Ok(json!({
                "status": "pending_confirmation",
                "intent_id": intent_id,
                "tx_signature": tx_signature,
                "required_commitment": "confirmed",
                "retryable": true,
                "database_writes": 0,
            }));
        }
        Ok(FundingTransactionRead::Confirmed(observation)) => observation,
        Err(error) => {
            return Ok(json!({
                "status": "rpc_error",
                "intent_id": intent_id,
                "error_class": error.code(),
                "reason": error.to_string(),
                "retryable": true,
                "database_writes": 0,
            }));
        }
    };

    if let Err(error) = verify_funding_transaction(&observation, &intent) {
        return Ok(json!({
            "status": "verification_failed",
            "intent_id": intent_id,
            "tx_signature": tx_signature,
            "reason_code": error.code(),
            "reason": error.reason(),
            "database_writes": 0,
        }));
    }

    // Idempotent success is still re-read and fully re-verified. This keeps
    // the second-CAS recovery contract honest: a same-signature retry never
    // trusts only the lifecycle row.
    if already_reserved {
        if intent
            .funding_finalized_slot
            .is_some_and(|slot| slot != observation.slot)
        {
            return Ok(json!({
                "status": "intent_integrity_error",
                "intent_id": intent_id,
                "reason": "the stored funding slot does not match the verified transaction",
                "database_writes": 0,
            }));
        }
        return Ok(success_response(
            &intent,
            &stored_rule,
            tx_signature,
            intent.funding_finalized_slot.or(Some(observation.slot)),
            observation.block_time_ms,
            observation.confirmation.as_str(),
            clock.now_ms(),
            true,
        ));
    }

    let submitted = funding_intents
        .mark_funding_submitted_if_required(intent_id, tx_signature)
        .await?;
    if submitted == 0 {
        return resolve_transition_race(
            funding_intents,
            &stored_rule,
            intent_id,
            tx_signature,
            observation.slot,
            observation.block_time_ms,
            observation.confirmation.as_str(),
            clock.now_ms(),
        )
        .await;
    }

    let reserved = funding_intents
        .mark_budget_reserved_if_submitted(intent_id, tx_signature, observation.slot)
        .await?;
    if reserved == 0 {
        return resolve_transition_race(
            funding_intents,
            &stored_rule,
            intent_id,
            tx_signature,
            observation.slot,
            observation.block_time_ms,
            observation.confirmation.as_str(),
            clock.now_ms(),
        )
        .await;
    }

    let Some(current) = funding_intents.get(intent_id).await? else {
        return Ok(json!({
            "status": "state_conflict",
            "intent_id": intent_id,
            "reason": "the intent disappeared after the state transition",
        }));
    };
    Ok(success_response(
        &current,
        &stored_rule,
        tx_signature,
        Some(observation.slot),
        observation.block_time_ms,
        observation.confirmation.as_str(),
        clock.now_ms(),
        false,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn resolve_transition_race(
    funding_intents: &Stage2W5hFundingIntentRepository,
    stored_rule: &StoredWatchRule,
    intent_id: &str,
    tx_signature: &str,
    observed_slot: u64,
    observed_block_time_ms: Option<i64>,
    confirmation: &str,
    now_ms: i64,
) -> Result<Value, ConfirmFundingInternalError> {
    let Some(current) = funding_intents.get(intent_id).await? else {
        return Ok(json!({
            "status": "state_conflict",
            "intent_id": intent_id,
            "reason": "the intent disappeared during the state transition",
        }));
    };
    if current.status == W5hIntentStatus::BudgetReserved
        && current.funding_signature.as_deref() == Some(tx_signature)
    {
        return Ok(success_response(
            &current,
            stored_rule,
            tx_signature,
            current.funding_finalized_slot.or(Some(observed_slot)),
            observed_block_time_ms,
            confirmation,
            now_ms,
            true,
        ));
    }
    if current.status == W5hIntentStatus::FundingSubmitted
        && current.funding_signature.as_deref() == Some(tx_signature)
    {
        return Ok(json!({
            "status": "funding_submitted",
            "intent_id": intent_id,
            "tx_signature": tx_signature,
            "retryable": true,
            "reason": "the verified signature is recorded but budget reservation did not complete; retry confirm_funding with the same signature",
            "mark_funding_invalid_called": false,
        }));
    }
    Ok(json!({
        "status": "state_conflict",
        "intent_id": intent_id,
        "current_status": current.status.as_str(),
        "reason": "the intent changed state or signature during confirmation",
    }))
}

fn success_response(
    intent: &W5hFundingIntent,
    stored_rule: &StoredWatchRule,
    tx_signature: &str,
    observed_slot: Option<u64>,
    observed_block_time_ms: Option<i64>,
    confirmation: &str,
    now_ms: i64,
    idempotent: bool,
) -> Value {
    let (funding_arrival_time_ms, funding_arrival_time_source) =
        if let Some(block_time_ms) = observed_block_time_ms {
            (block_time_ms, "funding_transaction.blockTime")
        } else {
            (now_ms, "confirmation_clock_fallback")
        };
    let arrived_after_funding_deadline = funding_arrival_time_ms > intent.expires_at_ms;
    let watch_rule_expired_at_funding_slot =
        observed_slot.is_some_and(|slot| slot >= stored_rule.rule.expires_at_slot);
    let mut response = json!({
        "status": "budget_reserved",
        "intent_id": intent.intent_id,
        "tx_signature": tx_signature,
        // Compatibility name: the old system wrote a confirmed tx slot here.
        "funding_finalized_slot": observed_slot,
        "confirmation_level": confirmation,
        "required_commitment": "confirmed",
        "idempotent": idempotent,
        "amount_raw": intent.amount_raw.to_string(),
        "funding_row_expires_at_ms": intent.expires_at_ms,
        "funding_deadline_basis": "funding_row.expires_at_ms",
        "funding_arrival_time_ms": funding_arrival_time_ms,
        "funding_arrival_time_source": funding_arrival_time_source,
        "arrived_after_funding_deadline": arrived_after_funding_deadline,
        "watch_rule": {
            "expires_at_slot": stored_rule.rule.expires_at_slot,
            "funding_transaction_slot": observed_slot,
            "expired_at_funding_slot": watch_rule_expired_at_funding_slot,
            "purpose": "the 480-slot WatchRule deadline determines whether the executor can obtain a lease",
        },
    });
    if arrived_after_funding_deadline {
        response["late_funding"] = json!({
            "refundable": true,
            "automatic_refund_available": false,
            "manual_refund_required": true,
            "notice": "此入金於過期後到達，已記錄為可退款；退款目前需人工處理",
            "warning": "funds may remain in the controlled ATA until the manual refund procedure is performed",
        });
    } else {
        response["late_funding"] = Value::Null;
    }
    response
}

fn watch_rule_matches_intent(rule: &StoredWatchRule, intent: &W5hFundingIntent) -> bool {
    hex_lower(&rule.rule.rule_id) == intent.intent_id
        && intent.rule_id_hex == intent.intent_id
        && hex_lower(&rule.canonical_rule_hash)
            .eq_ignore_ascii_case(&intent.canonical_rule_hash_hex)
        && rule.rule.max_input_amount_raw == intent.amount_raw
        && rule.rule.destination.to_base58() == intent.controlled_wallet
}

fn decode_rule_id(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut output = [0_u8; 16];
    for (index, byte) in output.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&value[offset..offset + 2], 16).ok()?;
    }
    Some(output)
}
