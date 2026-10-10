//! `env rollback <coordinate> --reason <text>`: `apply` of the previous
//! intended release (docs/plan/platform-deploy.md §11.2, R8).
//!
//! The previous intended release is the newest `helm history` revision whose
//! status is `deployed` or `superseded` and whose digest differs from the
//! current one; `failed` and `pending-*` revisions are skipped. The verb
//! synthesizes the environment's document from the authorities, as `show`
//! does, with that digest as `release`, and runs `apply` on it under the lock
//! it holds, with the reason in the Helm description. `helm rollback` is never
//! run. With no such revision the verb refuses and writes nothing.

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use wamn_control_registry::Triple;

use super::Platform;
use super::analyse::{Analysis, helm};
use super::document::DeclaredRelease;
use super::lock::LifecycleLock;
use crate::release_chart;

/// The refusal when no prior release is retained.
pub const NO_RETAINED_PRIOR_RELEASE: &str = "no retained prior release";

/// Roll the environment back to its previous intended release.
///
/// # Errors
///
/// When the lock is held, no prior release is retained, or `apply` fails.
pub async fn rollback(
    platform: &Platform,
    triple: &Triple,
    reason: &str,
) -> anyhow::Result<Analysis> {
    ensure!(!reason.trim().is_empty(), "--reason is empty");
    LifecycleLock::hold(&platform.system_database_url, triple, async |_| {
        let name = release_chart::release_name(&triple.org, &triple.project, triple.env.as_str())?;
        let Some(previous) = previous_intended(platform, &name).await? else {
            bail!("{NO_RETAINED_PRIOR_RELEASE} of {triple}; nothing was written");
        };
        let mut document = super::show::show(platform, triple).await?;
        document.release = DeclaredRelease::Digest(previous);
        super::apply_locked(platform, &document, Some(reason)).await
    })
    .await
}

/// The digest of the previous intended release, or none.
async fn previous_intended(platform: &Platform, name: &str) -> anyhow::Result<Option<String>> {
    let output = helm(platform, &["history", name, "--max", "50", "-o", "json"]).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            stderr.contains("not found"),
            "helm history {name} exited {}: {stderr}",
            output.status
        );
        return Ok(None);
    }
    let history: Vec<Value> =
        serde_json::from_slice(&output.stdout).context("decode helm history")?;
    let mut current = None;
    for revision in successful_newest_first(&history) {
        let number = revision.to_string();
        let output = helm(
            platform,
            &["get", "values", name, "--revision", &number, "-o", "json"],
        )
        .await?;
        ensure!(
            output.status.success(),
            "helm get values {name} --revision {number} exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let values: Value = serde_json::from_slice(&output.stdout).context("decode helm values")?;
        let Some(digest) = values["release"]["manifestDigest"].as_str() else {
            continue;
        };
        match &current {
            None => current = Some(digest.to_owned()),
            Some(current) if current != digest => return Ok(Some(digest.to_owned())),
            Some(_) => {}
        }
    }
    Ok(None)
}

/// The revisions whose status is `deployed` or `superseded`, newest first.
pub fn successful_newest_first(history: &[Value]) -> Vec<u64> {
    let mut revisions: Vec<u64> = history
        .iter()
        .filter(|entry| matches!(entry["status"].as_str(), Some("deployed" | "superseded")))
        .filter_map(|entry| entry["revision"].as_u64())
        .collect();
    revisions.sort_unstable_by(|a, b| b.cmp(a));
    revisions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failed_and_pending_revisions_are_never_a_rollback_target() {
        let history = vec![
            json!({"revision": 1, "status": "superseded"}),
            json!({"revision": 2, "status": "superseded"}),
            json!({"revision": 3, "status": "failed"}),
            json!({"revision": 4, "status": "deployed"}),
            json!({"revision": 5, "status": "pending-upgrade"}),
        ];
        assert_eq!(successful_newest_first(&history), [4, 2, 1]);
        assert!(successful_newest_first(&history[2..3]).is_empty());
    }
}
