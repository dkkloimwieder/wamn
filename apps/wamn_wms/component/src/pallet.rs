use wamn_wms_data_access::{AccessError, AccessErrorKind, pallet};

fn detail(error: &AccessError, key: &str) -> Option<String> {
    let value = error.detail().get(key)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
}

mod get {
    use super::{detail, pallet};
    use crate::exports::wamn_wms::pallet::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/pallet_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        pallet::get(connection, &request.id)
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
    use super::{AccessError, AccessErrorKind, detail, pallet};
    use crate::exports::wamn_wms::pallet::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/pallet_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        let query = query_input(request).map_err(|error| map_error(&error))?;
        pallet::query(connection, &query)
            .await
            .map(|page| contract::QueryResult {
                value: page
                    .item
                    .into_iter()
                    .map(|row| codec::row!(row, contract::QueryRow))
                    .collect(),
                next_cursor: page.next_cursor,
            })
            .map_err(|error| map_error(&error))
    }

    fn map_error(error: &AccessError) -> contract::QueryError {
        codec::map_error(error.kind().literal(), |key| detail(error, key))
    }

    fn query_input(request: contract::QueryRequest) -> Result<pallet::QueryInput, AccessError> {
        let filter = if request.status.is_none()
            && request.location_id.is_none()
            && request.pallet_code.is_none()
        {
            None
        } else {
            Some(pallet::Filter {
                status: request.status,
                location_id: request.location_id,
                pallet_code: request.pallet_code,
            })
        };
        let sort = match (
            request.sort_field.as_deref(),
            request.sort_direction.as_deref(),
        ) {
            (None, None) => None,
            (Some(field), Some(direction)) => Some(pallet::Sort {
                field: match field {
                    "pallet_code" => pallet::SortField::PalletCode,
                    "location_id" => pallet::SortField::LocationId,
                    "updated_at" => pallet::SortField::UpdatedAt,
                    "created_at" => pallet::SortField::CreatedAt,
                    _ => {
                        return Err(AccessError::field(
                            AccessErrorKind::InvalidInput,
                            "sort.field",
                        ));
                    }
                },
                direction: match direction {
                    "ascending" => pallet::CursorDirection::Ascending,
                    "descending" => pallet::CursorDirection::Descending,
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
        Ok(pallet::QueryInput {
            filter,
            sort,
            cursor: request.cursor,
            limit: request.limit,
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
