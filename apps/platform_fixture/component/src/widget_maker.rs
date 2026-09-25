use wamn_platform_fixture_data_access::{QueryInput, widget_maker};
use wamn_postgres_statements::Connection;

use crate::detail;

mod get {
    use super::{Connection, detail, widget_maker};
    use crate::exports::platform_fixture::widget_maker::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_maker_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        widget_maker::get(connection, &request.id)
            .await
            .map(|row| contract::GetResult {
                value: codec::row!(row, contract::GetRow),
            })
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget_maker.get", key)
                })
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

mod query {
    use super::{Connection, QueryInput, detail, widget_maker};
    use crate::exports::platform_fixture::widget_maker::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_maker_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        let input = QueryInput {
            filter: request.name,
            sort_field: request.sort_field,
            sort_direction: request.sort_direction,
            cursor: request.cursor,
            limit: request.limit,
        };
        widget_maker::query(connection, &input)
            .await
            .map(|page| contract::QueryResult {
                value: page
                    .item
                    .into_iter()
                    .map(|row| codec::row!(row, contract::QueryRow))
                    .collect(),
                next_cursor: page.next_cursor,
            })
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget_maker.query", key)
                })
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

mod list {
    use super::{Connection, detail, widget_maker};
    use crate::exports::platform_fixture::widget_maker::list as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_maker_list_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        _: contract::ListRequest,
    ) -> Result<contract::ListResult, contract::ListError> {
        widget_maker::list(connection)
            .await
            .map(|rows| contract::ListResult {
                rows: rows
                    .into_iter()
                    .map(|row| codec::row!(row, contract::ListRow))
                    .collect(),
            })
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget_maker.list", key)
                })
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
