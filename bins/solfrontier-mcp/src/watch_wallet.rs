//! Controlled-wallet review and signing adapter for the Phase 3b executor.
//!
//! This module owns no submission or confirmation capability. It loads only the
//! explicitly configured controlled-wallet keypair, drives the frozen
//! wallet-engine typestate pipeline, and returns signed bytes through a
//! verification gate. All public errors are fixed classes: neither key material
//! nor an RPC endpoint can be included in their display representation.

use std::{
    env, fs,
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use claw_risk_engine::{PolicyEvaluationContext, PolicySet};
use claw_solana_core::{
    fees::PriorityFeeStrategy, rpc::ClawRpcClient, BlockhashManager, RpcPool, SimulationClient,
};
use claw_types::{
    policy::{PolicyAction, PolicyCondition, PolicyRule},
    session::SessionId,
    solana::{CommitmentLevel, SolanaNetwork},
    transaction::{AccountRole, InstructionSummary, TokenTransfer, TransactionProposal},
};
use claw_wallet_engine::{
    pipeline::PipelineResult, ApprovalMode, LocalKeypairSigner, SecretKeystore, Signer, SignerRef,
    TransactionReviewPipeline, WalletError,
};
use solana_sdk::{
    compute_budget,
    pubkey::Pubkey,
    signature::Signature,
    transaction::{Transaction, VersionedTransaction},
};
use uuid::Uuid;

pub(crate) const CONTROLLED_WALLET_KEYPAIR_ENV: &str = "SOLFRONTIER_CONTROLLED_WALLET_KEYPAIR";

const AUDITED_SOLEND_PROGRAM_BS58: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const CLASSIC_TOKEN_PROGRAM_BS58: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const MAINNET_USDC_MINT_BS58: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SOLEND_REFRESH_RESERVE_TAG: u8 = 3;
const SOLEND_DEPOSIT_AND_OBLIGATION_TAG: u8 = 14;
const EXPECTED_INSTRUCTION_COUNT: usize = 4;
const EXPECTED_DEPOSIT_ACCOUNT_COUNT: usize = 14;
const REVIEWED_COMPUTE_UNIT_LIMIT_DATA: [u8; 5] = [2, 0x80, 0x1a, 0x06, 0];
const REVIEWED_COMPUTE_UNIT_PRICE_DATA: [u8; 9] = [3, 0x50, 0xc3, 0, 0, 0, 0, 0, 0];

/// A non-sensitive, stable failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchWalletError {
    KeypairConfigMissing,
    KeypairReadFailed,
    KeypairFormatInvalid,
    ControlledWalletMismatch,
    TransactionShapeRejected,
    SerializationFailed,
    AuditOutputFailed,
    AuditIntegrityFailed,
    SimulationFailed,
    PolicyRejected,
    PolicyNotAutoApproved,
    SigningFailed,
    SignedCaptureMissing,
    CaptureUnavailable,
    MessageBytesChanged,
    SignatureInvalid,
}

impl WatchWalletError {
    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::KeypairConfigMissing => "controlled_keypair_config_missing",
            Self::KeypairReadFailed => "controlled_keypair_read_failed",
            Self::KeypairFormatInvalid => "controlled_keypair_format_invalid",
            Self::ControlledWalletMismatch => "controlled_wallet_mismatch",
            Self::TransactionShapeRejected => "transaction_shape_rejected",
            Self::SerializationFailed => "transaction_serialization_failed",
            Self::AuditOutputFailed => {
                "execute_transaction_review_serialization_failed_before_signing"
            }
            Self::AuditIntegrityFailed => {
                "execute_transaction_review_integrity_failed_before_signing"
            }
            Self::SimulationFailed => "wallet_pipeline_simulation_failed",
            Self::PolicyRejected => "risk_policy_rejected",
            Self::PolicyNotAutoApproved => "risk_policy_not_auto_approved",
            Self::SigningFailed => "controlled_wallet_signing_failed",
            Self::SignedCaptureMissing => "signed_transaction_capture_missing",
            Self::CaptureUnavailable => "signed_transaction_capture_unavailable",
            Self::MessageBytesChanged => "reviewed_message_bytes_changed",
            Self::SignatureInvalid => "signed_transaction_signature_invalid",
        }
    }
}

impl std::fmt::Display for WatchWalletError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.class())
    }
}

impl std::error::Error for WatchWalletError {}

/// Canonical values already validated and fingerprint-bound by the executor.
///
/// They are checked once more against the actual Solana transaction before the
/// transaction enters the wallet pipeline. The semantic token-transfer summary
/// supplied to the risk engine is derived from these same values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalExecutionPolicy {
    pub(crate) solend_program_id: Pubkey,
    pub(crate) input_mint: Pubkey,
    pub(crate) input_amount_raw: u64,
    pub(crate) source_liquidity: Pubkey,
    pub(crate) reserve_liquidity_supply: Pubkey,
    /// Exact builder-produced message bytes with the all-zero placeholder
    /// blockhash. Wallet review permits only the pipeline's fresh blockhash to
    /// differ; every instruction/account/flag/data byte remains bound.
    pub(crate) expected_message_bytes: Vec<u8>,
}

/// A signer loaded into the frozen wallet-engine keystore.
///
/// The signer is intentionally private. Callers can inspect only the pinned
/// public key and can hand the value to [`WatchWalletPipeline`].
pub(crate) struct LoadedControlledWallet {
    pubkey: Pubkey,
    signer: SignerRef,
}

impl LoadedControlledWallet {
    pub(crate) fn pubkey(&self) -> Pubkey {
        self.pubkey
    }

    /// Handle to the keystore-held signer, for the refund rail's own pipeline.
    ///
    /// Purely additive: the execute rail's behaviour is unchanged. The signer
    /// is still never exposed outside the crate, and the value is a handle into
    /// the frozen keystore rather than key material.
    pub(crate) fn signer_ref(&self) -> SignerRef {
        self.signer.clone()
    }
}

/// Load the controlled wallet using the configured environment variable.
pub(crate) fn load_controlled_wallet_from_env(
    pinned_pubkey: Pubkey,
) -> Result<LoadedControlledWallet, WatchWalletError> {
    let path = env::var_os(CONTROLLED_WALLET_KEYPAIR_ENV)
        .filter(|value| !value.is_empty())
        .ok_or(WatchWalletError::KeypairConfigMissing)?;
    load_controlled_wallet(Path::new(&path), pinned_pubkey)
}

/// Load one Solana CLI JSON keypair through [`SecretKeystore`].
///
/// Both the caller-owned JSON file buffer and the parsed raw-key buffer are
/// overwritten before this function returns, on success and failure. This
/// module never logs the path, input bytes, or parser details.
pub(crate) fn load_controlled_wallet(
    path: &Path,
    pinned_pubkey: Pubkey,
) -> Result<LoadedControlledWallet, WatchWalletError> {
    let mut key_file_bytes = fs::read(path).map_err(|_| WatchWalletError::KeypairReadFailed)?;
    let keystore = SecretKeystore::new();

    let parsed_result = serde_json::from_slice::<Vec<u8>>(&key_file_bytes)
        .map_err(|_| WatchWalletError::KeypairFormatInvalid);
    key_file_bytes.fill(0);
    let mut parsed_key_bytes = parsed_result?;
    let load_result = keystore
        .load_from_bytes(&parsed_key_bytes)
        .map_err(|_| WatchWalletError::KeypairFormatInvalid);
    parsed_key_bytes.fill(0);
    let loaded_pubkey = load_result?;
    if loaded_pubkey != pinned_pubkey {
        return Err(WatchWalletError::ControlledWalletMismatch);
    }

    let signer: SignerRef = Arc::new(LocalKeypairSigner::new(loaded_pubkey, keystore));
    Ok(LoadedControlledWallet {
        pubkey: loaded_pubkey,
        signer,
    })
}

/// The result of simulation, policy approval, and controlled-wallet signing.
///
/// The signed byte vector remains private. A submitter must request it through
/// [`ReviewedSignedTransaction::submission_bytes`], which re-deserializes it and
/// repeats the approval/signature identity checks immediately before use.
pub(crate) struct ReviewedSignedTransaction {
    signature: Signature,
    signer_pubkey: Pubkey,
    last_valid_block_height: u64,
    approved_message_bytes: Vec<u8>,
    signed_message_bytes: Vec<u8>,
    signed_transaction_bytes: Vec<u8>,
}

impl ReviewedSignedTransaction {
    pub(crate) fn signature(&self) -> Signature {
        self.signature
    }

    pub(crate) fn last_valid_block_height(&self) -> u64 {
        self.last_valid_block_height
    }

    /// Return exactly the bytes captured from the wallet-engine signer.
    ///
    /// This is the final pre-submission check. It proves that the approved
    /// message, post-sign message, and message contained in the bytes supplied
    /// to the submitter are identical.
    pub(crate) fn submission_bytes(&self) -> Result<&[u8], WatchWalletError> {
        if self.approved_message_bytes != self.signed_message_bytes {
            return Err(WatchWalletError::MessageBytesChanged);
        }

        let transaction: Transaction = bincode::deserialize(&self.signed_transaction_bytes)
            .map_err(|_| WatchWalletError::SerializationFailed)?;
        let pre_submit_message_bytes = transaction.message.serialize();
        if pre_submit_message_bytes != self.approved_message_bytes
            || pre_submit_message_bytes != self.signed_message_bytes
        {
            return Err(WatchWalletError::MessageBytesChanged);
        }

        let signer_index = transaction
            .message
            .account_keys
            .iter()
            .position(|pubkey| pubkey == &self.signer_pubkey)
            .ok_or(WatchWalletError::SignatureInvalid)?;
        let signature = transaction
            .signatures
            .get(signer_index)
            .ok_or(WatchWalletError::SignatureInvalid)?;
        if self.signature == Signature::default() || signature != &self.signature {
            return Err(WatchWalletError::SignatureInvalid);
        }
        transaction
            .verify()
            .map_err(|_| WatchWalletError::SignatureInvalid)?;

        Ok(&self.signed_transaction_bytes)
    }
}

/// Production adapter around the frozen wallet-engine pipeline.
pub(crate) struct WatchWalletPipeline {
    pipeline: TransactionReviewPipeline,
    controlled_wallet: LoadedControlledWallet,
}

impl WatchWalletPipeline {
    pub(crate) fn new(rpc_pool: RpcPool, controlled_wallet: LoadedControlledWallet) -> Self {
        let rpc = ClawRpcClient::new(rpc_pool.clone(), CommitmentLevel::Confirmed);
        let simulation = SimulationClient::new(rpc.clone());
        let blockhashes = Arc::new(BlockhashManager::new(rpc_pool));
        let pipeline = TransactionReviewPipeline::new(
            rpc,
            simulation,
            blockhashes,
            PriorityFeeStrategy::None,
            ApprovalMode::Automatic,
        );
        Self {
            pipeline,
            controlled_wallet,
        }
    }

    /// Simulate, evaluate a real risk policy, require automatic approval, and
    /// sign through the wallet-engine typestate.
    pub(crate) async fn review_and_sign<F>(
        &self,
        intent_id: &str,
        session_id: SessionId,
        unsigned_transaction: Transaction,
        canonical: CanonicalExecutionPolicy,
        before_signing_audit: F,
    ) -> Result<ReviewedSignedTransaction, WatchWalletError>
    where
        F: FnOnce(&Transaction) -> Result<(), WatchWalletError>,
    {
        let summaries = validate_and_summarize(
            &unsigned_transaction,
            self.controlled_wallet.pubkey,
            &canonical,
        )?;
        let initial_transaction_bytes = bincode::serialize(&unsigned_transaction)
            .map_err(|_| WatchWalletError::SerializationFailed)?;
        let mut proposal = TransactionProposal {
            id: Uuid::new_v4(),
            session_id,
            wallet_pubkey: self.controlled_wallet.pubkey.to_string(),
            network: SolanaNetwork::MainnetBeta,
            description: format!("execute canonical Solend deposit for intent {intent_id}"),
            transaction_b64: BASE64_STANDARD.encode(initial_transaction_bytes),
            instructions_summary: summaries,
            created_at: chrono::Utc::now(),
        };

        let simulated = self
            .pipeline
            .simulate(&proposal, unsigned_transaction)
            .await
            .map_err(|_| WatchWalletError::SimulationFailed)?;

        // Simulation has attached the exact fresh blockhash. Make that exact
        // unsigned message visible before policy approval or signer access.
        // The signer wrapper can only be obtained from the success continuation,
        // so an absent audit record makes signing structurally unreachable.
        let capturing =
            after_successful_pre_signing_audit(simulated.inner(), before_signing_audit, || {
                Arc::new(CapturingSigner::new(self.controlled_wallet.signer.clone()))
            })?;
        let signer: SignerRef = capturing.clone();

        // Bind the proposal's serialized transaction field to the finalized
        // unsigned transaction (fresh blockhash included) that policy sees.
        let finalized_unsigned_bytes = bincode::serialize(simulated.inner())
            .map_err(|_| WatchWalletError::SerializationFailed)?;
        proposal.transaction_b64 = BASE64_STANDARD.encode(finalized_unsigned_bytes);
        proposal.instructions_summary =
            validate_and_summarize(simulated.inner(), self.controlled_wallet.pubkey, &canonical)?;

        let policy = execution_policy(&canonical)?;
        let evaluation = policy.evaluate(&PolicyEvaluationContext {
            proposal: &proposal,
            simulation_result: Some(simulated.simulation()),
            network: SolanaNetwork::MainnetBeta,
            session_id: &proposal.session_id,
            session_spend_lamports: 0,
            wallet_daily_spend_lamports: 0,
        });
        let verdict = evaluation.verdict;
        let approved = self
            .pipeline
            .evaluate_policy(&proposal, simulated, move |_| verdict.clone())
            .map_err(|error| match error {
                WalletError::PolicyBlocked { .. } => WatchWalletError::PolicyRejected,
                _ => WatchWalletError::PolicyRejected,
            })?;

        // Frozen wallet-engine currently treats some non-blocked, non-approved
        // verdicts as typestate-eligible. Require the positive verdict explicitly.
        if !approved.policy_verdict().is_auto_approved() {
            return Err(WatchWalletError::PolicyNotAutoApproved);
        }

        let approved_message_bytes = approved.inner().inner().message.serialize();
        let last_valid_block_height = approved.inner().last_valid_block_height();

        let result = self
            .pipeline
            .sign(&proposal, approved, &signer)
            .await
            .map_err(|_| WatchWalletError::SigningFailed)?;
        let pipeline_signature = match result {
            PipelineResult::Signed { signature, .. } => signature,
            _ => return Err(WatchWalletError::SigningFailed),
        };
        let captured = capturing.take_capture()?;
        if pipeline_signature != captured.signature.to_string() {
            return Err(WatchWalletError::SignatureInvalid);
        }

        let reviewed = ReviewedSignedTransaction {
            signature: captured.signature,
            signer_pubkey: self.controlled_wallet.pubkey,
            last_valid_block_height,
            approved_message_bytes,
            signed_message_bytes: captured.message_bytes,
            signed_transaction_bytes: captured.transaction_bytes,
        };
        // Run the same check once now; the submitter must call it again when it
        // takes the bytes.
        let _ = reviewed.submission_bytes()?;
        Ok(reviewed)
    }
}

/// Return access to the signing continuation only after the exact unsigned
/// transaction has been emitted successfully. Keeping this as a small seam
/// makes the human-audit-before-signer ordering independently testable.
fn after_successful_pre_signing_audit<T, A, N>(
    transaction: &Transaction,
    audit: A,
    next: N,
) -> Result<T, WatchWalletError>
where
    A: FnOnce(&Transaction) -> Result<(), WatchWalletError>,
    N: FnOnce() -> T,
{
    audit(transaction)?;
    Ok(next())
}

#[derive(Clone)]
struct CapturingSigner {
    inner: SignerRef,
    capture: Arc<Mutex<Option<SignedCapture>>>,
}

struct SignedCapture {
    signature: Signature,
    message_bytes: Vec<u8>,
    transaction_bytes: Vec<u8>,
}

impl CapturingSigner {
    fn new(inner: SignerRef) -> Self {
        Self {
            inner,
            capture: Arc::new(Mutex::new(None)),
        }
    }

    fn take_capture(&self) -> Result<SignedCapture, WatchWalletError> {
        self.capture
            .lock()
            .map_err(|_| WatchWalletError::CaptureUnavailable)?
            .take()
            .ok_or(WatchWalletError::SignedCaptureMissing)
    }
}

#[async_trait]
impl Signer for CapturingSigner {
    fn pubkey(&self) -> Pubkey {
        self.inner.pubkey()
    }

    async fn sign_transaction(
        &self,
        transaction: &mut Transaction,
    ) -> Result<Signature, WalletError> {
        let before = transaction.message.serialize();
        let signature = self.inner.sign_transaction(transaction).await?;
        let after = transaction.message.serialize();
        if before != after {
            return Err(WalletError::SigningFailed(
                "capturing_signer_message_changed".to_owned(),
            ));
        }
        let transaction_bytes = bincode::serialize(transaction)
            .map_err(|_| WalletError::Serialization("signed_capture_failed".to_owned()))?;
        let captured = SignedCapture {
            signature,
            message_bytes: after,
            transaction_bytes,
        };
        let mut slot = self
            .capture
            .lock()
            .map_err(|_| WalletError::SigningFailed("signed_capture_unavailable".to_owned()))?;
        if slot.is_some() {
            return Err(WalletError::SigningFailed(
                "signed_capture_already_present".to_owned(),
            ));
        }
        *slot = Some(captured);
        Ok(signature)
    }

    async fn sign_versioned(
        &self,
        _transaction: &mut VersionedTransaction,
    ) -> Result<Signature, WalletError> {
        Err(WalletError::SigningFailed(
            "versioned_transactions_not_supported_by_executor".to_owned(),
        ))
    }

    fn description(&self) -> String {
        format!("controlled-wallet-capture:{}", self.pubkey())
    }

    fn is_automatic(&self) -> bool {
        self.inner.is_automatic()
    }
}

fn execution_policy(canonical: &CanonicalExecutionPolicy) -> Result<PolicySet, WatchWalletError> {
    let approved_limit = canonical
        .input_amount_raw
        .checked_add(1)
        .ok_or(WatchWalletError::TransactionShapeRejected)?;
    Ok(PolicySet::new(
        vec![
            PolicyRule {
                name: "simulation-required".to_owned(),
                description: "Reject transactions without a successful simulation".to_owned(),
                condition: PolicyCondition::SimulationNotPassed,
                action: PolicyAction::Reject {
                    reason: "simulation did not pass".to_owned(),
                },
            },
            PolicyRule {
                name: "program-allowlist".to_owned(),
                description: "Reject programs outside ComputeBudget and pinned Solend".to_owned(),
                condition: PolicyCondition::ProgramNotInAllowlist,
                action: PolicyAction::Reject {
                    reason: "program outside executor allowlist".to_owned(),
                },
            },
            PolicyRule {
                name: "legacy-token-transfer".to_owned(),
                description: "Reject opaque legacy token transfers".to_owned(),
                condition: PolicyCondition::LegacyTokenTransferPresent,
                action: PolicyAction::Reject {
                    reason: "legacy token transfer is not reviewable".to_owned(),
                },
            },
            PolicyRule {
                name: "usdc-only".to_owned(),
                description: "Reject any non-USDC token movement".to_owned(),
                condition: PolicyCondition::MintNotInAllowlist {
                    allowed_mints: vec![MAINNET_USDC_MINT_BS58.to_owned()],
                },
                action: PolicyAction::Reject {
                    reason: "executor permits only mainnet USDC".to_owned(),
                },
            },
            PolicyRule {
                name: "canonical-amount-cap".to_owned(),
                description: "Reject token movement above the fingerprint-bound amount".to_owned(),
                condition: PolicyCondition::TokenAmountExceeds {
                    mint: MAINNET_USDC_MINT_BS58.to_owned(),
                    threshold: approved_limit,
                },
                action: PolicyAction::Reject {
                    reason: "amount exceeds canonical intent".to_owned(),
                },
            },
            PolicyRule {
                name: "canonical-solend-deposit".to_owned(),
                description: "Approve the fully validated canonical Solend deposit".to_owned(),
                condition: PolicyCondition::Always,
                action: PolicyAction::Approve,
            },
        ],
        vec![
            compute_budget::id().to_string(),
            AUDITED_SOLEND_PROGRAM_BS58.to_owned(),
        ],
        vec![],
    ))
}

fn validate_and_summarize(
    transaction: &Transaction,
    controlled_wallet: Pubkey,
    canonical: &CanonicalExecutionPolicy,
) -> Result<Vec<InstructionSummary>, WatchWalletError> {
    let audited_solend = parse_pinned(AUDITED_SOLEND_PROGRAM_BS58);
    let usdc = parse_pinned(MAINNET_USDC_MINT_BS58);
    let token_program = parse_pinned(CLASSIC_TOKEN_PROGRAM_BS58);
    if canonical.solend_program_id != audited_solend
        || canonical.input_mint != usdc
        || canonical.input_amount_raw == 0
        || !message_matches_builder_template(transaction, &canonical.expected_message_bytes)
        || transaction.message.instructions.len() != EXPECTED_INSTRUCTION_COUNT
        || transaction.message.account_keys.first() != Some(&controlled_wallet)
        || transaction.message.header.num_required_signatures != 1
        || transaction.signatures.len() != 1
        || transaction
            .signatures
            .iter()
            .any(|signature| *signature != Signature::default())
    {
        return Err(WatchWalletError::TransactionShapeRejected);
    }

    let program_ids = transaction
        .message
        .instructions
        .iter()
        .map(|instruction| {
            transaction
                .message
                .account_keys
                .get(instruction.program_id_index as usize)
                .copied()
                .ok_or(WatchWalletError::TransactionShapeRejected)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if program_ids
        != vec![
            compute_budget::id(),
            compute_budget::id(),
            audited_solend,
            audited_solend,
        ]
        || transaction.message.instructions[0].data != REVIEWED_COMPUTE_UNIT_LIMIT_DATA
        || transaction.message.instructions[1].data != REVIEWED_COMPUTE_UNIT_PRICE_DATA
    {
        return Err(WatchWalletError::TransactionShapeRejected);
    }

    let refresh = &transaction.message.instructions[2];
    if refresh.data.as_slice() != [SOLEND_REFRESH_RESERVE_TAG] {
        return Err(WatchWalletError::TransactionShapeRejected);
    }
    let deposit = &transaction.message.instructions[3];
    if deposit.data.len() != 9
        || deposit.data[0] != SOLEND_DEPOSIT_AND_OBLIGATION_TAG
        || deposit.accounts.len() != EXPECTED_DEPOSIT_ACCOUNT_COUNT
    {
        return Err(WatchWalletError::TransactionShapeRejected);
    }
    let encoded_amount = u64::from_le_bytes(
        deposit.data[1..9]
            .try_into()
            .map_err(|_| WatchWalletError::TransactionShapeRejected)?,
    );
    if encoded_amount != canonical.input_amount_raw
        || instruction_account(transaction, deposit, 0)? != canonical.source_liquidity
        || instruction_account(transaction, deposit, 3)? != canonical.reserve_liquidity_supply
        || instruction_account(transaction, deposit, 9)? != controlled_wallet
        || instruction_account(transaction, deposit, 12)? != controlled_wallet
        || instruction_account(transaction, deposit, 13)? != token_program
    {
        return Err(WatchWalletError::TransactionShapeRejected);
    }

    transaction
        .message
        .instructions
        .iter()
        .enumerate()
        .map(|(instruction_index, instruction)| {
            let program_id = program_ids[instruction_index];
            let accounts = instruction
                .accounts
                .iter()
                .map(|account_index| {
                    let index = usize::from(*account_index);
                    let pubkey = transaction
                        .message
                        .account_keys
                        .get(index)
                        .ok_or(WatchWalletError::TransactionShapeRejected)?;
                    Ok(AccountRole {
                        pubkey: pubkey.to_string(),
                        label: None,
                        is_signer: transaction.message.is_signer(index),
                        is_writable: transaction.message.is_maybe_writable(index, None),
                    })
                })
                .collect::<Result<Vec<_>, WatchWalletError>>()?;
            let token_transfer = (instruction_index == 3).then(|| TokenTransfer {
                mint: canonical.input_mint.to_string(),
                amount: canonical.input_amount_raw,
                decimals: Some(6),
                source: canonical.source_liquidity.to_string(),
                destination: canonical.reserve_liquidity_supply.to_string(),
            });
            Ok(InstructionSummary {
                program_id: program_id.to_string(),
                program_name: Some(
                    if program_id == compute_budget::id() {
                        "ComputeBudget"
                    } else {
                        "Solend"
                    }
                    .to_owned(),
                ),
                description: match instruction_index {
                    0 => "set compute unit limit",
                    1 => "set compute unit price",
                    2 => "refresh the fingerprint-bound Solend reserve",
                    3 => "deposit the fingerprint-bound USDC amount",
                    _ => "unsupported instruction",
                }
                .to_owned(),
                transfer_lamports: None,
                token_transfer,
                is_legacy_token_transfer: false,
                accounts,
            })
        })
        .collect()
}

fn message_matches_builder_template(
    transaction: &Transaction,
    expected_placeholder_message_bytes: &[u8],
) -> bool {
    let mut normalized = transaction.message.clone();
    normalized.recent_blockhash = Default::default();
    normalized.serialize() == expected_placeholder_message_bytes
}

fn instruction_account(
    transaction: &Transaction,
    instruction: &solana_sdk::instruction::CompiledInstruction,
    position: usize,
) -> Result<Pubkey, WatchWalletError> {
    let account_index = instruction
        .accounts
        .get(position)
        .ok_or(WatchWalletError::TransactionShapeRejected)?;
    transaction
        .message
        .account_keys
        .get(usize::from(*account_index))
        .copied()
        .ok_or(WatchWalletError::TransactionShapeRejected)
}

fn parse_pinned(value: &'static str) -> Pubkey {
    value
        .parse()
        .expect("reviewed protocol identity must be a valid pubkey")
}

#[cfg(test)]
mod tests {
    use super::*;
    use claw_types::{policy::PolicyVerdict, transaction::SimulationResult};
    use solana_sdk::{
        hash::Hash,
        message::Message,
        signature::{Keypair, Signer as SolanaSigner},
        system_instruction,
    };

    fn successful_simulation() -> SimulationResult {
        SimulationResult {
            success: true,
            error: None,
            compute_units_used: Some(100_000),
            logs: vec![],
            return_data: None,
            account_diffs: vec![],
            fee_lamports: None,
        }
    }

    #[test]
    fn controlled_keypair_loader_pins_pubkey_and_exposes_only_fixed_errors() {
        let keypair = Keypair::new();
        let path = std::env::temp_dir().join(format!(
            "solfrontier-controlled-wallet-test-{}.json",
            Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            serde_json::to_vec(&keypair.to_bytes().to_vec()).expect("serialize key fixture"),
        )
        .expect("write key fixture");

        let loaded = load_controlled_wallet(&path, keypair.pubkey()).expect("matching pin");
        assert_eq!(loaded.pubkey(), keypair.pubkey());
        assert_eq!(
            load_controlled_wallet(&path, Pubkey::new_unique())
                .err()
                .expect("mismatched pin must fail")
                .class(),
            "controlled_wallet_mismatch"
        );

        std::fs::remove_file(&path).expect("remove key fixture");
        let missing = load_controlled_wallet(&path, keypair.pubkey())
            .err()
            .expect("missing path must be sanitized");
        assert_eq!(missing.class(), "controlled_keypair_read_failed");
        assert_eq!(missing.to_string(), "controlled_keypair_read_failed");
    }

    fn proposal(program_id: &str, mint: &str, amount: u64) -> TransactionProposal {
        let session_id = SessionId::new();
        TransactionProposal {
            id: Uuid::new_v4(),
            session_id,
            wallet_pubkey: Pubkey::new_unique().to_string(),
            network: SolanaNetwork::MainnetBeta,
            description: "offline policy fixture".to_owned(),
            transaction_b64: String::new(),
            instructions_summary: vec![InstructionSummary {
                program_id: program_id.to_owned(),
                program_name: None,
                description: "semantic deposit movement".to_owned(),
                transfer_lamports: None,
                token_transfer: Some(TokenTransfer {
                    mint: mint.to_owned(),
                    amount,
                    decimals: Some(6),
                    source: Pubkey::new_unique().to_string(),
                    destination: Pubkey::new_unique().to_string(),
                }),
                is_legacy_token_transfer: false,
                accounts: vec![],
            }],
            created_at: chrono::Utc::now(),
        }
    }

    fn evaluate(
        policy: &PolicySet,
        proposal: &TransactionProposal,
        simulation: &SimulationResult,
    ) -> PolicyVerdict {
        policy
            .evaluate(&PolicyEvaluationContext {
                proposal,
                simulation_result: Some(simulation),
                network: SolanaNetwork::MainnetBeta,
                session_id: &proposal.session_id,
                session_spend_lamports: 0,
                wallet_daily_spend_lamports: 0,
            })
            .verdict
    }

    fn canonical(amount: u64) -> CanonicalExecutionPolicy {
        CanonicalExecutionPolicy {
            solend_program_id: parse_pinned(AUDITED_SOLEND_PROGRAM_BS58),
            input_mint: parse_pinned(MAINNET_USDC_MINT_BS58),
            input_amount_raw: amount,
            source_liquidity: Pubkey::new_unique(),
            reserve_liquidity_supply: Pubkey::new_unique(),
            expected_message_bytes: Vec::new(),
        }
    }

    #[test]
    fn builder_message_binding_allows_only_the_pipeline_blockhash() {
        let payer = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let instruction = system_instruction::transfer(&payer, &recipient, 1);
        let transaction = Transaction::new_unsigned(Message::new(&[instruction], Some(&payer)));
        let expected = transaction.message.serialize();

        let mut fresh = transaction.clone();
        fresh.message.recent_blockhash = Hash::new_unique();
        assert!(message_matches_builder_template(&fresh, &expected));

        fresh.message.instructions[0].data[4] ^= 1;
        assert!(!message_matches_builder_template(&fresh, &expected));
    }

    #[test]
    fn exact_audit_success_is_required_before_signer_access() {
        let payer = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let instruction = system_instruction::transfer(&payer, &recipient, 1);
        let transaction = Transaction::new_unsigned(Message::new(&[instruction], Some(&payer)));
        let order = Arc::new(Mutex::new(Vec::new()));

        let audit_order = order.clone();
        let signer_order = order.clone();
        let signer_access = after_successful_pre_signing_audit(
            &transaction,
            move |_| {
                audit_order.lock().expect("audit order lock").push("audit");
                Ok(())
            },
            move || {
                signer_order
                    .lock()
                    .expect("signer order lock")
                    .push("signer_access");
                true
            },
        )
        .expect("successful audit must release signer access");
        assert!(signer_access);
        assert_eq!(
            *order.lock().expect("final order lock"),
            vec!["audit", "signer_access"]
        );

        let signer_was_reached = Arc::new(Mutex::new(false));
        let signer_marker = signer_was_reached.clone();
        assert_eq!(
            after_successful_pre_signing_audit(
                &transaction,
                |_| Err(WatchWalletError::AuditOutputFailed),
                move || {
                    *signer_marker.lock().expect("signer marker lock") = true;
                },
            ),
            Err(WatchWalletError::AuditOutputFailed)
        );
        assert!(!*signer_was_reached.lock().expect("final signer marker lock"));
    }

    #[test]
    fn policy_approves_only_the_canonical_amount_and_mint() {
        let amount = 200_000;
        let policy = execution_policy(&canonical(amount)).expect("bounded amount");
        let proposal = proposal(AUDITED_SOLEND_PROGRAM_BS58, MAINNET_USDC_MINT_BS58, amount);
        assert!(evaluate(&policy, &proposal, &successful_simulation()).is_auto_approved());
    }

    #[test]
    fn reject_rules_precede_approval_for_program_mint_and_amount() {
        let amount = 200_000;
        let policy = execution_policy(&canonical(amount)).expect("bounded amount");
        let simulation = successful_simulation();

        let unknown_program = proposal(
            &Pubkey::new_unique().to_string(),
            MAINNET_USDC_MINT_BS58,
            amount,
        );
        assert!(evaluate(&policy, &unknown_program, &simulation).is_blocked());

        let wrong_mint = proposal(
            AUDITED_SOLEND_PROGRAM_BS58,
            &Pubkey::new_unique().to_string(),
            amount,
        );
        assert!(evaluate(&policy, &wrong_mint, &simulation).is_blocked());

        let excess = proposal(
            AUDITED_SOLEND_PROGRAM_BS58,
            MAINNET_USDC_MINT_BS58,
            amount + 1,
        );
        assert!(evaluate(&policy, &excess, &simulation).is_blocked());
    }

    #[test]
    fn failed_simulation_is_never_approved() {
        let amount = 200_000;
        let policy = execution_policy(&canonical(amount)).expect("bounded amount");
        let proposal = proposal(AUDITED_SOLEND_PROGRAM_BS58, MAINNET_USDC_MINT_BS58, amount);
        let mut simulation = successful_simulation();
        simulation.success = false;
        simulation.error = Some("offline fixture failure".to_owned());
        assert!(evaluate(&policy, &proposal, &simulation).is_blocked());
    }

    #[tokio::test]
    async fn captured_signature_binds_approved_signed_and_submitted_message_bytes() {
        let keypair = Keypair::new();
        let signer_pubkey = keypair.pubkey();
        let mut keypair_bytes = keypair.to_bytes();
        let keystore = SecretKeystore::new();
        keystore
            .load_from_bytes(&keypair_bytes)
            .expect("offline fixture key must load");
        keypair_bytes.fill(0);

        let recipient = Pubkey::new_unique();
        let instruction = system_instruction::transfer(&signer_pubkey, &recipient, 1);
        let mut transaction =
            Transaction::new_unsigned(Message::new(&[instruction], Some(&signer_pubkey)));
        transaction.message.recent_blockhash = Hash::new_unique();
        let approved_message_bytes = transaction.message.serialize();

        let inner: SignerRef = Arc::new(LocalKeypairSigner::new(signer_pubkey, keystore));
        let capturing = CapturingSigner::new(inner);
        let signature = capturing
            .sign_transaction(&mut transaction)
            .await
            .expect("offline transaction must sign");
        let captured = capturing
            .take_capture()
            .expect("signed transaction must be captured");
        assert_eq!(captured.signature, signature);

        let make_reviewed = |approved_message_bytes: Vec<u8>,
                             signed_message_bytes: Vec<u8>,
                             signed_transaction_bytes: Vec<u8>| {
            ReviewedSignedTransaction {
                signature,
                signer_pubkey,
                last_valid_block_height: 1,
                approved_message_bytes,
                signed_message_bytes,
                signed_transaction_bytes,
            }
        };

        let reviewed = make_reviewed(
            approved_message_bytes.clone(),
            captured.message_bytes.clone(),
            captured.transaction_bytes.clone(),
        );
        let submission_bytes = reviewed
            .submission_bytes()
            .expect("unchanged reviewed transaction must pass the submission gate");
        let submitted: Transaction =
            bincode::deserialize(submission_bytes).expect("captured bytes must deserialize");
        assert_eq!(approved_message_bytes, captured.message_bytes);
        assert_eq!(approved_message_bytes, submitted.message.serialize());
        submitted
            .verify()
            .expect("captured signature must verify against the submitted message");

        let mut drifted_approval = approved_message_bytes.clone();
        drifted_approval[0] ^= 1;
        let message_drift = make_reviewed(
            drifted_approval,
            captured.message_bytes.clone(),
            captured.transaction_bytes.clone(),
        );
        assert_eq!(
            message_drift.submission_bytes(),
            Err(WatchWalletError::MessageBytesChanged)
        );

        let mut drifted_signed_message = captured.message_bytes.clone();
        drifted_signed_message[0] ^= 1;
        let signed_message_drift = make_reviewed(
            approved_message_bytes.clone(),
            drifted_signed_message,
            captured.transaction_bytes.clone(),
        );
        assert_eq!(
            signed_message_drift.submission_bytes(),
            Err(WatchWalletError::MessageBytesChanged)
        );

        let mut drifted_transaction: Transaction =
            bincode::deserialize(&captured.transaction_bytes).expect("fixture must deserialize");
        drifted_transaction.message.recent_blockhash = Hash::new_unique();
        let bytes_drift = make_reviewed(
            approved_message_bytes,
            captured.message_bytes,
            bincode::serialize(&drifted_transaction).expect("drift fixture must serialize"),
        );
        assert_eq!(
            bytes_drift.submission_bytes(),
            Err(WatchWalletError::MessageBytesChanged)
        );
    }
}
