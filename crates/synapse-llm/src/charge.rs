//! Charge ids reported back to the caller of a non-streamed request
//!
//! Every wallet debit the gateway settles for a request is keyed by a fresh
//! idempotency key. Reporting those keys lets a product refund the charges of
//! a call it could not use (Aether's `credits/.../reverse` route undoes one
//! debit, at most once, by its key). A request may settle more than one debit
//! (a cascade bills each attempt), so the ledger keeps them all

use std::sync::{Arc, Mutex};

/// Response header listing a non-streamed request's wallet debits
///
/// Comma-separated idempotency keys, in settlement order. Absent when nothing
/// was debited (BYOK, a zero cost, an included-allowance product, or a
/// settlement that failed)
pub const CHARGE_ID_HEADER: &str = "x-synapse-charge-id";

/// The wallet debits settled for one request, installed on its extensions by
/// the handler of a non-streamed request
#[derive(Debug, Clone, Default)]
pub struct ChargeLedger(Arc<Mutex<Vec<String>>>);

impl ChargeLedger {
    /// Record a settled wallet debit by its idempotency key
    pub fn record(&self, charge_id: String) {
        if let Ok(mut ids) = self.0.lock() {
            ids.push(charge_id);
        }
    }

    /// The recorded debits, in settlement order
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.0.lock().map(|ids| ids.clone()).unwrap_or_default()
    }
}

/// Record a settled wallet debit on the request's ledger, if it has one
pub fn record_charge(extensions: &http::Extensions, charge_id: &str) {
    if let Some(ledger) = extensions.get::<ChargeLedger>() {
        ledger.record(charge_id.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_every_debit_in_order() {
        let ledger = ChargeLedger::default();
        let mut extensions = http::Extensions::new();
        extensions.insert(ledger.clone());

        record_charge(&extensions, "charge-1");
        record_charge(&extensions, "charge-2");

        assert_eq!(ledger.ids(), vec!["charge-1".to_owned(), "charge-2".to_owned()]);
    }

    #[test]
    fn a_request_without_a_ledger_records_nothing() {
        record_charge(&http::Extensions::new(), "charge-1");
    }
}
