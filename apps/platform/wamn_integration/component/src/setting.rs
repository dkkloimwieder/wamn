//! Read and replace one environment setting.
use super::exports::wamn_integration::setting::{read as read_contract, write as write_contract};
use wamn_postgres_statements::{Connection, Json, Transaction};

mod read_codec {
    use super::read_contract as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wit/setting_read_codec.rs"
    ));
}
mod write_codec {
    use super::write_contract as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wit/setting_write_codec.rs"
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

fn read_value(row: Option<read_sql::ReadSettingRow>) -> read_contract::ReadResult {
    read_contract::ReadResult {
        value: row.map(|row| row.value.0),
    }
}

async fn read(
    connection: &mut Connection,
    request: read_contract::ReadRequest,
) -> Result<read_contract::ReadResult, read_contract::ReadError> {
    let mut transaction = connection
        .begin()
        .await
        .map_err(|error| read_codec::map_error(super::statement_error(&error), |_| None))?;
    let row = read_sql::read_setting(&mut transaction, request.key)
        .await
        .map_err(|error| read_codec::map_error(super::statement_error(&error), |_| None))?;
    transaction
        .commit()
        .await
        .map_err(|error| read_codec::map_error(super::statement_error(&error), |_| None))?;
    Ok(read_value(row))
}

async fn write(
    transaction: &mut Transaction,
    request: write_contract::WriteRequest,
) -> Result<write_contract::WriteResult, write_contract::WriteError> {
    write_sql::insert_setting(
        transaction,
        request.key.clone(),
        Json(request.value.clone()),
    )
    .await
    .map_err(|error| write_codec::map_error(super::statement_error(&error), |_| None))?;
    write_sql::write_setting(transaction, request.key, Json(request.value))
        .await
        .map(|row| write_contract::WriteResult { key: row.key })
        .map_err(|error| write_codec::map_error(super::statement_error(&error), |_| None))
}

read_codec::export_operation!(
    super::Component,
    read_contract,
    super::wamn::node::types,
    Connection::new(),
    read,
    read_codec
);
write_codec::export_operation!(
    super::Component,
    write_contract,
    super::wamn::node::types,
    write,
    write_codec
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_settings_differ_from_json_null() {
        assert_eq!(read_value(None).value, None);
        assert_eq!(
            read_value(Some(read_sql::ReadSettingRow {
                value: Json("null".into())
            }))
            .value
            .as_deref(),
            Some("null")
        );
        assert_eq!(
            read_value(Some(read_sql::ReadSettingRow {
                value: Json("\"2026-10-21T12:00:00Z\"".into())
            }))
            .value
            .as_deref(),
            Some("\"2026-10-21T12:00:00Z\"")
        );
    }
}
