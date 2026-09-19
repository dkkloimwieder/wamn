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
        (): &mut (),
        request: contract::AdjustRequest,
    ) -> Result<contract::AdjustResult, contract::AdjustError> {
        inventory_adjust::execute(&inventory_adjust::AdjustCommand {
            idempotency_key: request.idempotency_key,
            pallet_id: request.pallet_id,
            product_id: request.product_id,
            status: request.status,
            quantity: request.quantity,
            reason_code: request.reason_code,
            expected_row_version: request.expected_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::AdjustResult {
            movement_id: value.movement_id,
            pallet_id: value.pallet_id,
            adjusted_quantity: value.adjusted_quantity,
            pallet_status: value.pallet_status,
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
        inventory_aggregate::execute(connection)
            .await
            .map(|rows| contract::AggregateResult {
                rows: rows
                    .into_iter()
                    .map(|row| codec::row!(row, contract::AggregateRow))
                    .collect(),
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
        (): &mut (),
        request: contract::MergeRequest,
    ) -> Result<contract::MergeResult, contract::MergeError> {
        inventory_merge::execute(&inventory_merge::MergeCommand {
            idempotency_key: request.idempotency_key,
            source_pallet_id: request.source_pallet_id,
            target_pallet_id: request.target_pallet_id,
            expected_row_version: request.expected_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::MergeResult {
            movement_id: value.movement_id,
            source_pallet_id: value.source_pallet_id,
            target_pallet_id: value.target_pallet_id,
            target_status: value.target_status,
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
        (): &mut (),
        request: contract::MoveRequest,
    ) -> Result<contract::MoveResult, contract::MoveError> {
        inventory_move::execute(&inventory_move::MoveCommand {
            idempotency_key: request.idempotency_key,
            pallet_id: request.pallet_id,
            to_location_id: request.to_location_id,
            expected_row_version: request.expected_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::MoveResult {
            movement_id: value.movement_id,
            pallet_id: value.pallet_id,
            location_id: value.location_id,
            pallet_status: value.pallet_status,
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
        (): &mut (),
        request: contract::SplitRequest,
    ) -> Result<contract::SplitResult, contract::SplitError> {
        inventory_split::execute(&inventory_split::SplitCommand {
            idempotency_key: request.idempotency_key,
            source_pallet_id: request.source_pallet_id,
            product_id: request.product_id,
            status: request.status,
            quantity: request.quantity,
            new_pallet_code: request.new_pallet_code,
            to_location_id: request.to_location_id,
            expected_row_version: request.expected_row_version,
            occurred_at: request.occurred_at,
        })
        .await
        .map(|value| contract::SplitResult {
            movement_id: value.movement_id,
            source_pallet_id: value.source_pallet_id,
            new_pallet_id: value.new_pallet_id,
            source_status: value.source_status,
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
