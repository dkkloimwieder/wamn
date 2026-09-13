//! Real delivery commands while the WMS fixture retains its minted release store.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_ctl::delivery::Candidate;
use wamn_ctl::dev::environment::ProvisionedRoute;
use wamn_ctl::print_release_env::ReleaseCarrier;
use wamn_gate_harness::journey::JourneyDocument;
use wamn_test_infrastructure::rendering::kubernetes_documents;
use wamn_test_infrastructure::{event_broker::EventBroker, platform};

use super::{application, bootstrap, checked, deployment, kubectl};
use crate::environment::{ORG, PROJECT, RELEASE_ID, TENANT};

pub(super) async fn run(
    repository: &Path,
    lifecycle: &Path,
    cluster: &str,
    work: &Path,
    target: &Path,
    evidence: &Path,
    source_head: &str,
    files: &bootstrap::BootstrapFiles,
    broker: &EventBroker,
    source: &async_nats::jetstream::stream::Config,
    document: &JourneyDocument,
    route: &ProvisionedRoute,
    release: &ReleaseCarrier,
    postgres_ip: &str,
    nats_url: &str,
    cancelled: &pg_walstream::CancellationToken,
) -> anyhow::Result<()> {
    let secrets = vec![
        route.database_url.clone(),
        document.system_pg_url.clone(),
        route.token.clone(),
    ];
    let adapter = repository.join("tools/delivery-owned");
    step(
        repository,
        evidence,
        "native-registry",
        &mut Command::new(&adapter)
            .arg("native-registry")
            .arg(cluster)
            .arg(work)
            .arg(source_head),
        &secrets,
        cancelled,
    )
    .await?;
    let image = fs::read_to_string(work.join("native-host-image"))?;
    deployment::install_application_secrets(document, files, cluster, work, postgres_ip).await?;
    super::install_event_secrets(cluster, work, broker, source).await?;
    let operator_values = work.join("operator-values.json");
    fs::write(
        &operator_values,
        serde_json::to_vec(&json!({"operator":{
            "watchNamespaces":[cluster],"hostNamespaces":[cluster],"allowSharedHosts":false,
        }}))?,
    )?;
    platform::install(
        repository,
        lifecycle,
        cluster,
        work,
        cluster,
        &operator_values,
    )
    .await?;
    let (base, _) = application::render_host(
        document,
        cluster,
        nats_url,
        postgres_ip,
        release.manifest_digest.as_str(),
        source,
        1,
        work,
    )?;
    fs::write(
        &base,
        crate::delivery::host_values(&fs::read_to_string(&base)?, &image)?,
    )?;
    let rendered = step(
        repository,
        evidence,
        "render-host",
        &mut Command::new(&adapter)
            .arg("render-host")
            .arg(cluster)
            .arg(work),
        &secrets,
        cancelled,
    )
    .await?;
    let mut host = None;
    for document in kubernetes_documents(&rendered)? {
        if document["kind"] == "Deployment" {
            ensure!(
                host.is_none(),
                "the host chart rendered more than one Deployment"
            );
            host = Some(document);
        } else {
            let path = work.join("host-infrastructure.json");
            fs::write(&path, serde_json::to_vec(&document)?)?;
            checked(kubectl(cluster, work).args(["apply", "-f"]).arg(path)).await?;
        }
    }
    let host = host.context("the existing chart rendered no host Deployment")?;
    ensure!(
        host["spec"]["template"]["spec"]["containers"][0]["image"] == image,
        "the host renderer changed the supplied native image"
    );
    let host_path = artifact(evidence, "host-deployment.json", &host)?;
    let flow_http = deployment::publish_runtime(
        repository,
        work,
        &document.flow_http_wasm,
        &format!("{}/flow-http:{cluster}", document.component_artifact_base),
        &evidence.join("flow-http-push.json"),
    )
    .await?;
    let rendered = application::render_workload(document, &flow_http, work)?;
    let mut http = kubernetes_documents(&fs::read(rendered)?)?;
    for document in &mut http {
        if document["kind"] == "Service" {
            document["spec"]["type"] = json!("NodePort");
        }
    }
    let http_path = artifact(
        evidence,
        "http-workload.json",
        &json!({"apiVersion":"v1","kind":"List","items":http}),
    )?;
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&http_path)).await?;
    let service: Value = serde_json::from_slice(
        &checked(kubectl(cluster, work).args([
            "-n",
            cluster,
            "get",
            "service",
            "flow-http",
            "-o",
            "json",
        ]))
        .await?,
    )?;
    let port = service["spec"]["ports"][0]["nodePort"]
        .as_u64()
        .context("the owned HTTP Service has a NodePort")?;
    ensure!(
        (1000..=65535).contains(&port),
        "the owned HTTP NodePort is invalid"
    );
    let address = bootstrap::kind_address(
        &deployment::inspect(lifecycle, &format!("{cluster}-control-plane")).await?,
    )?;
    let request = artifact(
        evidence,
        "request.json",
        &json!([{"request_id":"delivery-inventory-aggregate"}]),
    )?;
    let expected = artifact(
        evidence,
        "expected-response.json",
        &json!([{
            "request_id":"delivery-inventory-aggregate","value":{"rows":[{
                "product_id":"00000000-0000-0000-0000-000000000101",
                "location_id":"00000000-0000-0000-0000-000000000201",
                "status":"available","quantity":"10","pallet_count":1
            }]}
        }]),
    )?;
    let bearer = work.join("delivery-bearer");
    deployment::write_private(&bearer, route.token.as_bytes())?;
    let manifest = evidence.join("manifest.json");
    let candidate_path = evidence.join("candidate.json");
    let qualification = evidence.join("qualification.json");
    let binary = target.join("debug/wamn-ctl");
    let mut prepare = Command::new(&binary);
    prepare
        .arg("prepare-release")
        .args([
            "--database-url",
            &route.database_url,
            "--tenant",
            TENANT,
            "--effective-release-id",
            &RELEASE_ID.to_string(),
            "--artifact-base",
            &release.artifact_base,
        ])
        .arg("--target-directory")
        .arg(target)
        .arg("--manifest-output")
        .arg(&manifest)
        .arg("--candidate-output")
        .arg(&candidate_path)
        .args([
            "--host-image",
            &image,
            "--native-registry-endpoint",
            &format!("{cluster}-native-registry:5000"),
            "--native-registry-insecure",
        ]);
    for path in [&host_path, &http_path, &request, &expected] {
        prepare.arg("--deployment-file").arg(path);
    }
    step(
        repository,
        evidence,
        "prepare-release",
        &mut prepare,
        &secrets,
        cancelled,
    )
    .await?;
    let candidate = Candidate::read(&candidate_path)?;
    let release_identity = serde_json::to_value(candidate.manifest()?.0.release)?;
    crate::delivery::registry_files(&candidate, work)?;
    step(
        repository,
        evidence,
        "load-native",
        &mut Command::new(&adapter)
            .arg("load-native")
            .arg(cluster)
            .arg(work),
        &secrets,
        cancelled,
    )
    .await?;
    step(
        repository,
        evidence,
        "qualify-release",
        &mut Command::new(&binary)
            .arg("qualify-release")
            .arg("--repository")
            .arg(repository)
            .arg("--revision")
            .arg(source_head)
            .arg("--candidate")
            .arg(&candidate_path)
            .arg("--result")
            .arg(&qualification),
        &secrets,
        cancelled,
    )
    .await?;
    let publication = [
        "--database-url",
        &route.database_url,
        "--control-database-url",
        &document.system_pg_url,
        "--org",
        ORG,
        "--project",
        PROJECT,
        "--tenant",
        TENANT,
        "--effective-release-id",
        "1",
        "--artifact-base",
        &release.artifact_base,
        "--insecure-registry",
    ];
    for name in [
        "publish-qualified-release",
        "select-release",
        "deploy-release",
    ] {
        let mut command = Command::new(&binary);
        command
            .arg(name)
            .arg("--qualification")
            .arg(&qualification)
            .args(publication)
            .arg("--registry-auth-file")
            .arg(&document.registry_auth_file);
        if name == "deploy-release" {
            command
                .arg("--kubeconfig")
                .arg(work.join("kubeconfig"))
                .args([
                    "--context",
                    &format!("kind-{cluster}"),
                    "--namespace",
                    cluster,
                ])
                .arg("--host-deployment")
                .arg(&host_path)
                .args([
                    "--principal",
                    route
                        .management_principal_subject
                        .as_deref()
                        .context("the fixture has a management principal")?,
                    "--interaction-url",
                    &format!("http://{address}:{port}/inventory/aggregate"),
                    "--route-host",
                    &document.route_host,
                    "--http-workload",
                    "flow-http",
                ])
                .arg("--request-body")
                .arg(&request)
                .arg("--expected-response")
                .arg(&expected)
                .arg("--bearer-file")
                .arg(&bearer);
        }
        let output = step(
            repository,
            evidence,
            name,
            &mut command,
            &secrets,
            cancelled,
        )
        .await?;
        if name != "publish-qualified-release" {
            let result: Value = serde_json::from_slice(
                output
                    .split(|byte| *byte == b'\n')
                    .rfind(|line| !line.is_empty())
                    .context("the delivery command produced no result")?,
            )?;
            let identity_field = if name == "select-release" {
                "selected_release"
            } else {
                "deployed_release"
            };
            ensure!(
                result[identity_field] == release_identity
                    && result["result"] == "pass"
                    && result["manifest_digest"] == release.manifest_digest.as_str(),
                "the delivery command did not confirm the minted release digest"
            );
        }
    }
    Ok(())
}

fn artifact(evidence: &Path, name: &str, value: &Value) -> anyhow::Result<PathBuf> {
    let path = evidence.join(name);
    deployment::write_private(&path, &serde_json::to_vec_pretty(value)?)?;
    Ok(path)
}

async fn step(
    repository: &Path,
    evidence: &Path,
    name: &str,
    command: &mut Command,
    secrets: &[String],
    cancelled: &pg_walstream::CancellationToken,
) -> anyhow::Result<Vec<u8>> {
    ensure!(
        !cancelled.is_cancelled(),
        "owned delivery was canceled before {name}"
    );
    let redact = |text: String| {
        secrets
            .iter()
            .fold(text, |text, secret| text.replace(secret, "<private>"))
    };
    let standard = command.as_std();
    let arguments = std::iter::once(standard.get_program())
        .chain(standard.get_args())
        .map(|value| redact(value.to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    command.current_dir(repository).kill_on_drop(true);
    let output =
        wamn_ctl::delivery::qualification::execute_owned(command, Duration::from_secs(3 * 60 * 60))
            .await
            .map_err(|error| anyhow::anyhow!(redact(format!("{error:#}"))))
            .with_context(|| format!("execute owned delivery {name}"))?;
    artifact(
        evidence,
        &format!("{name}-command.json"),
        &json!({
            "command":arguments,"exit_code":output.status.code(),
            "stdout":redact(String::from_utf8_lossy(&output.stdout).into_owned()),
            "stderr":redact(String::from_utf8_lossy(&output.stderr).into_owned()),
        }),
    )?;
    ensure!(
        output.status.success(),
        "owned delivery {name} failed with {}; see its command result",
        output.status
    );
    Ok(output.stdout)
}
