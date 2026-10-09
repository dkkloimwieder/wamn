// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PackagingRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub located_at: wamn_postgres_statements::TimestampTz,
    pub location_id: wamn_postgres_statements::Uuid,
    pub packaging_code: String,
    pub row_version: i32,
    pub status: String,
    pub type_: String,
    pub updated_at: wamn_postgres_statements::TimestampTz,
    pub updated_by: wamn_postgres_statements::Uuid,
}

pub(crate) const CREATE_DIGEST: &str =
    "sha256:8be4213d29cd4fbc47391647bffa3452a7dae5364acd5e1b28db4e9783cd451c";
pub(crate) const GET_DIGEST: &str =
    "sha256:9d87126d9ce2c4c7d73bd6e476c0ec81e9b2919ec26a6d6c16ad56d843371aad";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:951987dcabf5e8646ab7fb9b1da105972b0982f798125323983fa962cb1b77a5";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:c007981aadbd47893f4b79f23a2d07da44db99eb0bfb7ed047fdb085a72bcab7";
pub(crate) const QUERY_2_DIGEST: &str =
    "sha256:cfe937e3a1df560adf2e5dbeaf78060da150c04dcd488bb19f3f47826b1ec997";
pub(crate) const QUERY_3_DIGEST: &str =
    "sha256:9b981a537af09c431d3b8471d5b5558fd3c1120d36f794a0f2e819f8e64105a6";
pub(crate) const QUERY_4_DIGEST: &str =
    "sha256:1b1f45069825c66c088498f84c95e52f379abc5f74d133e0796d87d10e640cc9";
pub(crate) const QUERY_5_DIGEST: &str =
    "sha256:6fe52ebe3bded9af8d025b63ca1a2752b411b44fedda344e70f444e8e900c275";
pub(crate) const QUERY_6_DIGEST: &str =
    "sha256:628a50d0808c2ea379b661b4e63f5c75fea084875cec9249773fbcf76e5976ea";
pub(crate) const QUERY_7_DIGEST: &str =
    "sha256:54b517d2244a29dfed4c9928ee3aa5e45a7f41789471b77821f85b36d4dddd17";

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
            located_at: row.decode("located_at")?,
            location_id: row.decode("location_id")?,
            packaging_code: row.decode("packaging_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
                    located_at: row.decode("located_at")?,
                    location_id: row.decode("location_id")?,
                    packaging_code: row.decode("packaging_code")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    type_: row.decode("type")?,
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
    type_: String,
    location_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<PackagingRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_code),
                wamn_postgres_statements::into_sql_value(type_),
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
            located_at: row.decode("located_at")?,
            location_id: row.decode("location_id")?,
            packaging_code: row.decode("packaging_code")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            type_: row.decode("type")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}
