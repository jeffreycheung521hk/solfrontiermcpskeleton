use super::*;
use claw_state_store::{
    Database, NewW5hFundingIntent, Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository,
};
use claw_types::canonical_rule_hash;
use solana_sdk::signature::Signature;

use crate::{
    finalize::build_watch_rule,
    propose::{DraftIntent, CONTROLLED_USDC_ATA_BS58, CONTROLLED_WALLET_BS58},
};

const AMOUNT_RAW: u64 = 250_000;
const CREATED_AT_MS: i64 = 1_000;
const EXPIRES_AT_MS: i64 = 181_000;

#[derive(Clone)]
struct MockReader {
    outcome: Result<FundingTransactionRead, FundingReadError>,
}

impl MockReader {
    fn confirmed(tx: ObservedFundingTransaction) -> Self {
        Self {
            outcome: Ok(FundingTransactionRead::Confirmed(tx)),
        }
    }

    fn pending() -> Self {
        Self {
            outcome: Ok(FundingTransactionRead::Pending),
        }
    }
}

impl FundingTransactionReader for MockReader {
    async fn read_funding_transaction(
        &self,
        _signature: &str,
    ) -> Result<FundingTransactionRead, FundingReadError> {
        self.outcome.clone()
    }
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl FundingClock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

struct TestStore {
    _db: Database,
    funding: Stage2W5hFundingIntentRepository,
    watch: Stage2WatchRuleRepository,
    params: ConfirmFundingParams,
    intent: W5hFundingIntent,
    stored_rule: StoredWatchRule,
}

async fn test_store() -> TestStore {
    let db = Database::open_in_memory().await.unwrap();
    let funding = Stage2W5hFundingIntentRepository::new(db.pool().clone());
    let watch = Stage2WatchRuleRepository::new(db.pool().clone());
    let draft = DraftIntent {
        action: "deposit",
        protocol: "solend",
        asset: "USDC",
        display_source: "save",
        comparison: ">",
        threshold_bps: 500,
        amount_raw: AMOUNT_RAW,
        expiry_seconds_after_finalize: 180,
        controlled_wallet: CONTROLLED_WALLET_BS58.to_owned(),
        controlled_usdc_ata: CONTROLLED_USDC_ATA_BS58.to_owned(),
        original_user_message_hash: "11".repeat(32),
    };
    let rule = build_watch_rule(&draft, 10_000);
    watch.insert(&rule).await.unwrap();
    let intent_id = hex_lower(&rule.rule_id);
    let rule_hash = hex_lower(&canonical_rule_hash(&rule));
    let user_wallet = Pubkey::new_unique();
    let mint = Pubkey::from_str(USDC_MINT_BS58).unwrap();
    let user_usdc_ata = get_associated_token_address(&user_wallet, &mint);
    funding
        .insert(&NewW5hFundingIntent {
            intent_id: intent_id.clone(),
            rule_id_hex: intent_id.clone(),
            canonical_rule_hash_hex: rule_hash,
            user_wallet: user_wallet.to_string(),
            user_usdc_ata: user_usdc_ata.to_string(),
            controlled_wallet: CONTROLLED_WALLET_BS58.to_owned(),
            controlled_usdc_ata: CONTROLLED_USDC_ATA_BS58.to_owned(),
            amount_raw: AMOUNT_RAW,
            threshold_bps: 500,
            save_display_apy_bps_at_creation: 600,
            native_onchain_apr_bps_at_creation: 550,
            created_at_ms: CREATED_AT_MS,
            expires_at_ms: EXPIRES_AT_MS,
        })
        .await
        .unwrap();
    let intent = funding.get(&intent_id).await.unwrap().unwrap();
    let stored_rule = watch.get(&rule.rule_id).await.unwrap().unwrap();
    let params = ConfirmFundingParams {
        intent_id,
        tx_signature: Signature::from([7_u8; 64]).to_string(),
    };
    TestStore {
        _db: db,
        funding,
        watch,
        params,
        intent,
        stored_rule,
    }
}

fn valid_observation(store: &TestStore) -> ObservedFundingTransaction {
    let source_pre = 1_000_000;
    let receiving_pre = 2_000_000;
    ObservedFundingTransaction {
        slot: 10_100,
        block_time_ms: Some(100_000),
        confirmation: ConfirmationLevel::Confirmed,
        succeeded: true,
        signer_pubkeys: vec![store.intent.user_wallet.clone()],
        memos: vec![format!(
            "claw:w5h:{}:{}",
            store.intent.intent_id, store.intent.canonical_rule_hash_hex
        )],
        transfer_checked: vec![ObservedTransferChecked {
            source: store.intent.user_usdc_ata.clone(),
            mint: USDC_MINT_BS58.to_owned(),
            destination: store.intent.controlled_usdc_ata.clone(),
            authority: store.intent.user_wallet.clone(),
            amount_raw: AMOUNT_RAW,
            decimals: USDC_DECIMALS,
        }],
        pre_token_balances: vec![
            ObservedTokenBalance {
                account: store.intent.user_usdc_ata.clone(),
                mint: USDC_MINT_BS58.to_owned(),
                owner: store.intent.user_wallet.clone(),
                amount_raw: source_pre,
                decimals: USDC_DECIMALS,
            },
            ObservedTokenBalance {
                account: store.intent.controlled_usdc_ata.clone(),
                mint: USDC_MINT_BS58.to_owned(),
                owner: store.intent.controlled_wallet.clone(),
                amount_raw: receiving_pre,
                decimals: USDC_DECIMALS,
            },
        ],
        post_token_balances: vec![
            ObservedTokenBalance {
                account: store.intent.user_usdc_ata.clone(),
                mint: USDC_MINT_BS58.to_owned(),
                owner: store.intent.user_wallet.clone(),
                amount_raw: source_pre - AMOUNT_RAW,
                decimals: USDC_DECIMALS,
            },
            ObservedTokenBalance {
                account: store.intent.controlled_usdc_ata.clone(),
                mint: USDC_MINT_BS58.to_owned(),
                owner: store.intent.controlled_wallet.clone(),
                amount_raw: receiving_pre + AMOUNT_RAW,
                decimals: USDC_DECIMALS,
            },
        ],
    }
}

async fn confirm(store: &TestStore, reader: &MockReader, now_ms: i64) -> Value {
    confirm_funding_json(
        &store.params,
        Some(reader),
        &store.funding,
        &store.watch,
        &FixedClock(now_ms),
    )
    .await
    .unwrap()
}

async fn assert_unflipped(store: &TestStore) {
    let current = store
        .funding
        .get(&store.params.intent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.status, W5hIntentStatus::FundingRequired);
    assert_eq!(current.funding_signature, None);
    assert_eq!(current.funding_finalized_slot, None);
}

#[tokio::test]
async fn memo_mismatch_does_not_flip_state() {
    let store = test_store().await;
    let mut tx = valid_observation(&store);
    tx.memos = vec!["claw:w5h:wrong:wrong".to_owned()];

    let response = confirm(&store, &MockReader::confirmed(tx), 100_000).await;

    assert_eq!(response["status"], "verification_failed");
    assert_eq!(response["reason_code"], "memo_mismatch");
    assert_eq!(response["database_writes"], 0);
    assert_unflipped(&store).await;
}

#[tokio::test]
async fn transfer_amount_mismatch_does_not_flip_state() {
    let store = test_store().await;
    let mut tx = valid_observation(&store);
    tx.transfer_checked[0].amount_raw -= 1;

    let response = confirm(&store, &MockReader::confirmed(tx), 100_000).await;

    assert_eq!(response["status"], "verification_failed");
    assert_eq!(response["reason_code"], "transfer_amount_mismatch");
    assert_unflipped(&store).await;
}

#[tokio::test]
async fn receiving_ata_mismatch_does_not_flip_state() {
    let store = test_store().await;
    let mut tx = valid_observation(&store);
    tx.transfer_checked[0].destination = Pubkey::new_unique().to_string();

    let response = confirm(&store, &MockReader::confirmed(tx), 100_000).await;

    assert_eq!(response["status"], "verification_failed");
    assert_eq!(response["reason_code"], "receiving_ata_mismatch");
    assert_unflipped(&store).await;
}

#[tokio::test]
async fn unregistered_funder_signer_does_not_flip_state() {
    let store = test_store().await;
    let mut tx = valid_observation(&store);
    tx.signer_pubkeys = vec![Pubkey::new_unique().to_string()];

    let response = confirm(&store, &MockReader::confirmed(tx), 100_000).await;

    assert_eq!(response["status"], "verification_failed");
    assert_eq!(response["reason_code"], "funder_not_signer");
    assert_unflipped(&store).await;
}

#[tokio::test]
async fn source_ata_delta_mismatch_does_not_flip_state() {
    let store = test_store().await;
    let mut tx = valid_observation(&store);
    let source_post = tx
        .post_token_balances
        .iter_mut()
        .find(|balance| balance.account == store.intent.user_usdc_ata)
        .unwrap();
    source_post.amount_raw += 1;

    let response = confirm(&store, &MockReader::confirmed(tx), 100_000).await;

    assert_eq!(response["status"], "verification_failed");
    assert_eq!(response["reason_code"], "source_delta_mismatch");
    assert_unflipped(&store).await;
}

#[tokio::test]
async fn pending_confirmation_is_retryable_and_does_not_flip_state() {
    let store = test_store().await;

    let response = confirm(&store, &MockReader::pending(), 100_000).await;

    assert_eq!(response["status"], "pending_confirmation");
    assert_eq!(response["required_commitment"], "confirmed");
    assert_eq!(response["retryable"], true);
    assert_unflipped(&store).await;
}

#[tokio::test]
async fn missing_rpc_config_does_not_flip_state() {
    let store = test_store().await;

    let response = confirm_funding_json(
        &store.params,
        None::<&MockReader>,
        &store.funding,
        &store.watch,
        &FixedClock(100_000),
    )
    .await
    .unwrap();

    assert_eq!(response["status"], "config_missing");
    assert_eq!(response["database_writes"], 0);
    assert_unflipped(&store).await;
}

#[tokio::test]
async fn expired_funding_is_reserved_but_explicitly_requires_manual_refund() {
    let store = test_store().await;
    let mut tx = valid_observation(&store);
    tx.block_time_ms = Some(181_001);

    let response = confirm(&store, &MockReader::confirmed(tx), 181_001).await;

    assert_eq!(response["status"], "budget_reserved");
    assert_eq!(response["arrived_after_funding_deadline"], true);
    assert_eq!(
        response["funding_deadline_basis"],
        "funding_row.expires_at_ms"
    );
    assert_eq!(
        response["funding_row_expires_at_ms"],
        store.intent.expires_at_ms
    );
    assert_eq!(
        response["funding_arrival_time_source"],
        "funding_transaction.blockTime"
    );
    assert_eq!(response["funding_arrival_time_ms"], 181_001);
    assert_eq!(
        response["watch_rule"]["expires_at_slot"],
        store.stored_rule.rule.expires_at_slot
    );
    assert_eq!(response["late_funding"]["refundable"], true);
    assert_eq!(
        response["late_funding"]["automatic_refund_available"],
        false
    );
    assert_eq!(response["late_funding"]["manual_refund_required"], true);
    assert!(response["late_funding"]["notice"]
        .as_str()
        .unwrap()
        .contains("退款目前需人工處理"));
    let current = store
        .funding
        .get(&store.params.intent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.status, W5hIntentStatus::BudgetReserved);
}

#[tokio::test]
async fn valid_proof_advances_both_cas_transitions() {
    let store = test_store().await;
    let tx = valid_observation(&store);

    let response = confirm(&store, &MockReader::confirmed(tx), 100_000).await;

    assert_eq!(response["status"], "budget_reserved");
    assert_eq!(response["confirmation_level"], "confirmed");
    assert_eq!(response["funding_finalized_slot"], 10_100);
    assert_eq!(response["arrived_after_funding_deadline"], false);
    let current = store
        .funding
        .get(&store.params.intent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.status, W5hIntentStatus::BudgetReserved);
    assert_eq!(
        current.funding_signature.as_deref(),
        Some(store.params.tx_signature.as_str())
    );
    assert_eq!(current.funding_finalized_slot, Some(10_100));
}

#[tokio::test]
async fn same_signature_retry_recovers_preseeded_funding_submitted() {
    let store = test_store().await;
    assert_eq!(
        store
            .funding
            .mark_funding_submitted_if_required(&store.params.intent_id, &store.params.tx_signature)
            .await
            .unwrap(),
        1
    );
    let tx = valid_observation(&store);

    let response = confirm(&store, &MockReader::confirmed(tx), 100_000).await;

    assert_eq!(response["status"], "budget_reserved");
    let current = store
        .funding
        .get(&store.params.intent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.status, W5hIntentStatus::BudgetReserved);
    assert_eq!(current.funding_finalized_slot, Some(10_100));
}

#[tokio::test]
async fn already_reserved_retry_revalidates_the_same_signature() {
    let store = test_store().await;
    let valid = valid_observation(&store);
    let first = confirm(&store, &MockReader::confirmed(valid.clone()), 100_000).await;
    assert_eq!(first["status"], "budget_reserved");

    let mut tampered = valid;
    tampered.memos = vec!["claw:w5h:wrong:wrong".to_owned()];
    let retry = confirm(&store, &MockReader::confirmed(tampered), 100_000).await;

    assert_eq!(retry["status"], "verification_failed");
    assert_eq!(retry["reason_code"], "memo_mismatch");
    let current = store
        .funding
        .get(&store.params.intent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.status, W5hIntentStatus::BudgetReserved);
}

#[test]
fn json_parsed_transaction_fixture_normalizes_required_proof() {
    let store_runtime = tokio::runtime::Runtime::new().unwrap();
    let store = store_runtime.block_on(test_store());
    let expected = valid_observation(&store);
    let response = json!({
        "slot": expected.slot,
        "blockTime": expected.block_time_ms.map(|millis| millis / 1_000),
        "transaction": {
            "message": {
                "accountKeys": [
                    {"pubkey": store.intent.user_usdc_ata, "signer": false, "writable": true},
                    {"pubkey": USDC_MINT_BS58, "signer": false, "writable": false},
                    {"pubkey": store.intent.controlled_usdc_ata, "signer": false, "writable": true},
                    {"pubkey": store.intent.user_wallet, "signer": true, "writable": false}
                ],
                "instructions": [
                    {
                        "programId": MEMO_PROGRAM_ID_BS58,
                        "parsed": expected.memos[0]
                    },
                    {
                        "programId": TOKEN_PROGRAM_ID_BS58,
                        "parsed": {
                            "type": "transferChecked",
                            "info": {
                                "source": store.intent.user_usdc_ata,
                                "mint": USDC_MINT_BS58,
                                "destination": store.intent.controlled_usdc_ata,
                                "authority": store.intent.user_wallet,
                                "tokenAmount": {
                                    "amount": AMOUNT_RAW.to_string(),
                                    "decimals": USDC_DECIMALS
                                }
                            }
                        }
                    }
                ]
            }
        },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance_json(0, &expected.pre_token_balances[0]),
                token_balance_json(2, &expected.pre_token_balances[1])
            ],
            "postTokenBalances": [
                token_balance_json(0, &expected.post_token_balances[0]),
                token_balance_json(2, &expected.post_token_balances[1])
            ]
        }
    });

    let actual = parse_json_parsed_transaction(&response, ConfirmationLevel::Confirmed).unwrap();

    assert_eq!(actual, expected);
}

fn token_balance_json(account_index: u64, balance: &ObservedTokenBalance) -> Value {
    json!({
        "accountIndex": account_index,
        "mint": balance.mint,
        "owner": balance.owner,
        "uiTokenAmount": {
            "amount": balance.amount_raw.to_string(),
            "decimals": balance.decimals
        }
    })
}
