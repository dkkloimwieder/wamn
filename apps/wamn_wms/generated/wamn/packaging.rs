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
    "sha256:dd78781bc618e6a43941093f3125c84d05d08da130fed61c75f58076b380d4f7";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:261a46585d86c07743aa0b2b18e72580fd9350bba0c6445bda05181bbf5fe44c";
pub(crate) const QUERY_2_DIGEST: &str =
    "sha256:3fd059dd269f0cbdb8f3bb63594e59a4303788386ea3009dc27f418d588e9667";
pub(crate) const QUERY_3_DIGEST: &str =
    "sha256:d4e87b195e88f215ec2d05da117098b13c1dc5050b1b868a315bf84b1e980c53";
pub(crate) const QUERY_4_DIGEST: &str =
    "sha256:48d7e16b45689ec2b1930d56731968b4343851e617f7199da2ecfd3827342da2";
pub(crate) const QUERY_5_DIGEST: &str =
    "sha256:707c7cf94de40bbe5afc22e7b1384d0a4e023a3edc4e4a635aec3d21f3163e1c";
pub(crate) const QUERY_6_DIGEST: &str =
    "sha256:0e4a32edfb28f00dab95d182c104a2a9f63a98d4a8aa6d2d2b2b6c13630b304f";
pub(crate) const QUERY_7_DIGEST: &str =
    "sha256:b71f3e85339254c53dc5bf309935823f61ad5d589f4d5eb11e8667ed7cacdf78";

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
