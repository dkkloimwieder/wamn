// @generated from the package manifest; do not edit.

// The generated `widget` handlers.

#[allow(unused_imports)]
use super::data::error::Error;
#[allow(unused_imports)]
use super::data::widget as model;
#[allow(unused_imports)]
use wamn_postgres_statements::{Connection, Transaction};

pub(crate) mod get {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::platform_fixture::widget::get as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_get_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        connection: &mut Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        model::get(connection, &request.id)
            .await
            .map(|row| contract::GetResult {
                value: codec::row!(row, contract::GetRow),
            })
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

pub(crate) mod query {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::platform_fixture::widget::query as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_query_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        connection: &mut Connection,
        request: contract::QueryRequest,
        rows: &mut codec::Rows,
    ) -> Result<contract::QueryEnd, contract::QueryError> {
        let input = model::QueryInput {
            code: request.code,
            note: request.note,
            maker_id: request.maker_id,
            sort_field: request.sort_field,
            sort_direction: request.sort_direction,
            cursor: request.cursor,
            limit: request.limit.expect("the codec fills the default limit"),
        };
        let refuse = |error: Error| codec::map_error(error.literal(), |key| error.detail(key));
        let mut page = model::query(connection, input).await.map_err(refuse)?;
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
        Connection::new(),
        handle,
        codec
    );
}

pub(crate) mod create {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::platform_fixture::widget::create as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_create_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        transaction: &mut Transaction,
        request: contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError> {
        model::create(
            transaction,
            request.code.flatten(),
            request.maker_id.flatten(),
            request.note.flatten(),
        )
        .await
        .map(|row| contract::CreateResult {
            value: codec::row!(row, contract::CreateRow),
        })
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

pub(crate) mod update {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::platform_fixture::widget::update as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_update_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        connection: &mut Connection,
        request: contract::UpdateRequest,
    ) -> Result<contract::UpdateResult, contract::UpdateError> {
        model::update(
            connection,
            &request.id,
            request.expected_edit_version,
            request.change.code,
            request.change.maker_id,
            request.change.note,
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

pub(crate) mod delete {
    #[allow(unused_imports)]
    use super::{Connection, Error, Transaction, model};
    use crate::exports::platform_fixture::widget::delete as contract;
    pub(crate) mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/widget_delete_codec.rs"
        ));
    }

    pub(crate) async fn handle(
        connection: &mut Connection,
        request: contract::DeleteRequest,
    ) -> Result<contract::DeleteResult, contract::DeleteError> {
        model::delete(connection, &request.id, request.expected_edit_version)
            .await
            .map(|row| contract::DeleteResult {
                value: codec::row!(row, contract::DeleteRow),
            })
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
