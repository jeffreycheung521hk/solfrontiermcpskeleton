//! Offline tests for the refund rail.
//!
//! Everything here runs without a network, a wallet, a keypair or the chain.
//! The two pieces that decide whether money can move twice -- the recovery
//! decision and the landed-refund verifier -- are pure functions precisely so
//! that every crash window is reachable from a test rather than only from an
//! outage.

use std::path::PathBuf;

use solana_sdk::pubkey::Pubkey;

use crate::funding::{
    ConfirmationLevel, ObservedFundingTransaction, ObservedTokenBalance, ObservedTransferChecked,
};
use crate::refund::{plan_from_row, verify_refund_landed, RefundVerifyError};
use crate::refund_builder::{
    build_unsigned_refund, placeholder_message_bytes, RefundBuildError, RefundPlan,
    REFUND_INSTRUCTION_COUNT,
};
use crate::refund_journal::{
    recovery_action, ChainPresence, JournalLookup, RecordedAttempt, RecoveryAction, RefundJournal,
};

const CONTROLLED_WALLET: &str = "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L";
const CONTROLLED_ATA: &str = "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3";
const USER_WALLET: &str = "C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW";
const USER_ATA: &str = "4bDArC4UVoBjecHUZaCKqhuhTYPoiKS4qpQQpxuvm1qn";
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const AMOUNT_RAW: u64 = 200_000;

fn key(value: &str) -> Pubkey {
    value.parse().expect("pinned base58 key")
}

fn plan() -> RefundPlan {
    RefundPlan {
        intent_id: "5b000000400d0300000000009a62dace".to_owned(),
        amount_raw: AMOUNT_RAW,
        mint: key(USDC_MINT),
        decimals: 6,
        controlled_wallet: key(CONTROLLED_WALLET),
        controlled_ata: key(CONTROLLED_ATA),
        user_wallet: key(USER_WALLET),
        user_ata: key(USER_ATA),
    }
}

// ---------------------------------------------------------------- builder

#[test]
fn builder_produces_a_non_submittable_transaction() {
    let transaction = build_unsigned_refund(&plan()).expect("build");
    assert_eq!(
        transaction.message.instructions.len(),
        REFUND_INSTRUCTION_COUNT
    );
    // Zero blockhash and an unset signature slot: only the wallet pipeline may
    // attach a real blockhash, and nothing here can reach a validator.
    assert_eq!(
        transaction.message.recent_blockhash,
        solana_sdk::hash::Hash::default()
    );
    assert_eq!(transaction.signatures.len(), 1);
    assert!(transaction
        .signatures
        .iter()
        .all(|signature| *signature == solana_sdk::signature::Signature::default()));
    // The controlled wallet is the fee payer and the only signer.
    assert_eq!(
        transaction.message.account_keys.first(),
        Some(&key(CONTROLLED_WALLET))
    );
    assert_eq!(transaction.message.header.num_required_signatures, 1);
}

#[test]
fn builder_encodes_the_exact_amount_and_direction() {
    let transaction = build_unsigned_refund(&plan()).expect("build");
    let transfer = &transaction.message.instructions[2];
    // tag 12 = TransferChecked, then a little-endian u64 amount, then decimals.
    assert_eq!(transfer.data[0], 12);
    let encoded = u64::from_le_bytes(transfer.data[1..9].try_into().expect("amount bytes"));
    assert_eq!(encoded, AMOUNT_RAW);
    assert_eq!(transfer.data[9], 6);

    let account =
        |position: usize| transaction.message.account_keys[transfer.accounts[position] as usize];
    assert_eq!(
        account(0),
        key(CONTROLLED_ATA),
        "source is the controlled ATA"
    );
    assert_eq!(account(2), key(USER_ATA), "destination is the funder's ATA");
    assert_eq!(account(3), key(CONTROLLED_WALLET), "authority signs");
}

#[test]
fn builder_refuses_degenerate_and_out_of_scope_plans() {
    let zero = RefundPlan {
        amount_raw: 0,
        ..plan()
    };
    assert_eq!(
        build_unsigned_refund(&zero),
        Err(RefundBuildError::AmountZero)
    );

    let wrong_decimals = RefundPlan {
        decimals: 9,
        ..plan()
    };
    assert_eq!(
        build_unsigned_refund(&wrong_decimals),
        Err(RefundBuildError::DecimalsRejected)
    );

    let same_account = RefundPlan {
        user_ata: key(CONTROLLED_ATA),
        ..plan()
    };
    assert_eq!(
        build_unsigned_refund(&same_account),
        Err(RefundBuildError::DegenerateTransfer)
    );
}

#[test]
fn placeholder_bytes_are_refused_once_a_blockhash_is_bound() {
    let mut transaction = build_unsigned_refund(&plan()).expect("build");
    assert!(placeholder_message_bytes(&transaction).is_ok());
    transaction.message.recent_blockhash = solana_sdk::hash::Hash::new_unique();
    assert!(
        placeholder_message_bytes(&transaction).is_err(),
        "a bound blockhash must not be mistaken for the builder template"
    );
}

// -------------------------------------------------------- recovery windows

fn attempt() -> RecordedAttempt {
    RecordedAttempt {
        signature: "SIGNATURE".to_owned(),
        last_valid_block_height: 200,
        signed_transaction: vec![1, 2, 3],
        recorded_at_ms: 0,
    }
}

fn found() -> JournalLookup {
    JournalLookup::Found(Box::new(attempt()))
}

#[test]
fn an_unreadable_journal_halts_because_neither_move_is_safe() {
    // The daemon cannot tell "never sent" from "sent and lost". Resending
    // nothing loses the money's trace; signing again can double-refund.
    let action = recovery_action(
        &JournalLookup::Unavailable {
            error_class: "refund_journal_row_corrupt",
        },
        ChainPresence::Absent,
        Some(1),
    );
    assert!(matches!(action, RecoveryAction::Halt { .. }));
}

#[test]
fn a_readable_empty_journal_is_the_only_provably_fresh_start() {
    let action = recovery_action(&JournalLookup::Absent, ChainPresence::Absent, Some(1));
    assert_eq!(
        action,
        RecoveryAction::SignNew {
            reason: "nothing_was_signed"
        }
    );
}

#[test]
fn a_valid_unlanded_attempt_is_resent_never_resigned() {
    // The window that causes double refunds. Re-signing here produces a second
    // transaction with a fresh blockhash that can land beside the first.
    let action = recovery_action(&found(), ChainPresence::Absent, Some(199));
    assert_eq!(
        action,
        RecoveryAction::ResendSame {
            signature: "SIGNATURE".to_owned()
        }
    );
}

#[test]
fn the_expiry_boundary_is_exclusive() {
    // At exactly last_valid_block_height the bytes are still acceptable.
    assert!(matches!(
        recovery_action(&found(), ChainPresence::Absent, Some(200)),
        RecoveryAction::ResendSame { .. }
    ));
    // One block later they can never land, so signing fresh is provable.
    assert_eq!(
        recovery_action(&found(), ChainPresence::Absent, Some(201)),
        RecoveryAction::SignNew {
            reason: "blockhash_expired"
        }
    );
}

#[test]
fn a_landed_attempt_is_reconciled_not_resent() {
    assert_eq!(
        recovery_action(&found(), ChainPresence::Found, Some(1)),
        RecoveryAction::Reconcile {
            signature: "SIGNATURE".to_owned()
        }
    );
}

#[test]
fn unknown_chain_state_or_unknown_height_halts() {
    assert!(matches!(
        recovery_action(&found(), ChainPresence::Unknown, Some(1)),
        RecoveryAction::Halt {
            reason: "chain_state_unknown"
        }
    ));
    assert!(matches!(
        recovery_action(&found(), ChainPresence::Absent, None),
        RecoveryAction::Halt {
            reason: "block_height_unknown"
        }
    ));
}

// ----------------------------------------------------------------- journal

struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "solfrontier-refund-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("create unique refund test directory");
        Self { path }
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let Ok(resolved_path) = std::fs::canonicalize(&self.path) else {
            return;
        };
        let Ok(resolved_temp) = std::fs::canonicalize(std::env::temp_dir()) else {
            return;
        };
        let has_test_prefix = resolved_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("solfrontier-refund-"));
        if resolved_path.parent() == Some(resolved_temp.as_path()) && has_test_prefix {
            let _ = std::fs::remove_dir_all(&resolved_path);
        }
    }
}

#[tokio::test]
async fn journal_records_then_reads_back_the_exact_bytes() {
    let directory = TempDirectory::new("journal-roundtrip");
    let journal = RefundJournal::for_database_path(&directory.path.join("main.db"));
    journal
        .record_before_broadcast("intent-a", &attempt())
        .await
        .expect("record");
    match journal.lookup("intent-a").await {
        JournalLookup::Found(read) => {
            assert_eq!(*read, attempt(), "the bytes must survive verbatim");
        }
        other => panic!("expected Found, got {other:?}"),
    }
}

#[tokio::test]
async fn recording_the_same_signature_twice_is_idempotent() {
    let directory = TempDirectory::new("journal-idempotent");
    let journal = RefundJournal::for_database_path(&directory.path.join("main.db"));
    journal
        .record_before_broadcast("intent-a", &attempt())
        .await
        .expect("first record");
    journal
        .record_before_broadcast("intent-a", &attempt())
        .await
        .expect("retrying the same attempt must be safe");
}

#[tokio::test]
async fn a_second_different_signature_is_refused() {
    // Overwriting would erase the only evidence of what may already be flying.
    let directory = TempDirectory::new("journal-conflict");
    let journal = RefundJournal::for_database_path(&directory.path.join("main.db"));
    journal
        .record_before_broadcast("intent-a", &attempt())
        .await
        .expect("first record");
    let different = RecordedAttempt {
        signature: "OTHER".to_owned(),
        ..attempt()
    };
    let error = journal
        .record_before_broadcast("intent-a", &different)
        .await
        .expect_err("a conflicting attempt must be refused");
    assert_eq!(error.class(), "refund_journal_conflicting_attempt");
}

#[tokio::test]
async fn a_missing_journal_reads_as_unavailable_not_absent() {
    // The distinction that keeps a deleted journal from authorising a fresh
    // signature. Lookup must never create the file it is reading.
    let directory = TempDirectory::new("journal-missing");
    let main_db = directory.path.join("main.db");
    let journal = RefundJournal::for_database_path(&main_db);
    let lookup = journal.lookup("intent-a").await;
    assert!(
        matches!(lookup, JournalLookup::Unavailable { .. }),
        "a missing journal is unknown, not empty: {lookup:?}"
    );
    assert!(
        !journal.path().exists(),
        "a recovery read must not create the journal it is reading"
    );
    assert!(matches!(
        recovery_action(&lookup, ChainPresence::Absent, Some(1)),
        RecoveryAction::Halt { .. }
    ));
}

// ------------------------------------------------------- landed verifier

fn balance(account: &str, owner: &str, amount_raw: u64) -> ObservedTokenBalance {
    ObservedTokenBalance {
        account: account.to_owned(),
        mint: USDC_MINT.to_owned(),
        owner: owner.to_owned(),
        amount_raw,
        decimals: 6,
    }
}

fn landed() -> ObservedFundingTransaction {
    ObservedFundingTransaction {
        slot: 999,
        block_time_ms: Some(0),
        confirmation: ConfirmationLevel::Finalized,
        succeeded: true,
        signer_pubkeys: vec![CONTROLLED_WALLET.to_owned()],
        memos: vec![],
        transfer_checked: vec![ObservedTransferChecked {
            source: CONTROLLED_ATA.to_owned(),
            mint: USDC_MINT.to_owned(),
            destination: USER_ATA.to_owned(),
            authority: CONTROLLED_WALLET.to_owned(),
            amount_raw: AMOUNT_RAW,
            decimals: 6,
        }],
        pre_token_balances: vec![
            balance(CONTROLLED_ATA, CONTROLLED_WALLET, AMOUNT_RAW),
            balance(USER_ATA, USER_WALLET, 0),
        ],
        post_token_balances: vec![
            balance(CONTROLLED_ATA, CONTROLLED_WALLET, 0),
            balance(USER_ATA, USER_WALLET, AMOUNT_RAW),
        ],
    }
}

#[test]
fn a_correct_refund_verifies_with_both_deltas_exact() {
    let proof = verify_refund_landed(&landed(), &plan()).expect("verify");
    assert_eq!(proof.source_delta, -(AMOUNT_RAW as i128));
    assert_eq!(proof.destination_delta, AMOUNT_RAW as i128);
    assert_eq!(proof.slot, 999);
}

#[test]
fn anything_short_of_finalized_is_refused() {
    // `confirmed` is the commitment funding accepts; a refund must not settle
    // for it. A confirmed block can still be rolled back, and a refund the
    // chain later forgets would be marked `refunded` with the money still in
    // the controlled ATA.
    let observed = ObservedFundingTransaction {
        confirmation: ConfirmationLevel::Confirmed,
        ..landed()
    };
    assert_eq!(
        verify_refund_landed(&observed, &plan()),
        Err(RefundVerifyError::NotFinalized)
    );
}

#[test]
fn a_missing_destination_post_balance_is_fatal_not_zero() {
    // Absence is never a value. A missing POST row on the destination would
    // mean nothing arrived, which is the opposite of a completed refund.
    let mut observed = landed();
    observed
        .post_token_balances
        .retain(|row| row.account != USER_ATA);
    assert_eq!(
        verify_refund_landed(&observed, &plan()),
        Err(RefundVerifyError::DestinationBalanceUnreadable)
    );
}

#[test]
fn a_missing_destination_pre_balance_means_the_account_was_created() {
    // The legitimate case for an absent PRE row: the ATA did not exist before
    // this transaction, so it held zero.
    let mut observed = landed();
    observed
        .pre_token_balances
        .retain(|row| row.account != USER_ATA);
    assert!(verify_refund_landed(&observed, &plan()).is_ok());
}

#[test]
fn deltas_that_do_not_match_the_amount_are_refused() {
    let mut short = landed();
    short.post_token_balances = vec![
        balance(CONTROLLED_ATA, CONTROLLED_WALLET, 1),
        balance(USER_ATA, USER_WALLET, AMOUNT_RAW),
    ];
    assert_eq!(
        verify_refund_landed(&short, &plan()),
        Err(RefundVerifyError::SourceDeltaMismatch)
    );

    let mut wrong_destination = landed();
    wrong_destination.post_token_balances = vec![
        balance(CONTROLLED_ATA, CONTROLLED_WALLET, 0),
        balance(USER_ATA, USER_WALLET, AMOUNT_RAW - 1),
    ];
    assert_eq!(
        verify_refund_landed(&wrong_destination, &plan()),
        Err(RefundVerifyError::DestinationDeltaMismatch)
    );
}

#[test]
fn a_failed_or_unsigned_transaction_never_verifies() {
    let failed = ObservedFundingTransaction {
        succeeded: false,
        ..landed()
    };
    assert_eq!(
        verify_refund_landed(&failed, &plan()),
        Err(RefundVerifyError::TransactionFailed)
    );

    let unsigned = ObservedFundingTransaction {
        signer_pubkeys: vec![USER_WALLET.to_owned()],
        ..landed()
    };
    assert_eq!(
        verify_refund_landed(&unsigned, &plan()),
        Err(RefundVerifyError::WrongSigner)
    );
}

#[test]
fn extra_transfers_or_an_unexpected_memo_are_refused() {
    let mut two_transfers = landed();
    let duplicate = two_transfers.transfer_checked[0].clone();
    two_transfers.transfer_checked.push(duplicate);
    assert_eq!(
        verify_refund_landed(&two_transfers, &plan()),
        Err(RefundVerifyError::TransferCount)
    );

    // This rail emits no memo, so one appearing means this is not our
    // transaction however well the balances line up.
    let with_memo = ObservedFundingTransaction {
        memos: vec!["claw:w5h:anything".to_owned()],
        ..landed()
    };
    assert_eq!(
        verify_refund_landed(&with_memo, &plan()),
        Err(RefundVerifyError::UnexpectedMemo)
    );
}

#[test]
fn the_funding_transaction_is_never_mistaken_for_its_own_refund() {
    // Same accounts, same amount, opposite direction.
    let mut funding = landed();
    funding.transfer_checked = vec![ObservedTransferChecked {
        source: USER_ATA.to_owned(),
        mint: USDC_MINT.to_owned(),
        destination: CONTROLLED_ATA.to_owned(),
        authority: USER_WALLET.to_owned(),
        amount_raw: AMOUNT_RAW,
        decimals: 6,
    }];
    funding.signer_pubkeys = vec![USER_WALLET.to_owned()];
    assert!(verify_refund_landed(&funding, &plan()).is_err());
}

// -------------------------------------------------------------- derivation

#[test]
fn the_plan_comes_entirely_from_the_funding_row() {
    // The operator names an intent and nothing else. If this test ever needs a
    // parameter that is not a column, the rail has grown an authority it was
    // designed not to have.
    let row = claw_state_store::W5hFundingIntent {
        intent_id: "5b000000400d0300000000009a62dace".to_owned(),
        rule_id_hex: "5b000000400d0300000000009a62dace".to_owned(),
        canonical_rule_hash_hex: "00".repeat(32),
        user_wallet: USER_WALLET.to_owned(),
        user_usdc_ata: USER_ATA.to_owned(),
        controlled_wallet: CONTROLLED_WALLET.to_owned(),
        controlled_usdc_ata: CONTROLLED_ATA.to_owned(),
        amount_raw: AMOUNT_RAW,
        threshold_bps: 90,
        save_display_apy_bps_at_creation: 0,
        native_onchain_apr_bps_at_creation: 0,
        created_at_ms: 0,
        expires_at_ms: 1,
        status: claw_state_store::W5hIntentStatus::BudgetReserved,
        funding_signature: None,
        funding_finalized_slot: None,
        execution_signature: None,
        refund_signature: None,
        last_error: None,
        updated_at_ms: 0,
    };
    let derived = plan_from_row(&row).expect("derive");
    assert_eq!(derived.amount_raw, AMOUNT_RAW);
    assert_eq!(derived.controlled_ata, key(CONTROLLED_ATA));
    assert_eq!(derived.user_ata, key(USER_ATA));
    assert_eq!(derived.mint, key(USDC_MINT), "the mint is pinned, not read");
    assert_eq!(derived.decimals, 6);
}
