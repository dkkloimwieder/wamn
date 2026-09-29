// @generated from the package manifest and migration IR; do not edit.

// The generated `widget_tag` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/widget_tag.rs"
    ));
}

pub use sql::WidgetTagRow;
pub use sql::WidgetTagUpdateRow;

const UPDATE_OPERATION: &str = "widget_tag.update";

/// `widget_tag.update`: change one row at the revision the caller last read.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: &str,
    expected_edit_version: i64,
    label: Option<Option<String>>,
) -> Result<WidgetTagRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    if matches!(label, Some(None)) {
        return Err(Error::invalid("change.label"));
    }
    let label_present = label.is_some();
    let label = label.flatten();
    let row = sql::update(
        transaction,
        id.clone(),
        expected_edit_version,
        label_present,
        label,
    )
    .await
    .map_err(|error| {
        Error::from_statement(
            &error,
            &Constraints {
                unique: sql::UPDATE_UNIQUE_CONSTRAINTS,
                foreign_key: sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
                check: sql::UPDATE_CHECK_CONSTRAINTS,
                exclusion: sql::UPDATE_EXCLUSION_CONSTRAINTS,
            },
            UPDATE_OPERATION,
        )
    })?;
    match row.outcome.as_deref() {
        Some("updated") => Ok(WidgetTagRow {
            edit_version: row.edit_version.ok_or_else(Error::internal)?,
            id: row.id.ok_or_else(Error::internal)?,
            label: row.label.ok_or_else(Error::internal)?,
        }),
        Some("not_found") => Err(Error::not_found(&id)),
        Some("concurrency_conflict") => Err(Error::conflict(
            expected_edit_version,
            row.observed_edit_version.ok_or_else(Error::internal)?,
        )),
        _ => Err(Error::internal()),
    }
}
