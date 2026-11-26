use anyhow::ensure;
use serde_json::Value;

pub(super) fn recovery(status: &str, app: bool) -> Option<Recovery> {
    match status {
        "pending-install" => Some(Recovery::Uninstall),
        "pending-upgrade" | "pending-rollback" => Some(Recovery::Rollback),
        "failed" if app => Some(Recovery::Rollback),
        _ => None,
    }
}

/// Select the newest successful revision before the one being recovered.
pub(super) fn app_recovery_revision(
    history: &[Value],
    interrupted: u32,
) -> anyhow::Result<Option<u32>> {
    let revision = history
        .iter()
        .filter_map(|entry| {
            let description = entry["description"].as_str().unwrap_or_default();
            match entry["status"].as_str() {
                Some("deployed") => {}
                // A failed Helm rollback can mark its predecessor superseded.
                Some("superseded")
                    if matches!(description, "Install complete" | "Upgrade complete")
                        || description.starts_with("Rollback to ") => {}
                _ => return None,
            }
            let revision = u32::try_from(entry["revision"].as_u64()?).ok()?;
            (revision < interrupted).then_some(revision)
        })
        .max();
    ensure!(
        revision.is_some()
            || (interrupted == 1
                && matches!(history, [entry] if entry["revision"] == 1
                    && matches!(entry["status"].as_str(), Some("failed" | "deployed")))),
        "Helm history contains no successful revision to restore"
    );
    Ok(revision)
}

/// What clears an operation that a killed writer left pending or failed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    /// A first install never finished: no revision ever served.
    Uninstall,
    /// An upgrade or rollback never finished. Inspect application history for its target.
    Rollback,
}
