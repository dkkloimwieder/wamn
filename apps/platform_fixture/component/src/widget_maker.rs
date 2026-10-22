use wamn_platform_fixture_data_access::widget_maker;
use wamn_postgres_statements::Transaction;

use crate::detail;

mod list {
    use super::{Transaction, detail, widget_maker};
    use crate::exports::platform_fixture::widget_maker::list as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/platform_fixture/wit/widget_maker_list_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Transaction,
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
                codec::map_error(error.error_type().literal(), |key| {
                    detail(&error, "widget_maker.list", key)
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
