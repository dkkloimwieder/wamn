//! Read one holding register through the package's connection binding.

#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits canonical ABI buffers with equal length and capacity"
)]

wit_bindgen::generate!({
    path: [
        "../../../crates/platform/runtime/wit/deps/wamn-connector",
        "../../../crates/execution/workflow/router/wit",
        "../../target/wamn/modbus_reader/wit",
        "wit",
    ],
    world: "wamn:modbus-example/application",
    generate_all,
    async: [
        "export:modbus-reader:register/read@0.1.0#run",
        "export:modbus-reader:register/read@0.1.0#run-json",
    ],
});

use exports::modbus_reader::register::read as contract;
use wamn::connector::reader::{self, ConnectorError};

struct Application;
export!(Application);

mod codec {
    use super::contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/modbus_reader/wit/register_read_codec.rs"
    ));
}

async fn read(
    (): &mut (),
    request: contract::ReadRequest,
) -> Result<contract::ReadResult, contract::ReadError> {
    let invalid = |field: &str| {
        contract::ReadError::InvalidInput(contract::InvalidInputDetail {
            field: field.to_owned(),
        })
    };
    let unit = u8::try_from(request.unit).map_err(|_| invalid("value.unit"))?;
    let address = u16::try_from(request.address).map_err(|_| invalid("value.address"))?;
    let [hi, lo] = address.to_be_bytes();
    let bytes = reader::get("registers", &[unit, hi, lo, 0, 1]).map_err(|error| match error {
        ConnectorError::Unavailable => contract::ReadError::Unavailable,
        ConnectorError::Timeout => contract::ReadError::Timeout,
        ConnectorError::Transport(_) => contract::ReadError::Retry,
        _ => contract::ReadError::InternalError,
    })?;
    let [hi, lo]: [u8; 2] = bytes
        .try_into()
        .map_err(|_| contract::ReadError::InternalError)?;
    Ok(contract::ReadResult {
        register: i32::from(u16::from_be_bytes([hi, lo])),
    })
}

codec::export_operation!(Application, contract, wamn::node::types, (), read, codec);
