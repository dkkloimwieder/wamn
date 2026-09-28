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
    use crate::exports::wamn_receiving::purchase_order::get as contract;
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

pub(crate) mod query {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::wamn_receiving::purchase_order::query as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/purchase_order_query_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        connection: &mut Connection,
        request: contract::QueryRequest,
        rows: &mut codec::Rows,
    ) -> Result<contract::QueryEnd, contract::QueryError> {
        let input = model::QueryInput {
            supplier_id: request.supplier_id,
            status: request.status,
            purchase_order_number: request.purchase_order_number,
            sort_field: request.sort_field,
            sort_direction: request.sort_direction,
            cursor: request.cursor,
            limit: request.limit.expect("the codec fills the default limit"),
        };
        let refuse = |error: Error| codec::map_error(error.literal(), |key| error.detail(key));
        let mut page = model::query(connection, input).await.map_err(refuse)?;
        while let Some(row) = page.next().await.map_err(refuse)? {
            rows.push(codec::row!(row, contract::QueryRow)).await?;
        }
        Ok(contract::QueryEnd {
            next_cursor: page.next_cursor(),
        })
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
    use crate::exports::wamn_receiving::purchase_order::update as contract;
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
            request.change.supplier_id,
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
