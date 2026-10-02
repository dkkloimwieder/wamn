use wamn_platform_fixture_data_access::widget;
use wamn_postgres_statements::{Connection, Transaction};

use crate::detail;

mod archive {
    use super::{Transaction, detail, widget};
    use crate::exports::platform_fixture::widget::archive as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_archive_codec.rs"
        ));
    }

    async fn handle(
        transaction: &mut Transaction,
        request: contract::ArchiveRequest,
    ) -> Result<contract::ArchiveResult, contract::ArchiveError> {
        widget::archive(transaction, &request.id, request.expected_edit_version)
            .await
            .map(|row| codec::row!(row, contract::ArchiveResult))
            .map_err(|error| {
                codec::map_error(error.error_type().literal(), |key| {
                    detail(&error, "widget.archive", key)
                })
            })
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
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
                codec::map_error(error.error_type().literal(), |key| {
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
    use super::{Transaction, detail, widget};
    use crate::exports::platform_fixture::widget::record_batch as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_record_batch_codec.rs"
        ));
    }

    async fn handle(
        transaction: &mut Transaction,
        request: contract::RecordBatchRequest,
    ) -> Result<contract::RecordBatchResult, contract::RecordBatchError> {
        // The contract declares `expected_edit_version`, `grade` and
        // `inspector_id` so that the component emitter meets a nested revision
        // input, an enum input and two selectors that read one list. The batch
        // does not use them.
        let batch = widget::Batch {
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
        widget::record_batch(transaction, &batch)
            .await
            .map(|row| codec::row!(row, contract::RecordBatchResult))
            .map_err(|error| {
                codec::map_error(error.error_type().literal(), |key| {
                    detail(&error, "widget.record_batch", key)
                })
            })
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        handle,
        codec
    );
}
