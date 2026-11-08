//! Apply a prior published release of one application without reversing SQL.

use super::{Platform, analyse::Analysis, document::SelectionChange};
use std::path::Path;

/// Select an unambiguous prior release, or an explicit target, then apply it.
pub async fn rollback(
    platform: &Platform,
    file: &Path,
    application: &str,
    release: Option<&str>,
) -> anyhow::Result<Analysis> {
    super::apply_selection(
        platform,
        file,
        application,
        SelectionChange::Rollback(release),
    )
    .await
}
