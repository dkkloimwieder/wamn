//! Apply the previous authored release of one application without reversing SQL.

use super::{Platform, analyse::Analysis, document::SelectionChange};
use std::path::Path;

/// Swap the application's current and previous selections, then apply its release.
pub async fn rollback(
    platform: &Platform,
    file: &Path,
    application: &str,
) -> anyhow::Result<Analysis> {
    super::apply_selection(platform, file, application, SelectionChange::Rollback).await
}
