use wamn_platform_fixture_data_access::generated::widget_tag as model;
use wamn_postgres_statements::Connection;

mod update {
    use super::{Connection, model};
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
        model::update(
            connection,
            &request.id,
            request.expected_edit_version,
            request.change.label,
        )
        .await
        .map(|row| codec::row!(row, contract::UpdateResult))
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
