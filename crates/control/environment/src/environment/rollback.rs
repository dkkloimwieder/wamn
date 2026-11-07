//! Apply the previous code release while preserving the selected web artifact.

pub(super) mod plan;
pub use plan::successful_newest_first;

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use wamn_control_registry::Triple;

use super::Platform;
use super::analyse::{Analysis, helm};
use super::document::DeclaredRelease;
use super::lock::LifecycleLock;

/// The refusal when no prior release is retained.
pub const NO_RETAINED_PRIOR_RELEASE: &str = "no retained prior release";

/// Restore the previous code release without changing the selected web artifact.
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
        let document = super::show::show(platform, triple).await?;
        let mut document = super::analyse::select_application(&document, None)?;
        let application = document
            .applications
            .keys()
            .next()
            .expect("one application");
        let name = super::analyse::application_release_name(&document, application)?;
        let Some(release) = previous_intended(platform, &name).await? else {
            bail!("{NO_RETAINED_PRIOR_RELEASE} of {triple}; nothing was written");
        };
        // DEP2-09 replaces environment rollback with the authored previous selection.
        for application in document.applications.values_mut() {
            application.current = DeclaredRelease::Digest(release.clone());
        }
        super::apply_document(platform, &document, None, Some(reason)).await
    })
    .await
}

/// The previous intended code release digest, or none.
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
    previous_revision(&history, async |revision| {
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
        serde_json::from_slice(&output.stdout).context("decode helm values")
    })
    .await
}

async fn previous_revision(
    history: &[Value],
    mut read_values: impl AsyncFnMut(u64) -> anyhow::Result<Value>,
) -> anyhow::Result<Option<String>> {
    let mut current = None;
    for revision in successful_newest_first(history) {
        let values = read_values(revision).await?;
        let Some(digest) = values["release"]["manifestDigest"].as_str() else {
            continue;
        };
        let intended = digest.to_owned();
        match &current {
            None => current = Some(intended),
            Some(current) if current != &intended => return Ok(Some(intended)),
            Some(_) => {}
        }
    }
    Ok(None)
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
