// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ListLocationsRow {
    pub id: uuid::Uuid,
    pub location_code: String,
}

pub(crate) const LIST_LOCATIONS_SQL: &str = include_str!("../../query/location.sql");
