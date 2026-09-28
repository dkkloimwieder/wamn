// @generated from the package manifest; do not edit.

// The generated `purchase_order` handlers.

#[allow(unused_imports)]
use super::data::error::Error;
#[allow(unused_imports)]
use super::data::purchase_order as model;
#[allow(unused_imports)]
use wamn_postgres_statements::{Connection, Transaction};

pub(crate) mod get {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::client_acme_receiving::purchase_order::get as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/purchase_order_get_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        connection: &mut Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        model::get(connection, &request.id)
            .await
            .map(|row| contract::GetResult {
                value: codec::row!(row, contract::GetRow),
            })
            .map_err(|error| codec::map_error(error.literal(), |key| error.detail(key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        Connection::new(),
        handle,
        codec
    );
}

pub(crate) mod update {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::client_acme_receiving::purchase_order::update as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/purchase_order_update_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        connection: &mut Connection,
        request: contract::UpdateRequest,
    ) -> Result<contract::UpdateResult, contract::UpdateError> {
        model::update(
            connection,
            &request.id,
            request.expected_row_version,
            request.change.acme_inspection_required,
            request.change.acme_quality_status,
        )
        .await
        .map(|row| codec::row!(row, contract::UpdateResult))
        .map_err(|error| codec::map_error(error.literal(), |key| error.detail(key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        Connection::new(),
        handle,
        codec
    );
}
