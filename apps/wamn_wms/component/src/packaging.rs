use wamn_wms_data_access::packaging;

use crate::detail;

mod get {
    use super::{detail, packaging};
    use crate::exports::wamn_wms::packaging::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/packaging_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        packaging::get(connection, &request.id)
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
    use super::{detail, packaging};
    use crate::exports::wamn_wms::packaging::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/packaging_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        packaging::query(connection, request.cursor.as_deref(), request.limit)
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

mod create {
    use crate::detail;
    use crate::exports::wamn_wms::packaging::create as contract;
    use wamn_wms_data_access::packaging_create;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/packaging_create_codec.rs"
        ));
    }
    async fn handle(
        (): &mut (),
        request: contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError> {
        packaging_create::execute(&packaging_create::CreateCommand {
            idempotency_key: request.idempotency_key,
            r#type: request.type_,
            code: request.code,
            location_id: request.location_id,
        })
        .await
        .map(|value| contract::CreateResult {
            operation_id: value.operation_id,
            packaging_id: value.packaging_id,
            type_: value.r#type,
            code: value.code,
            location_id: value.location_id,
            lifecycle: value.lifecycle,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        (),
        handle,
        codec
    );
}

mod close {
    use crate::detail;
    use crate::exports::wamn_wms::packaging::close as contract;
    use wamn_wms_data_access::packaging_close;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/packaging_close_codec.rs"
        ));
    }
    async fn handle(
        (): &mut (),
        request: contract::CloseRequest,
    ) -> Result<contract::CloseResult, contract::CloseError> {
        packaging_close::execute(&packaging_close::CloseCommand {
            idempotency_key: request.idempotency_key,
            packaging_id: request.packaging_id,
            expected_row_version: request.expected_row_version,
        })
        .await
        .map(|value| contract::CloseResult {
            operation_id: value.operation_id,
            packaging_id: value.packaging_id,
            type_: value.r#type,
            code: value.code,
            location_id: value.location_id,
            lifecycle: value.lifecycle,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        (),
        handle,
        codec
    );
}
