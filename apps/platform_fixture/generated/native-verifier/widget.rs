// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetRow {
    pub code: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub edit_version: i64,
    pub id: uuid::Uuid,
    pub maker_id: Option<uuid::Uuid>,
    pub note: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetCreateClaimRow {
    pub widget_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetCreateReplayRow {
    pub canonical_command: Vec<u8>,
    pub code: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub edit_version: i64,
    pub id: uuid::Uuid,
    pub maker_id: Option<uuid::Uuid>,
    pub note: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetUpdateRow {
    pub outcome: Option<String>,
    pub observed_edit_version: Option<i64>,
    pub code: Option<String>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub edit_version: Option<i64>,
    pub id: Option<uuid::Uuid>,
    pub maker_id: Option<uuid::Uuid>,
    pub note: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetDeleteRow {
    pub outcome: Option<String>,
}

pub(crate) const CREATE_0_SQL: &str = include_str!("../sql/widget/create_claim.sql");
pub(crate) const CREATE_1_SQL: &str = include_str!("../sql/widget/create_replay.sql");
pub(crate) const CREATE_2_SQL: &str = include_str!("../sql/widget/create.sql");
pub(crate) const DELETE_SQL: &str = include_str!("../sql/widget/delete.sql");
pub(crate) const GET_SQL: &str = include_str!("../sql/widget/get.sql");
pub(crate) const QUERY_0_SQL: &str = include_str!("../../query/widget.sql");
pub(crate) const QUERY_1_SQL: &str =
    include_str!("../../query/widget_by_created_at_descending.sql");
pub(crate) const UPDATE_SQL: &str = include_str!("../sql/widget/update.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn query_created_at_ascending_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_ascending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_created_at_descending_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn create_claim_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_claim_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn create_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_maker_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn create_note_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn update_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_expected_edit_version_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn update_code_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_code_value_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn update_maker_id_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_maker_id_value_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn update_note_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_note_value_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn delete_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn delete_expected_edit_version_bind_fixture() -> i64 {
    0_i64
}
