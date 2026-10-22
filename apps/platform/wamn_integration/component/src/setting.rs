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

fn read_value(
    row: Option<read_sql::ReadSettingRow>,
    carry: Option<String>,
) -> read_contract::ReadResult {
    read_contract::ReadResult {
        value: row.map(|row| row.value.0),
        carry,
    }
}

async fn read(
    connection: &mut Connection,
    request: read_contract::ReadRequest,
) -> Result<read_contract::ReadResult, super::NodeError> {
    let row = read_sql::read_setting(connection, request.key)
        .await
        .map_err(super::statement_error)?;
    Ok(read_value(row, request.carry))
}

async fn write(
    transaction: &mut Transaction,
    request: write_contract::WriteRequest,
) -> Result<write_contract::WriteResult, super::NodeError> {
    write_sql::write_setting(transaction, request.key, Json(request.value))
        .await
        .map(|row| write_contract::WriteResult { key: row.key })
        .map_err(super::statement_error)
}

export_operation!(
    read_contract,
    read_codec,
    ReadItem,
    ReadOutcome,
    ReadError,
    Connection::new(),
    read,
    async fn read_json(
        _context: super::wamn::node::types::NodeContext,
        input: String,
        want_wal_position: bool,
    ) -> Result<(super::wamn::node::types::Emission, Option<String>), super::NodeError> {
        let input = read_codec::decode(&input).map_err(|error| super::invalid(error.context()))?;
        read_codec::validate(&input).map_err(|error| super::invalid(error.context()))?;
        let mut connection = Connection::new();
        connection.want_wal_position(want_wal_position);
        let mut failure = None;
        let rows = read_codec::run(input, &mut connection, async |connection, request| {
            read(connection, request).await.map_err(|error| {
                failure = Some(error);
                read_contract::ReadError::InternalError
            })
        })
        .await;
        if let Some(error) = failure {
            return Err(error);
        }
        Ok((
            super::wamn::node::types::Emission {
                payload: read_codec::encode(&rows),
                port: None,
            },
            connection.wal_position().map(str::to_owned),
        ))
    }
);
export_operation!(
    write_contract,
    write_codec,
    WriteItem,
    WriteOutcome,
    WriteError,
    wamn_postgres_statements::operation_transaction()
        .await
        .map_err(super::statement_error)?,
    write
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carry_survives_absent_and_present_settings() {
        for row in [
            None,
            Some(read_sql::ReadSettingRow {
                value: Json("null".into()),
            }),
        ] {
            let carry = Some(r#"{"due":"2026-10-22T12:00:00Z"}"#.to_owned());
            assert_eq!(read_value(row, carry.clone()).carry, carry);
        }
    }

    #[test]
    fn absent_settings_differ_from_json_null() {
        assert_eq!(read_value(None, None).value, None);
        assert_eq!(
            read_value(
                Some(read_sql::ReadSettingRow {
                    value: Json("null".into())
                }),
                None
            )
            .value
            .as_deref(),
            Some("null")
        );
        assert_eq!(
            read_value(
                Some(read_sql::ReadSettingRow {
                    value: Json("\"2026-10-21T12:00:00Z\"".into())
                }),
                None
            )
            .value
            .as_deref(),
            Some("\"2026-10-21T12:00:00Z\"")
        );
    }
}
