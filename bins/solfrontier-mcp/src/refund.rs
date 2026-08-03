//! The refund rail: return a stranded funding amount to the wallet that paid it.
//!
//! A second narrow exception to INV-1, and it must be read as narrowly as the
//! first. Moving funds out of the controlled ATA needs the controlled wallet's
//! signature, so this is a non-MCP, operator-launched CLI pinned to one wallet,
//! one mint and one shape. No MCP tool and no LLM can reach it.
//!
//! The operator's authority is deliberately thin. They may name an intent and
//! nothing else: the amount, the source, the destination and the deadline are
//! all re-derived from the funding row, so there is no flag that can send a
//! different amount or send it somewhere else.
//!
//! Order, and none of it is optional:
//!
//!   recover -> lease -> re-derive -> build -> simulate -> policy -> Approved
//!           -> sign -> JOURNAL -> broadcast -> finalized -> verify deltas
//!           -> mark_refunded
//!
//! The journal step sits between signing and broadcasting because that is the
//! only place it helps. See `refund_journal.rs`.

use std::path::Path;

use claw_solana_core::{
    rpc::EndpointConfig,
    {RpcPool, RpcPoolConfig},
};
use claw_state_store::{
    Database, DatabaseConfig, Stage2W5hFundingIntentRepository, W5hFundingIntent, W5hIntentStatus,
};
use serde_json::{json, Value};
use solana_sdk::pubkey::Pubkey;

use crate::{
    funding::{ConfirmationLevel, FundingTransactionReader, ObservedFundingTransaction},
    refund_builder::{build_unsigned_refund, placeholder_message_bytes, RefundPlan},
    refund_journal::{
        recovery_action, ChainPresence, JournalLookup, RecordedAttempt, RecoveryAction,
        RefundJournal,
    },
    refund_wallet::{CanonicalRefundPolicy, RefundWalletPipeline},
    watch_submission::{submit_once_and_observe, NetworkOutcome, ReviewedSignedPayload},
    watch_wallet::load_controlled_wallet_from_env,
};

const MAINNET_USDC_MINT_BS58: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const REFUND_RPC_ENDPOINT_LABEL: &str = "refund";
const REFUND_RPC_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Why a refund was refused, as a stable class. Never carries an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefundVerifyError {
    NotFinalized,
    TransactionFailed,
    WrongSigner,
    TransferCount,
    TransferMismatch,
    SourceBalanceUnreadable,
    DestinationBalanceUnreadable,
    SourceDeltaMismatch,
    DestinationDeltaMismatch,
    UnexpectedMemo,
}

impl RefundVerifyError {
    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::NotFinalized => "refund_not_finalized",
            Self::TransactionFailed => "refund_transaction_failed",
            Self::WrongSigner => "refund_wrong_signer",
            Self::TransferCount => "refund_transfer_count",
            Self::TransferMismatch => "refund_transfer_mismatch",
            Self::SourceBalanceUnreadable => "refund_source_balance_unreadable",
            Self::DestinationBalanceUnreadable => "refund_destination_balance_unreadable",
            Self::SourceDeltaMismatch => "refund_source_delta_mismatch",
            Self::DestinationDeltaMismatch => "refund_destination_delta_mismatch",
            Self::UnexpectedMemo => "refund_unexpected_memo",
        }
    }
}

/// What the chain says actually happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefundProof {
    pub(crate) slot: u64,
    pub(crate) source_delta: i128,
    pub(crate) destination_delta: i128,
}

fn balance_of<'a>(
    rows: &'a [crate::funding::ObservedTokenBalance],
    account: &str,
    owner: &str,
    mint: &str,
) -> Option<&'a crate::funding::ObservedTokenBalance> {
    rows.iter()
        .find(|row| row.account == account && row.owner == owner && row.mint == mint)
}

/// The gate the operator asked for: finalized, and both deltas exact.
///
/// PURE. Production feeds it a normalized `getTransaction`; tests build the
/// observation directly, so every branch is reachable without a network.
///
/// A missing PRE row means the account was created in this transaction and
/// therefore held zero. A missing POST row is fatal in both directions: for the
/// source it would mean the account vanished, and for the destination it would
/// mean nothing arrived. Absence is never read as a value.
pub(crate) fn verify_refund_landed(
    observed: &ObservedFundingTransaction,
    plan: &RefundPlan,
) -> Result<RefundProof, RefundVerifyError> {
    if observed.confirmation != ConfirmationLevel::Finalized {
        return Err(RefundVerifyError::NotFinalized);
    }
    if !observed.succeeded {
        return Err(RefundVerifyError::TransactionFailed);
    }
    let controlled = plan.controlled_wallet.to_string();
    if !observed.signer_pubkeys.iter().any(|key| *key == controlled) {
        return Err(RefundVerifyError::WrongSigner);
    }
    // This rail emits no memo, so any memo means this is not our transaction.
    if !observed.memos.is_empty() {
        return Err(RefundVerifyError::UnexpectedMemo);
    }
    if observed.transfer_checked.len() != 1 {
        return Err(RefundVerifyError::TransferCount);
    }

    let mint = plan.mint.to_string();
    let source = plan.controlled_ata.to_string();
    let destination = plan.user_ata.to_string();
    let user = plan.user_wallet.to_string();

    let transfer = &observed.transfer_checked[0];
    if transfer.source != source
        || transfer.destination != destination
        || transfer.authority != controlled
        || transfer.mint != mint
        || transfer.amount_raw != plan.amount_raw
        || transfer.decimals != plan.decimals
    {
        return Err(RefundVerifyError::TransferMismatch);
    }

    let amount = i128::from(plan.amount_raw);

    let source_pre = balance_of(&observed.pre_token_balances, &source, &controlled, &mint)
        .map_or(0_i128, |row| i128::from(row.amount_raw));
    let source_post = balance_of(&observed.post_token_balances, &source, &controlled, &mint)
        .ok_or(RefundVerifyError::SourceBalanceUnreadable)?;
    let source_delta = i128::from(source_post.amount_raw) - source_pre;
    if source_delta != -amount {
        return Err(RefundVerifyError::SourceDeltaMismatch);
    }

    let destination_pre = balance_of(&observed.pre_token_balances, &destination, &user, &mint)
        .map_or(0_i128, |row| i128::from(row.amount_raw));
    let destination_post = balance_of(&observed.post_token_balances, &destination, &user, &mint)
        .ok_or(RefundVerifyError::DestinationBalanceUnreadable)?;
    let destination_delta = i128::from(destination_post.amount_raw) - destination_pre;
    if destination_delta != amount {
        return Err(RefundVerifyError::DestinationDeltaMismatch);
    }

    Ok(RefundProof {
        slot: observed.slot,
        source_delta,
        destination_delta,
    })
}

/// Turn a funding row into a refund plan. Every value comes from the row.
pub(crate) fn plan_from_row(row: &W5hFundingIntent) -> anyhow::Result<RefundPlan> {
    let parse = |value: &str, what: &'static str| -> anyhow::Result<Pubkey> {
        value
            .parse::<Pubkey>()
            .map_err(|_| anyhow::anyhow!("refund_row_field_invalid:{what}"))
    };
    Ok(RefundPlan {
        intent_id: row.intent_id.clone(),
        amount_raw: row.amount_raw,
        mint: MAINNET_USDC_MINT_BS58
            .parse::<Pubkey>()
            .map_err(|_| anyhow::anyhow!("refund_pinned_mint_invalid"))?,
        decimals: 6,
        controlled_wallet: parse(&row.controlled_wallet, "controlled_wallet")?,
        controlled_ata: parse(&row.controlled_usdc_ata, "controlled_usdc_ata")?,
        user_wallet: parse(&row.user_wallet, "user_wallet")?,
        user_ata: parse(&row.user_usdc_ata, "user_usdc_ata")?,
    })
}

/// Human-auditable disclosure of exactly what would move.
fn disclosure(plan: &RefundPlan, row: &W5hFundingIntent, execute: bool) -> Value {
    json!({
        "mode": if execute { "execute" } else { "dry_run" },
        "intent_id": plan.intent_id,
        "status": row.status.as_str(),
        "amount_raw": plan.amount_raw.to_string(),
        "mint": plan.mint.to_string(),
        "decimals": plan.decimals,
        "from": { "wallet": plan.controlled_wallet.to_string(), "ata": plan.controlled_ata.to_string() },
        "to":   { "wallet": plan.user_wallet.to_string(),       "ata": plan.user_ata.to_string() },
        "funding_expires_at_ms": row.expires_at_ms,
        "original_funding_signature": row.funding_signature,
        "derivation": "every value above comes from the funding row; no operator flag can change any of them",
    })
}

fn emit(value: &Value) {
    // stderr only: stdout belongs to JSON-RPC on the MCP path, and this report
    // is for a human reviewing an irreversible action.
    eprintln!("{value}");
}

fn refund_rpc_pool() -> Option<RpcPool> {
    let rpc_url = std::env::var("SOLFRONTIER_RPC_URL").ok()?;
    if rpc_url.trim().is_empty() {
        return None;
    }
    Some(RpcPool::new(RpcPoolConfig {
        endpoints: vec![EndpointConfig {
            url: rpc_url,
            label: REFUND_RPC_ENDPOINT_LABEL.to_owned(),
            is_write_endpoint: true,
        }],
        request_timeout: REFUND_RPC_REQUEST_TIMEOUT,
        ..RpcPoolConfig::default()
    }))
}

/// Refund one intent. Dry-run unless `execute` is explicit.
///
/// The dry run has no lease, no keypair, no signature, no broadcast and no
/// database write: it reads the row, derives the plan, builds the unsigned
/// transaction and prints it.
pub(crate) async fn run_refund(
    db_path: &Path,
    intent_id: &str,
    execute: bool,
) -> anyhow::Result<()> {
    let database = Database::open(&DatabaseConfig {
        path: db_path.to_string_lossy().into_owned(),
        ..DatabaseConfig::default()
    })
    .await?;
    let intents = Stage2W5hFundingIntentRepository::new(database.pool().clone());

    let row = intents
        .get(intent_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("refund_intent_not_found"))?;
    let plan = plan_from_row(&row)?;

    // Build first, so a dry run shows the exact shape without any privilege.
    let unsigned = build_unsigned_refund(&plan).map_err(|error| anyhow::anyhow!(error.class()))?;
    let template =
        placeholder_message_bytes(&unsigned).map_err(|error| anyhow::anyhow!(error.class()))?;

    let mut report = disclosure(&plan, &row, execute);
    report["unsigned_message_bytes_len"] = json!(template.len());
    report["submittable"] = json!(false);
    emit(&report);

    if !execute {
        emit(&json!({
            "mode": "dry_run",
            "result": "no_lease_no_key_no_signature_no_broadcast_no_write",
            "next": "re-run with --execute to move funds; this is irreversible",
        }));
        return Ok(());
    }

    // ---- execute ---------------------------------------------------------
    let journal = RefundJournal::for_database_path(db_path);
    let rpc_pool = refund_rpc_pool().ok_or_else(|| anyhow::anyhow!("rpc_config_missing"))?;

    // Recovery BEFORE anything else. A previous attempt may already be on
    // chain, and finding that out afterwards is finding out too late.
    let lookup = journal.lookup(intent_id).await;

    // A missing journal is ambiguous on its own: it could mean nothing was ever
    // signed, or it could mean the record of something that WAS signed has been
    // deleted. The main database resolves it, which is the whole reason it stays
    // the source of truth.
    //
    // The lease moves the row to `refunding` BEFORE anything is signed, so a row
    // still sitting at `budget_reserved` or `expired` proves no signature can
    // exist. Journal absence is then benign, and this is also the ordinary state
    // of the very first refund on a database.
    //
    // Once the row says `refunding`, absence means the opposite, and the halt
    // below stands.
    let never_leased = matches!(
        row.status,
        W5hIntentStatus::BudgetReserved | W5hIntentStatus::Expired
    );
    let lookup = match (&lookup, never_leased) {
        (JournalLookup::Unavailable { error_class }, true) => {
            emit(&json!({
                "journal": "absent, and the row was never leased for refund",
                "detail": error_class,
                "reasoning": "the lease precedes signing, so budget_reserved/expired proves no signature exists",
            }));
            JournalLookup::Absent
        }
        _ => lookup,
    };

    let action = recovery_action(&lookup, ChainPresence::Unknown, None);
    if let JournalLookup::Found(attempt) = &lookup {
        emit(&json!({
            "recovery": "a previous refund attempt is recorded for this intent",
            "signature": attempt.signature,
            "last_valid_block_height": attempt.last_valid_block_height,
            "action": format!("{action:?}"),
            "rule": "resend the same bytes while they are still valid; never re-sign",
        }));
        anyhow::bail!("refund_recovery_required");
    }
    if let RecoveryAction::Halt { reason } = action {
        anyhow::bail!("refund_halt:{reason}");
    }

    let controlled = load_controlled_wallet_from_env(plan.controlled_wallet)
        .map_err(|error| anyhow::anyhow!(error.class()))?;
    if controlled.pubkey() != plan.controlled_wallet {
        anyhow::bail!("controlled_wallet_mismatch");
    }

    // The lease is the single atomic gate. It refuses unless the row is still
    // budget_reserved or expired AND the funding deadline has passed, so an
    // intent the executor could still legitimately act on cannot be refunded
    // out from under it.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let leased = intents
        .lease_refund_if_expired_or_past(intent_id, now_ms)
        .await?;
    if leased != 1 {
        anyhow::bail!("refund_lease_refused");
    }
    emit(&json!({ "lease": "acquired", "status": W5hIntentStatus::Refunding.as_str() }));

    // Re-read and re-derive after the CAS: the plan that gets signed must come
    // from the row as it is now, not from the read that happened before.
    let post_lease = intents
        .get(intent_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("refund_row_vanished_after_lease"))?;
    let plan = plan_from_row(&post_lease)?;
    let unsigned = build_unsigned_refund(&plan).map_err(|error| anyhow::anyhow!(error.class()))?;
    let template =
        placeholder_message_bytes(&unsigned).map_err(|error| anyhow::anyhow!(error.class()))?;
    let canonical = CanonicalRefundPolicy::from_plan(&plan, template);

    let wallet = RefundWalletPipeline::new(rpc_pool.clone(), controlled);
    let reviewed = wallet
        .review_and_sign(
            intent_id,
            claw_types::session::SessionId::new(),
            unsigned,
            canonical,
            |transaction| {
                // Last human-auditable emission before the signer exists.
                let bytes = transaction.message.serialize();
                eprintln!(
                    "{}",
                    json!({
                        "pre_signing_audit": "exact unsigned message, fresh blockhash bound",
                        "message_bytes_len": bytes.len(),
                        "blockhash": transaction.message.recent_blockhash.to_string(),
                    })
                );
                Ok(())
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.class()))?;

    let signed_bytes = reviewed
        .submission_bytes()
        .map_err(|error| anyhow::anyhow!(error.class()))?
        .to_vec();
    let signature = reviewed.signature();

    // JOURNAL BEFORE BROADCAST. If this fails we abort having sent nothing:
    // an unbroadcast signature is harmless, an unrecorded broadcast is not.
    journal
        .record_before_broadcast(
            intent_id,
            &RecordedAttempt {
                signature: signature.to_string(),
                last_valid_block_height: reviewed.last_valid_block_height(),
                signed_transaction: signed_bytes.clone(),
                recorded_at_ms: chrono::Utc::now().timestamp_millis(),
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.class()))?;
    emit(&json!({
        "journal": "recorded before broadcast",
        "signature": signature.to_string(),
        "last_valid_block_height": reviewed.last_valid_block_height(),
    }));

    let outcome = submit_once_and_observe(
        &rpc_pool,
        &ReviewedSignedPayload {
            signed_transaction_bytes: signed_bytes,
            signature,
            last_valid_block_height: reviewed.last_valid_block_height(),
        },
    )
    .await;

    match outcome {
        NetworkOutcome::Finalized { signature, slot } => {
            emit(&json!({ "broadcast": "finalized", "signature": signature, "slot": slot }));

            // Finalized is necessary and not sufficient. The row is only marked
            // refunded once the chain itself shows both balances moving by
            // exactly the amount: a transaction can finalize successfully and
            // still not be the transfer we intended.
            let reader = crate::funding::configured_funding_reader_from_env()
                .ok_or_else(|| anyhow::anyhow!("refund_verify_reader_unavailable"))?;
            let observed = match reader.read_funding_transaction(&signature).await {
                Ok(crate::funding::FundingTransactionRead::Confirmed(observed)) => observed,
                Ok(crate::funding::FundingTransactionRead::Pending) => {
                    // Broadcast said finalized; the read says pending. Leave the
                    // row in `refunding` rather than guessing which is right.
                    anyhow::bail!("refund_verify_pending:{signature}")
                }
                Err(error) => anyhow::bail!("refund_verify_read_failed:{error}:{signature}"),
            };
            let proof = verify_refund_landed(&observed, &plan)
                .map_err(|error| anyhow::anyhow!("{}:{signature}", error.class()))?;
            emit(&json!({
                "verified": "both deltas exact at finalized commitment",
                "slot": proof.slot,
                "controlled_delta": proof.source_delta.to_string(),
                "user_delta": proof.destination_delta.to_string(),
            }));

            // Only now. This is the single write that ends the lifecycle, and
            // it is CAS-bound to `refunding`, so a competing writer cannot be
            // overwritten.
            let marked = intents
                .mark_refunded_if_refunding(intent_id, &signature)
                .await?;
            if marked != 1 {
                // The chain moved and the ledger did not. Say so loudly rather
                // than exiting zero: this is the cross-repository non-
                // transactional boundary that DEBT-P3-EXECUTION-1 records, and
                // the refund itself is complete and correct on chain.
                emit(&json!({
                    "result": "refund_landed_but_row_not_marked",
                    "signature": signature,
                    "action_required": "the refund is final on chain; reconcile the row by hand",
                }));
                anyhow::bail!("refund_marked_rows:{marked}:{signature}");
            }
            emit(&json!({
                "result": "refunded",
                "intent_id": intent_id,
                "signature": signature,
                "amount_raw": plan.amount_raw.to_string(),
                "status": W5hIntentStatus::Refunded.as_str(),
            }));
            Ok(())
        }
        NetworkOutcome::PreBroadcastFailure { error_class } => {
            // Provably nothing was sent, so the journal entry describes bytes
            // that never left. Still not safe to release the lease
            // automatically here; a human decides.
            anyhow::bail!("refund_pre_broadcast_failure:{error_class}")
        }
        NetworkOutcome::OnChainFailed {
            signature,
            error_class,
        } => {
            anyhow::bail!("refund_on_chain_failed:{error_class}:{signature}")
        }
        NetworkOutcome::Unknown {
            signature,
            error_class,
        } => {
            // The ambiguity barrier. The row stays in `refunding` and the
            // journal holds the exact bytes; recovery resolves it by blockhash
            // expiry, never by signing again.
            anyhow::bail!("refund_outcome_unknown:{error_class}:{signature}")
        }
    }
}

#[cfg(test)]
#[path = "refund_tests.rs"]
mod refund_tests;
