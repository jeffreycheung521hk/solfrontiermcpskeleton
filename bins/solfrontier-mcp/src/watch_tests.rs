use super::*;

use std::{
    collections::{hash_map::DefaultHasher, HashMap, VecDeque},
    ffi::OsString,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
};

use clap::Parser as _;
use claw_executor::FindingCode;
use claw_protocols::solend::raw::one_deposit_test_fixture;
use claw_state_store::{Database, DatabaseConfig, NewW5hFundingIntent};
use claw_types::{
    canonical_rule_hash,
    stage2_watch_rule::{
        ActionSpec, Comparison, Condition, ConditionLogic, RateKind, WatchRule, WithdrawMode,
        STAGE2_WATCH_RULE_SCHEMA_V1, STAGE2_WATCH_RULE_SCHEMA_V2,
    },
};
use solana_sdk::program_option::COption;

const SOLEND_PROGRAM: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const RESERVE: &str = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
const LENDING_MARKET: &str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
const LIQUIDITY_SUPPLY: &str = "8SheGtsopRUDzdiD6v6BR9a6bqZ9QwywYQY99Fp5meNf";
const PYTH_ORACLE: &str = "Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX";
const NULL_SWITCHBOARD_ORACLE: &str = "nu11111111111111111111111111111111111111111";
const COLLATERAL_MINT: &str = "993dVFL2uXWYeoXuEBFXR4BijeXdTv4s6BzsCjJZuwqk";
const COLLATERAL_SUPPLY: &str = "UtRy8gcEu9fCkDuUrU8EmC7Uc6FZy5NCwttzG7i6nkw";
const CONTROLLED_WALLET: &str = "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L";
const CONTROLLED_USDC_ATA: &str = "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3";
const CONTROLLED_COLLATERAL_ATA: &str = "BQazv4UQNFV8t4QGVntELr1ee3bCeTN8AvdPGRutFKn7";
const PROOF_OBLIGATION: &str = "BdFLjCcP9mCy557vNNGVbTUuvHxXsh8hc6jXzaPra1wN";
const LENDING_MARKET_AUTHORITY: &str = "DdZR6zRFiUt4S5mg7AV1uKB2z1f1WzcNYCaTEEWPAuby";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";
const AMOUNT_RAW: u64 = 500_000;
const FIXTURE_SLOT: u64 = 435_907_960;

// Public finalized account snapshot documented in the protocol crate. Keeping
// the include here test-only ensures the production watcher cannot grow a
// fixture/data injection path.
const MAINNET_RESERVE_FIXTURE_BASE64: &str = include_str!(
    "../../../crates/protocols/src/solend/fixtures/main_pool_usdc_reserve_slot_435907990.b64"
);

#[test]
fn watch_tests_cli_once_is_explicit_and_database_path_remains_global() {
    let cli = crate::Cli::try_parse_from([
        "solfrontier-mcp",
        "watch",
        "--once",
        "--db",
        "offline-watch.db",
    ])
    .expect("parse read-only watch command");
    assert_eq!(cli.db, PathBuf::from("offline-watch.db"));
    assert!(matches!(
        cli.command,
        Some(crate::Command::Watch { once: true })
    ));
}

#[derive(Debug)]
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "solfrontier-watch-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("create unique watch test directory");
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
            .is_some_and(|name| name.starts_with("solfrontier-watch-"));
        if resolved_path.parent() == Some(resolved_temp.as_path()) && has_test_prefix {
            let _ = std::fs::remove_dir_all(&resolved_path);
        }
    }
}

struct FileStore {
    _directory: TempDirectory,
    path: PathBuf,
    db: Database,
    funding: Stage2W5hFundingIntentRepository,
    rules: Stage2WatchRuleRepository,
}

impl FileStore {
    async fn new(label: &str) -> Self {
        let directory = TempDirectory::new(label);
        let path = directory.path.join("watch.db");
        let db = Database::open(&DatabaseConfig {
            path: path.to_string_lossy().into_owned(),
            max_connections: 1,
        })
        .await
        .expect("open migrated file-backed test database");
        let funding = Stage2W5hFundingIntentRepository::new(db.pool().clone());
        let rules = Stage2WatchRuleRepository::new(db.pool().clone());
        Self {
            _directory: directory,
            path,
            db,
            funding,
            rules,
        }
    }

    async fn close_writer(&self) {
        self.db.pool().close().await;
    }

    async fn open_reader(&self) -> ReadOnlyWatchStore {
        ReadOnlyWatchStore::open(&self.path)
            .await
            .expect("open read-only watch store")
    }
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl WatchClock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

struct SlotOnlySource(u64);

impl WatchChainSource for SlotOnlySource {
    async fn fetch_confirmed_slot(&self) -> Result<u64, FinalizeMarketReadError> {
        Ok(self.0)
    }

    async fn fetch_confirmed_reserve(
        &self,
        _reserve: Pubkey,
    ) -> Result<ConfirmedReserveObservation, FinalizeMarketReadError> {
        panic!("a statically blocked candidate must not read a reserve")
    }

    async fn fetch_confirmed_account(
        &self,
        _address: &Pubkey,
    ) -> Result<Option<Account>, FinalizeMarketReadError> {
        panic!("a statically blocked candidate must not read an account")
    }
}

struct FixtureChain {
    slots: Mutex<VecDeque<u64>>,
    last_slot: u64,
    reserve: ConfirmedReserveObservation,
    accounts: HashMap<Pubkey, Account>,
}

impl WatchChainSource for FixtureChain {
    async fn fetch_confirmed_slot(&self) -> Result<u64, FinalizeMarketReadError> {
        let mut slots = self.slots.lock().expect("fixture slot mutex");
        Ok(slots.pop_front().unwrap_or(self.last_slot))
    }

    async fn fetch_confirmed_reserve(
        &self,
        reserve: Pubkey,
    ) -> Result<ConfirmedReserveObservation, FinalizeMarketReadError> {
        assert_eq!(reserve, self.reserve.reserve_pubkey);
        Ok(self.reserve.clone())
    }

    async fn fetch_confirmed_account(
        &self,
        address: &Pubkey,
    ) -> Result<Option<Account>, FinalizeMarketReadError> {
        Ok(self.accounts.get(address).cloned())
    }
}

fn pubkey(value: &str) -> Pubkey {
    value.parse().expect("reviewed fixture pubkey")
}

fn pubkey_bytes(value: &str) -> PubkeyBytes {
    PubkeyBytes::from_base58(value).expect("reviewed fixture pubkey bytes")
}

fn deposit_rule(id_byte: u8) -> WatchRule {
    let controlled = pubkey_bytes(CONTROLLED_WALLET);
    WatchRule {
        schema_version: STAGE2_WATCH_RULE_SCHEMA_V2,
        rule_id: [id_byte; 16],
        user: PubkeyBytes::new([id_byte.wrapping_add(100); 32]),
        executor: controlled,
        delegated_wallet: controlled,
        created_at_slot: FIXTURE_SLOT.saturating_sub(100),
        expires_at_slot: FIXTURE_SLOT + 100,
        one_shot: true,
        condition_logic: ConditionLogic::All,
        conditions: vec![Condition::SolendReserveSupplyRate {
            reserve_pubkey: pubkey_bytes(RESERVE),
            lending_market: pubkey_bytes(LENDING_MARKET),
            solend_program_id: pubkey_bytes(SOLEND_PROGRAM),
            comparison: Comparison::Gt,
            threshold_bps: 1,
            rate_kind: RateKind::Apr,
            formula_version: 1,
            max_reserve_staleness_slots: 16,
            required_refresh_same_tx: true,
        }],
        action: ActionSpec::SolendDeposit {
            target_obligation: pubkey_bytes(PROOF_OBLIGATION),
            reserve_pubkey: pubkey_bytes(RESERVE),
            lending_market: pubkey_bytes(LENDING_MARKET),
            solend_program_id: pubkey_bytes(SOLEND_PROGRAM),
            input_mint: pubkey_bytes(USDC_MINT),
            input_amount_raw: AMOUNT_RAW,
        },
        max_input_amount_raw: AMOUNT_RAW,
        used_amount_raw: 0,
        destination: controlled,
        slippage_bps: 0,
    }
}

fn rule_threshold(rule: &WatchRule) -> u32 {
    match &rule.conditions[0] {
        Condition::SolendReserveSupplyRate { threshold_bps, .. } => *threshold_bps,
        _ => panic!("watch fixture must use a Solend rate condition"),
    }
}

async fn insert_reserved_funding(
    store: &FileStore,
    rule: &WatchRule,
    amount_raw: u64,
    expires_at_ms: i64,
    created_at_ms: i64,
    hash_override: Option<String>,
) {
    let intent_id = hex::encode(rule.rule_id);
    let canonical_rule_hash_hex =
        hash_override.unwrap_or_else(|| hex::encode(canonical_rule_hash(rule)));
    store
        .funding
        .insert(&NewW5hFundingIntent {
            intent_id: intent_id.clone(),
            rule_id_hex: intent_id.clone(),
            canonical_rule_hash_hex,
            user_wallet: CONTROLLED_WALLET.to_owned(),
            user_usdc_ata: CONTROLLED_USDC_ATA.to_owned(),
            controlled_wallet: CONTROLLED_WALLET.to_owned(),
            controlled_usdc_ata: CONTROLLED_USDC_ATA.to_owned(),
            amount_raw,
            threshold_bps: rule_threshold(rule),
            save_display_apy_bps_at_creation: 500,
            native_onchain_apr_bps_at_creation: 450,
            created_at_ms,
            expires_at_ms,
        })
        .await
        .expect("insert funding fixture");
    let signature = format!("offline-fixture-{intent_id}");
    assert_eq!(
        store
            .funding
            .mark_funding_submitted_if_required(&intent_id, &signature)
            .await
            .expect("submit funding fixture"),
        1
    );
    assert_eq!(
        store
            .funding
            .mark_budget_reserved_if_submitted(&intent_id, &signature, FIXTURE_SLOT)
            .await
            .expect("reserve funding fixture"),
        1
    );
}

async fn insert_candidate(
    store: &FileStore,
    rule: &WatchRule,
    amount_raw: u64,
    expires_at_ms: i64,
    created_at_ms: i64,
) {
    store.rules.insert(rule).await.expect("insert rule fixture");
    insert_reserved_funding(store, rule, amount_raw, expires_at_ms, created_at_ms, None).await;
}

fn token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let token = SplTokenAccount {
        mint,
        owner,
        amount,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };
    let mut data = vec![0_u8; SplTokenAccount::LEN];
    SplTokenAccount::pack(token, &mut data).expect("pack token fixture");
    Account {
        lamports: 1,
        data,
        owner: spl_token::id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn fixture_chain(collateral_missing: bool) -> FixtureChain {
    let reserve_data = BASE64_STANDARD
        .decode(MAINNET_RESERVE_FIXTURE_BASE64.trim())
        .expect("decode checked-in mainnet reserve fixture");
    let solend_program = pubkey(SOLEND_PROGRAM);
    let reserve_pubkey = pubkey(RESERVE);
    let controlled = pubkey(CONTROLLED_WALLET);
    let mut obligation = one_deposit_test_fixture();
    // Solend obligation owner is a fixed 32-byte field at offset 42.
    obligation.data[42..74].copy_from_slice(&controlled.to_bytes());

    let mut accounts = HashMap::new();
    accounts.insert(
        pubkey(PROOF_OBLIGATION),
        Account {
            lamports: 1,
            data: obligation.data,
            owner: solend_program,
            executable: false,
            rent_epoch: 0,
        },
    );
    accounts.insert(
        pubkey(CONTROLLED_USDC_ATA),
        token_account(pubkey(USDC_MINT), controlled, AMOUNT_RAW),
    );
    if !collateral_missing {
        accounts.insert(
            pubkey(CONTROLLED_COLLATERAL_ATA),
            token_account(pubkey(COLLATERAL_MINT), controlled, 0),
        );
    }

    FixtureChain {
        slots: Mutex::new(VecDeque::from([FIXTURE_SLOT, FIXTURE_SLOT])),
        last_slot: FIXTURE_SLOT,
        reserve: ConfirmedReserveObservation {
            current_confirmed_slot: FIXTURE_SLOT,
            reserve_pubkey,
            reserve_account: Account {
                lamports: 1,
                data: reserve_data,
                owner: solend_program,
                executable: false,
                rent_epoch: 0,
            },
        },
        accounts,
    }
}

fn row_for_rule<'a>(report: &'a WatchCycleReport, rule: &WatchRule) -> &'a WatchRowReport {
    let id = hex::encode(rule.rule_id);
    report
        .rows
        .iter()
        .find(|row| row.rule_id_hex.as_deref() == Some(id.as_str()))
        .expect("report row for rule")
}

fn sidecar_path(database: &Path, suffix: &str) -> PathBuf {
    let mut path = OsString::from(database.as_os_str());
    path.push(suffix);
    PathBuf::from(path)
}

fn sqlite_family_bytes(database: &Path) -> std::io::Result<BTreeMap<String, Vec<u8>>> {
    let mut family = BTreeMap::new();
    for suffix in ["", "-wal", "-shm"] {
        let path = sidecar_path(database, suffix);
        if path.exists() {
            family.insert(suffix.to_owned(), std::fs::read(path)?);
        }
    }
    Ok(family)
}

fn sqlite_family_manifest(family: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, (usize, u64)> {
    family
        .iter()
        .map(|(suffix, bytes)| {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            (suffix.clone(), (bytes.len(), hasher.finish()))
        })
        .collect()
}

#[tokio::test]
async fn watch_tests_wall_and_slot_deadline_endpoints_are_inclusive_blockers() {
    let store = FileStore::new("deadline-endpoints").await;
    let mut wall = deposit_rule(1);
    wall.created_at_slot = 100;
    wall.expires_at_slot = 151;
    insert_candidate(&store, &wall, AMOUNT_RAW, 1_000, 1).await;

    let mut slot = deposit_rule(2);
    slot.created_at_slot = 100;
    slot.expires_at_slot = 150;
    insert_candidate(&store, &slot, AMOUNT_RAW, 1_001, 2).await;

    store.close_writer().await;
    let reader = store.open_reader().await;
    let report = scan_once(&reader, Some(&SlotOnlySource(150)), &FixedClock(1_000)).await;
    reader.close().await;

    let wall_row = row_for_rule(&report, &wall);
    assert_eq!(wall_row.status, "wall_clock_expired");
    let wall_report = wall_row.candidate.as_ref().expect("wall candidate report");
    assert_eq!(wall_report.clocks.wall_clock_eligible, Some(false));
    assert_eq!(wall_report.clocks.slot_clock_eligible, Some(true));

    let slot_row = row_for_rule(&report, &slot);
    assert_eq!(slot_row.status, "slot_expired");
    let slot_report = slot_row.candidate.as_ref().expect("slot candidate report");
    assert_eq!(slot_report.clocks.wall_clock_eligible, Some(true));
    assert_eq!(slot_report.clocks.slot_clock_eligible, Some(false));
}

#[tokio::test]
async fn watch_tests_canonical_hash_mismatch_blocks_before_chain_reads() {
    let store = FileStore::new("hash-mismatch").await;
    let mut rule = deposit_rule(3);
    rule.created_at_slot = 100;
    rule.expires_at_slot = 200;
    store.rules.insert(&rule).await.expect("insert hash rule");
    let expected = hex::encode(canonical_rule_hash(&rule));
    let uppercase = expected.to_ascii_uppercase();
    assert_ne!(uppercase, expected, "fixture hash must contain a-f");
    insert_reserved_funding(&store, &rule, AMOUNT_RAW, 2_000, 1, Some(uppercase)).await;

    store.close_writer().await;
    let reader = store.open_reader().await;
    let report = scan_once(&reader, Some(&SlotOnlySource(150)), &FixedClock(1_000)).await;
    reader.close().await;

    let row = row_for_rule(&report, &rule);
    assert_eq!(row.status, "hash_mismatch");
    assert!(row
        .candidate
        .as_ref()
        .expect("hash candidate report")
        .findings
        .iter()
        .any(|finding| finding.code == FindingCode::CanonicalHashMismatch));
}

#[tokio::test]
async fn watch_tests_action_rule_and_funding_amounts_must_match_three_ways() {
    let store = FileStore::new("three-way-amount").await;

    let mut action_differs = deposit_rule(4);
    action_differs.created_at_slot = 100;
    action_differs.expires_at_slot = 200;
    let ActionSpec::SolendDeposit {
        input_amount_raw, ..
    } = &mut action_differs.action
    else {
        unreachable!()
    };
    *input_amount_raw += 1;
    insert_candidate(&store, &action_differs, AMOUNT_RAW, 2_000, 1).await;

    let mut maximum_differs = deposit_rule(5);
    maximum_differs.created_at_slot = 100;
    maximum_differs.expires_at_slot = 200;
    maximum_differs.max_input_amount_raw += 1;
    insert_candidate(&store, &maximum_differs, AMOUNT_RAW, 2_000, 2).await;

    let mut funding_differs = deposit_rule(6);
    funding_differs.created_at_slot = 100;
    funding_differs.expires_at_slot = 200;
    insert_candidate(&store, &funding_differs, AMOUNT_RAW + 1, 2_000, 3).await;

    store.close_writer().await;
    let reader = store.open_reader().await;
    let report = scan_once(&reader, Some(&SlotOnlySource(150)), &FixedClock(1_000)).await;
    reader.close().await;

    for rule in [&action_differs, &maximum_differs, &funding_differs] {
        let row = row_for_rule(&report, rule);
        assert_eq!(row.status, "amount_mismatch");
        let amounts = row
            .candidate
            .as_ref()
            .and_then(|candidate| candidate.amounts.as_ref())
            .expect("three-way amount evidence");
        assert!(!amounts.all_equal_and_nonzero);
    }
    assert_eq!(report.summary.get("amount_mismatch"), Some(&3));
}

#[tokio::test]
async fn watch_tests_both_orphan_shapes_are_reported_and_scanning_continues() {
    let store = FileStore::new("orphans").await;

    let mut funding_only = deposit_rule(7);
    funding_only.created_at_slot = 100;
    funding_only.expires_at_slot = 200;
    insert_reserved_funding(&store, &funding_only, AMOUNT_RAW, 2_000, 1, None).await;

    let mut continued = deposit_rule(8);
    continued.created_at_slot = 100;
    continued.expires_at_slot = 200;
    insert_candidate(&store, &continued, AMOUNT_RAW, 999, 2).await;

    let mut rule_only = deposit_rule(9);
    rule_only.created_at_slot = 100;
    rule_only.expires_at_slot = 200;
    store
        .rules
        .insert(&rule_only)
        .await
        .expect("insert orphan rule");

    store.close_writer().await;
    let reader = store.open_reader().await;
    let report = scan_once(&reader, Some(&SlotOnlySource(150)), &FixedClock(1_000)).await;
    reader.close().await;

    assert_eq!(
        row_for_rule(&report, &funding_only).status,
        "orphan_funding_only"
    );
    assert_eq!(row_for_rule(&report, &rule_only).status, "orphan_rule_only");
    assert_eq!(
        row_for_rule(&report, &continued).status,
        "wall_clock_expired",
        "a row after the funding orphan must still be inspected"
    );
    assert_eq!(report.summary.get("total"), Some(&3));
}

#[tokio::test]
async fn watch_tests_unsupported_v1_action_retains_both_expiry_findings() {
    let store = FileStore::new("v1-expiry").await;
    let mut legacy = deposit_rule(10);
    legacy.schema_version = STAGE2_WATCH_RULE_SCHEMA_V1;
    legacy.created_at_slot = 100;
    legacy.expires_at_slot = 150;
    legacy.action = ActionSpec::SolendWithdrawAllDelegated {
        target_obligation: pubkey_bytes(PROOF_OBLIGATION),
        reserve_pubkey: pubkey_bytes(RESERVE),
        lending_market: pubkey_bytes(LENDING_MARKET),
        destination_wallet: pubkey_bytes(CONTROLLED_WALLET),
        withdraw_mode: WithdrawMode::WithdrawAllDelegatedPosition,
    };
    insert_candidate(&store, &legacy, AMOUNT_RAW, 1_000, 1).await;

    store.close_writer().await;
    let reader = store.open_reader().await;
    let report = scan_once(&reader, Some(&SlotOnlySource(150)), &FixedClock(1_000)).await;
    reader.close().await;

    let row = row_for_rule(&report, &legacy);
    assert_eq!(row.status, "unsupported_action");
    let candidate = row.candidate.as_ref().expect("legacy candidate report");
    for expected in [
        FindingCode::UnsupportedAction,
        FindingCode::UnsupportedSchemaVersion,
        FindingCode::WallClockExpired,
        FindingCode::SlotExpired,
    ] {
        assert!(
            candidate
                .findings
                .iter()
                .any(|finding| finding.code == expected),
            "missing finding {expected:?}"
        );
    }
    assert!(candidate.findings.iter().all(|finding| {
        !matches!(
            finding.code,
            FindingCode::AmountMismatch | FindingCode::AmountZero
        )
    }));
}

#[tokio::test]
async fn watch_tests_executing_completed_and_revoked_rules_are_lifecycle_ineligible() {
    let store = FileStore::new("lifecycle").await;
    let mut executing = deposit_rule(11);
    executing.created_at_slot = 100;
    executing.expires_at_slot = 200;
    insert_candidate(&store, &executing, AMOUNT_RAW, 2_000, 1).await;
    assert_eq!(
        store
            .rules
            .mark_executing(&executing.rule_id, 77)
            .await
            .expect("mark fixture executing"),
        1
    );

    let mut completed = deposit_rule(12);
    completed.created_at_slot = 100;
    completed.expires_at_slot = 200;
    insert_candidate(&store, &completed, AMOUNT_RAW, 2_000, 2).await;
    assert_eq!(
        store
            .rules
            .mark_completed(&completed.rule_id, AMOUNT_RAW, 151)
            .await
            .expect("complete fixture rule"),
        1
    );

    let mut revoked = deposit_rule(13);
    revoked.created_at_slot = 100;
    revoked.expires_at_slot = 200;
    insert_candidate(&store, &revoked, AMOUNT_RAW, 2_000, 3).await;
    assert_eq!(
        store
            .rules
            .mark_revoked(&revoked.rule_id)
            .await
            .expect("revoke fixture rule"),
        1
    );

    store.close_writer().await;
    let reader = store.open_reader().await;
    let report = scan_once(&reader, Some(&SlotOnlySource(150)), &FixedClock(1_000)).await;
    reader.close().await;

    let executing_row = row_for_rule(&report, &executing);
    assert_eq!(executing_row.status, "ineligible_rule_lifecycle");
    assert_eq!(
        executing_row.error_class.as_deref(),
        Some("rule_lifecycle_status_ineligible")
    );
    assert_eq!(
        executing_row
            .rule_lifecycle
            .as_ref()
            .expect("executing lifecycle")
            .execution_nonce,
        77
    );

    for terminal in [&completed, &revoked] {
        let row = row_for_rule(&report, terminal);
        assert_eq!(row.status, "ineligible_rule_lifecycle");
        assert_eq!(
            row.error_class.as_deref(),
            Some("rule_lifecycle_flags_terminal")
        );
    }
    assert_eq!(report.summary.get("ineligible_rule_lifecycle"), Some(&3));
}

#[tokio::test]
async fn watch_tests_missing_collateral_ata_is_explicit_and_never_created() {
    let store = FileStore::new("ata-missing").await;
    let rule = deposit_rule(14);
    insert_candidate(&store, &rule, AMOUNT_RAW, 2_000, 1).await;

    store.close_writer().await;
    let reader = store.open_reader().await;
    let chain = fixture_chain(true);
    let report = scan_once(&reader, Some(&chain), &FixedClock(1_000)).await;
    reader.close().await;

    let row = row_for_rule(&report, &rule);
    assert_eq!(row.status, "ata_missing");
    assert!(row.unsigned_transaction.is_none());
    assert!(row
        .candidate
        .as_ref()
        .expect("ATA candidate report")
        .findings
        .iter()
        .any(|finding| finding.code == FindingCode::AtaMissing));
}

#[tokio::test]
async fn watch_tests_ready_report_is_unsigned_and_matches_mainnet_instruction_proof() {
    let store = FileStore::new("ready-proof").await;
    let rule = deposit_rule(15);
    insert_candidate(&store, &rule, AMOUNT_RAW, 2_000, 1).await;

    store.close_writer().await;
    let reader = store.open_reader().await;
    let chain = fixture_chain(false);
    let report = scan_once(&reader, Some(&chain), &FixedClock(1_000)).await;
    reader.close().await;

    let row = row_for_rule(&report, &rule);
    assert_eq!(row.status, "ready");
    let transaction = row
        .unsigned_transaction
        .as_ref()
        .expect("ready row must include unsigned transaction");
    assert_eq!(
        transaction.kind,
        "unsigned_transaction_with_placeholder_blockhash"
    );
    assert!(!transaction.sendable);
    assert_eq!(transaction.fee_payer, CONTROLLED_WALLET);
    assert_eq!(transaction.recent_blockhash, None);
    assert_eq!(transaction.signature_slots, 1);
    assert!(transaction.all_signature_slots_default);
    assert_eq!(transaction.input_amount_raw, AMOUNT_RAW.to_string());
    assert_eq!(transaction.compute_unit_limit, 400_000);
    assert_eq!(transaction.compute_unit_price_micro_lamports, 50_000);

    let labels: Vec<_> = transaction
        .instructions
        .iter()
        .map(|instruction| instruction.label)
        .collect();
    assert_eq!(
        labels,
        [
            "set_compute_unit_limit",
            "set_compute_unit_price",
            "refresh_reserve",
            "deposit_reserve_liquidity_and_obligation_collateral",
        ]
    );
    assert_eq!(
        transaction
            .instructions
            .iter()
            .map(|instruction| instruction.program_id.as_str())
            .collect::<Vec<_>>(),
        [
            COMPUTE_BUDGET_PROGRAM,
            COMPUTE_BUDGET_PROGRAM,
            SOLEND_PROGRAM,
            SOLEND_PROGRAM,
        ]
    );
    assert_eq!(
        transaction
            .instructions
            .iter()
            .map(|instruction| instruction.data_hex.as_str())
            .collect::<Vec<_>>(),
        [
            "02801a0600",
            "0350c3000000000000",
            "03",
            "0e20a1070000000000",
        ]
    );

    let refresh = &transaction.instructions[2];
    assert_eq!(
        refresh
            .accounts
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect::<Vec<_>>(),
        [
            RESERVE,
            PYTH_ORACLE,
            NULL_SWITCHBOARD_ORACLE,
            "SysvarC1ock11111111111111111111111111111111",
        ]
    );

    let deposit = &transaction.instructions[3];
    let expected = [
        (CONTROLLED_USDC_ATA, false, true),
        (CONTROLLED_COLLATERAL_ATA, false, true),
        (RESERVE, false, true),
        (LIQUIDITY_SUPPLY, false, true),
        (COLLATERAL_MINT, false, true),
        (LENDING_MARKET, false, false),
        (LENDING_MARKET_AUTHORITY, false, false),
        (COLLATERAL_SUPPLY, false, true),
        (PROOF_OBLIGATION, false, true),
        (CONTROLLED_WALLET, true, true),
        (PYTH_ORACLE, false, false),
        (NULL_SWITCHBOARD_ORACLE, false, false),
        (CONTROLLED_WALLET, true, false),
        (TOKEN_PROGRAM, false, false),
    ];
    assert_eq!(deposit.accounts.len(), 14);
    for (index, (actual, (expected_key, expected_signer, expected_writable))) in
        deposit.accounts.iter().zip(expected).enumerate()
    {
        assert_eq!(actual.position, index + 1);
        assert_eq!(actual.pubkey, expected_key);
        assert_eq!(actual.is_signer, expected_signer);
        assert_eq!(actual.is_writable, expected_writable);
    }

    let bytes = BASE64_STANDARD
        .decode(&transaction.transaction_base64)
        .expect("unsigned transaction report is base64");
    let decoded: Transaction =
        bincode::deserialize(&bytes).expect("unsigned transaction report is bincode");
    assert_eq!(decoded.message.recent_blockhash, Default::default());
    assert!(decoded
        .signatures
        .iter()
        .all(|signature| *signature == Default::default()));
}

#[tokio::test]
async fn watch_tests_scan_is_query_only_and_leaves_database_state_unchanged() {
    let store = FileStore::new("zero-write").await;
    let mut rule = deposit_rule(16);
    rule.created_at_slot = 100;
    rule.expires_at_slot = 200;
    insert_candidate(&store, &rule, AMOUNT_RAW, 999, 1).await;
    store.close_writer().await;

    let database_before = std::fs::read(&store.path).expect("snapshot database before watch");
    let reader = store.open_reader().await;
    let query_only: i64 = sqlx::query_scalar("PRAGMA query_only")
        .fetch_one(&reader.pool)
        .await
        .expect("read query_only pragma");
    assert_eq!(query_only, 1);
    let rule_id_hex = hex::encode(rule.rule_id);
    let funding_before = format!(
        "{:?}",
        reader
            .funding
            .get(&rule_id_hex)
            .await
            .expect("read funding before scan")
    );
    let rule_before = format!(
        "{:?}",
        reader
            .rules
            .get(&rule.rule_id)
            .await
            .expect("read rule before scan")
    );
    // SQLite may materialize its WAL shared-memory bookkeeping while opening a
    // read-only WAL database and issuing the first repository read. Snapshot
    // after that initialization so this assertion isolates the watcher scan
    // and rejected write attempt.
    let before =
        sqlite_family_bytes(&store.path).expect("snapshot initialized read-only SQLite family");

    let report = scan_once(&reader, Some(&SlotOnlySource(150)), &FixedClock(1_000)).await;
    assert_eq!(row_for_rule(&report, &rule).status, "wall_clock_expired");
    let write_error = reader
        .rules
        .mark_checked(&rule.rule_id, 151, 1_000)
        .await
        .expect_err("query_only repository connection must reject writes");
    let error_text = write_error.to_string().to_ascii_lowercase();
    assert!(
        error_text.contains("readonly")
            || error_text.contains("read-only")
            || error_text.contains("attempt to write"),
        "unexpected read-only error: {write_error}"
    );
    let funding_after = format!(
        "{:?}",
        reader
            .funding
            .get(&rule_id_hex)
            .await
            .expect("read funding after scan")
    );
    let rule_after = format!(
        "{:?}",
        reader
            .rules
            .get(&rule.rule_id)
            .await
            .expect("read rule after scan")
    );
    assert_eq!(funding_after, funding_before, "funding row changed");
    assert_eq!(rule_after, rule_before, "WatchRule row changed");

    let after = sqlite_family_bytes(&store.path).expect("snapshot SQLite family after scan");
    assert_eq!(
        sqlite_family_manifest(&after),
        sqlite_family_manifest(&before),
        "scan changed the SQLite family member set, size, or byte fingerprint"
    );
    assert_eq!(
        after, before,
        "scan and rejected write must leave every initialized family byte unchanged"
    );
    reader.close().await;
    let database_after = std::fs::read(&store.path).expect("snapshot database after watch");
    assert_eq!(
        database_after, database_before,
        "opening, scanning, and closing must leave the durable database byte-identical"
    );
}

fn acceptance_database_path() -> PathBuf {
    let database = std::env::var_os("SOLFRONTIER_ACCEPTANCE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../data/acceptance/phase2-mainnet-20260729-04.db")
        });
    assert!(
        database.is_file(),
        "SOLFRONTIER_ACCEPTANCE_DB must name an existing .db file: {}",
        database.display()
    );
    database
}

#[tokio::test]
#[ignore = "local acceptance only; copies the complete DB family and never opens the original"]
async fn watch_tests_local_acceptance_database_family_copy_is_read_only() {
    const PHASE2_V1_RULE_ID: &str = "32000000a0860100000000009a62dace";

    let original = acceptance_database_path();
    let original_before =
        sqlite_family_bytes(&original).expect("snapshot original acceptance DB family");
    assert_eq!(
        original_before.len(),
        3,
        "acceptance requires the complete .db/-wal/-shm family"
    );

    let directory = TempDirectory::new("local-acceptance");
    let file_name = original
        .file_name()
        .expect("acceptance database has a file name");
    let copied = directory.path.join(file_name);
    for suffix in ["", "-wal", "-shm"] {
        let source = sidecar_path(&original, suffix);
        let destination = sidecar_path(&copied, suffix);
        std::fs::copy(&source, &destination).unwrap_or_else(|error| {
            panic!(
                "copy acceptance family member {} to {}: {error}",
                source.display(),
                destination.display()
            )
        });
    }
    let copied_before = sqlite_family_bytes(&copied).expect("snapshot copied acceptance DB family");
    assert_eq!(copied_before.len(), 3);

    let reader = ReadOnlyWatchStore::open(&copied)
        .await
        .expect("open copied acceptance database read-only");
    let query_only: i64 = sqlx::query_scalar("PRAGMA query_only")
        .fetch_one(&reader.pool)
        .await
        .expect("read acceptance query_only pragma");
    assert_eq!(query_only, 1);
    let report = scan_once(
        &reader,
        Some(&SlotOnlySource(u64::MAX)),
        &FixedClock(i64::MAX),
    )
    .await;
    let v1_row = report
        .rows
        .iter()
        .find(|row| row.rule_id_hex.as_deref() == Some(PHASE2_V1_RULE_ID))
        .expect("historical Phase 2 v1 acceptance row");
    assert_eq!(v1_row.status, "unsupported_action");
    let v1_candidate = v1_row
        .candidate
        .as_ref()
        .expect("historical v1 candidate report");
    for expected in [
        FindingCode::UnsupportedAction,
        FindingCode::WallClockExpired,
        FindingCode::SlotExpired,
    ] {
        assert!(
            v1_candidate
                .findings
                .iter()
                .any(|finding| finding.code == expected),
            "historical v1 row is missing {expected:?}"
        );
    }
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize acceptance watch report")
    );
    reader.close().await;

    let copied_after =
        sqlite_family_bytes(&copied).expect("resnapshot copied acceptance DB family");
    assert_eq!(
        sqlite_family_manifest(&copied_after),
        sqlite_family_manifest(&copied_before),
        "query_only watch changed the copied family manifest"
    );
    assert_eq!(
        copied_after, copied_before,
        "query_only watch must leave every copied family byte unchanged"
    );
    let original_after =
        sqlite_family_bytes(&original).expect("resnapshot original acceptance DB family");
    assert_eq!(
        sqlite_family_manifest(&original_after),
        sqlite_family_manifest(&original_before),
        "local acceptance changed the original family manifest"
    );
    assert_eq!(
        original_after, original_before,
        "local acceptance must leave every original family byte unchanged"
    );
}
