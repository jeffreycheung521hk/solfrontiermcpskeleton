//! Pure, SDK-free validation for the Phase 3b dry-run executor.
//!
//! This crate deliberately has no database, RPC, Solana SDK, protocol builder,
//! environment, keypair, signer, or submission dependency. The binary adapter
//! reads immutable facts and builds a deliberately unsubmitable unsigned
//! Solana transaction only after this crate returns
//! [`ValidatedSolendPlanInputs`].

#![forbid(unsafe_code)]

mod candidate;
mod condition;
mod facts;
mod report;

pub use candidate::{
    preflight_candidate, validate_chain, CandidateInput, ChainOutcome, FundingSnapshot,
    PlanReadRequest, PreflightOutcome, PreparedDeposit, ValidatedSolendPlanInputs,
};
pub use condition::{compare_wad, BPS_WAD};
pub use facts::{
    ChainFacts, ClockSnapshot, DerivedAccounts, ObligationFacts, PreflightClockSnapshot,
    ReserveFacts, TokenAccountFacts,
};
pub use report::{
    AmountReport, CandidateClassification, CandidateFinding, CandidateReport, ClockReport,
    ConditionReport, FindingCode,
};
