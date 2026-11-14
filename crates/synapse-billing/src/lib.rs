#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

mod circuit;
pub mod client;
pub mod error;
pub mod recorder;
pub mod spend;
pub mod types;

pub use client::{AetherClient, round_up_to_ledger_precision};
pub use error::BillingError;
pub use recorder::{MeterKeys, UsageEvent, UsageRecorder};
pub use spend::{CREDITS_PER_USD, SpendCharge, SpendCheck, SpendPolicy, SpendTarget, usd_to_credits};
pub use types::{CreditCheckResponse, CreditDeductRequest, CreditDeductResponse};
