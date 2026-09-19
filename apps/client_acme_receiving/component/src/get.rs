//! Typed purchase-order get boundary.

use super::exports::client_acme_receiving::purchase_order::get as contract;
use super::{AccessError, access_detail};
use wamn_client_acme_receiving_data_access::operation;

pub(super) mod codec {
    use super::contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/purchase_order_get_codec.rs"
    ));
}

pub(super) async fn handle(
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::GetRequest,
) -> Result<contract::GetResult, contract::GetError> {
    operation::purchase_order_get(connection, &request.id)
        .await
        .map(|row| contract::GetResult {
            value: codec::row!(row, contract::GetRow),
        })
        .map_err(|error| map_error(&error, &request.id))
}

fn map_error(error: &AccessError, id: &str) -> contract::GetError {
    codec::map_error(error.kind().literal(), |key| {
        access_detail(error, key, "purchase_order.get", Some(("id", id)), None)
    })
}
