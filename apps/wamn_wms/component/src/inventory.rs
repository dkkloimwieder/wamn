use wamn_wms_data_access::{
    AccessError, inventory_adjust, inventory_aggregate, inventory_merge, inventory_move,
    inventory_split,
};

fn detail(error: &AccessError, key: &str) -> Option<String> {
    let value = error.detail().get(key)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
}

mod adjust {
    use super::{detail, inventory_adjust};
    use crate::exports::wamn_wms::inventory::adjust as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_adjust_codec.rs"
        ));
    }

    async fn handle(
        transaction: &mut wamn_postgres_statements::Transaction,
        request: contract::AdjustRequest,
    ) -> Result<contract::AdjustResult, contract::AdjustError> {
        inventory_adjust::execute(
            transaction,
            &inventory_adjust::AdjustCommand {
                packaging_id: request.packaging_id,
                product_id: request.product_id,
                status: request.status,
                quantity: request.quantity,
                reason_code: request.reason_code,
                expected_row_version: request.expected_row_version,
                occurred_at: request.occurred_at,
            },
        )
        .await
        .map(|value| contract::AdjustResult {
            transaction_ids: value.transaction_ids,
            packaging_id: value.packaging_id,
            adjusted_quantity: value.adjusted_quantity,
            packaging_status: value.packaging_status,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        handle,
        codec
    );
}

mod aggregate {
    use super::{detail, inventory_aggregate};
    use crate::exports::wamn_wms::inventory::aggregate as contract;
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
                    status: row.status,
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

mod merge {
    use super::{detail, inventory_merge};
    use crate::exports::wamn_wms::inventory::merge as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_merge_codec.rs"
        ));
    }

    async fn handle(
        transaction: &mut wamn_postgres_statements::Transaction,
        request: contract::MergeRequest,
    ) -> Result<contract::MergeResult, contract::MergeError> {
        inventory_merge::execute(
            transaction,
            &inventory_merge::MergeCommand {
                source_packaging_id: request.source_packaging_id,
                target_packaging_id: request.target_packaging_id,
                expected_row_version: request.expected_row_version,
                occurred_at: request.occurred_at,
            },
        )
        .await
        .map(|value| contract::MergeResult {
            transaction_ids: value.transaction_ids,
            source_packaging_id: value.source_packaging_id,
            target_packaging_id: value.target_packaging_id,
            target_status: value.target_status,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        handle,
        codec
    );
}

mod move_ {
    use super::{detail, inventory_move};
    use crate::exports::wamn_wms::inventory::move_ as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_move_codec.rs"
        ));
    }

    async fn handle(
        transaction: &mut wamn_postgres_statements::Transaction,
        request: contract::MoveRequest,
    ) -> Result<contract::MoveResult, contract::MoveError> {
        inventory_move::execute(
            transaction,
            &inventory_move::MoveCommand {
                packaging_id: request.packaging_id,
                to_location_id: request.to_location_id,
                expected_row_version: request.expected_row_version,
                occurred_at: request.occurred_at,
            },
        )
        .await
        .map(|value| contract::MoveResult {
            packaging_id: value.packaging_id,
            location_id: value.location_id,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        handle,
        codec
    );
}

mod split {
    use super::{detail, inventory_split};
    use crate::exports::wamn_wms::inventory::split as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/inventory_split_codec.rs"
        ));
    }

    async fn handle(
        transaction: &mut wamn_postgres_statements::Transaction,
        request: contract::SplitRequest,
    ) -> Result<contract::SplitResult, contract::SplitError> {
        inventory_split::execute(
            transaction,
            &inventory_split::SplitCommand {
                source_packaging_id: request.source_packaging_id,
                product_id: request.product_id,
                status: request.status,
                quantity: request.quantity,
                new_packaging_code: request.new_packaging_code,
                new_packaging_type: request.new_packaging_type,
                to_location_id: request.to_location_id,
                expected_row_version: request.expected_row_version,
                occurred_at: request.occurred_at,
            },
        )
        .await
        .map(|value| contract::SplitResult {
            transaction_ids: value.transaction_ids,
            source_packaging_id: value.source_packaging_id,
            new_packaging_id: value.new_packaging_id,
            source_status: value.source_status,
            row_version: value.row_version,
        })
        .map_err(|error| codec::map_error(error.kind().literal(), |key| detail(&error, key)))
    }
    codec::export_operation!(
        crate::Component,
        contract,
        crate::wamn::node::types,
        handle,
        codec
    );
}
