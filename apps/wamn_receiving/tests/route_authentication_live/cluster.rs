//! Receiving setup and assertions over an owned disposable cluster.

mod build;
mod cdc;
mod delivery_case;
mod deployment;
mod materializer_case;
mod postcommit_case;
mod resources;
mod route_cases;
mod session_cluster;

use std::collections::BTreeMap;
use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_catalog::ServingManifest;
use wamn_control::delivery::Candidate;
use wamn_control::package_artifact::OpenedPackage;
use wamn_control::provision_project_env::ProvisionedRoute;
use wamn_control::release_carrier::ReleaseCarrier;
use wamn_control::release_chart::{self, ImageSet, ValuesInput};
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_gate_harness::case_release::{self, NAMESPACE, Registry};
use wamn_gate_harness::journey::JourneyDocument;
use wamn_test_infrastructure::rendering::{
    EventIdentity, HostIdentity, HostRoleSecret, HostValuesInput, assert_rendered_identity,
    overlay_values, render_host_values,
};
use wamn_test_infrastructure::secrets::{HostSecret, HostSecretsInput, derive_host_secrets};

use super::{connect, identity, repository_root, routes};
use resources::{Resources, checked, write_private};
use wamn_control_provision::events::{
    advisory_stream_config, materializer_consumer_config, source_stream_config,
};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker::{self, EventBroker};

/// One Receiving cluster case's environment of the candidate release, which
/// it installs through the package stage and publishes nothing
/// (docs/plan/platform-deploy.md §8.2).
struct ReceivingCluster {
    resources: Resources,
    inputs: JourneyDocument,
    broker: EventBroker,
    nats_url: String,
    source: async_nats::jetstream::stream::Config,
    /// The candidate's packages, from their artifacts, by package id.
    packages: BTreeMap<String, OpenedPackage>,
    /// The materializer consumers that the candidate's packages declare.
    consumers: Vec<async_nats::jetstream::consumer::pull::Config>,
}

impl ReceivingCluster {
    /// The candidate this case runs.
    fn candidate(&self) -> anyhow::Result<&(Candidate, ServingManifest)> {
        self.resources
            .candidate
            .as_ref()
            .context("a Receiving cluster case runs a candidate")
    }
}

async fn start(evidence: &Path, gates_image: bool) -> anyhow::Result<ReceivingCluster> {
    let repository = repository_root()?;
    let mut cluster = resources::prepare(&repository, evidence, gates_image).await?;
    let (candidate, manifest) = cluster
        .candidate
        .as_ref()
        .context("a Receiving cluster case runs a candidate")?;
    // open_packages opens them in the order of the release's packages.
    let packages = manifest
        .release
        .packages
        .iter()
        .map(|package| package.package_id().to_owned())
        .zip(case_release::open_packages(candidate).await?)
        .collect::<BTreeMap<_, _>>();
    let consumers = declared_consumers(packages.values().map(OpenedPackage::root))?;
    let registry_password = resources::prepare_files(&cluster).await?;
    let scope = Triple::new(
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
    );
    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(&scope, 1);
    let broker = event_broker::prepare(
        &cluster.work,
        &scope,
        identity().tenant.as_str(),
        &source,
        &advisory,
        &consumers,
    )?;
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
    // The operator release, the environment releases and their hosts live in
    // the operator namespace (R6).
    checked(kubectl(&cluster).args(["create", "namespace", NAMESPACE])).await?;

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
    wamn_control::event_streams::provision(
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
    let inputs = journey_inputs(
        &cluster,
        &postgres_address.to_string(),
        &authority,
        registry_auth_file,
    );
    Ok(ReceivingCluster {
        resources: cluster,
        inputs,
        broker,
        nats_url,
        source,
        packages,
        consumers,
    })
}

/// The journey inputs of one cluster, from its owned PostgreSQL address and
/// registry authority. A later stage rebuilds the same inputs when it attaches.
fn journey_inputs(
    cluster: &Resources,
    postgres_address: &str,
    authority: &str,
    registry_auth_file: PathBuf,
) -> JourneyDocument {
    JourneyDocument {
        system_pg_url: format!("postgresql://postgres:probe@{postgres_address}:5432/wamn_system"),
        // The case reads no built component: the hosts pull the candidate's.
        component_directory: PathBuf::new(),
        compilation_cache_directory: cluster.work.join("wasmtime-cache"),
        flow_http_wasm: PathBuf::new(),
        component_artifact_base: format!("{authority}/wamn/components"),
        release_artifact_base: format!("{authority}/wamn/releases"),
        route_host: super::identity().route_host.clone(),
        registry_auth_file,
        host_secret_directory: cluster.work.join("host-secrets"),
        host_secret_namespace: cluster.name.clone(),
        operator_secret_output: cluster.work.join("operator-pat.json"),
        fresh_only_packages: None,
        postcommit: None,
        materializer: None,
        runtime: None,
    }
}

/// The materializer consumer of each registration of the packages at
/// `roots`, as the operator declares it to provisioning.
fn declared_consumers<'a>(
    roots: impl IntoIterator<Item = &'a Path>,
) -> anyhow::Result<Vec<async_nats::jetstream::consumer::pull::Config>> {
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
    for root in roots {
        let path = wamn_schema_generator::package_manifest_path(root);
        let manifest = wamn_schema_generator::PackageManifest::from_slice(&fs::read(path)?)?;
        for (name, operation) in &manifest.custom_operations {
            if let Some(registration) = &operation.registration {
                let durable = format!(
                    "mat_{}_{}_{}",
                    sanitize(identity().tenant.as_str()),
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
                    "evt.{}.{}.{}.{}.>",
                    identity().org,
                    identity().project,
                    identity().environment,
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

/// Copy the candidate's release and its components by digest into the case
/// registry, which the hosts read, then provision the environment and install
/// the candidate in it through the package stage. It publishes nothing
/// (docs/plan/platform-deploy.md §8.2, §10.2).
async fn provision(
    cluster: &ReceivingCluster,
) -> anyhow::Result<(ProvisionedRoute, ReleaseCarrier)> {
    let (candidate, _) = cluster.candidate()?;
    case_release::copy_release(candidate, &case_registry(&cluster.inputs)?).await?;
    let route = routes::install_candidate_release(&cluster.inputs, candidate).await?;
    let (_, digest) = candidate.manifest()?;
    Ok((route, carrier(&cluster.inputs, digest.to_string())?))
}

/// Seed the Receiving business rows and the order of the first materializer
/// trigger in the project database of `route`.
async fn seed(route: &ProvisionedRoute) -> anyhow::Result<()> {
    let (project, task) = connect(&route.database_url).await?;
    let seeded = async {
        super::environment::seed_receiving_business_rows(project.as_ref()).await?;
        super::environment::seed_materializer_trigger_rows(project.as_ref()).await
    }
    .await;
    drop(project);
    task.abort();
    seeded
}

/// The case's own registry, beside its component repository.
fn case_registry(inputs: &JourneyDocument) -> anyhow::Result<Registry> {
    Ok(Registry {
        authority: inputs
            .component_artifact_base
            .strip_suffix("/wamn/components")
            .context("the Receiving component repository has its declared suffix")?
            .to_owned(),
        auth_file: Some(inputs.registry_auth_file.clone()),
        insecure: true,
        ca_paths: Vec::new(),
    })
}

/// The carrier of the release `digest` in the release repository of
/// `inputs`.
fn carrier(inputs: &JourneyDocument, digest: String) -> anyhow::Result<ReleaseCarrier> {
    Ok(ReleaseCarrier {
        artifact_base: inputs.release_artifact_base.clone(),
        manifest_digest: wamn_catalog::ManifestDigest::parse(digest)?,
    })
}

/// The drain bound of a case environment release: the host pods' grace period
/// and `WAMN_DRAIN_BOUND_SECONDS`, as `deploy/platform/values-host-default.yaml`
/// sets them (R20).
const DRAIN_BOUND_SECONDS: u32 = 70;

/// One environment release a case installed through the release chart.
struct Deployed {
    /// The Helm release name, which is the host group.
    name: String,
    /// The `wamn.release` label of the manifest digest.
    label: String,
    /// The host image the release names, pinned by digest.
    host_image: String,
    /// The digest of the host image the nodes hold.
    runtime_digest: String,
}

impl Deployed {
    fn release<'a>(
        &'a self,
        resources: &'a Resources,
        carrier: &'a ReleaseCarrier,
    ) -> wamn_test_infrastructure::release_chart::Release<'a> {
        wamn_test_infrastructure::release_chart::Release {
            cluster: &resources.name,
            work: &resources.work,
            namespace: NAMESPACE,
            name: &self.name,
            label: &self.label,
            digest: carrier.manifest_digest.as_str(),
            evidence: &resources.evidence,
        }
    }

    /// The HTTP role Service.
    fn http_service(&self) -> String {
        release_chart::workload_name(&self.name, release_chart::RoleName::Http)
    }
}

/// Install the environment release of `carrier` through the release chart, as
/// `env apply` does (wamn-ps2tj.1, docs/plan/platform-deploy.md §9.2): the
/// host group and every role the release implies. The candidate's role
/// images are copied by digest into the case registry (R12, R15).
async fn install_release(
    cluster: &ReceivingCluster,
    carrier: &ReleaseCarrier,
    binding: &HostBinding<'_>,
) -> anyhow::Result<Deployed> {
    let resources = &cluster.resources;
    let (_, overlay) = prepare_host(resources, &cluster.inputs, carrier, binding).await?;
    let registry = case_registry(&cluster.inputs)?;
    // The manifest is the bytes that `qualify-release` pulled from OCI.
    let (candidate, manifest) = cluster.candidate()?;
    let (http, materializer) = case_release::copy_roles(candidate, &registry).await?;
    let (host_image, runtime_digest) = host_image(resources).await?;
    let (org, project, env) = (
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
    );
    let (name, label) = case_release::install(
        &case_release::Install {
            cluster: &resources.name,
            work: &resources.work,
        },
        &ImageSet {
            host: host_image.clone(),
            http,
            materializer,
        },
        &ValuesInput {
            org: org.to_owned(),
            project: project.to_owned(),
            env: env.to_owned(),
            manifest_digest: carrier.manifest_digest.to_string(),
            artifact_base: carrier.artifact_base.clone(),
            route_host: cluster.inputs.route_host.clone(),
            roles: release_chart::roles(manifest, org, project, env),
            drain_bound_seconds: DRAIN_BOUND_SECONDS,
            actor: "receiving cluster case".to_owned(),
            host_group: case_release::host_group(&fs::read_to_string(&overlay)?)?,
        },
    )
    .await?;
    Ok(Deployed {
        name,
        label,
        host_image,
        runtime_digest,
    })
}

/// The environment release name of the journey coordinate.
fn release_name() -> anyhow::Result<String> {
    release_chart::release_name(
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
    )
}

/// The cluster's host image pinned by the digest its nodes hold, and that digest.
async fn host_image(resources: &Resources) -> anyhow::Result<(String, String)> {
    let digest = wamn_test_infrastructure::workload::image_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        &resources.host_image,
        "release",
        &resources.evidence,
    )
    .await?;
    let pinned = if resources.host_image.contains('@') {
        resources.host_image.clone()
    } else {
        format!("{}@{digest}", resources.host_image)
    };
    Ok((pinned, digest))
}

/// What one installed host group binds to: how many replicas, which broker,
/// and the optional session issuer the overlay pins.
struct HostBinding<'a> {
    replicas: u32,
    nats_url: &'a str,
    native_nats_secrets: &'a [PathBuf],
    source: &'a async_nats::jetstream::stream::Config,
    session: Option<(&'a str, &'a str)>,
}

async fn prepare_host(
    cluster: &Resources,
    inputs: &JourneyDocument,
    carrier: &ReleaseCarrier,
    binding: &HostBinding<'_>,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let native_nats_secrets = binding.native_nats_secrets;
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
                WorkloadRoleFamily::Administration,
            ],
            guest_secret_file: PathBuf::from("guest-sql.json"),
            namespace: NAMESPACE.to_owned(),
            database_host,
        },
    )?;
    for secret in &secrets {
        apply(cluster, &secret.path).await?;
    }
    for secret in native_nats_secrets {
        apply(cluster, secret).await?;
    }
    install_route_credential(cluster, inputs, NAMESPACE).await?;
    let registry_secret = cluster.work.join("registry-pull.json");
    write_private(
        &registry_secret,
        &serde_json::to_vec(&json!({
            "apiVersion":"v1", "kind":"Secret", "type":"kubernetes.io/dockerconfigjson",
            "metadata":{"name":"wamn-registry-pull","namespace":NAMESPACE},
            "stringData":{".dockerconfigjson":fs::read_to_string(&inputs.registry_auth_file)?},
        }))?,
    )?;
    apply(cluster, &registry_secret).await?;
    let operator_values = cluster.work.join("operator-values.yaml");
    fs::write(
        &operator_values,
        serde_json::to_vec(&json!({"operator":{
            "watchNamespaces":[NAMESPACE],"hostNamespaces":[NAMESPACE],"allowSharedHosts":false,
        }}))?,
    )?;
    wamn_test_infrastructure::platform::install(
        &cluster.repository,
        &cluster.lifecycle,
        &cluster.name,
        &cluster.work,
        NAMESPACE,
        &operator_values,
    )
    .await?;
    render_host(cluster, inputs, carrier, &secrets, binding)
}

/// Render the base and overlay values of `cluster`'s one host group into its
/// work directory, bound to `secrets`.
fn render_host(
    cluster: &Resources,
    inputs: &JourneyDocument,
    carrier: &ReleaseCarrier,
    secrets: &[HostSecret],
    binding: &HostBinding<'_>,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let HostBinding {
        replicas,
        nats_url,
        source,
        session,
        ..
    } = *binding;
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
    let event = EventIdentity {
        org: identity().org.clone(),
        project: identity().project.clone(),
        environment: identity().environment.clone(),
    };
    let mut overlay = fs::read_to_string(
        cluster
            .repository
            .join("deploy/platform/values-host-receiving-pat.yaml"),
    )?;
    if cluster.candidate.is_some() {
        overlay = overlay_values(&overlay, &event)?;
    }
    let mut values = render_host_values(
        &fs::read_to_string(
            cluster
                .repository
                .join("deploy/platform/values-host-default.yaml"),
        )?,
        &overlay,
        &HostValuesInput {
            namespace: NAMESPACE.to_owned(),
            host_tag: cluster.name.clone(),
            replicas,
            component_artifact_base: inputs.component_artifact_base.clone(),
            release_artifact_base: carrier.artifact_base.clone(),
            manifest_digest: carrier.manifest_digest.to_string(),
            nats_url: nats_url.to_owned(),
            event,
            guest_secret_name: guest.name.clone(),
            role_secrets: roles,
            object_store_secret_name: None,
            stream_replicas: source.num_replicas,
            dup_window_secs: source.duplicate_window.as_secs(),
        },
    )?;
    if cluster.candidate.is_some() {
        values.base = super::delivery::host_values(&values.base, &cluster.host_image)?;
    }
    assert_rendered_identity(
        &values.overlay,
        &HostIdentity {
            org: identity().org.clone(),
            project: identity().project.clone(),
            schema: "receiving".to_owned(),
        },
    )?;
    let base = cluster.work.join("host-base.yaml");
    let overlay = cluster.work.join("host-overlay.yaml");
    fs::write(&base, values.base)?;
    let overlay_values = if let Some((issuer, instance_suffix)) = session {
        wamn_gate_harness::session_issuer::adjust_host(&values.overlay, issuer, instance_suffix)?
    } else {
        values.overlay
    };
    fs::write(&overlay, overlay_values)?;
    Ok((base, overlay))
}

async fn install_route_credential(
    cluster: &Resources,
    inputs: &JourneyDocument,
    namespace: &str,
) -> anyhow::Result<()> {
    ensure!(
        fs::metadata(&inputs.operator_secret_output)?
            .permissions()
            .mode()
            & 0o777
            == 0o600,
        "the operator Secret must have mode 0600"
    );
    let mut secret: Value = serde_json::from_slice(&fs::read(&inputs.operator_secret_output)?)?;
    ensure!(
        secret["kind"] == "Secret"
            && secret["type"] == "Opaque"
            && secret["metadata"]["annotations"]["wamn.io/credential-purpose"] == "operator"
            && secret["stringData"]["token"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
        "the operator Secret must keep its declared purpose and token"
    );
    secret["metadata"]["namespace"] = json!(namespace);
    let path = cluster.work.join("operator-pat-kube.json");
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

/// Name this test's new result directory and print its path.
///
/// `WAMN_RECEIVING_EVIDENCE_DIR` names an existing parent directory. The
/// parent is the system temporary directory when the variable is not set.
/// The test creates the returned directory.
fn evidence_directory() -> anyhow::Result<PathBuf> {
    let parent = std::env::var_os("WAMN_RECEIVING_EVIDENCE_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let path = parent
        .canonicalize()
        .with_context(|| {
            format!(
                "the Receiving result parent directory {} must exist",
                parent.display()
            )
        })?
        .join(format!(
            "wamn-receiving-results-{}",
            uuid::Uuid::new_v4().simple()
        ));
    println!("Receiving test results: {}", path.display());
    Ok(path)
}

async fn with_signals(
    evidence: &Path,
    operation: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    use futures_util::FutureExt as _;
    use tokio::signal::unix::{SignalKind, signal};
    wash_runtime::init_crypto();
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
    if let Err(error) = &result
        && evidence.is_dir()
    {
        fs::write(
            evidence.join("failure.json"),
            serde_json::to_vec_pretty(&json!({
                "verdict":"fail", "failure":format!("{error:#}"),
            }))?,
        )?;
    }
    result
}

#[tokio::test]
async fn cluster_startup_initializes_tls_client_provider() -> anyhow::Result<()> {
    let evidence = std::env::temp_dir().join(format!("receiving-tls-{}", uuid::Uuid::new_v4()));
    with_signals(&evidence, async {
        let _client = async_nats::rustls::ClientConfig::builder()
            .with_root_certificates(async_nats::rustls::RootCertStore::empty())
            .with_no_client_auth();
        Ok(())
    })
    .await
}

async fn released_http(
    cluster: &ReceivingCluster,
    replicas: u32,
) -> anyhow::Result<(ProvisionedRoute, String)> {
    let (route, carrier) = provision(cluster).await?;
    seed(&route).await?;
    serve_released_http(cluster, replicas, route, carrier).await
}

/// Install the release through the release chart and check its hosts and its
/// HTTP role. Returns the route and the HTTP role Service.
async fn serve_released_http(
    cluster: &ReceivingCluster,
    replicas: u32,
    route: ProvisionedRoute,
    carrier: ReleaseCarrier,
) -> anyhow::Result<(ProvisionedRoute, String)> {
    use wamn_test_infrastructure::{release_chart, workload};
    let secrets = deployment::native_secrets(cluster)?;
    let resources = &cluster.resources;
    let (issuer, instance) = session_cluster::prepare_application(cluster, &carrier).await?;
    let deployed = install_release(
        cluster,
        &carrier,
        &HostBinding {
            replicas,
            nats_url: &cluster.nats_url,
            native_nats_secrets: &secrets,
            source: &cluster.source,
            session: Some((&issuer, &instance)),
        },
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
            "release",
            &gates_evidence,
        )
        .await?;
    }
    let release = deployed.release(resources, &carrier);
    let hosts = release_chart::hosts_ready(
        &release,
        &deployed.host_image,
        &deployed.runtime_digest,
        replicas,
    )
    .await?;
    release_chart::role_placed(&release, "http", &hosts).await?;
    Ok((route, deployed.http_service()))
}
