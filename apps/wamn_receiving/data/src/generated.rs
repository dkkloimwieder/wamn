//! Migration-IR projections materialized in the Receiving package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `location.list` projection.
    pub(crate) mod location_list {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/location_list.rs"
        ));
    }

    /// Generated `purchase_order` projection and statement digests.
    pub(crate) mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/purchase_order.rs"
        ));
    }

    /// Generated `supplier` projection and claim accessors.
    ///
    /// A claim carries `select_participant` for a nested operation. No
    /// operation nests inside `supplier.create`, so that accessor stays unused.
    #[expect(
        dead_code,
        reason = "the generated claim offers participation this create does not use"
    )]
    pub(crate) mod supplier {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/supplier.rs"
        ));
    }

    /// Generated `receipt` projection and statement digests.
    pub(crate) mod receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/receipt.rs"
        ));
    }

    /// Generated `receiving.record_receipt` transaction accessors.
    pub(crate) mod receiving_record_receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/receiving_record_receipt.rs"
        ));
    }

    /// Generated `receiving.load_receipt_screen` projection.
    pub(crate) mod receiving_load_receipt_screen {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/receiving_load_receipt_screen.rs"
        ));
    }

    /// Generated `receiving.load_purchase_order_history` projection.
    pub(crate) mod receiving_load_purchase_order_history {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/receiving_load_purchase_order_history.rs"
        ));
    }
}
