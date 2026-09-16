// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PalletRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub pallet_code: String,
    pub row_version: i64,
    pub status: String,
    pub updated_at: wamn_postgres_statements::TimestampTz,
    pub updated_by: wamn_postgres_statements::Uuid,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:59ee1bf6f48b27780e89935e399a45d07383a480caf7975adbee30c6a531255c";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:ab0f5918c00f290756e66d76b201b6d76ddbed7eb023d11a60759f14aeccbdb3";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:1a9785556de188d8c6ede5f3cd9d6fad6a2dacf2df6c229877fe0859784a5ffb";
pub(crate) const QUERY_2_DIGEST: &str =
    "sha256:66b4eeabd83bdc28632f8e028c84dfd163c2b466a2451e8b49eedde057be85c6";
pub(crate) const QUERY_3_DIGEST: &str =
    "sha256:3d51f113fe0866ed4fe592a0283ce2b0fb4c57415cdeaf1767ad61db0c31b772";
pub(crate) const QUERY_4_DIGEST: &str =
    "sha256:3dade3da3f0a41604364daee48c15066b318d2175caeacb7c9aed48760bed2dd";
pub(crate) const QUERY_5_DIGEST: &str =
    "sha256:b9151b34b566ee8077e1b0e9c772d927941edd99653494040636b0445b440d99";
pub(crate) const QUERY_6_DIGEST: &str =
    "sha256:a626cf7ec2aa2db57149c49c4616025e2c1d0bc44d25c21facb0f660e38ee2dc";
pub(crate) const QUERY_7_DIGEST: &str =
    "sha256:db3edc9b06f2b06e2c1f100132b22ca3c709a0cb1a3866b74fd3e1baec10de81";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PalletRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_0_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_1_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_2_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_3_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_3_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_4_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_4_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_5_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_5_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_6_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_6_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
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
    let rows = connection
        .run(
            QUERY_7_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(pallet_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_7_DIGEST, rows, |row| {
        Ok(PalletRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            pallet_code: row.decode("pallet_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}
