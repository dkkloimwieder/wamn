use wamn_wms_data_access::inventory;

use crate::detail;

mod get {
    use super::{detail, inventory};
    use crate::exports::wamn_wms::inventory::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        inventory::get(connection, &request.id)
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
    use super::{detail, inventory};
    use crate::exports::wamn_wms::inventory::query as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_query_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        inventory::query(connection, request.cursor.as_deref(), request.limit)
            .await
            .map(|page| contract::QueryResult {
                value: page
                    .item
                    .into_iter()
                    .map(|row| codec::row!(row, contract::QueryRow))
                    .collect(),
                next_cursor: page.next_cursor,
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

mod move_ {
    use crate::detail;
    use crate::exports::wamn_wms::inventory::move_ as contract;
    use wamn_wms_data_access::inventory_move;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_move_codec.rs"
        ));
    }
    async fn handle(
        (): &mut (),
        request: contract::MoveRequest,
    ) -> Result<contract::MoveResult, contract::MoveError> {
        inventory_move::execute(&inventory_move::MoveCommand {
            idempotency_key: request.idempotency_key,
            inventory_id: request.inventory_id,
            to_packaging_id: request.to_packaging_id,
            to_location_id: request.to_location_id,
            expected_row_version: request.expected_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::MoveResult {
            operation_id: value.operation_id,
            inventory_id: value.inventory_id,
            product_id: value.product_id,
            packaging_id: value.packaging_id,
            location_id: value.location_id,
            quantity: value.quantity,
            disposition: value.disposition,
            lifecycle: value.lifecycle,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        (),
        handle,
        codec
    );
}

mod adjust {
    use crate::detail;
    use crate::exports::wamn_wms::inventory::adjust as contract;
    use wamn_wms_data_access::inventory_adjust;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_adjust_codec.rs"
        ));
    }
    async fn handle(
        (): &mut (),
        request: contract::AdjustRequest,
    ) -> Result<contract::AdjustResult, contract::AdjustError> {
        inventory_adjust::execute(&inventory_adjust::AdjustCommand {
            idempotency_key: request.idempotency_key,
            inventory_id: request.inventory_id,
            to_quantity: request.to_quantity,
            reason: request.reason,
            expected_row_version: request.expected_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::AdjustResult {
            operation_id: value.operation_id,
            inventory_id: value.inventory_id,
            product_id: value.product_id,
            packaging_id: value.packaging_id,
            location_id: value.location_id,
            quantity: value.quantity,
            disposition: value.disposition,
            lifecycle: value.lifecycle,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        (),
        handle,
        codec
    );
}

mod split {
    use crate::detail;
    use crate::exports::wamn_wms::inventory::split as contract;
    use wamn_wms_data_access::inventory_split;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_split_codec.rs"
        ));
    }
    async fn handle(
        (): &mut (),
        request: contract::SplitRequest,
    ) -> Result<contract::SplitResult, contract::SplitError> {
        inventory_split::execute(&inventory_split::SplitCommand {
            idempotency_key: request.idempotency_key,
            from_inventory_id: request.from_inventory_id,
            quantity: request.quantity,
            to_packaging_id: request.to_packaging_id,
            to_location_id: request.to_location_id,
            expected_row_version: request.expected_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::SplitResult {
            operation_id: value.operation_id,
            inventory_id: value.inventory_id,
            product_id: value.product_id,
            packaging_id: value.packaging_id,
            location_id: value.location_id,
            quantity: value.quantity,
            disposition: value.disposition,
            lifecycle: value.lifecycle,
            row_version: value.row_version,
            new_inventory_id: value.new_inventory_id,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        (),
        handle,
        codec
    );
}

mod merge {
    use crate::detail;
    use crate::exports::wamn_wms::inventory::merge as contract;
    use wamn_wms_data_access::inventory_merge;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_merge_codec.rs"
        ));
    }
    async fn handle(
        (): &mut (),
        request: contract::MergeRequest,
    ) -> Result<contract::MergeResult, contract::MergeError> {
        inventory_merge::execute(&inventory_merge::MergeCommand {
            idempotency_key: request.idempotency_key,
            from_inventory_id: request.from_inventory_id,
            to_inventory_id: request.to_inventory_id,
            expected_from_row_version: request.expected_from_row_version,
            expected_to_row_version: request.expected_to_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::MergeResult {
            operation_id: value.operation_id,
            inventory_id: value.inventory_id,
            product_id: value.product_id,
            packaging_id: value.packaging_id,
            location_id: value.location_id,
            quantity: value.quantity,
            disposition: value.disposition,
            lifecycle: value.lifecycle,
            row_version: value.row_version,
            from_inventory_id: value.from_inventory_id,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        (),
        handle,
        codec
    );
}

mod aggregate {
    use crate::detail;
    use crate::exports::wamn_wms::inventory::aggregate as contract;
    use wamn_wms_data_access::inventory_aggregate;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_aggregate_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        _: contract::AggregateRequest,
    ) -> Result<contract::AggregateResult, contract::AggregateError> {
        let rows = inventory_aggregate::execute(connection)
            .await
            .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))?;
        let rows = rows
            .into_iter()
            .map(|row| {
                Ok(contract::AggregateRow {
                    product_id: row.product_id.0,
                    location_id: row.location_id.0,
                    disposition: row.disposition,
                    quantity: row
                        .quantity
                        .ok_or(contract::AggregateError::InternalError)?
                        .0,
                    packaging_count: row
                        .packaging_count
                        .ok_or(contract::AggregateError::InternalError)?,
                })
            })
            .collect::<Result<_, contract::AggregateError>>()?;
        Ok(contract::AggregateResult { rows })
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
