use serde::{Deserialize, Serialize};

/// Stable primary disposition for one dry-run candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateClassification {
    Ready,
    OrphanFundingOnly,
    OrphanRuleOnly,
    EmptyCandidate,
    UnsupportedAction,
    UnsupportedRuleEnvelope,
    UnsupportedCondition,
    HashMismatch,
    IdentityMismatch,
    AmountMismatch,
    WallClockExpired,
    SlotExpired,
    ClockUnavailable,
    ReserveStale,
    AtaMissing,
    AccountMissing,
    AccountMismatch,
    SourceBalanceInsufficient,
    ConditionNotMet,
}

/// Machine-stable reason code. Reports retain all simultaneous findings even
/// though [`CandidateReport::classification`] selects one primary disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCode {
    OrphanFundingOnly,
    OrphanRuleOnly,
    EmptyCandidate,
    UnsupportedAction,
    UnsupportedSchemaVersion,
    MultiUseRule,
    UsedAmountNonZero,
    ThresholdOutOfRange,
    ControlledWalletEnvelopeMismatch,
    UnsupportedConditionShape,
    UnsupportedFormulaVersion,
    UnsupportedRateKind,
    RefreshNotRequired,
    ConditionActionMismatch,
    ConditionFundingMismatch,
    ControlledWalletMismatch,
    RuleIdMismatch,
    CanonicalHashMismatch,
    AmountMismatch,
    AmountZero,
    WallClockExpired,
    SlotExpired,
    ConfirmedSlotUnavailable,
    ReserveExplicitlyStale,
    ReserveSlotAhead,
    ReserveTooOld,
    AccountMissing,
    AtaMissing,
    ReserveIdentityMismatch,
    ObligationIdentityMismatch,
    SourceAtaMismatch,
    SourceTokenMismatch,
    SourceBalanceInsufficient,
    CollateralAtaMismatch,
    CollateralTokenMismatch,
    UnsupportedMintDecimals,
    UnsupportedSolendProgram,
    UnsupportedInputMint,
    UnsupportedTokenProgram,
    ConditionNotMet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateFinding {
    pub code: FindingCode,
    pub detail: String,
}

impl CandidateFinding {
    pub(crate) fn new(code: FindingCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockReport {
    pub now_ms: i64,
    pub funding_expires_at_ms: Option<i64>,
    pub wall_clock_eligible: Option<bool>,
    pub current_confirmed_slot: Option<u64>,
    pub rule_expires_at_slot: Option<u64>,
    pub slot_clock_eligible: Option<bool>,
    pub reserve_last_update_slot: Option<u64>,
    pub reserve_age_slots: Option<u64>,
    pub max_reserve_staleness_slots: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmountReport {
    pub action_input_amount_raw: Option<String>,
    pub rule_max_input_amount_raw: Option<String>,
    pub funding_amount_raw: Option<String>,
    pub all_equal_and_nonzero: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionReport {
    pub comparison: String,
    pub threshold_bps: u32,
    pub threshold_wad: String,
    pub observed_apr_wad: Option<String>,
    pub observed_apr_floor_bps: Option<String>,
    pub met: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReport {
    pub classification: CandidateClassification,
    pub intent_id: Option<String>,
    pub rule_id_hex: Option<String>,
    pub findings: Vec<CandidateFinding>,
    pub clocks: ClockReport,
    pub amounts: Option<AmountReport>,
    pub condition: Option<ConditionReport>,
}

impl CandidateReport {
    pub(crate) fn bare(
        classification: CandidateClassification,
        findings: Vec<CandidateFinding>,
        now_ms: i64,
        current_confirmed_slot: Option<u64>,
    ) -> Self {
        Self {
            classification,
            intent_id: None,
            rule_id_hex: None,
            findings,
            clocks: ClockReport {
                now_ms,
                funding_expires_at_ms: None,
                wall_clock_eligible: None,
                current_confirmed_slot,
                rule_expires_at_slot: None,
                slot_clock_eligible: None,
                reserve_last_update_slot: None,
                reserve_age_slots: None,
                max_reserve_staleness_slots: None,
            },
            amounts: None,
            condition: None,
        }
    }

    pub(crate) fn set_classification_from_findings(&mut self) {
        self.classification = primary_classification(&self.findings);
    }
}

fn has(findings: &[CandidateFinding], needle: FindingCode) -> bool {
    findings.iter().any(|finding| finding.code == needle)
}

pub(crate) fn primary_classification(findings: &[CandidateFinding]) -> CandidateClassification {
    use CandidateClassification as Class;
    use FindingCode as Code;

    if has(findings, Code::OrphanFundingOnly) {
        Class::OrphanFundingOnly
    } else if has(findings, Code::OrphanRuleOnly) {
        Class::OrphanRuleOnly
    } else if has(findings, Code::EmptyCandidate) {
        Class::EmptyCandidate
    } else if has(findings, Code::UnsupportedAction) {
        Class::UnsupportedAction
    } else if findings.iter().any(|finding| {
        matches!(
            finding.code,
            Code::UnsupportedSchemaVersion
                | Code::MultiUseRule
                | Code::UsedAmountNonZero
                | Code::ThresholdOutOfRange
                | Code::ControlledWalletEnvelopeMismatch
                | Code::UnsupportedSolendProgram
                | Code::UnsupportedInputMint
        )
    }) {
        Class::UnsupportedRuleEnvelope
    } else if findings.iter().any(|finding| {
        matches!(
            finding.code,
            Code::UnsupportedConditionShape
                | Code::UnsupportedFormulaVersion
                | Code::UnsupportedRateKind
                | Code::RefreshNotRequired
        )
    }) {
        Class::UnsupportedCondition
    } else if has(findings, Code::CanonicalHashMismatch) {
        Class::HashMismatch
    } else if findings.iter().any(|finding| {
        matches!(
            finding.code,
            Code::RuleIdMismatch
                | Code::ConditionActionMismatch
                | Code::ConditionFundingMismatch
                | Code::ControlledWalletMismatch
        )
    }) {
        Class::IdentityMismatch
    } else if findings
        .iter()
        .any(|finding| matches!(finding.code, Code::AmountMismatch | Code::AmountZero))
    {
        Class::AmountMismatch
    } else if has(findings, Code::WallClockExpired) {
        Class::WallClockExpired
    } else if has(findings, Code::SlotExpired) {
        Class::SlotExpired
    } else if has(findings, Code::ConfirmedSlotUnavailable) {
        Class::ClockUnavailable
    } else if findings.iter().any(|finding| {
        matches!(
            finding.code,
            Code::ReserveExplicitlyStale | Code::ReserveSlotAhead | Code::ReserveTooOld
        )
    }) {
        Class::ReserveStale
    } else if has(findings, Code::AtaMissing) {
        Class::AtaMissing
    } else if has(findings, Code::AccountMissing) {
        Class::AccountMissing
    } else if findings.iter().any(|finding| {
        matches!(
            finding.code,
            Code::ReserveIdentityMismatch
                | Code::ObligationIdentityMismatch
                | Code::SourceAtaMismatch
                | Code::SourceTokenMismatch
                | Code::CollateralAtaMismatch
                | Code::CollateralTokenMismatch
                | Code::UnsupportedMintDecimals
                | Code::UnsupportedTokenProgram
        )
    }) {
        Class::AccountMismatch
    } else if has(findings, Code::SourceBalanceInsufficient) {
        Class::SourceBalanceInsufficient
    } else if has(findings, Code::ConditionNotMet) {
        Class::ConditionNotMet
    } else {
        Class::Ready
    }
}
