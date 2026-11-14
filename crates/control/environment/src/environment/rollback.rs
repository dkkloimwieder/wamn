//! Apply a prior published release of one application without reversing SQL.

use super::{Platform, analyse::Analysis, document::ReleaseChange};
use wamn_registry::Triple;

/// Restore the latest successful release after a failed head, or a preceding selection.
pub async fn rollback(
    platform: &Platform,
    triple: &Triple,
    application: &str,
    release: Option<&str>,
) -> anyhow::Result<Analysis> {
    super::apply_selection(
        platform,
        triple,
        application,
        ReleaseChange::Rollback(release),
        None,
    )
    .await
}
