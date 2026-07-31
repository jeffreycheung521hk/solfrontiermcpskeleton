use claw_types::{
    canonical_intent::PubkeyBytes,
    stage2_watch_rule::{
        canonical_rule_hash, ActionSpec, Comparison, Condition, ConditionLogic, RateKind,
        WatchRule, STAGE2_WATCH_RULE_SCHEMA_V2,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    compare_wad,
    condition::BPS_WAD,
    facts::{ChainFacts, ClockSnapshot, PreflightClockSnapshot},
    report::{
        primary_classification, AmountReport, CandidateClassification, CandidateFinding,
        CandidateReport, ClockReport, ConditionReport, FindingCode,
    },
};

const SUPPORTED_FORMULA_VERSION: u8 = 1;
const REQUIRED_MAX_RESERVE_STALENESS_SLOTS: u32 = 16;
const REQUIRED_LIQUIDITY_MINT_DECIMALS: u8 = 6;
const MAX_SUPPORTED_THRESHOLD_BPS: u32 = 10_000;
const AUDITED_SOLEND_PROGRAM_BS58: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const CLASSIC_TOKEN_PROGRAM_BS58: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const USDC_MINT_BS58: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Immutable projection of one `budget_reserved` funding row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingSnapshot {
    pub intent_id: String,
    pub rule_id_hex: String,
    pub canonical_rule_hash_hex: String,
    pub controlled_wallet: PubkeyBytes,
    pub controlled_usdc_ata: PubkeyBytes,
    pub amount_raw: u64,
    pub threshold_bps: u32,
    pub expires_at_ms: i64,
}

/// One join result supplied by the read-only watcher adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInput {
    pub funding: Option<FundingSnapshot>,
    pub rule: Option<WatchRule>,
}

/// Static validation either blocks without chain reads or returns an opaque,
/// fingerprint-checked deposit request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightOutcome {
    Blocked(CandidateReport),
    NeedsChain(PreparedDeposit),
}

/// Chain validation either blocks or returns all SDK-neutral inputs needed by
/// the audited protocol-builder adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainOutcome {
    Blocked(CandidateReport),
    Ready(ValidatedSolendPlanInputs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CanonicalDepositAction {
    target_obligation: PubkeyBytes,
    reserve_pubkey: PubkeyBytes,
    lending_market: PubkeyBytes,
    solend_program_id: PubkeyBytes,
    input_mint: PubkeyBytes,
    input_amount_raw: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SupportedRateCondition {
    comparison: Comparison,
    threshold_bps: u32,
    max_reserve_staleness_slots: u32,
}

/// Opaque proof that static identity, amount, condition, and clock checks pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDeposit {
    funding: FundingSnapshot,
    delegated_wallet: PubkeyBytes,
    action: CanonicalDepositAction,
    condition: SupportedRateCondition,
    report: CandidateReport,
}

/// Accounts the binary may read after static validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanReadRequest {
    pub reserve_pubkey: PubkeyBytes,
    pub target_obligation: PubkeyBytes,
    pub input_mint: PubkeyBytes,
    pub delegated_wallet: PubkeyBytes,
    pub source_liquidity_ata: PubkeyBytes,
}

impl PreparedDeposit {
    pub fn read_request(&self) -> PlanReadRequest {
        PlanReadRequest {
            reserve_pubkey: self.action.reserve_pubkey,
            target_obligation: self.action.target_obligation,
            input_mint: self.action.input_mint,
            delegated_wallet: self.delegated_wallet,
            source_liquidity_ata: self.funding.controlled_usdc_ata,
        }
    }

    /// Human-auditable static evidence retained if a later account read,
    /// decode, or builder step fails.
    pub fn report(&self) -> &CandidateReport {
        &self.report
    }
}

fn pinned_pubkey(value: &'static str) -> PubkeyBytes {
    PubkeyBytes::from_base58(value).expect("reviewed protocol identity must be a valid pubkey")
}

fn audited_solend_program() -> PubkeyBytes {
    pinned_pubkey(AUDITED_SOLEND_PROGRAM_BS58)
}

fn classic_token_program() -> PubkeyBytes {
    pinned_pubkey(CLASSIC_TOKEN_PROGRAM_BS58)
}

fn usdc_mint() -> PubkeyBytes {
    pinned_pubkey(USDC_MINT_BS58)
}

/// Fully validated, SDK-neutral source data for the four-instruction dry run.
///
/// The binary adapter must use these values verbatim when invoking the
/// `claw-protocols` refresh/deposit builders. It may add only the separately
/// disclosed compute-budget policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSolendPlanInputs {
    pub solend_program_id: PubkeyBytes,
    /// Fingerprint-bound liquidity mint carried from `SolendDeposit`.
    pub input_mint: PubkeyBytes,
    pub input_amount_raw: u64,
    pub source_liquidity: PubkeyBytes,
    pub user_collateral: PubkeyBytes,
    pub reserve: PubkeyBytes,
    pub reserve_liquidity_supply: PubkeyBytes,
    pub reserve_collateral_mint: PubkeyBytes,
    pub lending_market: PubkeyBytes,
    pub destination_deposit_collateral: PubkeyBytes,
    pub obligation: PubkeyBytes,
    pub obligation_owner: PubkeyBytes,
    pub pyth_oracle: PubkeyBytes,
    pub switchboard_oracle: PubkeyBytes,
    pub user_transfer_authority: PubkeyBytes,
    pub token_program: PubkeyBytes,
    pub report: CandidateReport,
}

/// Perform all checks that need no RPC/account data.
///
/// All applicable blockers are accumulated before a primary classification is
/// selected. This is why an unsupported legacy action can also report both
/// elapsed deadlines in the same result.
pub fn preflight_candidate(
    input: CandidateInput,
    clocks: impl Into<PreflightClockSnapshot>,
) -> PreflightOutcome {
    let clocks = clocks.into();
    let (funding, rule) = match (input.funding, input.rule) {
        (Some(funding), Some(rule)) => (funding, rule),
        (Some(funding), None) => {
            let finding = CandidateFinding::new(
                FindingCode::OrphanFundingOnly,
                "budget_reserved funding row has no corresponding WatchRule",
            );
            let mut report = CandidateReport::bare(
                CandidateClassification::OrphanFundingOnly,
                vec![finding],
                clocks.now_ms,
                clocks.current_confirmed_slot,
            );
            report.intent_id = Some(funding.intent_id);
            report.rule_id_hex = Some(funding.rule_id_hex);
            return PreflightOutcome::Blocked(report);
        }
        (None, Some(rule)) => {
            let finding = CandidateFinding::new(
                FindingCode::OrphanRuleOnly,
                "WatchRule has no corresponding funding row",
            );
            let mut report = CandidateReport::bare(
                CandidateClassification::OrphanRuleOnly,
                vec![finding],
                clocks.now_ms,
                clocks.current_confirmed_slot,
            );
            report.rule_id_hex = Some(hex::encode(rule.rule_id));
            return PreflightOutcome::Blocked(report);
        }
        (None, None) => {
            return PreflightOutcome::Blocked(CandidateReport::bare(
                CandidateClassification::EmptyCandidate,
                vec![CandidateFinding::new(
                    FindingCode::EmptyCandidate,
                    "candidate contains neither a funding row nor a WatchRule",
                )],
                clocks.now_ms,
                clocks.current_confirmed_slot,
            ));
        }
    };

    let expected_rule_id_hex = hex::encode(rule.rule_id);
    let expected_hash_hex = hex::encode(canonical_rule_hash(&rule));
    let mut findings = Vec::new();

    if rule.schema_version != STAGE2_WATCH_RULE_SCHEMA_V2 {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedSchemaVersion,
            "dry-run supports only canonical WatchRule schema v2",
        ));
    }
    if !rule.one_shot {
        findings.push(CandidateFinding::new(
            FindingCode::MultiUseRule,
            "dry-run supports one-shot rules only",
        ));
    }
    if rule.used_amount_raw != 0 {
        findings.push(CandidateFinding::new(
            FindingCode::UsedAmountNonZero,
            "a dry-run candidate must have used_amount_raw == 0",
        ));
    }
    if rule.executor != rule.delegated_wallet || rule.destination != rule.delegated_wallet {
        findings.push(CandidateFinding::new(
            FindingCode::ControlledWalletEnvelopeMismatch,
            "executor and destination must equal the fingerprint-bound delegated wallet",
        ));
    }

    if funding.intent_id != expected_rule_id_hex || funding.rule_id_hex != expected_rule_id_hex {
        findings.push(CandidateFinding::new(
            FindingCode::RuleIdMismatch,
            "funding intent/rule id is not the exact canonical lowercase rule id",
        ));
    }
    if funding.canonical_rule_hash_hex != expected_hash_hex {
        findings.push(CandidateFinding::new(
            FindingCode::CanonicalHashMismatch,
            "funding canonical_rule_hash_hex is not the exact recomputed lowercase hash",
        ));
    }
    if funding.controlled_wallet != rule.delegated_wallet {
        findings.push(CandidateFinding::new(
            FindingCode::ControlledWalletMismatch,
            "funding controlled wallet differs from the fingerprint-bound delegated wallet",
        ));
    }

    let action = match &rule.action {
        ActionSpec::SolendDeposit {
            target_obligation,
            reserve_pubkey,
            lending_market,
            solend_program_id,
            input_mint,
            input_amount_raw,
        } => Some(CanonicalDepositAction {
            target_obligation: *target_obligation,
            reserve_pubkey: *reserve_pubkey,
            lending_market: *lending_market,
            solend_program_id: *solend_program_id,
            input_mint: *input_mint,
            input_amount_raw: *input_amount_raw,
        }),
        _ => {
            findings.push(CandidateFinding::new(
                FindingCode::UnsupportedAction,
                "WatchRule action is not ActionSpec::SolendDeposit",
            ));
            None
        }
    };

    let action_amount = action.map(|action| action.input_amount_raw);
    let amounts_equal = action_amount.is_some_and(|amount| {
        amount == rule.max_input_amount_raw && rule.max_input_amount_raw == funding.amount_raw
    });
    if action_amount.is_some() {
        if !amounts_equal {
            findings.push(CandidateFinding::new(
                FindingCode::AmountMismatch,
                "action, WatchRule maximum, and funding amounts are not exactly equal",
            ));
        }
        if action_amount == Some(0) || rule.max_input_amount_raw == 0 || funding.amount_raw == 0 {
            findings.push(CandidateFinding::new(
                FindingCode::AmountZero,
                "deposit amount must be non-zero",
            ));
        }
    }
    if action.is_some_and(|action| action.solend_program_id != audited_solend_program()) {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedSolendProgram,
            "canonical action does not pin the audited mainnet Solend program",
        ));
    }
    if action.is_some_and(|action| action.input_mint != usdc_mint()) {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedInputMint,
            "canonical action input mint is not mainnet USDC",
        ));
    }

    let wall_clock_eligible = clocks.now_ms < funding.expires_at_ms;
    if !wall_clock_eligible {
        findings.push(CandidateFinding::new(
            FindingCode::WallClockExpired,
            "now_ms is at or beyond funding.expires_at_ms",
        ));
    }
    let slot_clock_eligible = clocks
        .current_confirmed_slot
        .map(|slot| slot < rule.expires_at_slot);
    match slot_clock_eligible {
        Some(false) => findings.push(CandidateFinding::new(
            FindingCode::SlotExpired,
            "current_confirmed_slot is at or beyond WatchRule.expires_at_slot",
        )),
        None => findings.push(CandidateFinding::new(
            FindingCode::ConfirmedSlotUnavailable,
            "confirmed slot is unavailable; candidate cannot advance to account reads",
        )),
        Some(true) => {}
    }

    let condition = action.and_then(|action| supported_condition(&rule, action, &mut findings));
    if condition.is_some_and(|condition| condition.threshold_bps != funding.threshold_bps) {
        findings.push(CandidateFinding::new(
            FindingCode::ConditionFundingMismatch,
            "funding threshold differs from the fingerprint-bound condition threshold",
        ));
    }
    if condition.is_some_and(|condition| {
        !(1..=MAX_SUPPORTED_THRESHOLD_BPS).contains(&condition.threshold_bps)
    }) {
        findings.push(CandidateFinding::new(
            FindingCode::ThresholdOutOfRange,
            "threshold_bps must be in the inclusive supported range 1..=10000",
        ));
    }
    let condition_report = condition.map(|condition| ConditionReport {
        comparison: comparison_label(condition.comparison).to_owned(),
        threshold_bps: condition.threshold_bps,
        threshold_wad: (u128::from(condition.threshold_bps) * BPS_WAD).to_string(),
        observed_apr_wad: None,
        observed_apr_floor_bps: None,
        met: None,
    });

    let report = CandidateReport {
        classification: primary_classification(&findings),
        intent_id: Some(funding.intent_id.clone()),
        rule_id_hex: Some(expected_rule_id_hex),
        findings,
        clocks: ClockReport {
            now_ms: clocks.now_ms,
            funding_expires_at_ms: Some(funding.expires_at_ms),
            wall_clock_eligible: Some(wall_clock_eligible),
            current_confirmed_slot: clocks.current_confirmed_slot,
            rule_expires_at_slot: Some(rule.expires_at_slot),
            slot_clock_eligible,
            reserve_last_update_slot: None,
            reserve_age_slots: None,
            max_reserve_staleness_slots: condition
                .map(|condition| condition.max_reserve_staleness_slots),
        },
        amounts: Some(AmountReport {
            action_input_amount_raw: action_amount.map(|amount| amount.to_string()),
            rule_max_input_amount_raw: Some(rule.max_input_amount_raw.to_string()),
            funding_amount_raw: Some(funding.amount_raw.to_string()),
            all_equal_and_nonzero: amounts_equal && funding.amount_raw != 0,
        }),
        condition: condition_report,
    };

    match (report.findings.is_empty(), action, condition) {
        (true, Some(action), Some(condition)) => PreflightOutcome::NeedsChain(PreparedDeposit {
            funding,
            delegated_wallet: rule.delegated_wallet,
            action,
            condition,
            report,
        }),
        _ => PreflightOutcome::Blocked(report),
    }
}

fn supported_condition(
    rule: &WatchRule,
    action: CanonicalDepositAction,
    findings: &mut Vec<CandidateFinding>,
) -> Option<SupportedRateCondition> {
    if rule.condition_logic != ConditionLogic::All || rule.conditions.len() != 1 {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedConditionShape,
            "dry-run supports exactly one ALL-combined Solend reserve-rate condition",
        ));
        return None;
    }

    let Condition::SolendReserveSupplyRate {
        reserve_pubkey,
        lending_market,
        solend_program_id,
        comparison,
        threshold_bps,
        rate_kind,
        formula_version,
        max_reserve_staleness_slots,
        required_refresh_same_tx,
    } = &rule.conditions[0]
    else {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedConditionShape,
            "dry-run supports only SolendReserveSupplyRate",
        ));
        return None;
    };

    if *reserve_pubkey != action.reserve_pubkey
        || *lending_market != action.lending_market
        || *solend_program_id != action.solend_program_id
    {
        findings.push(CandidateFinding::new(
            FindingCode::ConditionActionMismatch,
            "condition reserve, market, or program differs from the canonical action",
        ));
    }
    if *rate_kind != RateKind::Apr {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedRateKind,
            "only the native Solend APR condition is supported",
        ));
    }
    if *formula_version != SUPPORTED_FORMULA_VERSION {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedFormulaVersion,
            "condition formula_version is not the reviewed WAD formula v1",
        ));
    }
    if *max_reserve_staleness_slots != REQUIRED_MAX_RESERVE_STALENESS_SLOTS {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedConditionShape,
            "max_reserve_staleness_slots must equal the reviewed value 16",
        ));
    }
    if !*required_refresh_same_tx {
        findings.push(CandidateFinding::new(
            FindingCode::RefreshNotRequired,
            "canonical deposit must require RefreshReserve in the same transaction",
        ));
    }

    if findings.iter().any(|finding| {
        matches!(
            finding.code,
            FindingCode::UnsupportedConditionShape
                | FindingCode::UnsupportedFormulaVersion
                | FindingCode::UnsupportedRateKind
                | FindingCode::RefreshNotRequired
                | FindingCode::ConditionActionMismatch
        )
    }) {
        return None;
    }

    Some(SupportedRateCondition {
        comparison: *comparison,
        threshold_bps: *threshold_bps,
        max_reserve_staleness_slots: *max_reserve_staleness_slots,
    })
}

/// Validate decoded account identities, reserve freshness, and the condition.
///
/// `final_clocks` must be sampled after the chain reads. Rechecking both
/// deadlines here closes the preflight-to-plan time-of-check/time-of-use gap.
pub fn validate_chain(
    prepared: PreparedDeposit,
    facts: ChainFacts,
    final_clocks: ClockSnapshot,
) -> ChainOutcome {
    let mut report = prepared.report;
    let mut findings = report.findings.clone();
    let action = prepared.action;
    let wallet = prepared.delegated_wallet;
    let reserve = facts.reserve;

    let funding_expires_at_ms = report
        .clocks
        .funding_expires_at_ms
        .expect("prepared candidates always carry the funding deadline");
    let rule_expires_at_slot = report
        .clocks
        .rule_expires_at_slot
        .expect("prepared candidates always carry the rule deadline");
    let wall_clock_eligible = final_clocks.now_ms < funding_expires_at_ms;
    let slot_clock_eligible = final_clocks.current_confirmed_slot < rule_expires_at_slot;
    report.clocks.now_ms = final_clocks.now_ms;
    report.clocks.wall_clock_eligible = Some(wall_clock_eligible);
    report.clocks.current_confirmed_slot = Some(final_clocks.current_confirmed_slot);
    report.clocks.slot_clock_eligible = Some(slot_clock_eligible);
    if !wall_clock_eligible {
        findings.push(CandidateFinding::new(
            FindingCode::WallClockExpired,
            "final now_ms is at or beyond funding.expires_at_ms",
        ));
    }
    if !slot_clock_eligible {
        findings.push(CandidateFinding::new(
            FindingCode::SlotExpired,
            "final confirmed slot is at or beyond WatchRule.expires_at_slot",
        ));
    }

    if reserve.address != action.reserve_pubkey
        || reserve.account_owner != action.solend_program_id
        || reserve.lending_market != action.lending_market
        || reserve.liquidity_mint != action.input_mint
    {
        findings.push(CandidateFinding::new(
            FindingCode::ReserveIdentityMismatch,
            "decoded reserve identity differs from the fingerprint-bound action",
        ));
    }
    if reserve.liquidity_mint_decimals != REQUIRED_LIQUIDITY_MINT_DECIMALS {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedMintDecimals,
            "the reviewed USDC rail requires reserve liquidity decimals == 6",
        ));
    }

    if facts.obligation.address != action.target_obligation
        || facts.obligation.account_owner != action.solend_program_id
        || facts.obligation.lending_market != action.lending_market
        || facts.obligation.obligation_owner != wallet
    {
        findings.push(CandidateFinding::new(
            FindingCode::ObligationIdentityMismatch,
            "decoded obligation identity, market, or owner does not match the rule",
        ));
    }

    if facts.derived.source_liquidity_ata != prepared.funding.controlled_usdc_ata {
        findings.push(CandidateFinding::new(
            FindingCode::SourceAtaMismatch,
            "derived source ATA differs from the funding row",
        ));
    }
    if facts.derived.token_program != classic_token_program() {
        findings.push(CandidateFinding::new(
            FindingCode::UnsupportedTokenProgram,
            "derived token program is not the pinned classic SPL Token program",
        ));
    }
    match facts.source_liquidity {
        None => findings.push(CandidateFinding::new(
            FindingCode::AccountMissing,
            "source liquidity token account is missing",
        )),
        Some(source) => {
            if source.address != facts.derived.source_liquidity_ata {
                findings.push(CandidateFinding::new(
                    FindingCode::SourceAtaMismatch,
                    "source token account address differs from the derived ATA",
                ));
            }
            if source.account_owner != facts.derived.token_program
                || source.mint != action.input_mint
                || source.token_owner != wallet
                || !source.initialized
                || source.frozen
            {
                findings.push(CandidateFinding::new(
                    FindingCode::SourceTokenMismatch,
                    "source token account program, mint, authority, or state is invalid",
                ));
            }
            if source.amount_raw < action.input_amount_raw {
                findings.push(CandidateFinding::new(
                    FindingCode::SourceBalanceInsufficient,
                    "source token account balance is smaller than the canonical deposit amount",
                ));
            }
        }
    }

    match facts.collateral {
        None => findings.push(CandidateFinding::new(
            FindingCode::AtaMissing,
            "controlled-wallet collateral ATA does not exist; dry-run never creates it",
        )),
        Some(collateral) => {
            if collateral.address != facts.derived.collateral_ata {
                findings.push(CandidateFinding::new(
                    FindingCode::CollateralAtaMismatch,
                    "collateral token account address differs from the derived ATA",
                ));
            }
            if collateral.account_owner != facts.derived.token_program
                || collateral.mint != reserve.collateral_mint
                || collateral.token_owner != wallet
                || !collateral.initialized
                || collateral.frozen
            {
                findings.push(CandidateFinding::new(
                    FindingCode::CollateralTokenMismatch,
                    "collateral token account program, mint, authority, or state is invalid",
                ));
            }
        }
    }

    let reserve_age_slots = if reserve.last_update_slot <= final_clocks.current_confirmed_slot {
        Some(
            final_clocks
                .current_confirmed_slot
                .saturating_sub(reserve.last_update_slot),
        )
    } else {
        findings.push(CandidateFinding::new(
            FindingCode::ReserveSlotAhead,
            "reserve last_update_slot is newer than the confirmed-slot observation",
        ));
        None
    };
    if reserve.last_update_stale {
        findings.push(CandidateFinding::new(
            FindingCode::ReserveExplicitlyStale,
            "reserve LastUpdate stale flag is set",
        ));
    }
    if reserve_age_slots
        .is_some_and(|age| age > u64::from(prepared.condition.max_reserve_staleness_slots))
    {
        findings.push(CandidateFinding::new(
            FindingCode::ReserveTooOld,
            "reserve age exceeds max_reserve_staleness_slots",
        ));
    }

    let reserve_observation_valid = !findings.iter().any(|finding| {
        matches!(
            finding.code,
            FindingCode::ReserveIdentityMismatch
                | FindingCode::UnsupportedMintDecimals
                | FindingCode::ReserveExplicitlyStale
                | FindingCode::ReserveSlotAhead
                | FindingCode::ReserveTooOld
        )
    });
    let condition_met = reserve_observation_valid.then(|| {
        compare_wad(
            facts.native_supply_apr_wad,
            prepared.condition.comparison,
            prepared.condition.threshold_bps,
        )
    });
    if condition_met == Some(false) {
        findings.push(CandidateFinding::new(
            FindingCode::ConditionNotMet,
            "full-precision native supply APR does not satisfy the canonical condition",
        ));
    }

    report.findings = findings;
    report.clocks.reserve_last_update_slot = Some(reserve.last_update_slot);
    report.clocks.reserve_age_slots = reserve_age_slots;
    if let (Some(condition), Some(condition_met)) = (report.condition.as_mut(), condition_met) {
        condition.observed_apr_wad = Some(facts.native_supply_apr_wad.to_string());
        condition.observed_apr_floor_bps =
            Some((facts.native_supply_apr_wad / BPS_WAD).to_string());
        condition.met = Some(condition_met);
    }
    report.set_classification_from_findings();

    if !report.findings.is_empty() {
        return ChainOutcome::Blocked(report);
    }

    ChainOutcome::Ready(ValidatedSolendPlanInputs {
        solend_program_id: action.solend_program_id,
        input_mint: action.input_mint,
        input_amount_raw: action.input_amount_raw,
        source_liquidity: facts.derived.source_liquidity_ata,
        user_collateral: facts.derived.collateral_ata,
        reserve: action.reserve_pubkey,
        reserve_liquidity_supply: reserve.liquidity_supply,
        reserve_collateral_mint: reserve.collateral_mint,
        lending_market: action.lending_market,
        destination_deposit_collateral: reserve.collateral_supply,
        obligation: action.target_obligation,
        obligation_owner: wallet,
        pyth_oracle: reserve.pyth_oracle,
        switchboard_oracle: reserve.switchboard_oracle,
        user_transfer_authority: wallet,
        token_program: facts.derived.token_program,
        report,
    })
}

fn comparison_label(comparison: Comparison) -> &'static str {
    match comparison {
        Comparison::Lt => "lt",
        Comparison::Lte => "lte",
        Comparison::Gt => "gt",
        Comparison::Gte => "gte",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        facts::{DerivedAccounts, ObligationFacts, ReserveFacts, TokenAccountFacts},
        CandidateClassification,
    };
    use claw_types::stage2_watch_rule::{
        ActionSpec, ConditionLogic, WatchRule, STAGE2_WATCH_RULE_SCHEMA_V2,
    };

    fn key(byte: u8) -> PubkeyBytes {
        PubkeyBytes::new([byte; 32])
    }

    fn rule() -> WatchRule {
        WatchRule {
            schema_version: STAGE2_WATCH_RULE_SCHEMA_V2,
            rule_id: [0x22; 16],
            user: key(1),
            executor: key(3),
            delegated_wallet: key(3),
            created_at_slot: 100,
            expires_at_slot: 200,
            one_shot: true,
            condition_logic: ConditionLogic::All,
            conditions: vec![Condition::SolendReserveSupplyRate {
                reserve_pubkey: key(4),
                lending_market: key(5),
                solend_program_id: audited_solend_program(),
                comparison: Comparison::Gt,
                threshold_bps: 50,
                rate_kind: RateKind::Apr,
                formula_version: 1,
                max_reserve_staleness_slots: 16,
                required_refresh_same_tx: true,
            }],
            action: ActionSpec::SolendDeposit {
                target_obligation: key(7),
                reserve_pubkey: key(4),
                lending_market: key(5),
                solend_program_id: audited_solend_program(),
                input_mint: usdc_mint(),
                input_amount_raw: 100_000,
            },
            max_input_amount_raw: 100_000,
            used_amount_raw: 0,
            destination: key(3),
            slippage_bps: 0,
        }
    }

    fn funding(rule: &WatchRule) -> FundingSnapshot {
        let id = hex::encode(rule.rule_id);
        FundingSnapshot {
            intent_id: id.clone(),
            rule_id_hex: id,
            canonical_rule_hash_hex: hex::encode(canonical_rule_hash(rule)),
            controlled_wallet: rule.delegated_wallet,
            controlled_usdc_ata: key(9),
            amount_raw: 100_000,
            threshold_bps: 50,
            expires_at_ms: 2_000,
        }
    }

    fn clocks() -> ClockSnapshot {
        ClockSnapshot {
            now_ms: 1_000,
            current_confirmed_slot: 150,
        }
    }

    fn prepared() -> PreparedDeposit {
        let rule = rule();
        match preflight_candidate(
            CandidateInput {
                funding: Some(funding(&rule)),
                rule: Some(rule),
            },
            clocks(),
        ) {
            PreflightOutcome::NeedsChain(prepared) => prepared,
            other => panic!("expected prepared candidate, got {other:?}"),
        }
    }

    fn chain_facts(reserve_age: u64) -> ChainFacts {
        let prepared = prepared();
        let read = prepared.read_request();
        let token_program = classic_token_program();
        let collateral_ata = key(11);
        ChainFacts {
            reserve: ReserveFacts {
                address: read.reserve_pubkey,
                account_owner: audited_solend_program(),
                last_update_slot: clocks().current_confirmed_slot - reserve_age,
                last_update_stale: false,
                lending_market: key(5),
                liquidity_mint: read.input_mint,
                liquidity_mint_decimals: 6,
                liquidity_supply: key(12),
                pyth_oracle: key(13),
                switchboard_oracle: key(14),
                collateral_mint: key(15),
                collateral_supply: key(16),
            },
            obligation: ObligationFacts {
                address: read.target_obligation,
                account_owner: audited_solend_program(),
                lending_market: key(5),
                obligation_owner: read.delegated_wallet,
            },
            source_liquidity: Some(TokenAccountFacts {
                address: read.source_liquidity_ata,
                account_owner: token_program,
                mint: read.input_mint,
                token_owner: read.delegated_wallet,
                amount_raw: 100_000,
                initialized: true,
                frozen: false,
            }),
            collateral: Some(TokenAccountFacts {
                address: collateral_ata,
                account_owner: token_program,
                mint: key(15),
                token_owner: read.delegated_wallet,
                amount_raw: 0,
                initialized: true,
                frozen: false,
            }),
            derived: DerivedAccounts {
                token_program,
                source_liquidity_ata: read.source_liquidity_ata,
                collateral_ata,
            },
            native_supply_apr_wad: 50 * BPS_WAD + 1,
        }
    }

    fn blocked_report(outcome: PreflightOutcome) -> CandidateReport {
        match outcome {
            PreflightOutcome::Blocked(report) => report,
            PreflightOutcome::NeedsChain(_) => panic!("expected blocker"),
        }
    }

    #[test]
    fn both_deadline_endpoints_fail_closed() {
        let rule = rule();
        let funding = funding(&rule);

        let wall = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(funding.clone()),
                rule: Some(rule.clone()),
            },
            ClockSnapshot {
                now_ms: funding.expires_at_ms,
                current_confirmed_slot: rule.expires_at_slot - 1,
            },
        ));
        assert_eq!(
            wall.classification,
            CandidateClassification::WallClockExpired
        );

        let slot = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(funding.clone()),
                rule: Some(rule.clone()),
            },
            ClockSnapshot {
                now_ms: funding.expires_at_ms - 1,
                current_confirmed_slot: rule.expires_at_slot,
            },
        ));
        assert_eq!(slot.classification, CandidateClassification::SlotExpired);

        assert!(matches!(
            preflight_candidate(
                CandidateInput {
                    funding: Some(funding),
                    rule: Some(rule),
                },
                ClockSnapshot {
                    now_ms: 1_999,
                    current_confirmed_slot: 199,
                },
            ),
            PreflightOutcome::NeedsChain(_)
        ));
    }

    #[test]
    fn unsupported_legacy_action_retains_both_expiry_findings() {
        let mut legacy = rule();
        legacy.action = ActionSpec::SolendWithdrawAllDelegated {
            target_obligation: key(7),
            reserve_pubkey: key(4),
            lending_market: key(5),
            destination_wallet: key(3),
            withdraw_mode:
                claw_types::stage2_watch_rule::WithdrawMode::WithdrawAllDelegatedPosition,
        };
        legacy.expires_at_slot = clocks().current_confirmed_slot;
        let mut funding = funding(&legacy);
        funding.expires_at_ms = 900;
        let report = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(funding),
                rule: Some(legacy),
            },
            clocks(),
        ));
        assert_eq!(
            report.classification,
            CandidateClassification::UnsupportedAction
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::WallClockExpired));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::SlotExpired));
        assert!(
            report
                .findings
                .iter()
                .all(|finding| !matches!(
                    finding.code,
                    FindingCode::AmountMismatch | FindingCode::AmountZero
                )),
            "a legacy action has no comparable deposit amount and must not gain a fabricated mismatch"
        );
    }

    #[test]
    fn exact_lowercase_hash_and_three_way_amount_are_enforced() {
        let rule = rule();
        let mut wrong_hash = funding(&rule);
        wrong_hash.canonical_rule_hash_hex =
            wrong_hash.canonical_rule_hash_hex.to_ascii_uppercase();
        let report = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(wrong_hash),
                rule: Some(rule.clone()),
            },
            clocks(),
        ));
        assert_eq!(report.classification, CandidateClassification::HashMismatch);

        let mut wrong_amount = funding(&rule);
        wrong_amount.amount_raw += 1;
        let report = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(wrong_amount),
                rule: Some(rule),
            },
            clocks(),
        ));
        assert_eq!(
            report.classification,
            CandidateClassification::AmountMismatch
        );
    }

    #[test]
    fn condition_action_and_funding_identity_must_match() {
        let mut mismatched_rule = rule();
        let Condition::SolendReserveSupplyRate { reserve_pubkey, .. } =
            &mut mismatched_rule.conditions[0]
        else {
            unreachable!("fixture condition is Solend");
        };
        *reserve_pubkey = key(99);
        let report = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(funding(&mismatched_rule)),
                rule: Some(mismatched_rule),
            },
            clocks(),
        ));
        assert_eq!(
            report.classification,
            CandidateClassification::IdentityMismatch
        );

        let rule = rule();
        let mut mismatched_funding = funding(&rule);
        mismatched_funding.threshold_bps += 1;
        let report = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(mismatched_funding),
                rule: Some(rule),
            },
            clocks(),
        ));
        assert_eq!(
            report.classification,
            CandidateClassification::IdentityMismatch
        );
    }

    #[test]
    fn both_orphan_shapes_are_explicit() {
        let rule = rule();
        let funding = funding(&rule);
        let funding_only = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(funding),
                rule: None,
            },
            clocks(),
        ));
        assert_eq!(
            funding_only.classification,
            CandidateClassification::OrphanFundingOnly
        );

        let rule_only = blocked_report(preflight_candidate(
            CandidateInput {
                funding: None,
                rule: Some(rule),
            },
            clocks(),
        ));
        assert_eq!(
            rule_only.classification,
            CandidateClassification::OrphanRuleOnly
        );
    }

    #[test]
    fn reserve_freshness_accepts_16_and_rejects_17_or_future() {
        assert!(matches!(
            validate_chain(prepared(), chain_facts(16), clocks()),
            ChainOutcome::Ready(_)
        ));

        let ChainOutcome::Blocked(old) = validate_chain(prepared(), chain_facts(17), clocks())
        else {
            panic!("17-slot-old reserve must fail");
        };
        assert_eq!(old.classification, CandidateClassification::ReserveStale);

        let mut future = chain_facts(0);
        future.reserve.last_update_slot = clocks().current_confirmed_slot + 1;
        let ChainOutcome::Blocked(future) = validate_chain(prepared(), future, clocks()) else {
            panic!("future reserve update must fail");
        };
        assert_eq!(future.classification, CandidateClassification::ReserveStale);
    }

    #[test]
    fn final_deadline_samples_close_the_rpc_read_to_plan_gap() {
        let facts = chain_facts(0);
        let ChainOutcome::Blocked(wall) = validate_chain(
            prepared(),
            facts.clone(),
            ClockSnapshot {
                now_ms: 2_000,
                current_confirmed_slot: 199,
            },
        ) else {
            panic!("reaching the wall-clock endpoint after RPC reads must block");
        };
        assert_eq!(
            wall.classification,
            CandidateClassification::WallClockExpired
        );
        assert!(wall
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::WallClockExpired));

        let ChainOutcome::Blocked(slot) = validate_chain(
            prepared(),
            facts,
            ClockSnapshot {
                now_ms: 1_999,
                current_confirmed_slot: 200,
            },
        ) else {
            panic!("reaching the slot endpoint after RPC reads must block");
        };
        assert_eq!(slot.classification, CandidateClassification::SlotExpired);
        assert!(slot
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::SlotExpired));
    }

    #[test]
    fn reviewed_program_mint_token_program_and_source_balance_are_pinned() {
        let mut wrong_program_rule = rule();
        let ActionSpec::SolendDeposit {
            solend_program_id, ..
        } = &mut wrong_program_rule.action
        else {
            unreachable!("fixture action is SolendDeposit");
        };
        *solend_program_id = key(99);
        let Condition::SolendReserveSupplyRate {
            solend_program_id, ..
        } = &mut wrong_program_rule.conditions[0]
        else {
            unreachable!("fixture condition is Solend");
        };
        *solend_program_id = key(99);
        let report = blocked_report(preflight_candidate(
            CandidateInput {
                funding: Some(funding(&wrong_program_rule)),
                rule: Some(wrong_program_rule),
            },
            clocks(),
        ));
        assert_eq!(
            report.classification,
            CandidateClassification::UnsupportedRuleEnvelope
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::UnsupportedSolendProgram));

        let mut wrong_token_program = chain_facts(0);
        wrong_token_program.derived.token_program = key(98);
        let ChainOutcome::Blocked(report) =
            validate_chain(prepared(), wrong_token_program, clocks())
        else {
            panic!("non-Tokenkeg adapter facts must block");
        };
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::UnsupportedTokenProgram));

        let mut insolvent = chain_facts(0);
        insolvent
            .source_liquidity
            .as_mut()
            .expect("source fixture")
            .amount_raw = 99_999;
        let ChainOutcome::Blocked(report) = validate_chain(prepared(), insolvent, clocks()) else {
            panic!("insufficient source balance must block");
        };
        assert_eq!(
            report.classification,
            CandidateClassification::SourceBalanceInsufficient
        );
    }

    #[test]
    fn invalid_reserve_does_not_publish_a_condition_decision() {
        let mut stale = chain_facts(0);
        stale.reserve.last_update_stale = true;
        let ChainOutcome::Blocked(report) = validate_chain(prepared(), stale, clocks()) else {
            panic!("stale reserve must block");
        };
        let condition = report.condition.expect("condition report");
        assert_eq!(condition.met, None);
        assert_eq!(condition.observed_apr_wad, None);
        assert_eq!(condition.observed_apr_floor_bps, None);
    }

    #[test]
    fn noncanonical_rule_envelope_is_rejected_before_chain_reads() {
        for mutation in 0..4 {
            let mut invalid = rule();
            match mutation {
                0 => invalid.schema_version = 1,
                1 => invalid.one_shot = false,
                2 => invalid.used_amount_raw = 1,
                3 => invalid.destination = key(99),
                _ => unreachable!(),
            }
            let report = blocked_report(preflight_candidate(
                CandidateInput {
                    funding: Some(funding(&invalid)),
                    rule: Some(invalid),
                },
                clocks(),
            ));
            assert_eq!(
                report.classification,
                CandidateClassification::UnsupportedRuleEnvelope
            );
        }
    }

    #[test]
    fn explicit_stale_and_missing_collateral_fail_closed() {
        let mut stale = chain_facts(0);
        stale.reserve.last_update_stale = true;
        let ChainOutcome::Blocked(stale) = validate_chain(prepared(), stale, clocks()) else {
            panic!("stale bit must fail");
        };
        assert_eq!(stale.classification, CandidateClassification::ReserveStale);

        let mut missing = chain_facts(0);
        missing.collateral = None;
        let ChainOutcome::Blocked(missing) = validate_chain(prepared(), missing, clocks()) else {
            panic!("missing collateral ATA must fail");
        };
        assert_eq!(missing.classification, CandidateClassification::AtaMissing);
    }

    #[test]
    fn ready_plan_uses_only_validated_action_and_account_facts() {
        let facts = chain_facts(0);
        let expected_reserve = facts.reserve;
        let expected_derived = facts.derived;
        let ChainOutcome::Ready(plan) = validate_chain(prepared(), facts, clocks()) else {
            panic!("valid facts must produce a plan");
        };
        assert_eq!(plan.input_amount_raw, 100_000);
        assert_eq!(plan.solend_program_id, audited_solend_program());
        assert_eq!(plan.reserve, key(4));
        assert_eq!(plan.obligation, key(7));
        assert_eq!(plan.lending_market, key(5));
        assert_eq!(plan.obligation_owner, key(3));
        assert_eq!(plan.user_transfer_authority, key(3));
        assert_eq!(plan.token_program, classic_token_program());
        assert_eq!(plan.source_liquidity, expected_derived.source_liquidity_ata);
        assert_eq!(plan.user_collateral, expected_derived.collateral_ata);
        assert_eq!(
            plan.reserve_liquidity_supply,
            expected_reserve.liquidity_supply
        );
        assert_eq!(
            plan.destination_deposit_collateral,
            expected_reserve.collateral_supply
        );
        assert_eq!(
            plan.reserve_collateral_mint,
            expected_reserve.collateral_mint
        );
        assert_eq!(plan.pyth_oracle, expected_reserve.pyth_oracle);
        assert_eq!(plan.switchboard_oracle, expected_reserve.switchboard_oracle);
        assert_eq!(plan.report.classification, CandidateClassification::Ready);
        assert_eq!(
            plan.report
                .condition
                .as_ref()
                .and_then(|condition| condition.met),
            Some(true)
        );
    }
}
