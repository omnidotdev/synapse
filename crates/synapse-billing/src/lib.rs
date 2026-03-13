#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

mod circuit;
pub mod client;
pub mod error;
pub mod recorder;
pub mod types;

pub use client::AetherClient;
pub use error::BillingError;
pub use recorder::{MeterKeys, UsageEvent, UsageRecorder};
pub use types::{CreditCheckResponse, CreditDeductRequest, CreditDeductResponse};
