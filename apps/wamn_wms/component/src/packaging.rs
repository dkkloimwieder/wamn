use wamn_wms_data_access::{AccessError, AccessErrorKind, packaging};

pub(crate) fn detail(error: &AccessError, key: &str) -> Option<String> {
    let value = error.detail().get(key)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
}

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

mod create {
    use super::{detail, packaging};
    use crate::exports::wamn_wms::packaging::create as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/packaging_create_codec.rs"
        ));
    }

    async fn handle(
        transaction: &mut wamn_postgres_statements::Transaction,
        request: contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError> {
        let packaging_code = request.packaging_code.flatten();
        let packaging_type = request.type_.flatten();
        let location_id = request.location_id.flatten();
        let status = request.status.flatten();
        let command = packaging::CreateCommand {
            packaging_code: packaging_code.as_deref(),
            r#type: packaging_type.as_deref(),
            location_id: location_id.as_deref(),
            status: status.as_deref(),
        };
        packaging::create(transaction, &command)
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

mod query {
    use super::{AccessError, AccessErrorKind, detail, packaging};
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
        rows: &mut codec::Rows,
    ) -> Result<contract::QueryEnd, contract::QueryError> {
        let query = query_input(request).map_err(|error| map_error(&error))?;
        let mut page = packaging::query(connection, &query)
            .await
            .map_err(|error| map_error(&error))?;
        while let Some(row) = page.next().await.map_err(|error| map_error(&error))? {
            rows.push(codec::row!(row, contract::QueryRow)).await?;
        }
        Ok(contract::QueryEnd {
            next_cursor: page.next_cursor(),
        })
    }

    fn map_error(error: &AccessError) -> contract::QueryError {
        codec::map_error(error.kind().literal(), |key| detail(error, key))
    }

    fn query_input(request: contract::QueryRequest) -> Result<packaging::QueryInput, AccessError> {
        let filter = if request.status.is_none()
            && request.location_id.is_none()
            && request.packaging_code.is_none()
        {
            None
        } else {
            Some(packaging::Filter {
                status: request.status,
                location_id: request.location_id,
                packaging_code: request.packaging_code,
            })
        };
        let sort = match (
            request.sort_field.as_deref(),
            request.sort_direction.as_deref(),
        ) {
            (None, None) => None,
            (Some(field), Some(direction)) => Some(packaging::Sort {
                field: match field {
                    "packaging_code" => packaging::SortField::PackagingCode,
                    "location_id" => packaging::SortField::LocationId,
                    "updated_at" => packaging::SortField::UpdatedAt,
                    "created_at" => packaging::SortField::CreatedAt,
                    _ => {
                        return Err(AccessError::field(
                            AccessErrorKind::InvalidInput,
                            "sort.field",
                        ));
                    }
                },
                direction: match direction {
                    "ascending" => packaging::CursorDirection::Ascending,
                    "descending" => packaging::CursorDirection::Descending,
                    _ => {
                        return Err(AccessError::field(
                            AccessErrorKind::InvalidInput,
                            "sort.direction",
                        ));
                    }
                },
            }),
            _ => return Err(AccessError::field(AccessErrorKind::InvalidInput, "sort")),
        };
        Ok(packaging::QueryInput {
            filter,
            sort,
            cursor: request.cursor,
            limit: request.limit.expect("the codec fills the default limit"),
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
