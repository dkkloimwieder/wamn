use wamn_wms_data_access::location;

use crate::pallet::detail;

mod get {
    use super::{detail, location};
    use crate::exports::wamn_wms::location::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/location_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        location::get(connection, &request.id)
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
    use super::{detail, location};
    use crate::exports::wamn_wms::location::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/location_query_codec.rs"
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
        let mut page = location::query(
            connection,
            request.location_code.as_deref(),
            request.cursor.as_deref(),
            limit,
        )
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

mod create {
    use super::{detail, location};
    use crate::exports::wamn_wms::location::create as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/location_create_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError> {
        let location_code = request.location_code.flatten();
        location::create(
            connection,
            &request.idempotency_key,
            location_code.as_deref(),
        )
        .await
        .map(|row| contract::CreateResult {
            value: codec::row!(row, contract::CreateRow),
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

mod update {
    use super::{detail, location};
    use crate::exports::wamn_wms::location::update as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/location_update_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::UpdateRequest,
    ) -> Result<contract::UpdateResult, contract::UpdateError> {
        let change = match request.change.location_code {
            None => location::CodeChange::Omitted,
            Some(None) => location::CodeChange::Null,
            Some(Some(value)) => location::CodeChange::Value(value),
        };
        location::update(
            connection,
            &request.id,
            request.expected_row_version,
            change,
        )
        .await
        .map(|row| codec::row!(row, contract::UpdateResult))
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
