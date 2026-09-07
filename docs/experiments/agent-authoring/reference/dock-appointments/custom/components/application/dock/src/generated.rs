//! Migration-IR projections materialized in the dock-appointments package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `appointment.book` transaction accessors.
    pub(crate) mod appointment_book {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/dock/generated/wamn/appointment_book.rs"
        ));
    }

    /// Generated `appointment.check_in` transaction accessors.
    pub(crate) mod appointment_check_in {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/dock/generated/wamn/appointment_check_in.rs"
        ));
    }

    /// Generated `appointment.query` projection.
    pub(crate) mod appointment_query {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/dock/generated/wamn/appointment_query.rs"
        ));
    }

    /// Generated `carrier.create` transaction accessors.
    pub(crate) mod carrier_create {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/dock/generated/wamn/carrier_create.rs"
        ));
    }

    /// Generated `dock.create` transaction accessors.
    pub(crate) mod dock_create {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/dock/generated/wamn/dock_create.rs"
        ));
    }
}
