//! Receiving setup and assertions over an owned disposable cluster.

mod build;
mod cdc;
mod default_case;
mod deployment;
mod materializer_case;
mod measurement;
mod measurement_cases;
mod operator_recovery;
mod postcommit_case;
mod postcommit_pair;
mod resources;
mod route_cases;
mod session_cases;
mod session_cluster;
mod startup_case;

use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_ctl::dev::environment::ProvisionedRoute;
use wamn_ctl::print_release_env::{ReleaseCarrier, lookup_release_carrier};
use wamn_gate_harness::journey::JourneyDocument;
use wamn_test_infrastructure::rendering::{
    EventIdentity, HostIdentity, HostRoleSecret, HostValuesInput, assert_rendered_identity,
    render_host_values,
};
use wamn_test_infrastructure::secrets::{HostSecretsInput, derive_host_secrets};

use super::{ENVIRONMENT, ORG, PROJECT, RELEASE_ID, TENANT, connect, repository_root, routes};
use build::Artifacts;
use resources::{Resources, checked, write_private};
use wamn_control_provision::events::{
    advisory_stream_config, materializer_consumer_config, source_stream_config,
};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker::{self, EventBroker};

struct ReceivingCluster {
    resources: Resources,
    inputs: JourneyDocument,
    artifacts: Artifacts,
    broker: EventBroker,
    nats_url: String,
    source: async_nats::jetstream::stream::Config,
}

async fn start(
    evidence: &Path,
    standard_images: bool,
    session_host: bool,
) -> anyhow::Result<ReceivingCluster> {
    let repository = repository_root()?;
    let mut cluster =
        resources::prepare(&repository, evidence, standard_images, session_host).await?;
    let artifacts =
        build::components_and_tools(&repository, &cluster.evidence, standard_images).await?;
    let registry_password = resources::prepare_files(&cluster).await?;
    let scope = Triple::new(ORG, PROJECT, ENVIRONMENT);
    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(&scope, 1);
    let consumers = declared_consumers()?;
    let broker = event_broker::prepare(
        &cluster.work,
        &scope,
        TENANT,
        &source,
        &advisory,
        &consumers,
    )?;
    if !standard_images {
        build::prepare_host_image(&cluster.work, &artifacts.target, &cluster.source)?;
    }
    resources::build_images(&mut cluster, standard_images).await?;
    resources::create(&mut cluster).await?;
    checked(kubectl(&cluster).args([
        "wait",
        "--for=condition=Ready",
        "nodes",
        "--all",
        "--timeout=180s",
    ]))
    .await?;
    checked(kubectl(&cluster).args(["create", "namespace", &cluster.name])).await?;

    let postgres = resources::inspect(&cluster, "postgres").await?;
    let postgres_address = resources::kind_address(&postgres)?;
    let loopback_port = resources::postgres_host_port(&postgres)?;
    let registry_address =
        resources::kind_address(&resources::inspect(&cluster, "registry").await?)?;
    let nats_address = resources::kind_address(&resources::inspect(&cluster, "nats").await?)?;
    ensure!(
        postgres_address != registry_address
            && postgres_address != nats_address
            && registry_address != nats_address,
        "the owned services must have distinct addresses"
    );
    postgres_ready(&cluster, postgres_address, loopback_port).await?;
    let authority = format!("{registry_address}:5000");
    let registry_auth_file =
        resources::registry_auth(&cluster, &authority, &registry_password).await?;
    let nats_url = format!("nats://{nats_address}:4222");
    event_broker::write_binding(&broker, &nats_url, &source)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let provisioning = loop {
        if let Ok(client) = event_broker::connect(&broker.provisioning, &nats_url).await {
            break client;
        }
        ensure!(
            Instant::now() < deadline,
            "the owned NATS broker did not become ready"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    wamn_ctl::event_streams::provision(
        &async_nats::jetstream::new(provisioning),
        &scope,
        source.num_replicas,
        source.duplicate_window,
        &consumers,
    )
    .await?;
    let admin_url = format!("postgresql://postgres:probe@{postgres_address}:5432/postgres");
    let (admin, task) = connect(&admin_url).await?;
    let result = admin.batch_execute("CREATE DATABASE wamn_system").await;
    task.abort();
    result.context("create the owned system database")?;
    let inputs = JourneyDocument {
        system_pg_url: format!("postgresql://postgres:probe@{postgres_address}:5432/wamn_system"),
        component_directory: artifacts.components.clone(),
        compilation_cache_directory: cluster.work.join("wasmtime-cache"),
        flow_http_wasm: artifacts.http.clone(),
        component_artifact_base: format!("{authority}/wamn/components"),
        release_artifact_base: format!("{authority}/wamn/releases"),
        route_host: "receiving.localhost".to_owned(),
        registry_auth_file,
        host_secret_directory: cluster.work.join("host-secrets"),
        host_secret_namespace: cluster.name.clone(),
        route_caller_secret_output: cluster.work.join("route-caller-pat.json"),
        fresh_only_packages: None,
        overlay_compatibility: None,
        postcommit: None,
        materializer: None,
        runtime: None,
    };
    Ok(ReceivingCluster {
        resources: cluster,
        inputs,
        artifacts,
        broker,
        nats_url,
        source,
    })
}

fn declared_consumers() -> anyhow::Result<Vec<async_nats::jetstream::consumer::pull::Config>> {
    let mut consumers = Vec::new();
    let sanitize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    for package in super::JOURNEY_PACKAGES {
        let path = super::journey_package_root(package, None).join("wamn.json");
        let manifest = wamn_schema_generator::PackageManifest::from_slice(&fs::read(path)?)?;
        for (name, operation) in &manifest.custom_operations {
            if let Some(registration) = &operation.registration {
                let durable = format!(
                    "mat_{}_{}_{}",
                    sanitize(TENANT),
                    sanitize(&manifest.package.id),
                    sanitize(name)
                );
                ensure!(
                    !consumers.iter().any(
                        |consumer: &async_nats::jetstream::consumer::pull::Config| consumer
                            .durable_name
                            .as_deref()
                            == Some(durable.as_str())
                    ),
                    "the app registrations have colliding durable names"
                );
                let filter = format!(
                    "evt.{ORG}.{PROJECT}.{ENVIRONMENT}.{}.>",
                    wamn_event_wire::subject_token(&registration.entity)
                );
                consumers.push(materializer_consumer_config(
                    &durable,
                    &filter,
                    Duration::from_secs(30),
                    5,
                ));
            }
        }
    }
    Ok(consumers)
}

async fn postgres_ready(cluster: &Resources, address: Ipv4Addr, port: u16) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let logs = checked(
            Command::new(&cluster.lifecycle)
                .arg("container-logs")
                .arg(format!("{}-postgres", cluster.name)),
        )
        .await?;
        let markers = String::from_utf8_lossy(&logs)
            .matches("database system is ready to accept connections")
            .count();
        if markers >= 2
            && postgres_18(&format!(
                "postgresql://postgres:probe@127.0.0.1:{port}/postgres"
            ))
            .await
            && postgres_18(&format!(
                "postgresql://postgres:probe@{address}:5432/postgres"
            ))
            .await
        {
            fs::write(cluster.evidence.join("postgres-readiness.log"), logs)?;
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "owned PostgreSQL 18 did not reach final externally reachable readiness"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn postgres_18(url: &str) -> bool {
    let Ok(Ok((client, connection))) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_postgres::connect(url, tokio_postgres::NoTls),
    )
    .await
    else {
        return false;
    };
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        client.query_one(
            "SELECT current_setting('server_version_num')::int >= 180000",
            &[],
        ),
    )
    .await;
    task.abort();
    matches!(result, Ok(Ok(row)) if row.get::<_, bool>(0))
}

async fn provision(
    inputs: &JourneyDocument,
    artifacts: &Artifacts,
) -> anyhow::Result<(ProvisionedRoute, ReleaseCarrier)> {
    let route = routes::receiving_pat_journey(
        inputs,
        &artifacts.target.join("debug/wamn-scenario-worker"),
        inputs.fresh_only_packages.is_some(),
    )
    .await?;
    let carrier = lookup_release_carrier(
        &route.database_url,
        TENANT,
        RELEASE_ID,
        &inputs.release_artifact_base,
    )
    .await?;
    Ok((route, carrier))
}

async fn install_host(
    cluster: &Resources,
    inputs: &JourneyDocument,
    carrier: &ReleaseCarrier,
    replicas: u32,
    nats_url: &str,
    native_nats_secrets: &[PathBuf],
    source: &async_nats::jetstream::stream::Config,
    session: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    let database_host = reqwest::Url::parse(&inputs.system_pg_url)?
        .host_str()
        .context("the owned system URL has a host")?
        .to_owned();
    let secrets = derive_host_secrets(
        &inputs.host_secret_directory,
        &HostSecretsInput {
            role_families: vec![
                WorkloadRoleFamily::ExecutorPlatform,
                WorkloadRoleFamily::IdentityReader,
                WorkloadRoleFamily::HttpAdmitter,
                WorkloadRoleFamily::EventMaterializer,
            ],
            guest_secret_file: PathBuf::from("guest-sql.json"),
            namespace: cluster.name.clone(),
            database_host,
        },
    )?;
    for secret in &secrets {
        apply(cluster, &secret.path).await?;
    }
    for secret in native_nats_secrets {
        apply(cluster, secret).await?;
    }
    install_route_credential(cluster, inputs).await?;
    let registry_secret = cluster.work.join("registry-pull.json");
    write_private(
        &registry_secret,
        &serde_json::to_vec(&json!({
            "apiVersion":"v1", "kind":"Secret", "type":"kubernetes.io/dockerconfigjson",
            "metadata":{"name":"wamn-registry-pull","namespace":cluster.name},
            "stringData":{".dockerconfigjson":fs::read_to_string(&inputs.registry_auth_file)?},
        }))?,
    )?;
    apply(cluster, &registry_secret).await?;
    let operator_values = cluster.work.join("operator-values.yaml");
    fs::write(
        &operator_values,
        serde_json::to_vec(&json!({"operator":{
            "watchNamespaces":[cluster.name],"hostNamespaces":[cluster.name],"allowSharedHosts":false,
        }}))?,
    )?;
    wamn_test_infrastructure::platform::install(
        &cluster.repository,
        &cluster.lifecycle,
        &cluster.name,
        &cluster.work,
        &cluster.name,
        &operator_values,
    )
    .await?;
    let guest = secrets
        .iter()
        .find(|secret| secret.family == WorkloadRoleFamily::App)
        .context("the Receiving guest credential is present")?;
    let roles = secrets
        .iter()
        .filter(|secret| secret.family != WorkloadRoleFamily::App)
        .map(|secret| HostRoleSecret {
            family: secret.family,
            name: secret.name.clone(),
        })
        .collect();
    let values = render_host_values(
        &fs::read_to_string(
            cluster
                .repository
                .join("deploy/platform/values-host-default.yaml"),
        )?,
        &fs::read_to_string(
            cluster
                .repository
                .join("deploy/platform/values-host-receiving-pat.yaml"),
        )?,
        &HostValuesInput {
            namespace: cluster.name.clone(),
            host_tag: cluster.name.clone(),
            replicas,
            component_artifact_base: inputs.component_artifact_base.clone(),
            release_artifact_base: carrier.artifact_base.clone(),
            manifest_digest: carrier.manifest_digest.to_string(),
            nats_url: nats_url.to_owned(),
            event: EventIdentity {
                org: ORG.to_owned(),
                project: PROJECT.to_owned(),
                environment: ENVIRONMENT.to_owned(),
            },
            guest_secret_name: guest.name.clone(),
            role_secrets: roles,
            object_store_secret_name: None,
            stream_replicas: source.num_replicas,
            dup_window_secs: source.duplicate_window.as_secs(),
        },
    )?;
    assert_rendered_identity(
        &values.overlay,
        &HostIdentity {
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            schema: "receiving".to_owned(),
        },
    )?;
    let base = cluster.work.join("host-base.yaml");
    let overlay = cluster.work.join("host-overlay.yaml");
    fs::write(&base, values.base)?;
    let overlay_values = if let Some((issuer, instance_suffix)) = session {
        session_cluster::adjust_host(&values.overlay, issuer, instance_suffix)?
    } else {
        values.overlay
    };
    fs::write(&overlay, overlay_values)?;
    checked(
        Command::new(&cluster.lifecycle)
            .arg("install-host")
            .arg(&cluster.name)
            .arg(&cluster.work)
            .arg(&cluster.name)
            .arg(&base)
            .arg(&overlay),
    )
    .await?;
    checked(kubectl(cluster).args([
        "-n",
        &cluster.name,
        "rollout",
        "status",
        "deployment/hostgroup-default",
        "--timeout=240s",
    ]))
    .await?;
    Ok(())
}

async fn install_route_credential(
    cluster: &Resources,
    inputs: &JourneyDocument,
) -> anyhow::Result<()> {
    ensure!(
        fs::metadata(&inputs.route_caller_secret_output)?
            .permissions()
            .mode()
            & 0o777
            == 0o600,
        "the route-caller Secret must have mode 0600"
    );
    let mut secret: Value = serde_json::from_slice(&fs::read(&inputs.route_caller_secret_output)?)?;
    ensure!(
        secret["kind"] == "Secret"
            && secret["type"] == "Opaque"
            && secret["metadata"]["annotations"]["wamn.io/credential-purpose"] == "route-caller"
            && secret["stringData"]["token"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
        "the route-caller Secret must keep its declared purpose and token"
    );
    secret["metadata"]["namespace"] = json!(cluster.name);
    let path = cluster.work.join("route-caller-pat-kube.json");
    write_private(&path, &serde_json::to_vec(&secret)?)?;
    apply(cluster, &path).await
}

fn kubectl(cluster: &Resources) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(cluster.work.join("kubeconfig"))
        .arg("--context")
        .arg(format!("kind-{}", cluster.name));
    command
}

async fn apply(cluster: &Resources, path: &Path) -> anyhow::Result<()> {
    checked(kubectl(cluster).args(["apply", "-f"]).arg(path)).await?;
    Ok(())
}

async fn evidence_directory() -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os("WAMN_RECEIVING_EVIDENCE_DIR").context(
        "WAMN_RECEIVING_EVIDENCE_DIR must name a new absolute directory under repository docs/perf",
    )?);
    ensure!(
        path.is_absolute() && !path.exists(),
        "Receiving evidence must use a new absolute directory"
    );
    let parent = path
        .parent()
        .context("Receiving evidence has a parent")?
        .canonicalize()
        .context("create the evidence parent before running the Receiving test")?;
    let repository = repository_root()?;
    let common = checked(Command::new("git").current_dir(&repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]))
    .await?;
    let common = PathBuf::from(std::str::from_utf8(&common)?.trim());
    let results_root = common
        .parent()
        .context("the Git directory has a repository parent")?
        .join("docs/perf")
        .canonicalize()?;
    ensure!(
        parent.starts_with(&results_root),
        "Receiving evidence must be under the main repository docs/perf"
    );
    Ok(parent.join(
        path.file_name()
            .context("Receiving evidence has a directory name")?,
    ))
}

async fn with_signals(
    evidence: &Path,
    operation: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    use futures_util::FutureExt as _;
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let result = {
        let operation = std::panic::AssertUnwindSafe(operation).catch_unwind();
        tokio::select! {
            result = operation => result.unwrap_or_else(|_| Err(anyhow::anyhow!("the Receiving test panicked"))),
            _ = interrupt.recv() => Err(anyhow::anyhow!("the Receiving test received SIGINT")),
            _ = terminate.recv() => Err(anyhow::anyhow!("the Receiving test received SIGTERM")),
            _ = hangup.recv() => Err(anyhow::anyhow!("the Receiving test received SIGHUP")),
        }
    };
    if let Err(error) = &result {
        if evidence.is_dir() {
            fs::write(
                evidence.join("failure.json"),
                serde_json::to_vec_pretty(&json!({
                    "verdict":"fail", "failure":format!("{error:#}"),
                }))?,
            )?;
        }
    }
    result
}

async fn assert_source_unchanged(resources: &Resources) -> anyhow::Result<()> {
    let source = checked(
        Command::new("git")
            .current_dir(&resources.repository)
            .args(["rev-parse", "HEAD"]),
    )
    .await?;
    ensure!(
        String::from_utf8(source)?.trim() == resources.source,
        "the source commit changed during the test"
    );
    let status = checked(
        Command::new("git")
            .current_dir(&resources.repository)
            .args([
                "status",
                "--porcelain",
                "--untracked-files=normal",
                "--",
                ".",
                ":(exclude).beads/issues.jsonl",
                ":(exclude).beads/interactions.jsonl",
            ]),
    )
    .await?;
    ensure!(status.is_empty(), "the source tree changed during the test");
    Ok(())
}

async fn released_http(
    cluster: &ReceivingCluster,
    replicas: u32,
) -> anyhow::Result<(
    ProvisionedRoute,
    ReleaseCarrier,
    wamn_test_infrastructure::workload::HostObservation,
    Value,
)> {
    use wamn_test_infrastructure::workload;
    let (route, carrier) = provision(&cluster.inputs, &cluster.artifacts).await?;
    let secrets = deployment::native_secrets(cluster)?;
    let resources = &cluster.resources;
    install_host(
        resources,
        &cluster.inputs,
        &carrier,
        replicas,
        &cluster.nats_url,
        &secrets,
        &cluster.source,
        None,
    )
    .await?;
    let digest = workload::image_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        &resources.host_image,
        &resources.source,
        "release",
        &resources.evidence,
    )
    .await?;
    if resources.gates_image.is_some() {
        postcommit_case::assert_gates_layers(resources).await?;
        let gates_evidence = resources.evidence.join("gates-image");
        fs::create_dir(&gates_evidence)?;
        workload::image_ready(
            &resources.lifecycle,
            &resources.name,
            &resources.work,
            resources
                .gates_image
                .as_deref()
                .context("the test built its gates image")?,
            &resources.source,
            "release",
            &gates_evidence,
        )
        .await?;
    }
    let hosts = workload::hosts_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        &resources.name,
        &resources.host_image,
        &digest,
        replicas,
        &resources.evidence,
    )
    .await?;
    let image = deployment::publish_http(cluster).await?;
    deployment::install_http(cluster, &image).await?;
    let observed = workload::http_ready(
        &resources.name,
        &resources.work,
        &resources.name,
        &hosts,
        &resources.evidence,
    )
    .await?;
    Ok((route, carrier, hosts, observed))
}
