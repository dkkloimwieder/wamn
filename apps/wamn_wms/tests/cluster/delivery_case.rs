//! The WMS release delivered with `wamn-ctl env apply` on the kept delivery
//! cluster (docs/plan/platform-deploy.md §10.1, §17.2 Epic 6, issue `wamn-qh1g`).
//!
//! The WMS fixture provisions the environment, applies the package and
//! publishes the release in its project database, on its own CloudNativePG
//! `Cluster` of the kept cluster. The case then qualifies and publishes the
//! release, pushes the role images, writes the environment document and runs
//! `env apply` (`wamn_gate_harness::delivery`). The environment that apply
//! adopts predates it, so the document is `env show` of it with the release.
//!
//! Run it after the Receiving delivery case on the same kept cluster: it then
//! checks A12 with two applications.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::ensure;
use serde_json::json;
use tokio::process::Command;
use wamn_control::delivery::Candidate;
use wamn_control::environment::EventBroker;
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_registry::Triple;
use wamn_gate_harness::delivery::{
    self, Application, Inputs, application_directory, coordinate_name, share_kubeconfig,
};
use wamn_gate_harness::journey::JourneyDocument;
use wamn_gate_harness::session_issuer::{IssuerCluster, prepare_application};
use wamn_test_infrastructure::delivery_cluster::{
    KeptCluster, NAMESPACE, ensure_database, start_container,
};
use wamn_test_infrastructure::event_broker;

use super::{application, bootstrap, build, checked, deployment};
use crate::environment::identity;
use crate::wms_runtime_live::write_result;

/// The application's CloudNativePG namespace on the kept cluster.
const DATABASE_NAMESPACE: &str = "wms-pg";

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl, openssl, WAMN_DELIVERY_CLUSTER"]
async fn owned_release_delivery() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "docker", "kind", "kubectl", "helm", "jq", "curl", "openssl",
    ]);
    ensure!(
        Candidate::from_env()?.is_none(),
        "the owned setup mints its own candidate"
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    super::source_clean(&repository).await?;
    let head = String::from_utf8(
        checked(Command::new("git").current_dir(&repository).args([
            "rev-parse",
            "--verify",
            "HEAD",
        ]))
        .await?,
    )?
    .trim()
    .to_owned();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| repository.join("target"), PathBuf::from);
    let target = if target.is_absolute() {
        target
    } else {
        repository.join(target)
    };
    // WAMN_WMS_EVIDENCE_DIR names an existing parent directory.
    let parent =
        std::env::var_os("WAMN_WMS_EVIDENCE_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let evidence = parent.canonicalize()?.join(format!(
        "wamn-wms-delivery-{}",
        uuid::Uuid::new_v4().simple()
    ));
    DirBuilder::new().mode(0o700).create(&evidence)?;
    println!("WMS delivery results: {}", evidence.display());
    let started = Instant::now();
    let kept = delivery::kept_cluster(&repository).await?;
    let result = Box::pin(run(&repository, &kept, &target, &evidence, &head)).await;
    write_result(
        &evidence,
        "result.json",
        &json!({
            "source":head,"cluster":kept.name,"seconds":started.elapsed().as_secs(),
            "result":if result.is_ok() { "pass" } else { "fail" },
            "failure":result.as_ref().err().map(|error| format!("{error:#}")),
        }),
    )?;
    result
}

async fn run(
    repository: &Path,
    kept: &KeptCluster,
    target: &Path,
    evidence: &Path,
    head: &str,
) -> anyhow::Result<()> {
    let work = application_directory(kept, "wms")?;
    share_kubeconfig(kept, &work)?;
    let owner = format!("{}-wms", kept.name);
    let lifecycle = repository.join("tools/wms-cluster-journey-run");
    build::build(repository, target, evidence, false).await?;
    let files = bootstrap::prepare(repository, &work)?;
    fs::copy(&kept.registry_auth, work.join("docker/config.json"))?;
    for directory in ["host-secrets", "wasmtime-cache"] {
        DirBuilder::new().mode(0o700).create(work.join(directory))?;
    }
    let triple = Triple::new(
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
    );

    // The event broker, the labels store and the database of this application.
    let source = source_stream_config(&triple, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(&triple, source.num_replicas);
    let consumers = crate::environment::declared_consumers()?;
    let broker = event_broker::prepare(
        &work,
        &triple,
        identity().tenant.as_str(),
        &source,
        &advisory,
        &consumers,
    )?;
    let nats = start_container(
        &format!("{owner}-nats"),
        &[
            "--volume",
            &format!(
                "{}:/etc/nats/event-nats.conf:ro",
                broker.configuration.display()
            ),
            "nats:2.10-alpine",
            "--config",
            "/etc/nats/event-nats.conf",
            "--jetstream",
            "--store_dir",
            "/data",
        ],
    )
    .await?;
    let nats_url = format!("nats://{nats}:4222");
    let minio = start_container(
        &format!("{owner}-minio"),
        &[
            "--env-file",
            &work.join("minio.env").to_string_lossy(),
            "minio/minio:RELEASE.2025-09-07T16-13-09Z",
            "server",
            "/data",
        ],
    )
    .await?;
    let minio_endpoint = format!("http://{minio}:9000");
    deployment::wait_minio(&minio_endpoint).await?;
    checked(
        Command::new(&lifecycle)
            .arg("install-labels")
            .arg(&owner)
            .arg(&work),
    )
    .await?;
    let database = ensure_database(
        kept,
        repository,
        DATABASE_NAMESPACE,
        crate::environment::CLUSTER,
        &format!("{owner}-postgres"),
    )
    .await?;
    let admin_url = database.url("postgres");
    deployment::wait_postgres(&admin_url).await?;

    // The fixture: control store, environment, package and release.
    let document = JourneyDocument {
        system_pg_url: database.url("wamn_system"),
        component_directory: target.join("virtualized/std-empty-environment"),
        compilation_cache_directory: work.join("wasmtime-cache"),
        flow_http_wasm: target.join("wasm32-wasip2/release/http_route.wasm"),
        component_artifact_base: format!("{}/wamn/components", kept.http_registry),
        release_artifact_base: format!("{}/wamn/releases", kept.http_registry),
        route_host: identity().route_host.clone(),
        registry_auth_file: work.join("docker/config.json"),
        host_secret_directory: work.join("host-secrets"),
        host_secret_namespace: NAMESPACE.to_owned(),
        operator_secret_output: work.join("operator-pat.json"),
        fresh_only_packages: None,
        overlay_compatibility: None,
        postcommit: None,
        materializer: None,
        runtime: None,
    };
    let (route, release) = application::prepare_application(
        &document,
        &work,
        &admin_url,
        &application::PublicationInputs {
            scenario_worker: &target.join("debug/wamn-scenario-worker"),
            label_render: &target.join("wasm32-wasip2/release/label_render.wasm"),
            minio_endpoint: &minio_endpoint,
            publish_only: true,
        },
        evidence,
    )
    .await?;
    event_broker::write_binding(&broker, &nats_url, &source)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let provisioning = loop {
        match event_broker::connect(&broker.provisioning, &nats_url).await {
            Ok(client) => break client,
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    };
    wamn_control::event_streams::provision(
        &async_nats::jetstream::new(provisioning.clone()),
        &triple,
        source.num_replicas,
        source.duplicate_window,
        &consumers,
    )
    .await?;
    provisioning.drain().await?;

    let app = Application {
        repository,
        kept,
        owner: &owner,
        work: &work,
        evidence,
        source: head,
        target,
        triple: triple.clone(),
        tenant: identity().tenant.as_str(),
        route_host: identity().route_host.as_str(),
        project_database_url: &route.database_url,
        system_database_url: &document.system_pg_url,
        release_digest: release.manifest_digest.as_str(),
        release_artifact_base: &release.artifact_base,
        secrets: vec![
            route.database_url.clone(),
            document.system_pg_url.clone(),
            admin_url.clone(),
            route.token.clone(),
            database.password.clone(),
        ],
    };

    // The session issuer, which the published attachments need, in the
    // namespace named after the kept cluster.
    delivery::build_images(&app).await?;
    kept.apply(&json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":kept.name}}))
        .await?;
    let identity_image = format!("wamn-identity:{owner}");
    let (issuer, instance) = prepare_application(
        &IssuerCluster {
            name: &kept.name,
            work: &work,
            evidence,
            repository,
            lifecycle: &lifecycle,
            source: head,
            system_database_url: &document.system_pg_url,
            identity_image: Some(&identity_image),
            org: identity().org.as_str(),
            project: identity().project.as_str(),
            environment: identity().environment.as_str(),
            tenant: identity().tenant.as_str(),
        },
        release.manifest_digest.as_str(),
    )
    .await?;

    // The environment's Secrets in the operator namespace. The credential
    // Secrets are named from the coordinate already, so apply finds them and
    // prepares no second generation. The fixed names of the overlay get
    // copies named from the coordinate (§9.1).
    deployment::install_application_secrets(&document, &files, &kept.name, &work, &database.host)
        .await?;
    let renames: Vec<(String, String)> = [
        "wamn-event-nats",
        "wamn-materializer-nats",
        "host-session-public-ca",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), coordinate_name(name, &triple)))
    .collect();
    let runtime_password = fs::read_to_string(&broker.runtime.password_file)?;
    deployment::apply_secret(
        &kept.name,
        &work,
        &json!({"apiVersion":"v1","kind":"Secret",
            "metadata":{"name":renames[0].1,"namespace":NAMESPACE},"type":"Opaque",
            "stringData":{"username":broker.runtime.username,"password":runtime_password,
                "org":triple.org,"project":triple.project,"environment":triple.env.as_str(),
                "stream_replicas":source.num_replicas.to_string(),
                "dup_window_secs":source.duplicate_window.as_secs().to_string()}}),
    )
    .await?;
    deployment::apply_secret(
        &kept.name,
        &work,
        &json!({"apiVersion":"v1","kind":"Secret",
            "metadata":{"name":renames[1].1,"namespace":NAMESPACE},"type":"Opaque",
            "stringData":{"binding.json":fs::read_to_string(&broker.binding)?}}),
    )
    .await?;
    kept.copy_config_map(&kept.name, &renames[2].0, &renames[2].1)
        .await?;
    let (_, overlay) = application::render_host(
        &document,
        &application::HostBinding {
            host_tag: &owner,
            nats_url: &nats_url,
            database_host: &database.host,
            manifest_digest: release.manifest_digest.as_str(),
            source: &source,
            replicas: 1,
            session: (&issuer, &instance),
        },
        &work,
    )?;

    let delivered = delivery::deliver(
        &app,
        &Inputs {
            overlay: &overlay,
            renames: &renames,
            database_namespace: DATABASE_NAMESPACE,
            database_host: &database.host,
            events: EventBroker {
                nats_url: nats_url.clone(),
                nats_username: broker.provisioning.username.clone(),
                nats_password_file: broker.provisioning.password_file.clone(),
                stream_replicas: source.num_replicas,
                dup_window_secs: source.duplicate_window.as_secs(),
            },
        },
    )
    .await?;
    write_result(
        evidence,
        "delivered.json",
        &json!({"release_name":delivered.release_name,"host_image":delivered.host_image,
            "plan":delivered.plan,"release":release.manifest_digest.as_str()}),
    )?;
    // The nodes hold the native images; the owned registry goes.
    checked(
        Command::new(repository.join("tools/delivery-owned"))
            .arg("remove-native")
            .arg(&owner)
            .arg(&work),
    )
    .await?;
    super::source_clean(repository).await
}
