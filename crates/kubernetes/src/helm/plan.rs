use anyhow::ensure;
use serde_json::Value;

pub(super) fn recovery(status: &str, application: bool) -> Option<Recovery> {
    match status {
        "pending-install" => Some(Recovery::Uninstall),
        "pending-upgrade" | "pending-rollback" => Some(Recovery::Rollback),
        "failed" if application => Some(Recovery::Rollback),
        _ => None,
    }
}

/// Select the last successful revision, or uninstall a failed first install.
pub(super) fn application_recovery_revision(history: &[Value]) -> anyhow::Result<Option<u32>> {
    let revision = history
        .iter()
        .filter_map(|entry| {
            let description = entry["description"].as_str().unwrap_or_default();
            let deployed = match entry["status"].as_str() {
                Some("deployed") => true,
                // A failed Helm rollback can mark its predecessor superseded.
                Some("superseded")
                    if matches!(description, "Install complete" | "Upgrade complete")
                        || description.starts_with("Rollback to ") =>
                {
                    false
                }
                _ => return None,
            };
            let revision = u32::try_from(entry["revision"].as_u64()?).ok()?;
            Some((deployed, revision))
        })
        .max()
        .map(|(_, revision)| revision);
    ensure!(
        revision.is_some()
            || matches!(history, [entry] if entry["revision"] == 1 && entry["status"] == "failed"),
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
