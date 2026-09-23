//! The `widget_tag` model update.

use wamn_postgres_statements::Connection;

use crate::error::{AccessError, AccessErrorKind, Constraints};
use crate::generated::wamn::widget_tag as sql;
use crate::scalar;

#[doc(inline)]
pub use crate::generated::wamn::widget_tag::WidgetTagRow;

const UPDATE: Constraints = Constraints {
    unique: sql::UPDATE_UNIQUE_CONSTRAINTS,
    foreign_key: sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
    check: sql::UPDATE_CHECK_CONSTRAINTS,
};

/// Change one widget tag at the revision the caller last read.
///
/// `label` is `None` to leave the label as it is.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn update(
    connection: &mut Connection,
    id: &str,
    expected_edit_version: i64,
    label: Option<Option<String>>,
) -> Result<WidgetTagRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    if matches!(label, Some(None)) {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "change.label",
        ));
    }
    let row = sql::update(
        connection,
        id.clone(),
        expected_edit_version,
        label.is_some(),
        label.flatten(),
    )
    .await
    .map_err(|error| AccessError::from_statement(&error, UPDATE))?;
    match row.outcome.as_deref() {
        Some("updated") => Ok(WidgetTagRow {
            edit_version: row.edit_version.ok_or_else(AccessError::internal)?,
            id: row.id.ok_or_else(AccessError::internal)?,
            label: row.label.ok_or_else(AccessError::internal)?,
        }),
        Some("not_found") => Err(AccessError::missing(&id.0)),
        Some("concurrency_conflict") => Err(AccessError::conflict(
            expected_edit_version,
            row.observed_edit_version
                .ok_or_else(AccessError::internal)?,
        )),
        _ => Err(AccessError::internal()),
    }
}
