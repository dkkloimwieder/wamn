use wamn_platform_fixture_data_access::{QueryInput, widget};
use wamn_postgres_statements::Connection;

use crate::detail;

mod get {
    use super::{Connection, detail, widget};
    use crate::exports::platform_fixture::widget::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        widget::get(connection, &request.id)
            .await
            .map(|row| contract::GetResult {
                value: codec::row!(row, contract::GetRow),
            })
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget.get", key)
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
    use super::{Connection, QueryInput, detail, widget};
    use crate::exports::platform_fixture::widget::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        let input = QueryInput {
            filter: request.code,
            sort_field: request.sort_field,
            sort_direction: request.sort_direction,
            cursor: request.cursor,
            limit: request.limit,
        };
        widget::query(connection, &input)
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
                    detail(&error, "widget.query", key)
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

mod create {
    use super::{Connection, detail, widget};
    use crate::exports::platform_fixture::widget::create as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_create_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError> {
        widget::create(
            connection,
            &request.idempotency_key,
            request.code.flatten().as_deref(),
            request.maker_id.flatten().as_deref(),
            request.note.flatten().as_deref(),
        )
        .await
        .map(|row| contract::CreateResult {
            value: codec::row!(row, contract::CreateRow),
        })
        .map_err(|error| {
            codec::map_error(error.kind().literal(), |key| {
                detail(&error, "widget.create", key)
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

mod update {
    use super::{Connection, detail, widget};
    use crate::exports::platform_fixture::widget::update as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_update_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::UpdateRequest,
    ) -> Result<contract::UpdateResult, contract::UpdateError> {
        let change = widget::Change {
            code: request.change.code,
            maker_id: request.change.maker_id,
            note: request.change.note,
        };
        widget::update(
            connection,
            &request.id,
            request.expected_edit_version,
            change,
        )
        .await
        .map(|row| codec::row!(row, contract::UpdateResult))
        .map_err(|error| {
            codec::map_error(error.kind().literal(), |key| {
                detail(&error, "widget.update", key)
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

mod delete {
    use super::{Connection, detail, widget};
    use crate::exports::platform_fixture::widget::delete as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_delete_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::DeleteRequest,
    ) -> Result<contract::DeleteResult, contract::DeleteError> {
        widget::delete(connection, &request.id, request.expected_edit_version)
            .await
            .map(|row| contract::DeleteResult {
                value: codec::row!(row, contract::DeleteRow),
            })
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget.delete", key)
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

mod archive {
    use super::{Connection, detail, widget};
    use crate::exports::platform_fixture::widget::archive as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_archive_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::ArchiveRequest,
    ) -> Result<contract::ArchiveResult, contract::ArchiveError> {
        widget::archive(connection, &request.id, request.expected_edit_version)
            .await
            .map(|row| codec::row!(row, contract::ArchiveResult))
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget.archive", key)
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
    use super::{Connection, detail, widget};
    use crate::exports::platform_fixture::widget::list as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_list_codec.rs"
        ));
    }

    /// The list statement takes no parameter, so the request's `selector`
    /// and `maker_id` narrow nothing.
    async fn handle(
        connection: &mut Connection,
        _: contract::ListRequest,
    ) -> Result<contract::ListResult, contract::ListError> {
        widget::list(connection)
            .await
            .map(|rows| contract::ListResult {
                rows: rows
                    .into_iter()
                    .map(|row| codec::row!(row, contract::ListRow))
                    .collect(),
            })
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget.list", key)
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

mod record_batch {
    use super::{Connection, detail, widget};
    use crate::exports::platform_fixture::widget::record_batch as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_record_batch_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::RecordBatchRequest,
    ) -> Result<contract::RecordBatchResult, contract::RecordBatchError> {
        // The contract declares `expected_edit_version` so that the component
        // emitter meets a nested revision input. The batch does not use it.
        let batch = widget::Batch {
            idempotency_key: request.idempotency_key,
            note: request.note,
            maker_id: request.maker_id,
            line: request
                .line
                .into_iter()
                .map(|line| widget::Line {
                    widget_id: line.widget_id,
                    amount: line.amount,
                })
                .collect(),
        };
        widget::record_batch(connection, &batch)
            .await
            .map(|row| codec::row!(row, contract::RecordBatchResult))
            .map_err(|error| {
                codec::map_error(error.kind().literal(), |key| {
                    detail(&error, "widget.record_batch", key)
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
