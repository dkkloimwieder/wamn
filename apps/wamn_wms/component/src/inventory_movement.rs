use wamn_wms_data_access::inventory_movement;

use crate::pallet::detail;

mod get {
    use super::{detail, inventory_movement};
    use crate::exports::wamn_wms::inventory_movement::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_movement_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        inventory_movement::get(connection, &request.id)
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
    use super::{detail, inventory_movement};
    use crate::exports::wamn_wms::inventory_movement::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_movement_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::QueryRequest,
        rows: &mut codec::Rows,
    ) -> Result<contract::QueryEnd, contract::QueryError> {
        let refuse = |error: wamn_wms_data_access::AccessError| {
            codec::map_error(error.kind().literal(), |key| detail(&error, key))
        };
        let limit = request.limit.expect("the codec fills the default limit");
        let mut page = inventory_movement::query(connection, request.cursor.as_deref(), limit)
            .await
            .map_err(refuse)?;
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
        wamn_postgres_statements::Connection::new(),
        handle,
        codec
    );
}
