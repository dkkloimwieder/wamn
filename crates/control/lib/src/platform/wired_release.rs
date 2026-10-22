//! Gate, author, and publish the wirings of a release inside one Job pod.

use std::path::Path;

use anyhow::Context as _;
use wamn_authoring_model::{
    AuthoringCommand, AuthoringDocument, AuthoringRequest, AuthoringRequestEnvelope,
    AuthoringScope, Gate, SCHEMA_VERSION,
};
use wamn_catalog::PackageCoordinate;

/// Build the authoring request for a package's resolved wiring document.
///
/// # Errors
///
/// Returns an error for an unreadable document or a missing wiring identity.
pub fn gate_request(
    package: &PackageCoordinate,
    project: &str,
    environment: &str,
    path: &Path,
) -> anyhow::Result<AuthoringDocument> {
    let document = wamn_schema_generator::operation_reference::read_authored_document(
        path,
        wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
    )?;
    let wiring_id = document["wiring-id"]
        .as_str()
        .context("the wiring document names no wiring-id")?;
    Ok(AuthoringDocument::Request(Box::new(
        AuthoringRequestEnvelope::Command(AuthoringRequest {
            schema_version: SCHEMA_VERSION.to_owned(),
            command_id: format!(
                "gate-{}-{}-{wiring_id}",
                package.package_id(),
                package.package_version()
            ),
            command: AuthoringCommand::Gate(Gate {
                scope: AuthoringScope {
                    project_id: project.into(),
                    environment: environment.into(),
                },
                package_id: package.package_id().into(),
                package_version: package.package_version().into(),
                document,
            }),
        }),
    )))
}

/// Inputs for the one-process mint, gate, authorship, and publication operation.
pub struct PublishWiredReleaseRequest {
    pub release: crate::publish_release::PublishReleaseRequest,
    pub project: String,
    pub issuer: crate::pat_client::PatIssuerConfig,
    pub registry: crate::package_artifact::PackageRegistry,
    pub release_artifact_base: String,
}

impl std::fmt::Debug for PublishWiredReleaseRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishWiredReleaseRequest")
            .finish_non_exhaustive()
    }
}

/// Mint in memory, gate every wiring, author it, and publish the release.
///
/// # Errors
///
/// Refuses a failed gate or publication. Diagnostics exclude private inputs.
pub async fn publish_wired_release(
    mut request: PublishWiredReleaseRequest,
) -> anyhow::Result<String> {
    use crate::release_composition::{PackageInput, load_wirings};
    use std::time::Duration;

    anyhow::ensure!(
        request.release.wirings.is_empty(),
        "wired release derives its wirings from the package artifacts"
    );
    let triple = wamn_control_registry::Triple::new(
        &request.release.org,
        &request.project,
        request.release.environment.as_str(),
    );
    let pat = crate::provision_project_env::mint_management_author_pat(
        &request.release.control_database_url,
        &request.issuer,
        &triple,
    )
    .await
    .map_err(|_| anyhow::anyhow!("mint management-author PAT failed"))?;
    request.release.verified_publisher_principal = format!(
        "wamn-management-author-{}--{}--{}",
        triple.org, triple.project, triple.env
    );
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .build()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        if http
            .get("http://127.0.0.1:18090/")
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "authoring sidecar did not become ready"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    for package in &request.release.packages {
        let opened = crate::package_artifact::open_package(
            &request.registry,
            package.package_id(),
            package.package_version(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("read wired release package failed"))?;
        let input = PackageInput {
            root: opened.root().to_owned(),
            manifest: wamn_schema_generator::PackageManifest::from_slice(&opened.manifest()?)?,
        };
        for wiring in load_wirings(&[input])? {
            let body = gate_request(
                package,
                &request.project,
                &request.release.environment,
                &wiring.path,
            )?;
            let reply = http
                .post("http://127.0.0.1:18090/authoring")
                .bearer_auth(&pat.token)
                .json(&body)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("authoring gate request failed"))?;
            anyhow::ensure!(
                reply.status().is_success(),
                "authoring gate HTTP request refused"
            );
            let reply = reply
                .json::<serde_json::Value>()
                .await
                .map_err(|_| anyhow::anyhow!("authoring gate returned an invalid response"))?;
            require_completed(&body, reply)?;
            crate::author_wiring::author_wiring_in_databases(
                &request.release.database_url,
                &request.release.control_database_url,
                &request.registry,
                &request.release.tenant,
                package.package_id(),
                package.package_version(),
                &wiring.wiring,
            )
            .await
            .map_err(|_| anyhow::anyhow!("author wiring failed"))?;
            request
                .release
                .wirings
                .push(crate::publish_release::ReleaseWiringTarget {
                    package_id: package.package_id().into(),
                    package_version: package.package_version().into(),
                    wiring_id: wiring.wiring.wiring_id.clone(),
                    wiring_version: wiring.wiring.version,
                });
        }
    }
    anyhow::ensure!(
        !request.release.wirings.is_empty(),
        "wired release has no wiring"
    );
    drop(pat);
    let digest = crate::publish_release::publish_release(
        request.release,
        &request.registry,
        &request.release_artifact_base,
    )
    .await
    .context("publish wired release failed")?;
    Ok(digest.to_string())
}

fn require_completed(request: &AuthoringDocument, reply: serde_json::Value) -> anyhow::Result<()> {
    use wamn_authoring_model::{AuthoringOutcome, AuthoringResponseEnvelope, AuthoringSuccess};
    let AuthoringDocument::Request(request) = request else {
        anyhow::bail!("authoring gate requires a request");
    };
    let AuthoringRequestEnvelope::Command(request) = request.as_ref() else {
        anyhow::bail!("authoring gate requires a command");
    };
    let reply: AuthoringDocument = serde_json::from_value(reply)
        .map_err(|_| anyhow::anyhow!("authoring gate returned an invalid response"))?;
    if let AuthoringDocument::Response(reply) = reply
        && let AuthoringResponseEnvelope::Command(reply) = *reply
        && reply.command_id == request.command_id
        && reply.schema_version == SCHEMA_VERSION
        && let AuthoringOutcome::Completed(success) = reply.outcome
        && matches!(*success, AuthoringSuccess::Gate(_))
    {
        return Ok(());
    }
    anyhow::bail!("authoring gate did not complete");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_completed_gate_for_this_command_allows_publication() {
        let request: AuthoringDocument = serde_json::from_value(json!({
            "document": "request",
            "body": {
                "schema-version": SCHEMA_VERSION,
                "command-id": "gate-package-1.0.0-wiring",
                "command": {
                    "type": "gate",
                    "input": {
                        "scope": {"project-id": "project", "environment": "dev"},
                        "package-id": "package",
                        "package-version": "1.0.0",
                        "document": {}
                    }
                }
            }
        }))
        .unwrap();
        let reply = json!({
            "document": "response",
            "body": {
                "schema-version": SCHEMA_VERSION,
                "command-id": "gate-package-1.0.0-wiring",
                "outcome": {
                    "status": "completed",
                    "value": {
                        "command": "gate",
                        "result": {
                            "report-id": "hash",
                            "validated-draft": {"validated-draft-id": "hash"}
                        }
                    }
                }
            }
        });
        require_completed(&request, reply.clone()).unwrap();
        for (field, value) in [
            ("command-id", json!("another-command")),
            ("schema-version", json!("unknown")),
            (
                "outcome",
                json!({"status": "refused", "private": "sentinel-token"}),
            ),
            ("outcome", json!({"status": "completed"})),
            ("outcome", json!({"status": "pending"})),
        ] {
            let mut invalid = reply.clone();
            invalid["body"][field] = value;
            let error = require_completed(&request, invalid).unwrap_err();
            assert!(!format!("{error:#}").contains("sentinel-token"));
        }
        assert!(require_completed(&request, serde_json::to_value(&request).unwrap()).is_err());
    }
}
