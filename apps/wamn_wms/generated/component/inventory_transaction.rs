// @generated from the package manifest; do not edit.

// The generated `inventory_transaction` handlers.

#[allow(unused_imports)]
use super::data::error::Error;
#[allow(unused_imports)]
use super::data::inventory_transaction as model;
#[allow(unused_imports)]
use wamn_postgres_statements::{Connection, Transaction};

mod get {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::wamn_wms::inventory_transaction::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_transaction_get_codec.rs"
        ));
    }

    async fn handle(
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

mod query {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::wamn_wms::inventory_transaction::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_transaction_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::QueryRequest,
        rows: &mut codec::Rows,
    ) -> Result<contract::QueryEnd, contract::QueryError> {
        let input = model::QueryInput {
            sort_field: None,
            sort_direction: None,
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
