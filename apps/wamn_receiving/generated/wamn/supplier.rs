// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct SupplierRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub name: String,
}

pub(crate) const CREATE_DIGEST: &str =
    "sha256:f7d147b2502e8b2ea6a4161bdf377c3ff7b38499a41c42232502327851fca1fb";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:c3b2e06e9e6e007917f55f4a3e48aaddb8008014687da62f64d2df159bba954e";

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] = &["supplier_id_pkey"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<SupplierRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(SupplierRow {
                    created_at: row.decode("created_at")?,
                    id: row.decode("id")?,
                    name: row.decode("name")?,
                })
            },
        )
        .await
}

pub(crate) async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    name: String,
) -> Result<SupplierRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(name)],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_DIGEST, rows, |row| {
        Ok(SupplierRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })
}
