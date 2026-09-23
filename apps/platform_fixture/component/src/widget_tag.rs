use wamn_platform_fixture_data_access::widget_tag;
use wamn_postgres_statements::Connection;

use crate::detail;

mod update {
    use super::{Connection, detail, widget_tag};
    use crate::exports::platform_fixture::widget_tag::update as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_tag_update_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut Connection,
        request: contract::UpdateRequest,
    ) -> Result<contract::UpdateResult, contract::UpdateError> {
        widget_tag::update(
            connection,
            &request.id,
            request.expected_edit_version,
            request.change.label,
        )
        .await
        .map(|row| codec::row!(row, contract::UpdateResult))
        .map_err(|error| {
            codec::map_error(error.kind().literal(), |key| {
                detail(&error, "widget_tag.update", key)
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
