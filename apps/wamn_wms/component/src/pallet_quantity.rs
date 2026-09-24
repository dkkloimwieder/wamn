use wamn_wms_data_access::pallet_quantity;

use crate::pallet::detail;

mod get {
    use super::{detail, pallet_quantity};
    use crate::exports::wamn_wms::pallet_quantity::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/pallet_quantity_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        pallet_quantity::get(connection, &request.id)
            .await
            .map(|row| contract::GetResult {
                value: codec::row!(row, contract::GetRow),
            })
            .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        wamn_postgres_statements::Connection::new(),
        handle,
        codec
    );
}

mod query {
    use super::{detail, pallet_quantity};
    use crate::exports::wamn_wms::pallet_quantity::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/pallet_quantity_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        pallet_quantity::query(connection, request.cursor.as_deref(), request.limit)
            .await
            .map(|page| contract::QueryResult {
                value: page
                    .item
                    .into_iter()
                    .map(|row| codec::row!(row, contract::QueryRow))
                    .collect(),
                next_cursor: page.next_cursor,
            })
            .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        wamn_postgres_statements::Connection::new(),
        handle,
        codec
    );
}
