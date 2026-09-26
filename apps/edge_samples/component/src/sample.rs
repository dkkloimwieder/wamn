use wamn_edge_samples_data_access::{AccessError, sample};

fn detail(error: &AccessError, key: &str) -> Option<String> {
    let value = error.detail().get(key)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
}

mod get {
    use super::{detail, sample};
    use crate::exports::edge_samples::sample::get as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/sample_get_codec.rs"
        ));
    }

    async fn handle(
        connection: &mut wamn_postgres_statements::Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        sample::get(connection, &request.id)
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

/// The stateless device command: one frame becomes one sample, with no SQL.
/// The engine's per-item intent is its replay guard.
mod read {
    use crate::exports::edge_samples::sample::read as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/sample_read_codec.rs"
        ));
    }

    async fn handle(
        (): &mut (),
        request: contract::ReadRequest,
    ) -> Result<contract::ReadResult, contract::ReadError> {
        let frame = request.frame.trim();
        if frame.is_empty() {
            return Err(contract::ReadError::InvalidInput(
                contract::InvalidInputDetail {
                    field: "value.frame".to_owned(),
                },
            ));
        }
        Ok(contract::ReadResult {
            frame: frame.to_owned(),
            captured_at: request.captured_at,
        })
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

mod record {
    use super::{detail, sample};
    use crate::exports::edge_samples::sample::record as contract;
    mod codec {
        use super::contract;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wit/sample_record_codec.rs"
        ));
    }

    async fn handle(
        (): &mut (),
        request: contract::RecordRequest,
    ) -> Result<contract::RecordResult, contract::RecordError> {
        sample::record(&sample::RecordCommand {
            idempotency_key: request.idempotency_key,
            frame: request.frame,
            captured_at: request.captured_at,
        })
        .await
        .map(|sample_id| contract::RecordResult { sample_id })
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
