//! Pure protocol-specific types, decoders, and unsigned builders.
//!
//! This crate contains deterministic protocol logic only. Network access,
//! persistence, approval state, signing, and transaction submission belong in
//! higher layers.

#![forbid(unsafe_code)]

pub mod jupiter;
pub mod solend;
