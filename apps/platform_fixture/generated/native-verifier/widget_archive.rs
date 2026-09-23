// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ArchiveRow {
    pub id: uuid::Uuid,
    pub edit_version: i64,
}

pub(crate) const ARCHIVE_SQL: &str = include_str!("../../command/widget/archive.sql");

pub(crate) fn archive_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
