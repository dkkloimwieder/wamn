// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PurchaseOrderRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub purchase_order_number: String,
    pub row_version: i64,
    pub status: String,
    pub supplier_id: wamn_postgres_statements::Uuid,
    pub updated_at: wamn_postgres_statements::TimestampTz,
}

#[derive(Debug)]
pub struct PurchaseOrderUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i64>,
    pub status: Option<String>,
    pub supplier_id: Option<wamn_postgres_statements::Uuid>,
    pub updated_at: Option<wamn_postgres_statements::TimestampTz>,
}

pub(crate) const GET_DIGEST: &str = "sha256:2eb6a5c78c23fe93f83c17877b291b537411cce8c055b38b252a17a11dd52873";
pub(crate) const QUERY_0_DIGEST: &str = "sha256:2616468fa644ab572c2ab3d74311dce067b663bffd09fae7907ba29cbbfe3e91";
pub(crate) const QUERY_1_DIGEST: &str = "sha256:8a7ce5edefaed70996ac4182ad191cf892ddcd3178981b249e0511182200c950";
pub(crate) const QUERY_2_DIGEST: &str = "sha256:4b1f38c061676e4b8bd4fc44c01577bea443c38eb1632cbf6a02f1f4aca2369b";
pub(crate) const QUERY_3_DIGEST: &str = "sha256:448b7ddc5b5f2e2dc37ac6db2bdfc320cb2a79e85d98114b23b6146c06aa964d";
pub(crate) const QUERY_4_DIGEST: &str = "sha256:986271d6c6fac8f4e5c0e4d021134926e12ad4387a470e7092efdd675044dfc3";
pub(crate) const QUERY_5_DIGEST: &str = "sha256:18ed39e10175923001444a85136dfe8debfd23d87239c173adc2514953dd4bf1";
pub(crate) const UPDATE_DIGEST: &str = "sha256:ae9bc70de2245821b2776e694c9f169e7bd6dde8895767adeafc42f36f00e71f";

pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(GET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(id),
    ]).await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_purchase_order_number_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_0_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(supplier_id_filter),
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_0_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_purchase_order_number_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_1_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(supplier_id_filter),
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_1_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_status_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_2_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(supplier_id_filter),
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_2_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_status_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_3_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(supplier_id_filter),
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_3_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_4_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(supplier_id_filter),
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_4_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_5_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(supplier_id_filter),
        wamn_postgres_statements::into_sql_value(status_filter),
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_5_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}

pub(crate) async fn update(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i64,
    supplier_id_present: bool,
    supplier_id_value: Option<wamn_postgres_statements::Uuid>,
) -> Result<PurchaseOrderUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection.run(UPDATE_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(id),
        wamn_postgres_statements::into_sql_value(expected_row_version),
        wamn_postgres_statements::into_sql_value(supplier_id_present),
        wamn_postgres_statements::into_sql_value(supplier_id_value),
    ]).await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(PurchaseOrderUpdateRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
        })
    })
}
