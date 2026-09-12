//! Real delivery commands over the retained Receiving release store.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::Deserialize as _;
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_ctl::delivery::Candidate;

use super::super::{ORG, PROJECT, RELEASE_ID, TENANT};
use super::{ReceivingCluster, apply, checked, deployment, kubectl, resources};

#[tokio::test]
#[ignore = "runs release preparation, qualification, publication, and deployment on owned services"]
async fn owned_release_delivery() -> anyhow::Result<()> {
    ensure!(
        Candidate::from_env()?.is_none(),
        "owned setup mints its own candidate"
    );
    let evidence = super::evidence_directory().await?;
    let cancelled = pg_walstream::CancellationToken::new();
    let mut operation = Box::pin(async {
        let mut cluster = super::start(&evidence, true, false).await?;
        let result = exercise(&cluster, &cancelled).await;
        if result.is_err() {
            resources::capture_failure(&cluster.resources).await;
        }
        let cleanup = resources::remove(&mut cluster.resources).await;
        let result = if cancelled.is_cancelled() {
            Err(anyhow::anyhow!("owned delivery was canceled"))
        } else {
            result
        };
        fs::write(
            evidence.join("delivery-result.json"),
            serde_json::to_vec_pretty(&json!({
                "source_commit":cluster.resources.source,
                "result":if result.is_ok() && cleanup.is_ok() { "pass" } else { "fail" },
                "failure":result.as_ref().err().map(|error|format!("{error:#}")),
                "cleanup_failure":cleanup.as_ref().err().map(|error|format!("{error:#}")),
            }))?,
        )?;
        result?;
        cleanup
    });
    wash_runtime::init_crypto();
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let cause = tokio::select! {
        result = &mut operation => return result,
        _ = interrupt.recv() => "owned delivery was interrupted",
        _ = terminate.recv() => "owned delivery was terminated",
        _ = hangup.recv() => "owned delivery lost its session",
    };
    cancelled.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(180), &mut operation).await;
    anyhow::bail!(cause)
}

async fn exercise(
    cluster: &ReceivingCluster,
    cancelled: &pg_walstream::CancellationToken,
) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let route = super::super::routes::mint_receiving_release(
        &cluster.inputs,
        &cluster.artifacts.target.join("debug/wamn-scenario-worker"),
    )
    .await?;
    let carrier = wamn_ctl::print_release_env::lookup_release_carrier(
        &route.database_url,
        TENANT,
        RELEASE_ID,
        &cluster.inputs.release_artifact_base,
    )
    .await?;
    let secrets = vec![
        route.database_url.clone(),
        cluster.inputs.system_pg_url.clone(),
        route.token.clone(),
    ];
    let adapter = resources.repository.join("tools/delivery-owned");
    step(
        cluster,
        "native-registry",
        &mut Command::new(&adapter)
            .arg("native-registry")
            .arg(&resources.name)
            .arg(&resources.work)
            .arg(&resources.source),
        &secrets,
        cancelled,
    )
    .await?;
    let host_image = fs::read_to_string(resources.work.join("native-host-image"))?;
    let gates_image = fs::read_to_string(resources.work.join("native-gates-image"))?;
    let executor_image = fs::read_to_string(resources.work.join("native-executor-image"))?;
    let native_secrets = deployment::native_secrets(cluster)?;
    let (base, _) = super::prepare_host(
        resources,
        &cluster.inputs,
        &carrier,
        1,
        &cluster.nats_url,
        &native_secrets,
        &cluster.source,
        None,
    )
    .await?;
    fs::write(
        &base,
        super::super::delivery::host_values(&fs::read_to_string(&base)?, &host_image)?,
    )?;
    let rendered = step(
        cluster,
        "render-host",
        &mut Command::new(&adapter)
            .arg("render-host")
            .arg(&resources.name)
            .arg(&resources.work),
        &secrets,
        cancelled,
    )
    .await?;
    let mut host = None;
    for document in yaml_documents(&rendered)? {
        if document["kind"] == "Deployment" {
            ensure!(
                host.is_none(),
                "the host chart rendered more than one Deployment"
            );
            host = Some(document);
        } else {
            let path = resources.work.join("host-infrastructure.json");
            fs::write(&path, serde_json::to_vec(&document)?)?;
            apply(resources, &path).await?;
        }
    }
    let host = host.context("the existing chart rendered no host Deployment")?;
    ensure!(
        host["spec"]["template"]["spec"]["containers"][0]["image"] == host_image,
        "the host renderer changed the supplied native image"
    );
    let host_path = artifact(cluster, "host-deployment.json", &host)?;
    let executor_path = artifact(
        cluster,
        "executor-deployment.json",
        &executor(cluster, &carrier, &executor_image)?,
    )?;
    let http_image = deployment::publish_http(cluster).await?;
    let mut http = yaml_documents(deployment::http_workload(cluster, &http_image)?.as_bytes())?;
    for document in &mut http {
        if document["kind"] == "Service" {
            document["spec"]["type"] = json!("NodePort");
        }
    }
    let http_path = artifact(
        cluster,
        "http-workload.json",
        &json!({"apiVersion":"v1","kind":"List","items":http}),
    )?;
    apply(resources, &http_path).await?;
    let service: Value = serde_json::from_slice(
        &checked(kubectl(resources).args([
            "-n",
            &resources.name,
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
    let address = resources::kind_address(&resources::inspect(resources, "control-plane").await?)?;
    let request = artifact(
        cluster,
        "request.json",
        &json!([{"request_id":"delivery-location-list"}]),
    )?;
    let expected = artifact(
        cluster,
        "expected-response.json",
        &json!([{
            "request_id":"delivery-location-list","value":{"rows":[{
                "id":"00000000-0000-0000-0000-000000000201","location_code":"DOCK-1"
            }]}
        }]),
    )?;
    let bearer = resources.work.join("delivery-bearer");
    resources::write_private(&bearer, route.token.as_bytes())?;
    let manifest = resources.evidence.join("manifest.json");
    let candidate_path = resources.evidence.join("candidate.json");
    let qualification = resources.evidence.join("qualification.json");
    let binary = cluster.artifacts.target.join("debug/wamn-ctl");
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
            &carrier.artifact_base,
        ])
        .arg("--target-directory")
        .arg(&cluster.artifacts.target)
        .arg("--manifest-output")
        .arg(&manifest)
        .arg("--candidate-output")
        .arg(&candidate_path)
        .args([
            "--host-image",
            &host_image,
            "--gates-image",
            &gates_image,
            "--executor-image",
            &executor_image,
            "--native-registry-endpoint",
            &format!("{}-native-registry:5000", resources.name),
            "--native-registry-insecure",
        ]);
    for path in [&host_path, &executor_path, &http_path, &request, &expected] {
        prepare.arg("--deployment-file").arg(path);
    }
    step(
        cluster,
        "prepare-release",
        &mut prepare,
        &secrets,
        cancelled,
    )
    .await?;
    let candidate = Candidate::read(&candidate_path)?;
    let release_identity = serde_json::to_value(candidate.manifest()?.0.release)?;
    super::super::delivery::registry_files(&candidate, &resources.work)?;
    step(
        cluster,
        "load-native",
        &mut Command::new(&adapter)
            .arg("load-native")
            .arg(&resources.name)
            .arg(&resources.work),
        &secrets,
        cancelled,
    )
    .await?;
    step(
        cluster,
        "qualify-release",
        &mut Command::new(&binary)
            .arg("qualify-release")
            .arg("--repository")
            .arg(&resources.repository)
            .arg("--revision")
            .arg(&resources.source)
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
        &cluster.inputs.system_pg_url,
        "--org",
        ORG,
        "--project",
        PROJECT,
        "--tenant",
        TENANT,
        "--effective-release-id",
        "1",
        "--artifact-base",
        &carrier.artifact_base,
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
            .arg(&cluster.inputs.registry_auth_file);
        if name == "deploy-release" {
            command
                .arg("--kubeconfig")
                .arg(resources.work.join("kubeconfig"))
                .args([
                    "--context",
                    &format!("kind-{}", resources.name),
                    "--namespace",
                    &resources.name,
                ])
                .arg("--host-deployment")
                .arg(&host_path)
                .arg("--executor-deployment")
                .arg(&executor_path)
                .args([
                    "--principal",
                    route
                        .management_principal_subject
                        .as_deref()
                        .context("the fixture has a management principal")?,
                    "--interaction-url",
                    &format!("http://{address}:{port}/location/list"),
                    "--route-host",
                    &cluster.inputs.route_host,
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
        let output = step(cluster, name, &mut command, &secrets, cancelled).await?;
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
                    && result["manifest_digest"] == carrier.manifest_digest.as_str(),
                "the delivery command did not confirm the minted release digest"
            );
        }
    }
    super::assert_source_unchanged(resources).await
}

fn artifact(cluster: &ReceivingCluster, name: &str, value: &Value) -> anyhow::Result<PathBuf> {
    let path = cluster.resources.evidence.join(name);
    resources::write_private(&path, &serde_json::to_vec_pretty(value)?)?;
    Ok(path)
}

fn yaml_documents(bytes: &[u8]) -> anyhow::Result<Vec<Value>> {
    serde_yaml::Deserializer::from_slice(bytes)
        .map(Value::deserialize)
        .filter_map(|result| match result {
            Ok(Value::Null) => None,
            other => Some(other.map_err(Into::into)),
        })
        .collect()
}

fn executor(
    cluster: &ReceivingCluster,
    carrier: &wamn_ctl::print_release_env::ReleaseCarrier,
    image: &str,
) -> anyhow::Result<Value> {
    let mut document = yaml_documents(&fs::read(
        cluster
            .resources
            .repository
            .join("deploy/platform/executor.yaml"),
    )?)?
    .into_iter()
    .find(|document| document["kind"] == "Deployment")
    .context("the existing executor Deployment is present")?;
    document["metadata"]["namespace"] = json!(cluster.resources.name);
    document["spec"]["replicas"] = json!(1);
    let container = &mut document["spec"]["template"]["spec"]["containers"][0];
    container["image"] = json!(image);
    container["args"] = json!(["--log-level=info", "--allow-insecure-registries"]);
    for entry in container["env"]
        .as_array_mut()
        .context("the executor has explicit environment entries")?
    {
        for (name, value) in [
            ("WAMN_PROJECT", PROJECT),
            ("WAMN_SCHEMA", "receiving"),
            ("WAMN_EVT_NATS_URL", cluster.nats_url.as_str()),
            ("WAMN_RELEASE_ARTIFACT_BASE", carrier.artifact_base.as_str()),
            (
                "WAMN_RELEASE_MANIFEST_DIGEST",
                carrier.manifest_digest.as_str(),
            ),
            (
                "WAMN_COMPONENT_ARTIFACT_BASE",
                cluster.inputs.component_artifact_base.as_str(),
            ),
        ] {
            if entry["name"] == name {
                *entry = json!({"name":name,"value":value});
            }
        }
        for (name, stem) in [
            ("WAMN_PG_URL", "guest-sql"),
            ("WAMN_EXECUTOR_PLATFORM_PG_URL", "executor-platform"),
            ("WAMN_HTTP_ADMITTER_PG_URL", "http-admitter"),
        ] {
            if entry["name"] == name {
                let secret: Value = serde_json::from_slice(&fs::read(
                    cluster
                        .inputs
                        .host_secret_directory
                        .join(format!("{stem}.json")),
                )?)?;
                entry["valueFrom"]["secretKeyRef"]["name"] = secret["metadata"]["name"].clone();
            }
        }
    }
    container["volumeMounts"]
        .as_array_mut()
        .context("the executor has volume mounts")?
        .retain(|mount| mount["name"] != "registry-ca");
    document["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .context("the executor has volumes")?
        .retain(|volume| volume["name"] != "registry-ca");
    Ok(document)
}

async fn step(
    cluster: &ReceivingCluster,
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
    command
        .current_dir(&cluster.resources.repository)
        .kill_on_drop(true);
    let output =
        wamn_ctl::delivery::qualification::execute_owned(command, Duration::from_secs(3 * 60 * 60))
            .await
            .with_context(|| format!("execute owned delivery {name}"))?;
    artifact(
        cluster,
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
