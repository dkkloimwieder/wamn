#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component for the platform fixture overlay.
//!
//! The overlay declares one operation, `widget.get`, which reads the field the
//! overlay adds. One generated statement serves it, so the component calls the
//! generated accessor itself and has no data-access crate.

use wamn_postgres_statements::{Connection, StatementErrorKind, Uuid};

use exports::platform_fixture_overlay::widget::get as contract;

wit_bindgen::generate!({
    world: "platform-fixture-overlay:component/fixture-overlay@0.1.0",
    inline: r#"
        package platform-fixture-overlay:component@0.1.0;

        world fixture-overlay {
          import wamn:postgres/types@0.1.0;
          import wamn:postgres/statements@0.1.0;
          export platform-fixture-overlay:widget/get@1.0.0;
        }
    "#,
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres",
        "../generated/wit/deps/platform-fixture-overlay-widget",
    ],
    generate_all,
    async: true,
});

/// Generated `widget` projection and statement digest.
mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/widget.rs"
    ));
}

mod codec {
    use super::contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/widget_get_codec.rs"
    ));
}

struct Component;

/// Load one widget with the overlay field. The codec has already re-spelled
/// the id as a canonical UUID.
async fn get(
    connection: &mut Connection,
    request: contract::GetRequest,
) -> Result<contract::GetResult, contract::GetError> {
    let literal = match sql::get(connection, Uuid(request.id.clone())).await {
        Ok(Some(row)) => {
            return Ok(contract::GetResult {
                value: codec::row!(row, contract::GetRow),
            });
        }
        Ok(None) => "not_found",
        Err(error) => match error.kind() {
            StatementErrorKind::SerializationFailure
            | StatementErrorKind::ConnectionUnavailable => "retry",
            StatementErrorKind::StatementTimeout => "timeout",
            StatementErrorKind::PermissionDenied => "permission_denied",
            _ => "internal_error",
        },
    };
    Err(codec::map_error(literal, |key| match key {
        "field" => Some("id".to_owned()),
        "id" => Some(request.id.clone()),
        "operation" => Some("widget.get".to_owned()),
        _ => None,
    }))
}

codec::export_operation!(
    Component,
    contract,
    wamn::node::types,
    Connection::new(),
    get,
    codec
);

export!(Component);
