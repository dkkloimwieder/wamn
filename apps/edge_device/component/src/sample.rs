/// The stateless device command: one frame becomes one sample, with no SQL.
/// The engine's per-item intent is its replay guard.
mod read {
    use crate::exports::edge_device::sample::read as contract;
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
