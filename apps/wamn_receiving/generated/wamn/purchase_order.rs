// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PurchaseOrderRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub purchase_order_number: String,
    pub row_version: i32,
    pub status: String,
    pub supplier_id: wamn_postgres_statements::Uuid,
    pub updated_at: wamn_postgres_statements::TimestampTz,
    pub updated_by: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct PurchaseOrderUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i32>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub created_by: Option<wamn_postgres_statements::Uuid>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i32>,
    pub status: Option<String>,
    pub supplier_id: Option<wamn_postgres_statements::Uuid>,
    pub updated_at: Option<wamn_postgres_statements::TimestampTz>,
    pub updated_by: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:49a2aa0628387bc2717872be320426a14bb964ba8a657f71095160d81ed9ff77";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:b48e78bb9da8170de66064defded66a5c00cc54a8657bbf99f31426dd1b6cd9b";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:af007d9afb72784344c5228106e107f2b422ee9607ce1d7ef2778dfdf46b2cf1";
pub(crate) const QUERY_2_DIGEST: &str =
    "sha256:5ce76f719b022fb817ec4525209a29387be043b668c9d46c3be4182b2e2753c8";
pub(crate) const QUERY_3_DIGEST: &str =
    "sha256:4422b4c406082f926b9e7e64a5c3a38a014343fc4f2af5bdc0e2c8efc566c295";
pub(crate) const QUERY_4_DIGEST: &str =
    "sha256:0a8fe0095f9ce49032ea72f325e14217cd3ea8893daa3372ec97a45f863a5fb7";
pub(crate) const QUERY_5_DIGEST: &str =
    "sha256:2711394f5fec568dfd0b4009336a8f07a77291a067a2f40b7c0f79d7a0234011";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:7b419d6f23fbd1ed1a111d3bcae64f854245f3b359fcb042dce355741eab330c";

pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &["purchase_order_supplier_id_fkey"];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}

pub(crate) async fn query_purchase_order_number_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    purchase_order_number_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PurchaseOrderRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(purchase_order_number_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PurchaseOrderRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    purchase_order_number: row.decode("purchase_order_number")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    supplier_id: row.decode("supplier_id")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_purchase_order_number_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    purchase_order_number_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PurchaseOrderRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(purchase_order_number_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PurchaseOrderRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    purchase_order_number: row.decode("purchase_order_number")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    supplier_id: row.decode("supplier_id")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_status_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    purchase_order_number_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PurchaseOrderRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(purchase_order_number_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PurchaseOrderRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    purchase_order_number: row.decode("purchase_order_number")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    supplier_id: row.decode("supplier_id")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_status_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    purchase_order_number_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PurchaseOrderRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_3_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(purchase_order_number_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PurchaseOrderRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    purchase_order_number: row.decode("purchase_order_number")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    supplier_id: row.decode("supplier_id")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    purchase_order_number_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PurchaseOrderRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_4_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(purchase_order_number_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PurchaseOrderRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    purchase_order_number: row.decode("purchase_order_number")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    supplier_id: row.decode("supplier_id")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    purchase_order_number_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PurchaseOrderRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_5_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(purchase_order_number_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(PurchaseOrderRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    purchase_order_number: row.decode("purchase_order_number")?,
                    row_version: row.decode("row_version")?,
                    status: row.decode("status")?,
                    supplier_id: row.decode("supplier_id")?,
                    updated_at: row.decode("updated_at")?,
                    updated_by: row.decode("updated_by")?,
                })
            },
        )
        .await
}

pub(crate) async fn update(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i32,
    supplier_id_present: bool,
    supplier_id_value: Option<wamn_postgres_statements::Uuid>,
) -> Result<PurchaseOrderUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_row_version),
                wamn_postgres_statements::into_sql_value(supplier_id_present),
                wamn_postgres_statements::into_sql_value(supplier_id_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(PurchaseOrderUpdateRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}
