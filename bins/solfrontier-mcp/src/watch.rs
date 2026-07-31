//! Read-only Phase 3b dry-run watcher.
//!
//! This module deliberately has no lease, state transition, keypair,
//! signature, simulation, submission, or confirmation capability. It opens
//! the existing state store with SQLite `mode=ro` plus `query_only`, reads
//! canonical `SolendDeposit` actions, validates live account facts, and emits
//! a complete but deliberately unsubmitable unsigned transaction to stderr
//! for human review.

use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use claw_executor::{
    preflight_candidate, validate_chain, CandidateClassification, CandidateInput, CandidateReport,
    ChainFacts, ChainOutcome, ClockSnapshot, DerivedAccounts, FundingSnapshot, ObligationFacts,
    PreflightClockSnapshot, PreflightOutcome, PreparedDeposit, ReserveFacts, TokenAccountFacts,
    ValidatedSolendPlanInputs,
};
use claw_protocols::solend::{
    amount::UnderlyingAmount,
    ata::derive_associated_token_address,
    deposit::{
        build_deposit_reserve_liquidity_and_obligation_collateral_instruction,
        DepositInstructionInputs,
    },
    raw::{decode_obligation, decode_reserve},
    refresh::{build_refresh_instructions, RefreshPlanInputs, ReserveRefreshInput},
};
use claw_state_store::{
    Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository, StoredWatchRule, W5hFundingIntent,
    WatchRuleStatus,
};
use claw_types::canonical_intent::PubkeyBytes;
use serde::Serialize;
use solana_sdk::{
    account::Account, compute_budget::ComputeBudgetInstruction, instruction::Instruction,
    message::Message, program_pack::Pack, pubkey::Pubkey, transaction::Transaction,
};
use spl_token::state::{Account as SplTokenAccount, AccountState};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};

use crate::finalize_market::{
    configured_finalize_market_source_from_env, native_supply_apr_from_reserve_data,
    ConfirmedReserveObservation, FinalizeMarketReadError, RpcSaveFinalizeMarketSource,
};

const WATCH_INTERVAL: Duration = Duration::from_secs(30);
const WATCH_BATCH_LIMIT: usize = 128;
const WATCH_FETCH_LIMIT: u32 = WATCH_BATCH_LIMIT as u32 + 1;
const COMPUTE_UNIT_LIMIT: u32 = 400_000;
const COMPUTE_UNIT_PRICE_MICRO_LAMPORTS: u64 = 50_000;

#[derive(Debug)]
struct ReadOnlyWatchStore {
    pool: SqlitePool,
    funding: Stage2W5hFundingIntentRepository,
    rules: Stage2WatchRuleRepository,
}

impl ReadOnlyWatchStore {
    async fn open(path: &Path) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false)
            .foreign_keys(true)
            .pragma("query_only", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Ok(Self {
            funding: Stage2W5hFundingIntentRepository::new(pool.clone()),
            rules: Stage2WatchRuleRepository::new(pool.clone()),
            pool,
        })
    }

    async fn close(self) {
        self.pool.close().await;
    }
}

trait WatchClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

struct SystemWatchClock;

impl WatchClock for SystemWatchClock {
    fn now_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

#[allow(async_fn_in_trait)]
trait WatchChainSource: Send + Sync {
    async fn fetch_confirmed_slot(&self) -> Result<u64, FinalizeMarketReadError>;
    async fn fetch_confirmed_reserve(
        &self,
        reserve: Pubkey,
    ) -> Result<ConfirmedReserveObservation, FinalizeMarketReadError>;
    async fn fetch_confirmed_account(
        &self,
        address: &Pubkey,
    ) -> Result<Option<Account>, FinalizeMarketReadError>;
}

impl WatchChainSource for RpcSaveFinalizeMarketSource {
    async fn fetch_confirmed_slot(&self) -> Result<u64, FinalizeMarketReadError> {
        RpcSaveFinalizeMarketSource::fetch_confirmed_slot(self).await
    }

    async fn fetch_confirmed_reserve(
        &self,
        reserve: Pubkey,
    ) -> Result<ConfirmedReserveObservation, FinalizeMarketReadError> {
        RpcSaveFinalizeMarketSource::fetch_confirmed_reserve_observation(self, reserve).await
    }

    async fn fetch_confirmed_account(
        &self,
        address: &Pubkey,
    ) -> Result<Option<Account>, FinalizeMarketReadError> {
        RpcSaveFinalizeMarketSource::fetch_confirmed_account(self, address).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WatchCycleReport {
    mode: &'static str,
    database_access: &'static str,
    current_confirmed_slot: Option<u64>,
    slot_error_class: Option<String>,
    rows: Vec<WatchRowReport>,
    summary: BTreeMap<String, u64>,
    scan_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WatchRowReport {
    status: String,
    intent_id: Option<String>,
    rule_id_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_lifecycle: Option<RuleLifecycleReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate: Option<CandidateReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unsigned_transaction: Option<UnsignedTransactionReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RuleLifecycleReport {
    status: &'static str,
    completed: bool,
    revoked: bool,
    execution_nonce: u64,
}

impl WatchRowReport {
    fn from_candidate(report: CandidateReport) -> Self {
        Self {
            status: classification_label(report.classification).to_owned(),
            intent_id: report.intent_id.clone(),
            rule_id_hex: report.rule_id_hex.clone(),
            rule_lifecycle: None,
            candidate: Some(report),
            error_class: None,
            unsigned_transaction: None,
        }
    }

    fn database_only(
        status: &'static str,
        intent_id: Option<String>,
        rule_id_hex: Option<String>,
    ) -> Self {
        Self {
            status: status.to_owned(),
            intent_id,
            rule_id_hex,
            rule_lifecycle: None,
            candidate: None,
            error_class: None,
            unsigned_transaction: None,
        }
    }

    fn error(
        status: &'static str,
        error_class: impl Into<String>,
        intent_id: Option<String>,
        rule_id_hex: Option<String>,
    ) -> Self {
        Self {
            status: status.to_owned(),
            intent_id,
            rule_id_hex,
            rule_lifecycle: None,
            candidate: None,
            error_class: Some(error_class.into()),
            unsigned_transaction: None,
        }
    }

    fn candidate_error(
        status: &'static str,
        error_class: impl Into<String>,
        candidate: CandidateReport,
    ) -> Self {
        Self {
            status: status.to_owned(),
            intent_id: candidate.intent_id.clone(),
            rule_id_hex: candidate.rule_id_hex.clone(),
            rule_lifecycle: None,
            candidate: Some(candidate),
            error_class: Some(error_class.into()),
            unsigned_transaction: None,
        }
    }

    fn with_lifecycle(mut self, lifecycle: RuleLifecycleReport) -> Self {
        self.rule_lifecycle = Some(lifecycle);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct UnsignedTransactionReport {
    kind: &'static str,
    sendable: bool,
    fee_payer: String,
    recent_blockhash: Option<String>,
    transaction_base64: String,
    signature_slots: usize,
    all_signature_slots_default: bool,
    input_amount_raw: String,
    compute_unit_limit: u32,
    compute_unit_price_micro_lamports: u64,
    instructions: Vec<InstructionReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct InstructionReport {
    instruction_index: usize,
    label: &'static str,
    program_id: String,
    data_hex: String,
    accounts: Vec<AccountMetaReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AccountMetaReport {
    position: usize,
    pubkey: String,
    is_signer: bool,
    is_writable: bool,
}

/// Run the read-only watcher. Output intentionally stays on stderr so stdout
/// remains reserved for MCP JSON-RPC in the server mode.
pub(crate) async fn run_watch(db_path: &Path, once: bool) -> anyhow::Result<()> {
    let store = ReadOnlyWatchStore::open(db_path).await?;
    let source = configured_finalize_market_source_from_env();
    tracing::warn!(
        "DRY RUN ONLY: watch cannot load keys, sign, broadcast, lease, or change database state"
    );

    loop {
        let report = scan_once(&store, source.as_ref(), &SystemWatchClock).await;
        match serde_json::to_string_pretty(&report) {
            Ok(json) => eprintln!("{json}"),
            Err(_) => tracing::error!("watch dry-run report serialization failed"),
        }
        if once {
            break;
        }
        tokio::time::sleep(WATCH_INTERVAL).await;
    }

    store.close().await;
    Ok(())
}

async fn scan_once<S: WatchChainSource>(
    store: &ReadOnlyWatchStore,
    source: Option<&S>,
    clock: &dyn WatchClock,
) -> WatchCycleReport {
    let (current_confirmed_slot, slot_error_class) = match source {
        Some(source) => match source.fetch_confirmed_slot().await {
            Ok(slot) => (Some(slot), None),
            Err(error) => (None, Some(error.category().to_owned())),
        },
        None => (None, Some("config_missing".to_owned())),
    };

    let mut rows = Vec::new();
    let mut scan_errors = Vec::new();

    let mut funding_rows = match store.funding.list_budget_reserved(WATCH_FETCH_LIMIT).await {
        Ok(rows) => rows,
        Err(_) => {
            scan_errors.push("funding_scan_failed".to_owned());
            Vec::new()
        }
    };
    if funding_rows.len() > WATCH_BATCH_LIMIT {
        funding_rows.truncate(WATCH_BATCH_LIMIT);
        scan_errors.push("funding_scan_truncated".to_owned());
    }

    for funding in funding_rows {
        let lookup_id = match exact_rule_id(&funding.intent_id) {
            Some(id) => id,
            None => {
                rows.push(WatchRowReport::error(
                    "malformed_funding_row",
                    "invalid_intent_id",
                    Some(funding.intent_id),
                    Some(funding.rule_id_hex),
                ));
                continue;
            }
        };

        let stored_rule = match store.rules.get(&lookup_id).await {
            Ok(rule) => rule,
            Err(_) => {
                rows.push(WatchRowReport::error(
                    "rule_read_error",
                    "state_store_read_failed",
                    Some(funding.intent_id),
                    Some(funding.rule_id_hex),
                ));
                continue;
            }
        };

        let Some(stored_rule) = stored_rule else {
            rows.push(WatchRowReport::database_only(
                "orphan_funding_only",
                Some(funding.intent_id),
                Some(funding.rule_id_hex),
            ));
            continue;
        };

        let lifecycle = lifecycle_report(&stored_rule);
        if let Some(error_class) = ineligible_lifecycle(&stored_rule) {
            rows.push(
                WatchRowReport::error(
                    "ineligible_rule_lifecycle",
                    error_class,
                    Some(funding.intent_id),
                    Some(funding.rule_id_hex),
                )
                .with_lifecycle(lifecycle),
            );
            continue;
        }

        let snapshot = match funding_snapshot(&funding) {
            Ok(snapshot) => snapshot,
            Err(error_class) => {
                rows.push(
                    WatchRowReport::error(
                        "malformed_funding_row",
                        error_class,
                        Some(funding.intent_id),
                        Some(funding.rule_id_hex),
                    )
                    .with_lifecycle(lifecycle),
                );
                continue;
            }
        };
        let preflight = preflight_candidate(
            CandidateInput {
                funding: Some(snapshot),
                rule: Some(stored_rule.rule),
            },
            PreflightClockSnapshot {
                now_ms: clock.now_ms(),
                current_confirmed_slot,
            },
        );

        match preflight {
            PreflightOutcome::Blocked(report) => {
                rows.push(WatchRowReport::from_candidate(report).with_lifecycle(lifecycle));
            }
            PreflightOutcome::NeedsChain(prepared) => {
                let Some(source) = source else {
                    rows.push(
                        WatchRowReport::error(
                            "chain_unavailable",
                            slot_error_class
                                .as_deref()
                                .unwrap_or("confirmed_slot_unavailable"),
                            Some(funding.intent_id),
                            Some(funding.rule_id_hex),
                        )
                        .with_lifecycle(lifecycle),
                    );
                    continue;
                };
                rows.push(
                    inspect_candidate(prepared, source, clock)
                        .await
                        .with_lifecycle(lifecycle),
                );
            }
        }
    }

    let mut pending_rules = match store
        .rules
        .list_pending_lifecycle_limit(WATCH_FETCH_LIMIT)
        .await
    {
        Ok(rows) => rows,
        Err(_) => {
            scan_errors.push("watch_rule_scan_failed".to_owned());
            Vec::new()
        }
    };
    if pending_rules.len() > WATCH_BATCH_LIMIT {
        pending_rules.truncate(WATCH_BATCH_LIMIT);
        scan_errors.push("watch_rule_scan_truncated".to_owned());
    }
    for stored in pending_rules {
        let lifecycle = lifecycle_report(&stored);
        let rule_id_hex = hex::encode(stored.rule.rule_id);
        match store.funding.get(&rule_id_hex).await {
            Ok(Some(_)) => {}
            Ok(None) => rows.push(
                WatchRowReport::database_only("orphan_rule_only", None, Some(rule_id_hex))
                    .with_lifecycle(lifecycle),
            ),
            Err(_) => rows.push(
                WatchRowReport::error(
                    "funding_read_error",
                    "state_store_read_failed",
                    None,
                    Some(rule_id_hex),
                )
                .with_lifecycle(lifecycle),
            ),
        }
    }

    let mut summary = BTreeMap::new();
    summary.insert("total".to_owned(), rows.len() as u64);
    for row in &rows {
        *summary.entry(row.status.clone()).or_insert(0) += 1;
    }

    WatchCycleReport {
        mode: "dry_run",
        database_access: "sqlite_read_only_query_only",
        current_confirmed_slot,
        slot_error_class,
        rows,
        summary,
        scan_errors,
    }
}

fn lifecycle_report(stored: &StoredWatchRule) -> RuleLifecycleReport {
    RuleLifecycleReport {
        status: stored.status.as_str(),
        completed: stored.completed,
        revoked: stored.revoked,
        execution_nonce: stored.execution_nonce,
    }
}

fn ineligible_lifecycle(stored: &StoredWatchRule) -> Option<&'static str> {
    if stored.completed || stored.revoked {
        return Some("rule_lifecycle_flags_terminal");
    }
    if !matches!(
        stored.status,
        WatchRuleStatus::Active | WatchRuleStatus::ConditionMet
    ) {
        return Some("rule_lifecycle_status_ineligible");
    }
    None
}

fn exact_rule_id(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    hex::decode(value).ok()?.try_into().ok()
}

fn funding_snapshot(funding: &W5hFundingIntent) -> Result<FundingSnapshot, &'static str> {
    let controlled_wallet = PubkeyBytes::from_base58(&funding.controlled_wallet)
        .map_err(|_| "invalid_controlled_wallet")?;
    let controlled_usdc_ata = PubkeyBytes::from_base58(&funding.controlled_usdc_ata)
        .map_err(|_| "invalid_controlled_usdc_ata")?;
    Ok(FundingSnapshot {
        intent_id: funding.intent_id.clone(),
        rule_id_hex: funding.rule_id_hex.clone(),
        canonical_rule_hash_hex: funding.canonical_rule_hash_hex.clone(),
        controlled_wallet,
        controlled_usdc_ata,
        amount_raw: funding.amount_raw,
        threshold_bps: funding.threshold_bps,
        expires_at_ms: funding.expires_at_ms,
    })
}

async fn inspect_candidate<S: WatchChainSource>(
    prepared: PreparedDeposit,
    source: &S,
    clock: &dyn WatchClock,
) -> WatchRowReport {
    let audit_candidate = prepared.report().clone();
    let request = prepared.read_request();
    let reserve_address = to_pubkey(request.reserve_pubkey);

    let observation = match source.fetch_confirmed_reserve(reserve_address).await {
        Ok(observation) => observation,
        Err(error) => {
            return WatchRowReport::candidate_error(
                "chain_read_error",
                error.category(),
                audit_candidate,
            );
        }
    };
    let decoded_reserve = match decode_reserve(&observation.reserve_account.data) {
        Ok(reserve) => reserve,
        Err(_) => {
            return WatchRowReport::candidate_error(
                "decode_error",
                "reserve_decode_failed",
                audit_candidate,
            );
        }
    };
    let native_supply_apr_wad =
        match native_supply_apr_from_reserve_data(&observation.reserve_account.data) {
            Ok((wad, _)) => wad,
            Err(error) => {
                return WatchRowReport::candidate_error(
                    "decode_error",
                    error.category(),
                    audit_candidate,
                );
            }
        };

    let obligation_address = to_pubkey(request.target_obligation);
    let obligation_account = match source.fetch_confirmed_account(&obligation_address).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            return WatchRowReport::candidate_error(
                "account_missing",
                "obligation_account_missing",
                audit_candidate,
            );
        }
        Err(error) => {
            return WatchRowReport::candidate_error(
                "chain_read_error",
                error.category(),
                audit_candidate,
            );
        }
    };
    let decoded_obligation = match decode_obligation(&obligation_account.data) {
        Ok(obligation) => obligation,
        Err(_) => {
            return WatchRowReport::candidate_error(
                "decode_error",
                "obligation_decode_failed",
                audit_candidate,
            );
        }
    };

    let token_program = spl_token::id();
    let controlled_wallet = to_pubkey(request.delegated_wallet);
    let input_mint = to_pubkey(request.input_mint);
    let source_liquidity_ata =
        derive_associated_token_address(&controlled_wallet, &input_mint, &token_program);
    let collateral_ata = derive_associated_token_address(
        &controlled_wallet,
        &decoded_reserve.collateral_mint,
        &token_program,
    );

    let source_liquidity = match source.fetch_confirmed_account(&source_liquidity_ata).await {
        Ok(account) => match decode_token_account(source_liquidity_ata, account) {
            Ok(account) => account,
            Err(error_class) => {
                return WatchRowReport::candidate_error(
                    "decode_error",
                    error_class,
                    audit_candidate,
                );
            }
        },
        Err(error) => {
            return WatchRowReport::candidate_error(
                "chain_read_error",
                error.category(),
                audit_candidate,
            );
        }
    };
    let collateral = match source.fetch_confirmed_account(&collateral_ata).await {
        Ok(account) => match decode_token_account(collateral_ata, account) {
            Ok(account) => account,
            Err(error_class) => {
                return WatchRowReport::candidate_error(
                    "decode_error",
                    error_class,
                    audit_candidate,
                );
            }
        },
        Err(error) => {
            return WatchRowReport::candidate_error(
                "chain_read_error",
                error.category(),
                audit_candidate,
            );
        }
    };

    let final_slot = match source.fetch_confirmed_slot().await {
        Ok(slot) => slot,
        Err(error) => {
            return WatchRowReport::candidate_error(
                "chain_read_error",
                error.category(),
                audit_candidate,
            );
        }
    };

    let facts = ChainFacts {
        reserve: ReserveFacts {
            address: from_pubkey(observation.reserve_pubkey),
            account_owner: from_pubkey(observation.reserve_account.owner),
            last_update_slot: decoded_reserve.last_update_slot,
            last_update_stale: decoded_reserve.last_update_stale,
            lending_market: from_pubkey(decoded_reserve.lending_market),
            liquidity_mint: from_pubkey(decoded_reserve.liquidity_mint),
            liquidity_mint_decimals: decoded_reserve.liquidity_mint_decimals,
            liquidity_supply: from_pubkey(decoded_reserve.liquidity_supply),
            pyth_oracle: from_pubkey(decoded_reserve.pyth_oracle),
            switchboard_oracle: from_pubkey(decoded_reserve.switchboard_oracle),
            collateral_mint: from_pubkey(decoded_reserve.collateral_mint),
            collateral_supply: from_pubkey(decoded_reserve.collateral_supply),
        },
        obligation: ObligationFacts {
            address: from_pubkey(obligation_address),
            account_owner: from_pubkey(obligation_account.owner),
            lending_market: from_pubkey(decoded_obligation.lending_market),
            obligation_owner: from_pubkey(decoded_obligation.owner),
        },
        source_liquidity,
        collateral,
        derived: DerivedAccounts {
            token_program: from_pubkey(token_program),
            source_liquidity_ata: from_pubkey(source_liquidity_ata),
            collateral_ata: from_pubkey(collateral_ata),
        },
        native_supply_apr_wad,
    };

    match validate_chain(
        prepared,
        facts,
        ClockSnapshot {
            now_ms: clock.now_ms(),
            current_confirmed_slot: final_slot,
        },
    ) {
        ChainOutcome::Blocked(report) => WatchRowReport::from_candidate(report),
        ChainOutcome::Ready(plan) => match build_unsigned_transaction_report(&plan) {
            Ok(unsigned_transaction) => WatchRowReport {
                status: "ready".to_owned(),
                intent_id: plan.report.intent_id.clone(),
                rule_id_hex: plan.report.rule_id_hex.clone(),
                rule_lifecycle: None,
                candidate: Some(plan.report),
                error_class: None,
                unsigned_transaction: Some(unsigned_transaction),
            },
            Err(error_class) => {
                WatchRowReport::candidate_error("plan_build_error", error_class, plan.report)
            }
        },
    }
}

fn decode_token_account(
    address: Pubkey,
    account: Option<Account>,
) -> Result<Option<TokenAccountFacts>, &'static str> {
    let Some(account) = account else {
        return Ok(None);
    };
    let decoded =
        SplTokenAccount::unpack(&account.data).map_err(|_| "token_account_decode_failed")?;
    Ok(Some(TokenAccountFacts {
        address: from_pubkey(address),
        account_owner: from_pubkey(account.owner),
        mint: from_pubkey(decoded.mint),
        token_owner: from_pubkey(decoded.owner),
        amount_raw: decoded.amount,
        initialized: decoded.state == AccountState::Initialized,
        frozen: decoded.state == AccountState::Frozen,
    }))
}

fn build_unsigned_transaction_report(
    plan: &ValidatedSolendPlanInputs,
) -> Result<UnsignedTransactionReport, &'static str> {
    let solend_program_id = to_pubkey(plan.solend_program_id);
    let compute_limit = ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_UNIT_LIMIT);
    let compute_price =
        ComputeBudgetInstruction::set_compute_unit_price(COMPUTE_UNIT_PRICE_MICRO_LAMPORTS);
    let refresh = build_refresh_instructions(RefreshPlanInputs {
        solend_program_id,
        reserves: vec![ReserveRefreshInput {
            reserve_pubkey: to_pubkey(plan.reserve),
            pyth_oracle: to_pubkey(plan.pyth_oracle),
            switchboard_oracle: to_pubkey(plan.switchboard_oracle),
        }],
        obligation: None,
    });
    let refresh_reserve = refresh
        .instructions
        .into_iter()
        .next()
        .ok_or("refresh_plan_empty")?;
    let deposit = build_deposit_reserve_liquidity_and_obligation_collateral_instruction(
        DepositInstructionInputs {
            solend_program_id,
            amount: UnderlyingAmount::new(plan.input_amount_raw),
            source_liquidity: to_pubkey(plan.source_liquidity),
            user_collateral: to_pubkey(plan.user_collateral),
            reserve: to_pubkey(plan.reserve),
            reserve_liquidity_supply: to_pubkey(plan.reserve_liquidity_supply),
            reserve_collateral_mint: to_pubkey(plan.reserve_collateral_mint),
            lending_market: to_pubkey(plan.lending_market),
            destination_deposit_collateral: to_pubkey(plan.destination_deposit_collateral),
            obligation: to_pubkey(plan.obligation),
            obligation_owner: to_pubkey(plan.obligation_owner),
            pyth_oracle: to_pubkey(plan.pyth_oracle),
            switchboard_oracle: to_pubkey(plan.switchboard_oracle),
            user_transfer_authority: to_pubkey(plan.user_transfer_authority),
        },
    )
    .map_err(|_| "deposit_builder_rejected")?;
    let instructions = vec![
        ("set_compute_unit_limit", compute_limit),
        ("set_compute_unit_price", compute_price),
        ("refresh_reserve", refresh_reserve),
        (
            "deposit_reserve_liquidity_and_obligation_collateral",
            deposit,
        ),
    ];
    let instruction_reports = instructions
        .iter()
        .cloned()
        .enumerate()
        .map(|(instruction_index, (label, instruction))| {
            instruction_report(instruction_index, label, instruction)
        })
        .collect();
    let fee_payer = to_pubkey(plan.obligation_owner);
    let message_instructions: Vec<Instruction> = instructions
        .into_iter()
        .map(|(_, instruction)| instruction)
        .collect();
    let message = Message::new(&message_instructions, Some(&fee_payer));
    let transaction = Transaction::new_unsigned(message);
    let transaction_bytes =
        bincode::serialize(&transaction).map_err(|_| "unsigned_transaction_serialize_failed")?;

    Ok(UnsignedTransactionReport {
        // `Message::new` uses the all-zero default blockhash and
        // `Transaction::new_unsigned` uses only default signature slots. The
        // serialized transaction is therefore complete enough to audit, but
        // intentionally impossible to submit.
        kind: "unsigned_transaction_with_placeholder_blockhash",
        sendable: false,
        fee_payer: fee_payer.to_string(),
        recent_blockhash: None,
        transaction_base64: BASE64_STANDARD.encode(transaction_bytes),
        signature_slots: transaction.signatures.len(),
        all_signature_slots_default: transaction
            .signatures
            .iter()
            .all(|signature| *signature == Default::default()),
        input_amount_raw: plan.input_amount_raw.to_string(),
        compute_unit_limit: COMPUTE_UNIT_LIMIT,
        compute_unit_price_micro_lamports: COMPUTE_UNIT_PRICE_MICRO_LAMPORTS,
        instructions: instruction_reports,
    })
}

fn instruction_report(
    instruction_index: usize,
    label: &'static str,
    instruction: Instruction,
) -> InstructionReport {
    InstructionReport {
        instruction_index,
        label,
        program_id: instruction.program_id.to_string(),
        data_hex: hex::encode(instruction.data),
        accounts: instruction
            .accounts
            .into_iter()
            .enumerate()
            .map(|(index, account)| AccountMetaReport {
                position: index + 1,
                pubkey: account.pubkey.to_string(),
                is_signer: account.is_signer,
                is_writable: account.is_writable,
            })
            .collect(),
    }
}

fn classification_label(classification: CandidateClassification) -> &'static str {
    match classification {
        CandidateClassification::Ready => "ready",
        CandidateClassification::OrphanFundingOnly => "orphan_funding_only",
        CandidateClassification::OrphanRuleOnly => "orphan_rule_only",
        CandidateClassification::EmptyCandidate => "empty_candidate",
        CandidateClassification::UnsupportedAction => "unsupported_action",
        CandidateClassification::UnsupportedRuleEnvelope => "unsupported_rule_envelope",
        CandidateClassification::UnsupportedCondition => "unsupported_condition",
        CandidateClassification::HashMismatch => "hash_mismatch",
        CandidateClassification::IdentityMismatch => "identity_mismatch",
        CandidateClassification::AmountMismatch => "amount_mismatch",
        CandidateClassification::WallClockExpired => "wall_clock_expired",
        CandidateClassification::SlotExpired => "slot_expired",
        CandidateClassification::ClockUnavailable => "clock_unavailable",
        CandidateClassification::ReserveStale => "reserve_stale",
        CandidateClassification::AtaMissing => "ata_missing",
        CandidateClassification::AccountMissing => "account_missing",
        CandidateClassification::AccountMismatch => "account_mismatch",
        CandidateClassification::SourceBalanceInsufficient => "source_balance_insufficient",
        CandidateClassification::ConditionNotMet => "condition_not_met",
    }
}

fn to_pubkey(value: PubkeyBytes) -> Pubkey {
    Pubkey::new_from_array(*value.as_bytes())
}

fn from_pubkey(value: Pubkey) -> PubkeyBytes {
    PubkeyBytes::new(value.to_bytes())
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
