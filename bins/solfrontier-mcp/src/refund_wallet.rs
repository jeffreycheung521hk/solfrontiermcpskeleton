//! Simulation, policy, typestate approval and signing for a refund transfer.
//!
//! This deliberately duplicates the shape of `watch_wallet.rs` rather than
//! generalising it. The execute rail is the one path in this project with
//! mainnet evidence behind it; widening its validator to also accept a plain
//! SPL transfer would loosen exactly the checks that make it trustworthy, and
//! mixing that refactor into the commit that introduces a new money-moving rail
//! is the sort of thing PC-2 exists to prevent. Two narrow validators, each
//! rejecting everything that is not its own single shape, are safer than one
//! that accepts two.
//!
//! What is shared, and only what is shared: the error taxonomy, the keypair
//! loader, and the frozen wallet-engine pipeline underneath.
//!
//! Gate order is identical to the execute rail and is not negotiable:
//!   shape check -> simulate -> pre-signing audit -> policy -> Approved
//!   typestate -> sign -> byte-identity check.

use std::sync::{Arc, Mutex};

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
    pipeline::PipelineResult, ApprovalMode, Signer, SignerRef, TransactionReviewPipeline,
    WalletError,
};
use solana_sdk::{
    compute_budget,
    pubkey::Pubkey,
    signature::Signature,
    transaction::{Transaction, VersionedTransaction},
};
use uuid::Uuid;

use crate::{
    refund_builder::{RefundPlan, REFUND_INSTRUCTION_COUNT},
    watch_wallet::{LoadedControlledWallet, WatchWalletError},
};

const MAINNET_USDC_MINT_BS58: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// SPL `TransferChecked` discriminator.
const SPL_TRANSFER_CHECKED_TAG: u8 = 12;
/// tag + u64 amount + u8 decimals.
const SPL_TRANSFER_CHECKED_DATA_LEN: usize = 10;
/// source, mint, destination, authority.
const SPL_TRANSFER_CHECKED_ACCOUNT_COUNT: usize = 4;

/// Values already re-derived from the funding row and the chain, bound once
/// more against the actual transaction before it enters the wallet pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalRefundPolicy {
    pub(crate) mint: Pubkey,
    pub(crate) amount_raw: u64,
    pub(crate) source_ata: Pubkey,
    pub(crate) destination_ata: Pubkey,
    /// Exact builder message bytes with the all-zero placeholder blockhash.
    /// Wallet review permits only the pipeline's fresh blockhash to differ.
    pub(crate) expected_message_bytes: Vec<u8>,
}

impl CanonicalRefundPolicy {
    pub(crate) fn from_plan(plan: &RefundPlan, expected_message_bytes: Vec<u8>) -> Self {
        Self {
            mint: plan.mint,
            amount_raw: plan.amount_raw,
            source_ata: plan.controlled_ata,
            destination_ata: plan.user_ata,
            expected_message_bytes,
        }
    }
}

/// A refund that passed every gate, with the exact bytes that were signed.
pub(crate) struct ReviewedSignedRefund {
    signature: Signature,
    last_valid_block_height: u64,
    approved_message_bytes: Vec<u8>,
    signed_message_bytes: Vec<u8>,
    signed_transaction_bytes: Vec<u8>,
}

impl ReviewedSignedRefund {
    pub(crate) fn signature(&self) -> Signature {
        self.signature
    }

    pub(crate) fn last_valid_block_height(&self) -> u64 {
        self.last_valid_block_height
    }

    /// The bytes to broadcast, released only if the approved message, the
    /// signed message and the message inside the serialized transaction are
    /// byte-identical. A refund whose approved and submitted bytes differ is
    /// not the refund that was reviewed.
    pub(crate) fn submission_bytes(&self) -> Result<&[u8], WatchWalletError> {
        if self.approved_message_bytes != self.signed_message_bytes {
            return Err(WatchWalletError::SignatureInvalid);
        }
        let decoded: Transaction = bincode::deserialize(&self.signed_transaction_bytes)
            .map_err(|_| WatchWalletError::SerializationFailed)?;
        if decoded.message.serialize() != self.signed_message_bytes {
            return Err(WatchWalletError::SignatureInvalid);
        }
        if decoded.signatures.len() != 1 || decoded.signatures[0] != self.signature {
            return Err(WatchWalletError::SignatureInvalid);
        }
        decoded
            .verify()
            .map_err(|_| WatchWalletError::SignatureInvalid)?;
        Ok(&self.signed_transaction_bytes)
    }
}

pub(crate) struct RefundWalletPipeline {
    pipeline: TransactionReviewPipeline,
    controlled_wallet: LoadedControlledWallet,
}

impl RefundWalletPipeline {
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

    pub(crate) fn controlled_pubkey(&self) -> Pubkey {
        self.controlled_wallet.pubkey()
    }

    /// Simulate, evaluate a real risk policy, require automatic approval, and
    /// sign through the wallet-engine typestate.
    pub(crate) async fn review_and_sign<F>(
        &self,
        intent_id: &str,
        session_id: SessionId,
        unsigned_transaction: Transaction,
        canonical: CanonicalRefundPolicy,
        before_signing_audit: F,
    ) -> Result<ReviewedSignedRefund, WatchWalletError>
    where
        F: FnOnce(&Transaction) -> Result<(), WatchWalletError>,
    {
        let wallet = self.controlled_wallet.pubkey();
        let summaries = validate_and_summarize(&unsigned_transaction, wallet, &canonical)?;
        let initial_bytes = bincode::serialize(&unsigned_transaction)
            .map_err(|_| WatchWalletError::SerializationFailed)?;
        let mut proposal = TransactionProposal {
            id: Uuid::new_v4(),
            session_id,
            wallet_pubkey: wallet.to_string(),
            network: SolanaNetwork::MainnetBeta,
            description: format!("refund the funded USDC amount for intent {intent_id}"),
            transaction_b64: BASE64_STANDARD.encode(initial_bytes),
            instructions_summary: summaries,
            created_at: chrono::Utc::now(),
        };

        let simulated = self
            .pipeline
            .simulate(&proposal, unsigned_transaction)
            .await
            .map_err(|_| WatchWalletError::SimulationFailed)?;

        // The exact unsigned message, fresh blockhash included, must be
        // emitted before the signer is reachable. Obtaining the signer only
        // from the audit's success continuation makes an unaudited signature
        // structurally impossible rather than merely forbidden.
        let capturing =
            after_successful_pre_signing_audit(simulated.inner(), before_signing_audit, || {
                Arc::new(CapturingSigner::new(self.controlled_wallet.signer_ref()))
            })?;
        let signer: SignerRef = capturing.clone();

        let finalized_unsigned_bytes = bincode::serialize(simulated.inner())
            .map_err(|_| WatchWalletError::SerializationFailed)?;
        proposal.transaction_b64 = BASE64_STANDARD.encode(finalized_unsigned_bytes);
        proposal.instructions_summary =
            validate_and_summarize(simulated.inner(), wallet, &canonical)?;

        let policy = refund_policy(&canonical)?;
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
            .map_err(|_| WatchWalletError::PolicyRejected)?;

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

        let reviewed = ReviewedSignedRefund {
            signature: captured.signature,
            last_valid_block_height,
            approved_message_bytes,
            signed_message_bytes: captured.message_bytes,
            signed_transaction_bytes: captured.transaction_bytes,
        };
        let _ = reviewed.submission_bytes()?;
        Ok(reviewed)
    }
}

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

/// Reject everything that is not exactly this refund, then describe what is
/// left for the policy engine.
fn validate_and_summarize(
    transaction: &Transaction,
    controlled_wallet: Pubkey,
    canonical: &CanonicalRefundPolicy,
) -> Result<Vec<InstructionSummary>, WatchWalletError> {
    let usdc = MAINNET_USDC_MINT_BS58
        .parse::<Pubkey>()
        .map_err(|_| WatchWalletError::TransactionShapeRejected)?;
    if canonical.mint != usdc
        || canonical.amount_raw == 0
        || canonical.source_ata == canonical.destination_ata
        || !message_matches_builder_template(transaction, &canonical.expected_message_bytes)
        || transaction.message.instructions.len() != REFUND_INSTRUCTION_COUNT
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
    if program_ids != vec![compute_budget::id(), compute_budget::id(), spl_token::id()] {
        return Err(WatchWalletError::TransactionShapeRejected);
    }

    // The transfer, byte for byte. Amount and decimals live in the instruction
    // data, so both are checked there rather than taken on trust from a
    // decoded summary.
    let transfer = &transaction.message.instructions[2];
    if transfer.data.len() != SPL_TRANSFER_CHECKED_DATA_LEN
        || transfer.data[0] != SPL_TRANSFER_CHECKED_TAG
        || transfer.accounts.len() != SPL_TRANSFER_CHECKED_ACCOUNT_COUNT
    {
        return Err(WatchWalletError::TransactionShapeRejected);
    }
    let encoded_amount = u64::from_le_bytes(
        transfer.data[1..9]
            .try_into()
            .map_err(|_| WatchWalletError::TransactionShapeRejected)?,
    );
    if encoded_amount != canonical.amount_raw
        || transfer.data[9] != 6
        || instruction_account(transaction, transfer, 0)? != canonical.source_ata
        || instruction_account(transaction, transfer, 1)? != canonical.mint
        || instruction_account(transaction, transfer, 2)? != canonical.destination_ata
        || instruction_account(transaction, transfer, 3)? != controlled_wallet
    {
        return Err(WatchWalletError::TransactionShapeRejected);
    }

    transaction
        .message
        .instructions
        .iter()
        .enumerate()
        .map(|(index, instruction)| {
            let program_id = program_ids[index];
            let accounts = instruction
                .accounts
                .iter()
                .map(|position| {
                    let key = transaction
                        .message
                        .account_keys
                        .get(*position as usize)
                        .copied()
                        .ok_or(WatchWalletError::TransactionShapeRejected)?;
                    Ok(AccountRole {
                        pubkey: key.to_string(),
                        label: None,
                        is_signer: transaction.message.is_signer(*position as usize),
                        is_writable: transaction
                            .message
                            .is_maybe_writable(*position as usize, None),
                    })
                })
                .collect::<Result<Vec<_>, WatchWalletError>>()?;
            let token_transfer = (index == 2).then(|| TokenTransfer {
                mint: canonical.mint.to_string(),
                amount: canonical.amount_raw,
                decimals: Some(6),
                source: canonical.source_ata.to_string(),
                destination: canonical.destination_ata.to_string(),
            });
            Ok(InstructionSummary {
                program_id: program_id.to_string(),
                program_name: Some(
                    if program_id == compute_budget::id() {
                        "ComputeBudget"
                    } else {
                        "SPL Token"
                    }
                    .to_owned(),
                ),
                description: match index {
                    0 => "set compute unit limit",
                    1 => "set compute unit price",
                    2 => "return the funded USDC amount to the registered funder",
                    _ => "unsupported instruction",
                }
                .to_owned(),
                transfer_lamports: None,
                token_transfer,
                // TransferChecked carries the mint and the decimals, which is
                // precisely what the legacy-transfer condition exists to
                // distinguish from opaque tag-3 Transfer. Both are pinned
                // above, so the amount and mint conditions can actually see
                // this instruction.
                is_legacy_token_transfer: false,
                accounts,
            })
        })
        .collect()
}

fn refund_policy(canonical: &CanonicalRefundPolicy) -> Result<PolicySet, WatchWalletError> {
    let approved_limit = canonical
        .amount_raw
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
                description: "Reject programs outside ComputeBudget and the SPL token program"
                    .to_owned(),
                condition: PolicyCondition::ProgramNotInAllowlist,
                action: PolicyAction::Reject {
                    reason: "program outside refund allowlist".to_owned(),
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
                    reason: "the refund rail permits only mainnet USDC".to_owned(),
                },
            },
            PolicyRule {
                name: "refund-amount-cap".to_owned(),
                description: "Reject token movement above the funded amount".to_owned(),
                condition: PolicyCondition::TokenAmountExceeds {
                    mint: MAINNET_USDC_MINT_BS58.to_owned(),
                    threshold: approved_limit,
                },
                action: PolicyAction::Reject {
                    reason: "amount exceeds the funded amount".to_owned(),
                },
            },
            PolicyRule {
                name: "canonical-refund".to_owned(),
                description: "Approve the fully validated refund transfer".to_owned(),
                condition: PolicyCondition::Always,
                action: PolicyAction::Approve,
            },
        ],
        vec![
            compute_budget::id().to_string(),
            spl_token::id().to_string(),
        ],
        vec![],
    ))
}

/// Everything except the blockhash must match the builder's template. The
/// blockhash is the one field the pipeline is allowed to replace.
fn message_matches_builder_template(transaction: &Transaction, expected: &[u8]) -> bool {
    let mut candidate = transaction.message.clone();
    candidate.recent_blockhash = solana_sdk::hash::Hash::default();
    candidate.serialize() == expected
}

fn instruction_account(
    transaction: &Transaction,
    instruction: &solana_sdk::instruction::CompiledInstruction,
    position: usize,
) -> Result<Pubkey, WatchWalletError> {
    let index = instruction
        .accounts
        .get(position)
        .ok_or(WatchWalletError::TransactionShapeRejected)?;
    transaction
        .message
        .account_keys
        .get(*index as usize)
        .copied()
        .ok_or(WatchWalletError::TransactionShapeRejected)
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
            "versioned_transactions_not_supported_by_refund_rail".to_owned(),
        ))
    }

    fn description(&self) -> String {
        format!("controlled-wallet-refund-capture:{}", self.pubkey())
    }

    fn is_automatic(&self) -> bool {
        self.inner.is_automatic()
    }
}
