// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ProductRow {
    pub id: uuid::Uuid,
    pub product_code: String,
}
