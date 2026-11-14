//! Read and replace one environment setting.
use super::exports::wamn_integration::setting::{read as read_contract, write as write_contract};
use wamn_postgres_statements::{Connection, Json, Transaction};

mod read_operation {
    use super::read_contract as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wit/setting_read_operation.rs"
    ));
}
mod write_operation {
    use super::write_contract as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wit/setting_write_operation.rs"
    ));
}
mod read_sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wamn/setting_read.rs"
    ));
}
mod write_sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wamn/setting_write.rs"
    ));
}

fn read_value(
    row: Option<read_sql::ReadSettingRow>,
    carry: Option<crate::wamn::node::types::JsonValue>,
) -> read_contract::ReadResult {
    read_contract::ReadResult {
        value: row.map(|row| crate::wamn::node::types::JsonValue {
            encoded_json: row.value.0,
        }),
        carry,
    }
}

async fn read(
    connection: &mut Connection,
    request: read_contract::ReadRequest,
) -> Result<read_contract::ReadResult, super::NodeError> {
    let row = read_sql::read_setting(connection, request.key)
        .await
        .map_err(|error| super::statement_error(&error))?;
    Ok(read_value(row, request.carry))
}

async fn write(
    transaction: &mut Transaction,
    request: write_contract::WriteRequest,
) -> Result<write_contract::WriteResult, super::NodeError> {
    write_sql::write_setting(transaction, request.key, Json(request.value.encoded_json))
        .await
        .map(|row| write_contract::WriteResult { key: row.key })
        .map_err(|error| super::statement_error(&error))
}

export_operation!(
    read_contract,
    read_operation,
    ReadItem,
    ReadOutcome,
    ReadError,
    Connection::new(),
    read,
);
export_operation!(
    write_contract,
    write_operation,
    WriteItem,
    WriteOutcome,
    WriteError,
    wamn_postgres_statements::operation_transaction()
        .await
        .map_err(|error| super::statement_error(&error))?,
    write
);
