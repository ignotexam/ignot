//! Exams for AI models that are sealed until a public clock opens them.
//!
//! An exam is generated from a seed, sealed to a future round of the drand randomness beacon, opened with that
//! round's signature, and graded mechanically. Every step here is a pure function of its inputs: this crate
//! reads no clock, no file and no network, so the same code runs in a command line and in a browser, and
//! gives the same bytes in both.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod agefile;
pub mod budget;
pub mod chain;
pub mod date;
pub mod drand;
pub mod exam;
pub mod generate;
pub mod grade;
pub mod hash;
pub mod merkle;
pub mod results;
pub mod rng;
pub mod seal;
pub mod timelock;
pub mod verify;

/// The project's name, written into every manifest.
pub const NAME: &str = "ignot";

/// Bumped whenever a generator would give different questions for the same seed.
pub const GENERATOR_VERSION: &str = "1";

/// Everything that can go wrong in this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A value handed in from outside does not have the shape it must have.
    #[error("{0}")]
    Invalid(String),
    /// JSON that could not be read or written.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The sealed data is not something this crate can open.
    #[error("sealed data: {0}")]
    Sealed(String),
    /// The beacon signature is not the drand signature of the round.
    #[error("the signature is not drand's signature of round {0}")]
    Beacon(u64),
}

/// This crate's result type.
pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(message.into()))
}
