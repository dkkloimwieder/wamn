// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ItemRow {
    pub id: uuid::Uuid,
    pub item_number: String,
}
