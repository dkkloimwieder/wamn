// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PackagingRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub packaging_code: String,
    pub row_version: i32,
    pub status: String,
    pub r#type: String,
    pub updated_at: wamn_postgres_statements::TimestampTz,
    pub updated_by: wamn_postgres_statements::Uuid,
}

pub(crate) const CREATE_DIGEST: &str =
    "sha256:898a64c23401060fde5706968227d43cb0cef070f6bc02b1633c4506d75c8357";
pub(crate) const GET_DIGEST: &str =
    "sha256:385031f095c3d1af3a821db75cf5b2c39d632d12bd0b16a72a5435e41c5b2ee6";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:c97bbed6b93cfecb816a045ff5cc453d8de820e7d1264a18911d202829ff455a";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:7d1542dc5294c049797dea2f7dd200f77ab9e17828d8bb462659b065c5e30771";
pub(crate) const QUERY_2_DIGEST: &str =
    "sha256:4aeac50d0fb396063002b9e6a701fddd48cba31faec8c9e381c9c37bc8317652";
pub(crate) const QUERY_3_DIGEST: &str =
    "sha256:14626c78b2db531076bbe73ae423d5b125c874b376010e438aad42ddd0f67b30";
pub(crate) const QUERY_4_DIGEST: &str =
    "sha256:b7e19bb1a6d9fe884fdea971c06fee86fe36438253e570dd4ad1c54b339bdcd7";
pub(crate) const QUERY_5_DIGEST: &str =
    "sha256:e0ecb4e8b04a8eba9a43684cd9b575b9586c1b754f4004bee66987e5cf8abf44";
pub(crate) const QUERY_6_DIGEST: &str =
    "sha256:3d82df595efed2bfb853d9c8e9ea0731275b46aa851363dd469c43dedd314588";
pub(crate) const QUERY_7_DIGEST: &str =
    "sha256:e39db5d058c403281ef31b0b1b578073c64872cfb47de0057ce7e8757e010212";

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] =
    &["packaging_id_pkey", "packaging_packaging_code_key"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &["packaging_location_id_fkey"];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] =
    &["packaging_status_check", "packaging_type_check"];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PackagingRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            packaging_code: row.decode("packaging_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            r#type: row.decode("type")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}

pub(crate) async fn query_packaging_code_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_packaging_code_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_location_id_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::Uuid>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_location_id_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::Uuid>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_3_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_updated_at_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_4_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_updated_at_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_5_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_6_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    status_filter: Option<wamn_postgres_statements::Json>,
    location_id_filter: Option<wamn_postgres_statements::Json>,
    packaging_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PackagingRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_7_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(location_id_filter),
                wamn_postgres_statements::into_sql_value(packaging_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PackagingRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    r#type: row.decode("type")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    packaging_code: String,
    r#type: String,
    location_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<PackagingRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_code),
                wamn_postgres_statements::into_sql_value(r#type),
                wamn_postgres_statements::into_sql_value(location_id),
                wamn_postgres_statements::into_sql_value(status),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_DIGEST, rows, |row| {
        Ok(PackagingRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            packaging_code: row.decode("packaging_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            r#type: row.decode("type")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}
