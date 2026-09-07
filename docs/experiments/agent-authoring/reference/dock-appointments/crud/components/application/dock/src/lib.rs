//! One package-grain component exporting every dock-appointment operation.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`]
//! accessors and the generated Wamn projections; this crate authors no SQL.
//! What is authored here is the ORDER those statements run in and what each
//! refusal means.

mod appointment_book;
mod appointment_check_in;
mod appointment_query;
mod carrier_create;
mod dock_create;
mod error;
mod generated;
pub mod operation;
mod scalar;

pub use error::{AccessError, AccessErrorKind};

/// The five exported operations, bound to the world this component ships.
///
/// The bindings are wasm-only. `wit-bindgen` names an export
/// `wamn-dock:appointment/book@1.0.0#run`, which is a legal Component Model
/// extern name and not a legal ELF one, so a host `cdylib` cannot link them.
/// Gating them here keeps `cargo test` on the host working without changing
/// one byte of the guest.
#[cfg(target_arch = "wasm32")]
#[expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]
mod guest {
    use exports::wamn_dock::appointment::book::Guest as AppointmentBook;
    use exports::wamn_dock::appointment::check_in::Guest as AppointmentCheckIn;
    use exports::wamn_dock::appointment::query::Guest as AppointmentQuery;
    use exports::wamn_dock::carrier::create::Guest as CarrierCreate;
    use exports::wamn_dock::dock::create::Guest as DockCreate;
    use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};

    wit_bindgen::generate!({
        world: "wamn-dock:component/dock@1.0.0",
        inline: r#"
            package wamn-dock:component@1.0.0;

            world dock {
              import wamn:postgres/types@0.1.0;
              import wamn:postgres/statements@0.1.0;
              export wamn-dock:carrier/create@1.0.0;
              export wamn-dock:dock/create@1.0.0;
              export wamn-dock:appointment/book@1.0.0;
              export wamn-dock:appointment/check-in@1.0.0;
              export wamn-dock:appointment/query@1.0.0;
            }
        "#,
        path: [
            "../../data/receiving-data/wit/deps/wamn-node",
            "../../data/receiving-data/wit/deps/wamn-postgres",
            "wit/deps/wamn-dock-appointment",
            "wit/deps/wamn-dock-carrier",
            "wit/deps/wamn-dock-dock",
        ],
        generate_all,
    });

    struct Component;

    fn invoke_operation<F>(operation: F) -> Result<Emission, NodeError>
    where
        F: Future<Output = Result<String, crate::operation::InvocationError>>,
    {
        futures_executor::block_on(operation)
            .map(|payload| Emission {
                payload,
                port: None,
            })
            .map_err(|error| {
                NodeError::InvalidInput(ErrorDetail {
                    message: error.context().to_owned(),
                    code: Some(error.code().to_owned()),
                })
            })
    }

    impl CarrierCreate for Component {
        fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
            invoke_operation(crate::operation::carrier_create_operation(&input))
        }
    }

    impl DockCreate for Component {
        fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
            invoke_operation(crate::operation::dock_create_operation(&input))
        }
    }

    impl AppointmentBook for Component {
        fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
            invoke_operation(crate::operation::appointment_book_operation(&input))
        }
    }

    impl AppointmentCheckIn for Component {
        fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
            invoke_operation(crate::operation::appointment_check_in_operation(&input))
        }
    }

    impl AppointmentQuery for Component {
        fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
            invoke_operation(crate::operation::appointment_query_operation(&input))
        }
    }

    export!(Component);
}
