//! Migration-IR projections materialized in the Receiving package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `location.list` projection.
    pub(crate) mod location_list {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/location_list.rs"
        ));
    }

    /// Generated `purchase_order` projection and statement digests.
    pub(crate) mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/purchase_order.rs"
        ));
    }

    // `4862faee` taught the generator to emit a fourth slice. Nothing consumes
    // it: `AllowedConstraints` carries three fields, and giving it an
    // `exclusion` one changes the classifier and its cases, which is its own
    // change (wamn-10yt.66). The other three reach `purchase_order.rs`.
    const _: &[&str] = purchase_order::UPDATE_EXCLUSION_CONSTRAINTS;

    /// Generated `receipt` projection and statement digests.
    pub(crate) mod receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/receipt.rs"
        ));
    }

    /// Generated `receiving.record_receipt` transaction accessors.
    pub(crate) mod receiving_record_receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/receiving_record_receipt.rs"
        ));
    }

    /// Generated `receiving.load_receipt_screen` projection.
    pub(crate) mod receiving_load_receipt_screen {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/receiving_load_receipt_screen.rs"
        ));
    }
}
