//! Read one holding register through the package's connection binding.

#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits canonical ABI buffers with equal length and capacity"
)]

wit_bindgen::generate!({
    path: [
        "../../../wit/wamn-integration",
        "../../platform/no-std/modbus/wit",
        "../../../wit/wamn-node",
        "../../target/wamn/modbus_reader/wit",
        "wit",
    ],
    world: "wamn:modbus-example/application",
    generate_all,
    async: [
        "export:modbus-reader:register/read@0.1.1#run",
        "export:modbus-reader:register/read@0.1.1#run-json",
    ],
});

use exports::modbus_reader::register::read as contract;
use registers::{self as read_registers, Error};

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
    unit: &mut u8,
    request: contract::ReadRequest,
) -> Result<contract::ReadResult, contract::ReadError> {
    let invalid = |field: &str| {
        contract::ReadError::InvalidInput(contract::InvalidInputDetail {
            field: field.to_owned(),
        })
    };
    let address = u16::try_from(request.address).map_err(|_| invalid("value.address"))?;
    let registers =
        read_registers::read_registers(*unit, address, 1).map_err(|error| match error {
            Error::Unavailable => contract::ReadError::Unavailable,
            Error::Timeout => contract::ReadError::Timeout,
            Error::Transport(_) => contract::ReadError::Retry,
            Error::AuthorityDenied => contract::ReadError::InternalError,
        })?;
    let [register]: [u16; 1] = registers
        .try_into()
        .map_err(|_| contract::ReadError::InternalError)?;
    Ok(contract::ReadResult {
        register: i32::from(register),
    })
}

use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};

fn invalid(message: &str) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.to_owned(),
        code: Some("invalid_input".into()),
    })
}

impl contract::Guest for Application {
    async fn run(
        context: NodeContext,
        input: Vec<contract::ReadItem>,
    ) -> Result<Vec<contract::ReadOutcome>, NodeError> {
        codec::validate(&input).map_err(|error| invalid(error.context()))?;
        let mut unit = serde_json::from_str::<serde_json::Value>(&context.config)
            .ok()
            .and_then(|value| {
                value["unit"]
                    .as_u64()
                    .and_then(|unit| u8::try_from(unit).ok())
            })
            .filter(|unit| (1..=247).contains(unit))
            .ok_or_else(|| invalid("unit must be between 1 and 247"))?;
        Ok(codec::run(input, &mut unit, read).await)
    }

    async fn run_json(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = codec::decode(&input).map_err(|error| invalid(error.context()))?;
        let output = <Self as contract::Guest>::run(context, input).await?;
        Ok(Emission {
            payload: codec::encode(&output),
            port: None,
        })
    }
}
