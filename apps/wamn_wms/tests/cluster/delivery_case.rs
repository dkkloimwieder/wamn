//! The WMS release delivered with `wamn-ctl env apply` on the kept delivery
//! cluster (docs/architecture/overview.md#environment-lifecycle, wamn-qh1g).
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
use wamn_control_provision::events::source_stream_config;
use wamn_control_registry::Triple;
use wamn_environment_control::environment::{EventBroker, Platform};
use wamn_gate_harness::delivery::{
    self, Application, Inputs, application_directory, coordinate_name, share_kubeconfig,
};
use wamn_gate_harness::journey::JourneyDocument;
use wamn_gate_harness::session_issuer::{IssuerCluster, prepare_application};
use wamn_identity_client::PatIssuerConfig;
use wamn_release_control::release_chart::Target;
use wamn_test_infrastructure::delivery_cluster::{
    KeptCluster, NAMESPACE, ensure_database, start_container,
};
use wamn_test_infrastructure::platform;

use super::{application, bootstrap, build, deployment};
use crate::environment::identity;
use crate::wms_runtime_live::write_result;

/// The application's CloudNativePG namespace on the kept cluster.
const DATABASE_NAMESPACE: &str = "wms-pg";

#[tokio::test]
#[ignore = "requires: bin:docker, bin:kind, bin:kubectl, bin:helm, bin:openssl, env:WAMN_DELIVERY_CLUSTER"]
async fn owned_release_delivery() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:docker, bin:kind, bin:kubectl, bin:helm, bin:openssl, env:WAMN_DELIVERY_CLUSTER",
    );
    ensure!(
        std::env::var_os(wamn_release_control::delivery::CANDIDATE_ENV).is_none(),
        "the owned setup mints its own candidate"
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
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
    build::build(&repository, &target, &evidence).await?;
    delivery::preflight(&target.join("wasm32-wasip2/release/http_route.wasm")).await?;
    let kept = delivery::kept_cluster(
        &repository,
        &Triple::new(
            identity().org.as_str(),
            identity().project.as_str(),
            identity().environment.as_str(),
        ),
    )
    .await?;
    let result = Box::pin(run(&repository, &kept, &target, &evidence)).await;
    write_result(
        &evidence,
        "result.json",
        &json!({
            "cluster":kept.name,"seconds":started.elapsed().as_secs(),
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
) -> anyhow::Result<()> {
    let work = application_directory(kept, "wms")?;
    share_kubeconfig(kept, &work)?;
    let owner = format!("{}-wms", kept.name);
    let files = bootstrap::prepare(&work).await?;
    fs::copy(&kept.registry_auth, work.join("docker/config.json"))?;
    for directory in ["host-secrets", "wasmtime-cache"] {
        DirBuilder::new().mode(0o700).create(work.join(directory))?;
    }
    let triple = Triple::new(
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
    );

    // The kept cluster's one NATS, the labels store and the database of this
    // application.
    let source = source_stream_config(&triple, 1, Duration::from_secs(120));
    let consumers =
        crate::environment::declared_consumers(&crate::environment::package_manifest()?);
    let nats =
        platform::cluster_nats(&work, &kept.name, identity().env_key().as_str(), NAMESPACE).await?;
    let minio = start_container(
        &format!("{owner}-minio"),
        &[
            "--env-file",
            &work.join("minio.env").to_string_lossy(),
            "chainguard/minio@sha256:aea6c163749e1caca37f907976dcf337177ddf5f558808f2aa591618fd3a6eef",
            "server",
            "/tmp/data",
        ],
    )
    .await?;
    let minio_endpoint = format!("http://{minio}:9000");
    deployment::wait_minio(&minio_endpoint).await?;
    super::deployment::labels(&owner, &work, true).await?;
    let database_inputs = wamn_environment_control::platform::journey::delivery_database(
        repository,
        DATABASE_NAMESPACE,
        crate::environment::CLUSTER,
    )?;
    let database = ensure_database(
        kept,
        repository,
        DATABASE_NAMESPACE,
        crate::environment::CLUSTER,
        &format!("{owner}-postgres"),
        &database_inputs.objects,
        &database_inputs.fixture_objects,
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
        postcommit: None,
        materializer: None,
        runtime: None,
    };
    let (route, credentials) = application::provision(&document, &work, &admin_url).await?;
    let release = application::publish(
        &document,
        &route,
        &credentials,
        &application::PublicationInputs {
            label_render: &target.join("wasm32-wasip2/release/label_render.wasm"),
            minio_endpoint: &minio_endpoint,
        },
        // The package stage reads the kept registry over TLS.
        &Platform {
            auth_nats: None,
            system_database_url: document.system_pg_url.clone(),
            target: Target {
                kubeconfig: kept.kubeconfig(),
                context: kept.context(),
                namespace: NAMESPACE.to_owned(),
            },
            chart: wamn_gate_harness::case_release::release_chart()?,
            release_artifact_base: format!("{}/wamn/releases", kept.registry),
            registry_auth_file: document.registry_auth_file.clone(),
            oci_ca_paths: vec![kept.ca.clone()],
            component_artifact_base: Some(format!("{}/wamn/components", kept.registry)),
            package_artifact_base: Some(format!("{}/wamn/packages", kept.registry)),
            database_namespace: DATABASE_NAMESPACE.to_owned(),
            database_host: Some(database.host.clone()),
            database_port: 5432,
            host_group: None,
            pat_issuer: PatIssuerConfig::default(),
            events: None,
        },
        evidence,
    )
    .await?;
    application::seed(&route, evidence).await?;
    let provisioning = nats.provisioning.connect(&nats.server).await?;
    wamn_environment_control::event_streams::provision(
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
        target,
        triple: triple.clone(),
        route_host: identity().route_host.as_str(),
        system_database_url: &document.system_pg_url,
        release_digest: release.manifest_digest.as_str(),
        secrets: vec![
            route.database_url.clone(),
            document.system_pg_url.clone(),
            admin_url.clone(),
            route.token.clone(),
            database.password.clone(),
        ],
    };

    // The session issuer, which the published attachments need, in the
    // application's own namespace: the kept cluster serves one issuer per
    // application.
    delivery::load_images(&app).await?;
    kept.apply(&json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":owner}}))
        .await?;
    let identity_image = format!("wamn-identity:{owner}");
    let (issuer, instance) = prepare_application(
        &IssuerCluster {
            name: &kept.name,
            namespace: &owner,
            work: &work,
            evidence,
            repository,
            system_database_url: &document.system_pg_url,
            identity_image: Some(&identity_image),
            org: identity().org.as_str(),
            project: identity().project.as_str(),
            environment: identity().environment.as_str(),
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
    let renames: Vec<(String, String)> = ["host-session-public-ca"]
        .into_iter()
        .map(|name| (name.to_owned(), coordinate_name(name, &triple)))
        .collect();
    kept.copy_config_map(&owner, &renames[0].0, &renames[0].1)
        .await?;
    let (_, overlay) = application::render_host(
        &document,
        &application::HostBinding {
            host_tag: &owner,
            database_host: &database.host,
            manifest_digest: release.manifest_digest.as_str(),
            source: &source,
            replicas: 1,
            session: (&issuer, &instance),
            candidate: None,
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
                nats: nats.provisioning.platform_nats(&nats.server),
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
    )
}
