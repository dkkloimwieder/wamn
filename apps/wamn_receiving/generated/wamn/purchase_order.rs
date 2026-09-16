// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PurchaseOrderRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub purchase_order_number: String,
    pub row_version: i64,
    pub status: String,
    pub supplier_id: wamn_postgres_statements::Uuid,
    pub updated_at: wamn_postgres_statements::TimestampTz,
    pub updated_by: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct PurchaseOrderUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub created_by: Option<wamn_postgres_statements::Uuid>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i64>,
    pub status: Option<String>,
    pub supplier_id: Option<wamn_postgres_statements::Uuid>,
    pub updated_at: Option<wamn_postgres_statements::TimestampTz>,
    pub updated_by: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:49a2aa0628387bc2717872be320426a14bb964ba8a657f71095160d81ed9ff77";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:9bd3e9f80676150bbc60545e5409ba75c886184129dbf35fb2bf49676f5edae3";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:7b2a4336071091e8271cee0a59abd007465dd1f302c7565de4649f2cdd12ba38";
pub(crate) const QUERY_2_DIGEST: &str =
    "sha256:ba31a18e45b651d8897812a8ba261392745ed4fbbf1725f574d7dffd7444f6fd";
pub(crate) const QUERY_3_DIGEST: &str =
    "sha256:c39f74894247cfa16c18acb8c43c463b7bd37f39a53b76f325c05b1b04b2a177";
pub(crate) const QUERY_4_DIGEST: &str =
    "sha256:69e14cbd3cc603aeb3bb33fbf82c6499b952454cbddac8103d547b9c38555127";
pub(crate) const QUERY_5_DIGEST: &str =
    "sha256:b00b6afd3a821a4e0ae4cb67371453bb122863c690015753ddfe8e5a46a63cf8";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:3fa55cbeab65f425936a22bc3fe824b683c80a80c837a3ac616587ee42f85cec";

pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
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
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_0_DIGEST, rows, |row| {
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

pub(crate) async fn query_purchase_order_number_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_1_DIGEST, rows, |row| {
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

pub(crate) async fn query_status_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_2_DIGEST, rows, |row| {
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

pub(crate) async fn query_status_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_3_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_3_DIGEST, rows, |row| {
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

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_4_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_4_DIGEST, rows, |row| {
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

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    supplier_id_filter: Option<wamn_postgres_statements::Json>,
    status_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_5_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(supplier_id_filter),
                wamn_postgres_statements::into_sql_value(status_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_5_DIGEST, rows, |row| {
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

pub(crate) async fn update(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i64,
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
