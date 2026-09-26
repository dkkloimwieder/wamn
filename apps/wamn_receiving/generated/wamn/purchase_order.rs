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
    "sha256:1abdbcba49c4af65f9fea47e01316cd287fbeb21fb9b837c76bdf187483b5ac6";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:0c43d1689be1795b1a001078d344c0389f59333caef929776a69d573f4825d79";
pub(crate) const QUERY_2_DIGEST: &str =
    "sha256:e7dfab6733e3af8eb29f361ba5dce9be32cc8a45ca4ef57b99a59dd18dda574e";
pub(crate) const QUERY_3_DIGEST: &str =
    "sha256:6b87adb2903c7c95af114b60c4d880a977c5798355f98b58f2e1225e87468544";
pub(crate) const QUERY_4_DIGEST: &str =
    "sha256:7549edaba8f6ca79d2b5f02db8aadf29d247f5f017a68b312d742a1f3d8368ac";
pub(crate) const QUERY_5_DIGEST: &str =
    "sha256:bce5c7f01ce47ff857c9018860832280a7837a749f9aaad363ecf8cb9268ee01";
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
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i32,
    supplier_id_present: bool,
    supplier_id_value: Option<wamn_postgres_statements::Uuid>,
) -> Result<PurchaseOrderUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection
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
