//! Typed supplier creation boundary over the generated claim.

use super::exports::wamn_receiving::supplier::create as contract;
use wamn_receiving_data_access::supplier;

pub(super) mod codec {
    use super::contract;
    include!("../../generated/wit/supplier_create_codec.rs");
}

pub(super) async fn execute(
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::CreateRequest,
) -> Result<contract::CreateResult, contract::CreateError> {
    // The input states three things about a name: a value, an explicit null,
    // and nothing at all. The column is `NOT NULL`, so the last two refuse
    // together on the field the operator filled.
    let name = request.name.flatten();
    supplier::create(connection, &request.idempotency_key, name.as_deref())
        .await
        .map(|row| contract::CreateResult {
            value: codec::row!(row, contract::CreateRow),
        })
        .map_err(|error| {
            codec::map_error(error.kind().literal(), |key| {
                super::reads::error_detail(&error, key, "supplier.create", None, None)
            })
        })
}

codec::export_operation!(
    super::Component,
    super::exports::wamn_receiving::supplier::create,
    super::wamn::node::types,
    wamn_postgres_statements::Connection::new(),
    execute,
    codec
);
