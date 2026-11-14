use crate::data::widget_maker;
use wamn_postgres_statements::Connection;

use crate::detail;

mod list {
    use super::{Connection, detail, widget_maker};
    use crate::exports::platform_fixture::widget_maker::list as contract;
    mod operation {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/platform_fixture/wit/widget_maker_list_operation.rs"
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
                    .map(|row| operation::row!(row, contract::ListRow))
                    .collect(),
            })
            .map_err(|error| {
                operation::map_error(error.error_type().literal(), |key| {
                    detail(&error, "widget_maker.list", key)
                })
            })
    }
    operation::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        Connection::new(),
        handle,
        operation
    );
}
