//! `env rollback <coordinate> --reason <text>`: `apply` of the previous
//! intended release and client pin (docs/architecture/overview.md#environment-lifecycle).
//!
//! The previous intended revision is the newest `helm history` revision whose
//! status is `deployed` or `superseded` and whose release digest and client pin
//! pair differs from the current pair. Failed and pending revisions are skipped.
//! A web-only change can keep the same release digest. The verb
//! synthesizes the environment's document from the authorities, as `show`
//! does, then restores that revision's `release` and `client`.
//! An absent client clears the current pin. It runs `apply` under the lifecycle
//! lock, with the reason in the Helm description. It never runs `helm rollback`.
//! With no such revision, the verb refuses and writes nothing.

pub(super) mod plan;
pub use plan::successful_newest_first;

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use wamn_control_registry::Triple;

use super::Platform;
use super::analyse::{Analysis, helm};
use super::document::{Client, DeclaredRelease};
use super::lock::LifecycleLock;

/// The refusal when no prior release is retained.
pub const NO_RETAINED_PRIOR_RELEASE: &str = "no retained prior release";

/// Restore the environment's previous intended release and client pin.
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
        let Some((release, client)) =
            previous_intended(platform, &name, document.legacy_hostname()).await?
        else {
            bail!("{NO_RETAINED_PRIOR_RELEASE} of {triple}; nothing was written");
        };
        // DEP2-09 replaces environment rollback with the authored previous selection.
        for application in document.applications.values_mut() {
            application.current = DeclaredRelease::Digest(release.clone());
            application.client.clone_from(&client);
        }
        super::apply_document(platform, &document, None, Some(reason)).await
    })
    .await
}

/// The previous intended release digest and client pin, or none.
async fn previous_intended(
    platform: &Platform,
    name: &str,
    host: &str,
) -> anyhow::Result<Option<(String, Option<Client>)>> {
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
    previous_revision(&history, host, async |revision| {
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
    host: &str,
    mut read_values: impl AsyncFnMut(u64) -> anyhow::Result<Value>,
) -> anyhow::Result<Option<(String, Option<Client>)>> {
    let mut current = None;
    for revision in successful_newest_first(history) {
        let values = read_values(revision).await?;
        let Some(digest) = values["release"]["manifestDigest"].as_str() else {
            continue;
        };
        let intended = (
            digest.to_owned(),
            super::show::revision_client(&values, host)?,
        );
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

    const HOST: &str = "wms.example.com";

    fn client(web: &str) -> Client {
        Client {
            package: "wms".into(),
            web: wamn_execution_contract::bytes_sha256(web.as_bytes()),
        }
    }

    fn revision(number: u64, status: &str, release: &str, client: Option<Client>) -> Value {
        let mut values = json!({"release": {"manifestDigest": release}});
        if let Some(client) = client {
            values["webClients"] = json!({HOST: client.package});
            values["webDigests"] = json!({HOST: client.web});
        }
        json!({"revision": number, "status": status, "values": values})
    }

    async fn previous(history: &[Value]) -> anyhow::Result<Option<(String, Option<Client>)>> {
        previous_revision(history, HOST, async |number| {
            Ok(history
                .iter()
                .find(|entry| entry["revision"] == number)
                .expect("the revision is in history")["values"]
                .clone())
        })
        .await
    }

    #[tokio::test]
    async fn web_only_rollback_restores_the_previous_web_with_the_same_release() {
        let release = wamn_execution_contract::bytes_sha256(b"release A");
        let history = [
            revision(2, "superseded", &release, Some(client("web 1"))),
            revision(5, "deployed", &release, Some(client("web 2"))),
            revision(6, "pending-upgrade", &release, Some(client("pending web"))),
            revision(1, "superseded", &release, Some(client("older web"))),
            revision(4, "failed", &release, Some(client("failed web"))),
            revision(3, "superseded", &release, Some(client("web 2"))),
        ];
        assert_eq!(
            previous(&history).await.unwrap(),
            Some((release, Some(client("web 1"))))
        );
    }

    #[tokio::test]
    async fn release_rollback_restores_that_revisions_web_pin() {
        let a = wamn_execution_contract::bytes_sha256(b"release A");
        let b = wamn_execution_contract::bytes_sha256(b"release B");
        let history = [
            revision(1, "superseded", &a, Some(client("web 1"))),
            revision(2, "deployed", &b, Some(client("web 2"))),
        ];
        assert_eq!(
            previous(&history).await.unwrap(),
            Some((a, Some(client("web 1"))))
        );
    }

    #[tokio::test]
    async fn rollback_restores_an_absent_client_and_can_restore_a_removed_client() {
        let release = wamn_execution_contract::bytes_sha256(b"release");
        for (prior, current) in [(None, Some(client("web"))), (Some(client("web")), None)] {
            let history = [
                revision(1, "superseded", &release, prior.clone()),
                revision(2, "deployed", &release, current),
            ];
            assert_eq!(
                previous(&history).await.unwrap(),
                Some((release.clone(), prior))
            );
        }
    }

    #[tokio::test]
    async fn identical_pairs_have_no_rollback_target() {
        let release = wamn_execution_contract::bytes_sha256(b"release");
        for client in [None, Some(client("web"))] {
            let history = [
                revision(1, "superseded", &release, client.clone()),
                revision(2, "deployed", &release, client),
            ];
            assert_eq!(previous(&history).await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn rollback_refuses_an_incomplete_client_pin() {
        let release = wamn_execution_contract::bytes_sha256(b"release");
        let mut history = [
            revision(1, "superseded", &release, Some(client("web 1"))),
            revision(2, "deployed", &release, Some(client("web 2"))),
        ];
        history[0]["values"]
            .as_object_mut()
            .unwrap()
            .remove("webDigests");
        assert!(
            previous(&history)
                .await
                .unwrap_err()
                .to_string()
                .contains(HOST)
        );
    }

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
