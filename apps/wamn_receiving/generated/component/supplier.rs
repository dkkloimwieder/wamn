// @generated from the package manifest; do not edit.

// The generated `supplier` handlers.

#[allow(unused_imports)]
use super::data::error::Error;
#[allow(unused_imports)]
use super::data::supplier as model;
#[allow(unused_imports)]
use wamn_postgres_statements::{Connection, Transaction};

pub(crate) mod query {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::wamn_receiving::supplier::query as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/supplier_query_codec.rs"
        ));
    }

    pub(crate) async fn handle(
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

pub(crate) mod create {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::wamn_receiving::supplier::create as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/supplier_create_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        transaction: &mut Transaction,
        request: contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError> {
        model::create(transaction, request.name.flatten())
            .await
            .map(|row| contract::CreateResult {
                value: codec::row!(row, contract::CreateRow),
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
