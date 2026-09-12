//! The closed profile set.
//!
//! Two profiles describe WMS traffic. Their deterministic byte contract is in
//! `docs/testing/deterministic.md#event-traffic`.
//! The WMS package did not exist when these profiles were introduced.
//! Their stream tests do not establish application execution.
//! A third profile needs an application requirement.

use serde_json::{Value, json};

use crate::Lcg;

/// Which event shape a stream carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileKind {
    /// Handheld scanner traffic: a scan of a pallet at a location.
    ScanEvents,
    /// Opening inventory: a quantity of a product at a location.
    SeedInventory,
}

impl ProfileKind {
    /// The wire name carried on every [`Event::kind`](crate::Event::kind).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScanEvents => "scan_event",
            Self::SeedInventory => "seed_inventory",
        }
    }

    /// The payload for one event at `sequence`, drawn from `lcg`.
    pub(crate) fn body(self, lcg: &mut Lcg, sequence: u64) -> Value {
        match self {
            Self::ScanEvents => json!({
                "pallet_id": format!("PAL-{:06}", lcg.below(500_000)),
                "location_id": format!("LOC-{:04}", lcg.below(2_000)),
                "scanner_id": format!("HH-{:03}", lcg.below(120)),
                "scan_sequence": sequence,
            }),
            Self::SeedInventory => json!({
                "product_id": format!("SKU-{:05}", lcg.below(20_000)),
                "location_id": format!("LOC-{:04}", lcg.below(2_000)),
                "quantity": lcg.below(480) + 1,
                "status": if lcg.below(10) == 0 { "held" } else { "available" },
            }),
        }
    }
}
