//! Explicit release selection and serialized Kubernetes deployment.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::Value;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{ServingManifest, ServingRelease, WiringActivationFacts};
use wamn_runtime::release_manifest_source::ReleaseManifestSource;

use super::{Qualification, publication};
use crate::print_release_env::ReleaseSnapshot;
use crate::push_release_manifest::PushReleaseManifestRequest;

const SELECT_HEAD: &str = "SELECT effective_release_id FROM catalog.effective_release_heads WHERE tenant_id = $1 AND environment = $2 FOR UPDATE";
const SELECT_RELEASE: &str = "INSERT INTO catalog.effective_release_heads (tenant_id, environment, effective_release_id) VALUES ($1, $2, $3) ON CONFLICT (tenant_id, environment) DO UPDATE SET effective_release_id = EXCLUDED.effective_release_id, updated_at = now()";
const DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(900);

/// Exact Kubernetes inputs of one deployment into an explicitly named environment.
#[derive(Clone, Debug)]
pub struct DeployRequest {
    pub kubeconfig: PathBuf,
    pub context: String,
    pub namespace: String,
    /// Existing HTTP WorkloadDeployment that must become ready before the application request.
    pub http_workload: Option<String>,
    /// Existing rendered native Kubernetes Deployment JSON for the host.
    pub host_deployment: PathBuf,
    /// Required when the qualified candidate includes an executor image.
    pub executor_deployment: Option<PathBuf>,
    pub identity_deployment: Option<PathBuf>,
    pub principal: String,
    /// A released POST route reached through this deployment's ingress.
    pub interaction_url: String,
    pub route_host: String,
    pub request_body: PathBuf,
    pub expected_response: PathBuf,
    /// Private credential file, excluded from qualified artifacts and output.
    pub bearer_file: PathBuf,
}

/// Release one environment now selects, with the digest that selection names.
#[derive(Clone, Debug)]
pub struct SelectedRelease {
    pub release: ServingRelease,
    pub manifest_digest: String,
}

/// Release one environment now serves, with the source that qualified it.
#[derive(Clone, Debug)]
pub struct DeployedRelease {
    pub source_commit: String,
    pub release: ServingRelease,
    pub manifest_digest: String,
}

/// Select one published release for its environment without deploying it.
pub async fn select(
    qualification: &Path,
    release: &PushReleaseManifestRequest,
) -> anyhow::Result<SelectedRelease> {
    let qualification = Qualification::read(qualification)?;
    let snapshot = publication::checked_snapshot(&qualification, release).await?;
    publication::require_published(&qualification, &snapshot, release).await?;
    let (mut client, connection) = tokio_postgres::connect(&release.database_url, NoTls).await?;
    let connection = tokio::spawn(connection);
    let result = async {
        let transaction = client.transaction().await?;
        claim(&transaction, &snapshot.manifest.release).await?;
        // This existing row also serializes an older promote command's upsert.
        transaction
            .execute(
                SELECT_RELEASE,
                &[
                    &release.tenant,
                    &snapshot.manifest.release.environment,
                    &i32::try_from(release.effective_release_id)?,
                ],
            )
            .await?;
        transaction.commit().await?;
        Ok(SelectedRelease {
            release: snapshot.manifest.release.clone(),
            manifest_digest: snapshot.carrier.manifest_digest.as_str().to_owned(),
        })
    }
    .await;
    connection.abort();
    result
}

/// Deploy exact qualified artifacts while the selection remains current.
///
/// Cancellation is the caller's: this future holds the selection row lock, so
/// dropping it rolls the whole deployment transaction back.
pub async fn deploy_release(
    qualification: &Path,
    release: &PushReleaseManifestRequest,
    request: &DeployRequest,
) -> anyhow::Result<DeployedRelease> {
    ensure!(
        request.kubeconfig.is_absolute() && request.kubeconfig.is_file(),
        "deployment requires an explicit existing kubeconfig"
    );
    ensure!(
        !request.context.is_empty()
            && !request.namespace.is_empty()
            && !request.principal.is_empty(),
        "deployment requires context, namespace, and principal"
    );
    if let Some(workload) = &request.http_workload {
        ensure!(
            !workload.is_empty()
                && workload
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "the HTTP workload must be a Kubernetes resource name"
        );
    }
    let qualification = Qualification::read(qualification)?;
    let snapshot = publication::checked_snapshot(&qualification, release).await?;
    publication::require_published(&qualification, &snapshot, release).await?;
    let source = ReleaseManifestSource::new(
        &release.artifact_base,
        release.insecure_registry,
        &release.registry_auth_file,
    )?
    .with_ca_paths(&release.oci_ca_paths)?;
    let pulled = source
        .pull_verified(snapshot.carrier.manifest_digest.as_str())
        .await?;
    ensure!(
        pulled == snapshot.manifest.canonical_bytes(),
        "published artifact differs from the qualified snapshot"
    );
    let mut documents = vec![deployment_document(
        &qualification,
        &request.host_deployment,
        request,
        &snapshot,
        &qualification.candidate.host_image,
        "host",
    )?];
    for (role, image, path) in [
        (
            "executor",
            &qualification.candidate.executor_image,
            &request.executor_deployment,
        ),
        (
            "identity",
            &qualification.candidate.identity_image,
            &request.identity_deployment,
        ),
    ] {
        match (image, path) {
            (Some(image), Some(path)) => documents.push(deployment_document(
                &qualification,
                path,
                request,
                &snapshot,
                image,
                role,
            )?),
            (None, None) => {}
            (Some(_), None) if role == "identity" => {}
            _ => anyhow::bail!("{role} deployment and qualified image must be supplied together"),
        }
    }
    let body = pinned_bytes(&qualification, &request.request_body)?;
    let expected: Value =
        serde_json::from_slice(&pinned_bytes(&qualification, &request.expected_response)?)?;
    require_released_route(&snapshot.manifest, &request.interaction_url, &request.route_host)?;
    let token = fs::read_to_string(&request.bearer_file)
        .context("read the private deployment caller credential")?;
    ensure!(
        !token.trim().is_empty(),
        "the deployment caller credential is empty"
    );
    ensure!(
        !qualification
            .artifact_hashes
            .contains_key(&request.bearer_file),
        "caller credentials must not be qualified artifacts"
    );
    let (mut client, connection) = tokio_postgres::connect(&release.database_url, NoTls)
        .await
        .context("connect to the selected deployment database")?;
    let mut connection = tokio::spawn(connection);
    let result = tokio::select! {
        result = tokio::time::timeout(DEPLOYMENT_TIMEOUT,
            deploy(&mut client, &qualification, &snapshot, request, &documents, body, expected, token.trim())) =>
            result.context("the deployment exceeded its time bound").and_then(|result| result),
        _ = &mut connection => Err(anyhow::anyhow!("the deployment database connection ended; activation was canceled")),
    };
    connection.abort();
    result
}

async fn deploy(
    client: &mut Client,
    qualification: &Qualification,
    snapshot: &ReleaseSnapshot,
    args: &DeployRequest,
    documents: &[Value],
    request: Vec<u8>,
    expected: Value,
    token: &str,
) -> anyhow::Result<DeployedRelease> {
    let transaction = client.transaction().await?;
    claim(&transaction, &snapshot.manifest.release).await?;
    require_selected(&transaction, &snapshot.manifest.release).await?;
    require_compatible_schema(&transaction, &snapshot.manifest).await?;
    qualification.assert_artifacts()?;
    // Keep the selection row locked through workload readiness, authenticated
    // execution, and the catalog activation commit. No task reselects itself.
    for document in documents {
        kubectl(
            args,
            &["apply", "-f", "-"],
            Some(serde_json::to_vec(document)?),
        )
        .await?;
    }
    for document in documents {
        let name = document["metadata"]["name"]
            .as_str()
            .context("deployment has a name")?;
        kubectl(
            args,
            &[
                "rollout",
                "status",
                &format!("deployment/{name}"),
                "--timeout=300s",
            ],
            None,
        )
        .await?;
        let actual: Value = serde_json::from_slice(
            &kubectl(args, &["get", "deployment", name, "-o", "json"], None).await?,
        )?;
        require_no_other_containers(&actual)?;
        require_supplied_fields(
            document
                .pointer("/spec/template/spec/containers")
                .context("qualified containers")?,
            actual
                .pointer("/spec/template/spec/containers")
                .context("ready containers")?,
        )?;
    }
    if let Some(workload) = &args.http_workload {
        kubectl(
            args,
            &[
                "wait",
                "--for=condition=Ready",
                &format!("workloaddeployment/{workload}"),
                "--timeout=300s",
            ],
            None,
        )
        .await?;
    }
    authenticated_interaction(
        &args.interaction_url,
        &args.route_host,
        token,
        request,
        expected,
    )
    .await?;
    qualification.assert_artifacts()?;
    require_selected(&transaction, &snapshot.manifest.release).await?;
    activate(&transaction, &snapshot.manifest, &args.principal).await?;
    transaction.commit().await?;
    Ok(DeployedRelease {
        source_commit: qualification.source_commit.clone(),
        release: snapshot.manifest.release.clone(),
        manifest_digest: snapshot.carrier.manifest_digest.as_str().to_owned(),
    })
}

async fn claim(transaction: &Transaction<'_>, release: &ServingRelease) -> anyhow::Result<()> {
    transaction
        .query_one(
            "SELECT set_config('app.tenant', $1, true)",
            &[&release.tenant_id],
        )
        .await?;
    Ok(())
}

async fn require_selected(
    transaction: &Transaction<'_>,
    release: &ServingRelease,
) -> anyhow::Result<()> {
    let row = transaction
        .query_opt(SELECT_HEAD, &[&release.tenant_id, &release.environment])
        .await?
        .context("the environment has no selected release")?;
    ensure!(
        row.get::<_, i32>(0) == i32::try_from(release.effective_release_id.get())?,
        "the deployment selection was superseded"
    );
    Ok(())
}

async fn migration_signature(
    transaction: &Transaction<'_>,
    tenant: &str,
    package: &str,
    version: &str,
) -> anyhow::Result<Vec<(i32, String, String)>> {
    let migrations = transaction
        .query(
            wamn_schema_control::sql::select_package_migrations_sql(),
            &[&tenant, &package, &version],
        )
        .await?;
    ensure!(
        !migrations.is_empty(),
        "selected package has no applied migration records"
    );
    Ok(migrations
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect())
}

async fn require_compatible_schema(
    transaction: &Transaction<'_>,
    manifest: &ServingManifest,
) -> anyhow::Result<()> {
    let release = &manifest.release;
    // The package owner records migrations in the transaction that applies
    // them. Its lineage lock prevents a concurrent application from changing
    // the installed leaf while this deployment checks or activates it.
    for package in &release.packages {
        transaction
            .query_one(
                crate::apply_package::LOCK_PACKAGE_SQL,
                &[&release.tenant_id, &package.package_id()],
            )
            .await?;
        let installed = transaction
            .query_opt(
                crate::apply_package::SELECT_CURRENT_PACKAGE_VERSION_SQL,
                &[&release.tenant_id, &package.package_id()],
            )
            .await?
            .context("the target lacks the selected package")?
            .get::<_, String>(0);
        let selected = migration_signature(
            transaction,
            &release.tenant_id,
            package.package_id(),
            package.package_version(),
        )
        .await?;
        let actual = migration_signature(
            transaction,
            &release.tenant_id,
            package.package_id(),
            &installed,
        )
        .await?;
        ensure!(
            actual == selected,
            "schema-changing deployment requires a fresh target; existing-data upgrades are unsupported"
        );
    }
    Ok(())
}

async fn activate(
    transaction: &Transaction<'_>,
    manifest: &ServingManifest,
    principal: &str,
) -> anyhow::Result<()> {
    for wiring in &manifest.wirings {
        let environment = &manifest.release.environment;
        let current = transaction.query_opt("SELECT confirmed_definition_hash, enabled FROM catalog.wiring_activation WHERE tenant_id = $1 AND package_id = $2 AND environment = $3 AND wiring_id = $4 FOR UPDATE", &[&manifest.release.tenant_id, &wiring.package_id, &environment, &wiring.wiring_id]).await?;
        if current.is_some_and(|row| {
            row.get::<_, String>(0) == wiring.graph_hash.as_str() && row.get::<_, bool>(1)
        }) {
            continue;
        }
        let facts = transaction
            .query_one(
                wamn_catalog::activation_facts(),
                &[
                    &wiring.package_id,
                    &environment,
                    &wiring.wiring_id,
                    &wiring.graph_hash.as_str(),
                ],
            )
            .await?;
        wamn_catalog::validate_wiring_activation(
            &wiring.package_id,
            environment,
            &wiring.wiring_id,
            true,
            WiringActivationFacts {
                tombstoned: facts.get("tombstoned"),
                definition_in_release: facts.get("definition_in_release"),
            },
        )?;
        transaction
            .execute(
                wamn_catalog::flip_activation(),
                &[
                    &wiring.package_id,
                    &environment,
                    &wiring.wiring_id,
                    &wiring.graph_hash.as_str(),
                    &true,
                ],
            )
            .await?;
        transaction
            .query_one(
                wamn_catalog::record_activation_event(),
                &[
                    &wiring.package_id,
                    &environment,
                    &wiring.wiring_id,
                    &true,
                    &wiring.graph_hash.as_str(),
                    &Option::<String>::None,
                    &principal,
                    &"deploy-qualified-release",
                ],
            )
            .await?;
    }
    Ok(())
}

fn pinned_bytes(qualification: &Qualification, path: &Path) -> anyhow::Result<Vec<u8>> {
    let digest = qualification
        .artifact_hashes
        .get(path)
        .context("deployment input was not included in qualification")?;
    let bytes = fs::read(path).context("read the qualified deployment input")?;
    ensure!(
        *digest
            == format!(
                "sha256:{}",
                hex::encode(ring::digest::digest(&ring::digest::SHA256, &bytes))
            ),
        "deployment input changed after qualification"
    );
    Ok(bytes)
}

fn deployment_document(
    qualification: &Qualification,
    path: &Path,
    args: &DeployRequest,
    snapshot: &ReleaseSnapshot,
    image: &str,
    role: &str,
) -> anyhow::Result<Value> {
    let document: Value = serde_json::from_slice(&pinned_bytes(qualification, path)?)?;
    require_no_other_containers(&document)?;
    ensure!(
        document["apiVersion"] == "apps/v1"
            && document["kind"] == "Deployment"
            && document["metadata"]["namespace"] == args.namespace
            && document["metadata"]["name"]
                .as_str()
                .is_some_and(|name| !name.is_empty()),
        "deployment input must be one named Deployment in the explicit namespace"
    );
    let containers = document
        .pointer("/spec/template/spec/containers")
        .and_then(Value::as_array)
        .context("deployment input has no containers")?;
    ensure!(
        containers.len() == 1 && containers[0]["image"] == image,
        "deployment image differs from the qualified artifact"
    );
    let container = &containers[0];
    if role == "host" {
        let arguments = container["args"]
            .as_array()
            .context("host deployment requires explicit release flags")?;
        for (flag, expected) in [
            (
                "--release-artifact-base",
                snapshot.carrier.artifact_base.as_str(),
            ),
            (
                "--release-manifest-digest",
                snapshot.carrier.manifest_digest.as_str(),
            ),
        ] {
            ensure!(
                argument(arguments, flag)?.as_deref() == Some(expected),
                "host release flag differs from the qualified artifact"
            );
        }
    } else if role == "executor" {
        let environment = container["env"]
            .as_array()
            .context("executor deployment requires release environment entries")?;
        for (name, expected) in [
            (
                "WAMN_RELEASE_ARTIFACT_BASE",
                snapshot.carrier.artifact_base.as_str(),
            ),
            (
                "WAMN_RELEASE_MANIFEST_DIGEST",
                snapshot.carrier.manifest_digest.as_str(),
            ),
        ] {
            let values = environment
                .iter()
                .filter(|entry| entry["name"] == name)
                .collect::<Vec<_>>();
            ensure!(
                values.len() == 1
                    && values[0]["value"] == expected
                    && values[0].get("valueFrom").is_none(),
                "executor release environment differs from the qualified artifact"
            );
        }
    }
    Ok(document)
}

fn argument(arguments: &[Value], flag: &str) -> anyhow::Result<Option<String>> {
    let mut matches = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        let value = argument
            .as_str()
            .context("host arguments must be strings")?;
        if value == flag {
            matches.push(
                arguments
                    .get(index + 1)
                    .and_then(Value::as_str)
                    .context("release flag lacks its value")?
                    .to_owned(),
            );
        } else if let Some(value) = value.strip_prefix(&format!("{flag}=")) {
            matches.push(value.to_owned());
        }
    }
    ensure!(matches.len() <= 1, "host deployment repeats a release flag");
    Ok(matches.pop())
}

fn require_no_other_containers(document: &Value) -> anyhow::Result<()> {
    for key in ["initContainers", "ephemeralContainers"] {
        ensure!(
            document
                .pointer(&format!("/spec/template/spec/{key}"))
                .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
            "deployment contains an additional unqualified container image"
        );
    }
    Ok(())
}

fn require_supplied_fields(expected: &Value, actual: &Value) -> anyhow::Result<()> {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, value) in expected {
                require_supplied_fields(
                    value,
                    actual
                        .get(key)
                        .context("ready deployment lacks a qualified field")?,
                )?;
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            ensure!(
                expected.len() == actual.len(),
                "ready deployment has a different number of qualified inputs"
            );
            for (expected, actual) in expected.iter().zip(actual) {
                require_supplied_fields(expected, actual)?;
            }
        }
        _ => ensure!(
            expected == actual,
            "ready deployment carries different qualified inputs"
        ),
    }
    Ok(())
}

fn require_released_route(
    manifest: &ServingManifest,
    input: &str,
    host: &str,
) -> anyhow::Result<()> {
    let url = url::Url::parse(input)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "deployment interaction requires an HTTP URL without embedded credentials"
    );
    ensure!(
        manifest
            .attachments
            .values()
            .any(
                |attachment| attachment.kind == wamn_catalog::AttachmentKind::Http
                    && attachment
                        .definition
                        .pointer("/route/path")
                        .and_then(Value::as_str)
                        == Some(url.path())
                    && attachment
                        .definition
                        .pointer("/route/method")
                        .and_then(Value::as_str)
                        == Some("POST")
                    && attachment
                        .definition
                        .pointer("/route/host")
                        .and_then(Value::as_str)
                        == Some(host)
                    && wamn_catalog::parse_attachment_auth_policy(&attachment.auth_policy)
                        .is_some_and(|policy| policy.allows_pat())
                    && attachment.registered_operation.is_some()
            ),
        "the authenticated interaction must target a PAT operation in the selected release"
    );
    Ok(())
}

async fn authenticated_interaction(
    url: &str,
    host: &str,
    bearer: &str,
    body: Vec<u8>,
    expected: Value,
) -> anyhow::Result<()> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .post(url)
        .header(reqwest::header::HOST, host)
        .bearer_auth(bearer)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await?
        .error_for_status()?;
    ensure!(
        response.status().is_success(),
        "the authenticated operation did not return success"
    );
    let actual: Value = serde_json::from_slice(&response.bytes().await?)?;
    ensure!(
        actual == expected,
        "the authenticated operation returned an unexpected result"
    );
    Ok(())
}

async fn kubectl(
    args: &DeployRequest,
    arguments: &[&str],
    input: Option<Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(&args.kubeconfig)
        .arg("--context")
        .arg(&args.context)
        .arg("--namespace")
        .arg(&args.namespace)
        .args(arguments)
        .process_group(0)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .context("start the selected Kubernetes operation")?;
    let _group = CommandGroup(
        Pid::from_raw(i32::try_from(
            child.id().context("Kubernetes child has no process ID")?,
        )?)
        .context("Kubernetes child has an invalid process ID")?,
    );
    if let Some(bytes) = input {
        child
            .stdin
            .take()
            .context("open Kubernetes input")?
            .write_all(&bytes)
            .await?;
    }
    let output = child.wait_with_output().await?;
    ensure!(
        output.status.success(),
        "the selected Kubernetes operation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

#[derive(Debug)]
struct CommandGroup(Pid);

impl Drop for CommandGroup {
    fn drop(&mut self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}

#[cfg(test)]
#[path = "deployment_tests.rs"]
mod tests;
