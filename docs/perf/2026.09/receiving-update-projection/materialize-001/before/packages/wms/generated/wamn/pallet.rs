// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PalletRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub pallet_code: String,
    pub row_version: i64,
    pub status: String,
    pub updated_at: wamn_postgres_statements::TimestampTz,
}

pub(crate) const GET_DIGEST: &str = "sha256:5d1c6823fbfd92249a2aa7f2ef32ae68c7f8dcea402a0445a2268b6c5bbe89b9";
pub(crate) const QUERY_0_DIGEST: &str = "sha256:0ffb462ced2154ea4eb6abcb97c3c6c5551780fca3e4558a88223652f1fdc081";
pub(crate) const QUERY_1_DIGEST: &str = "sha256:12dffafe7a7ad065bdb61b6098cda17f0f4240d85886f9ae4e206baf0f76c7d2";
pub(crate) const QUERY_2_DIGEST: &str = "sha256:7a2804291a43ead7fe73e124e2c16de5b509c8bfb00f4e810bc38a1e08289fce";
pub(crate) const QUERY_3_DIGEST: &str = "sha256:398b50da5833accfdda221902f1b7e11b6f176f28595ed6cddf6c15ea67c70d4";
pub(crate) const QUERY_4_DIGEST: &str = "sha256:96f8476ca0f6d7212461fe9a274ab1da5747704070e461015cac0ffd4c34d375";
pub(crate) const QUERY_5_DIGEST: &str = "sha256:7b234eda921a228673eb91de24df5d4159ec55a65bf5ba0dc8c864e3bd40a0eb";
pub(crate) const QUERY_6_DIGEST: &str = "sha256:bf0d1e3ecbc0c28de1b76e3251676e38c54ba063d30945890c0fba022726f78e";
pub(crate) const QUERY_7_DIGEST: &str = "sha256:c12cb794daff757f02a8df8a91b27e9d1328cd2e4deded765a19fcb0c9aa80d6";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(GET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(id),
    ]).await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_pallet_code_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_0_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_0_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_pallet_code_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_1_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_1_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_location_id_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::Uuid>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_2_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_2_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_location_id_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::Uuid>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_3_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_3_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_updated_at_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_4_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_4_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_updated_at_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_5_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_5_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_6_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_6_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    pallet_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_7_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(location_id_filter),
        wamn_postgres_statements::into_sql_value(pallet_code_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_7_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}
