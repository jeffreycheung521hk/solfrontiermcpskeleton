//! Unsigned refund transaction construction.
//!
//! A refund is the funding transfer in reverse: the controlled ATA pays the
//! registered funder's ATA back the exact amount that arrived. It is a plain
//! SPL `transferChecked` and touches no protocol program.
//!
//! This module is deliberately separate from the Solend execute builder rather
//! than a generalisation of it. `CanonicalExecutionPolicy` binds a Solend
//! program id, a source liquidity account and a reserve liquidity supply; none
//! of those exist here, and widening that structure to cover both shapes would
//! loosen the checks that guard the execute rail — the rail that already has
//! mainnet evidence behind it. Two narrow validators are safer than one general
//! one.
//!
//! Like the execute builder, the transaction leaves here with an all-zero
//! placeholder blockhash and default signature slots: it is not submittable,
//! and the wallet pipeline is the only thing that may attach a real blockhash.

use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, hash::Hash, instruction::Instruction,
    message::Message, pubkey::Pubkey, transaction::Transaction,
};

/// Compute-unit ceiling for a single SPL transfer. Far below the execute rail's
/// budget because the work is one instruction against one program.
pub(crate) const REFUND_COMPUTE_UNIT_LIMIT: u32 = 60_000;
/// Same priority fee the execute rail was reviewed with (50,000 micro-lamports).
pub(crate) const REFUND_COMPUTE_UNIT_PRICE_MICRO_LAMPORTS: u64 = 50_000;

/// compute budget limit, compute budget price, transferChecked.
pub(crate) const REFUND_INSTRUCTION_COUNT: usize = 3;

/// Everything the refund transfer is pinned to.
///
/// Every field is re-derived from the funding row and the chain immediately
/// before use; nothing here is taken from an operator flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefundPlan {
    pub(crate) intent_id: String,
    /// Exact base units to return. Always the full funded amount: this rail has
    /// no partial-refund semantics, and inventing them would put the funding
    /// row's `amount_raw` and the transfer out of exact correspondence.
    pub(crate) amount_raw: u64,
    pub(crate) mint: Pubkey,
    pub(crate) decimals: u8,
    /// Signer and owner of the source ATA.
    pub(crate) controlled_wallet: Pubkey,
    pub(crate) controlled_ata: Pubkey,
    /// The wallet that funded the intent, and its ATA. The refund can go
    /// nowhere else.
    pub(crate) user_wallet: Pubkey,
    pub(crate) user_ata: Pubkey,
}

/// A stable, non-sensitive failure class. Never carries an address or an
/// operator-supplied value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefundBuildError {
    /// The amount is zero, so there is nothing to return.
    AmountZero,
    /// USDC on this rail is six decimals; anything else is out of scope.
    DecimalsRejected,
    /// The source and destination are the same account.
    DegenerateTransfer,
    /// Serialization of the built message failed.
    SerializationFailed,
}

impl RefundBuildError {
    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::AmountZero => "refund_amount_zero",
            Self::DecimalsRejected => "refund_decimals_rejected",
            Self::DegenerateTransfer => "refund_degenerate_transfer",
            Self::SerializationFailed => "refund_serialization_failed",
        }
    }
}

impl std::fmt::Display for RefundBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.class())
    }
}

impl std::error::Error for RefundBuildError {}

/// USDC decimals on the only mint this rail supports.
const SUPPORTED_DECIMALS: u8 = 6;

/// Build the unsigned refund transaction.
///
/// The result carries a zero blockhash and unset signatures, so it cannot be
/// submitted as-is. That is the same non-submittable shape the dry-run path
/// prints, and it is what the wallet pipeline expects to receive.
pub(crate) fn build_unsigned_refund(plan: &RefundPlan) -> Result<Transaction, RefundBuildError> {
    if plan.amount_raw == 0 {
        return Err(RefundBuildError::AmountZero);
    }
    if plan.decimals != SUPPORTED_DECIMALS {
        return Err(RefundBuildError::DecimalsRejected);
    }
    if plan.controlled_ata == plan.user_ata {
        return Err(RefundBuildError::DegenerateTransfer);
    }

    let transfer = spl_token::instruction::transfer_checked(
        &spl_token::id(),
        &plan.controlled_ata,
        &plan.mint,
        &plan.user_ata,
        &plan.controlled_wallet,
        &[],
        plan.amount_raw,
        plan.decimals,
    )
    .map_err(|_| RefundBuildError::SerializationFailed)?;

    let instructions: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(REFUND_COMPUTE_UNIT_LIMIT),
        ComputeBudgetInstruction::set_compute_unit_price(REFUND_COMPUTE_UNIT_PRICE_MICRO_LAMPORTS),
        transfer,
    ];

    // The fee payer is the controlled wallet: it is the only signer, and it is
    // the account whose authority the transfer needs anyway.
    let message = Message::new(&instructions, Some(&plan.controlled_wallet));
    Ok(Transaction::new_unsigned(message))
}

/// The exact placeholder-blockhash message bytes the wallet pipeline will bind
/// against. Kept beside the builder so the two can never drift.
pub(crate) fn placeholder_message_bytes(
    transaction: &Transaction,
) -> Result<Vec<u8>, RefundBuildError> {
    if transaction.message.recent_blockhash != Hash::default() {
        return Err(RefundBuildError::SerializationFailed);
    }
    Ok(transaction.message.serialize())
}
