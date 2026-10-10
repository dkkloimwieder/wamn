//! The Receiving release delivered with `wamn-ctl env apply` on the kept
//! delivery cluster (docs/plan/platform-deploy.md §10.1, §17.2 Epic 6, issue
//! `wamn-qh1g`).
//!
//! The Receiving fixture provisions the environment and publishes the release
//! in its project database, on its own CloudNativePG `Cluster` of the kept
//! cluster. The case then qualifies and publishes the release, pushes the role
//! images, writes the environment document and runs `env apply`
//! (`wamn_gate_harness::delivery`). The environment that apply adopts predates
//! it, so the document is `env show` of it with the release.
//!
//! Run it first on a fresh kept cluster; the WMS delivery case then checks
//! A12 with two applications.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control::delivery::Candidate;
use wamn_control::environment::EventBroker;
use wamn_control::print_release_env::lookup_release_carrier;
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_provision::workload_role::WorkloadRoleFamily;
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
use wamn_test_infrastructure::secrets::{HostSecretsInput, derive_host_secrets};

use super::super::{FIRST_RELEASE, connect, identity, release_digest_at, repository_root, routes};
use super::{HostBinding, build, render_host, resources};

/// The application's CloudNativePG namespace on the kept cluster.
const DATABASE_NAMESPACE: &str = "receiving-pg";
/// The pool `provision_journey_control` stamps on the journey org.
const DATABASE_CLUSTER: &str = "route-auth-pg18";

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl, cargo-sqlx, openssl, WAMN_DELIVERY_CLUSTER"]
async fn owned_release_delivery() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "docker",
        "kind",
        "kubectl",
        "helm",
        "jq",
        "curl",
        "cargo-sqlx",
        "openssl",
    ]);
    ensure!(
        Candidate::from_env()?.is_none(),
        "the owned setup mints its own candidate"
    );
    wash_runtime::init_crypto();
    let repository = repository_root()?;
    let head = resources::committed_head(&repository).await?;
    let evidence = super::evidence_directory()?;
    DirBuilder::new().mode(0o700).create(&evidence)?;
    let started = Instant::now();
    let kept = delivery::kept_cluster(&repository).await?;
    let result = Box::pin(run(&repository, &kept, &evidence, &head)).await;
    fs::write(
        evidence.join("delivery-result.json"),
        serde_json::to_vec_pretty(&json!({
            "source_commit":head,"cluster":kept.name,"seconds":started.elapsed().as_secs(),
            "result":if result.is_ok() { "pass" } else { "fail" },
            "failure":result.as_ref().err().map(|error| format!("{error:#}")),
        }))?,
    )?;
    result
}

#[expect(clippy::too_many_lines, reason = "one delivery, step after step")]
async fn run(
    repository: &Path,
    kept: &KeptCluster,
    evidence: &Path,
    head: &str,
) -> anyhow::Result<()> {
    let work = application_directory(kept, "receiving")?;
    share_kubeconfig(kept, &work)?;
    let owner = format!("{}-receiving", kept.name);
    let lifecycle = repository.join("tools/receiving-cluster-journey-run");
    let artifacts = build::components_and_tools(repository, evidence, false).await?;
    for directory in ["docker", "host-secrets", "wasmtime-cache"] {
        DirBuilder::new().mode(0o700).create(work.join(directory))?;
    }
    fs::copy(&kept.registry_auth, work.join("docker/config.json"))?;
    let triple = Triple::new(
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
    );

    // The event broker and the database of this application.
    let source = source_stream_config(&triple, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(&triple, 1);
    let consumers = super::declared_consumers()?;
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
        &async_nats::jetstream::new(provisioning),
        &triple,
        source.num_replicas,
        source.duplicate_window,
        &consumers,
    )
    .await?;
    let database = ensure_database(
        kept,
        repository,
        DATABASE_NAMESPACE,
        DATABASE_CLUSTER,
        &format!("{owner}-postgres"),
    )
    .await?;
    let (admin, task) = connect(&database.url("postgres")).await?;
    let created = admin.batch_execute("CREATE DATABASE wamn_system").await;
    task.abort();
    created.context("create the Receiving system database")?;

    // The fixture: control store, environment, packages and release.
    let inputs = JourneyDocument {
        system_pg_url: database.url("wamn_system"),
        component_directory: artifacts.components.clone(),
        compilation_cache_directory: work.join("wasmtime-cache"),
        flow_http_wasm: artifacts.http.clone(),
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
    let route = routes::publish_receiving_release(
        &inputs,
        &artifacts.target.join("debug/wamn-scenario-worker"),
    )
    .await?;
    let (project, project_task) = connect(&route.database_url).await?;
    let seeded = super::super::environment::seed_receiving_business_rows(project.as_ref()).await;
    project_task.abort();
    seeded?;
    let carrier = lookup_release_carrier(
        &route.database_url,
        identity().tenant.as_str(),
        &release_digest_at(&route.database_url, FIRST_RELEASE).await?,
        &inputs.release_artifact_base,
    )
    .await?;
    let digest = carrier.manifest_digest.to_string();

    let app = Application {
        repository,
        kept,
        owner: &owner,
        work: &work,
        evidence,
        source: head,
        target: &artifacts.target,
        triple: triple.clone(),
        tenant: identity().tenant.as_str(),
        route_host: identity().route_host.as_str(),
        project_database_url: &route.database_url,
        system_database_url: &inputs.system_pg_url,
        release_digest: &digest,
        release_artifact_base: &carrier.artifact_base,
        secrets: vec![
            route.database_url.clone(),
            inputs.system_pg_url.clone(),
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
            system_database_url: &inputs.system_pg_url,
            identity_image: Some(&identity_image),
            org: identity().org.as_str(),
            project: identity().project.as_str(),
            environment: identity().environment.as_str(),
            tenant: identity().tenant.as_str(),
        },
        &digest,
    )
    .await?;

    // The environment's Secrets in the operator namespace. The credential
    // Secrets are named from the coordinate already, so apply finds them and
    // prepares no second generation. The fixed names of the overlay get
    // copies named from the coordinate (§9.1).
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
            database_host: database.host.clone(),
        },
    )?;
    for secret in &secrets {
        let body: Value = serde_json::from_slice(&fs::read(&secret.path)?)?;
        kept.apply(&body).await?;
    }
    let renames: Vec<(String, String)> = [
        "wamn-event-nats",
        "wamn-materializer-nats",
        "host-session-public-ca",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), coordinate_name(name, &triple)))
    .collect();
    kept.apply(&json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
    "metadata":{"name":renames[0].1,"namespace":NAMESPACE},
    "stringData":{
        "username":broker.runtime.username,
        "password":fs::read_to_string(&broker.runtime.password_file)?,
        "org":triple.org,"project":triple.project,"environment":triple.env.as_str(),
        "stream_replicas":source.num_replicas.to_string(),
        "dup_window_secs":source.duplicate_window.as_secs().to_string(),
    }}))
    .await?;
    kept.apply(&json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":renames[1].1,"namespace":NAMESPACE},
        "stringData":{"binding.json":fs::read_to_string(&broker.binding)?}}))
        .await?;
    kept.copy_config_map(&kept.name, &renames[2].0, &renames[2].1)
        .await?;
    let host = resources::delivery(repository, &work, evidence, &owner, head);
    let (_, overlay) = render_host(
        &host,
        &inputs,
        &carrier,
        &secrets,
        &HostBinding {
            replicas: 1,
            nats_url: &nats_url,
            native_nats_secrets: &[],
            source: &source,
            session: Some((&issuer, &instance)),
        },
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
    delivery::write_evidence(
        evidence,
        "delivered.json",
        &json!({"release_name":delivered.release_name,"host_image":delivered.host_image,
            "plan":delivered.plan,"release":digest}),
    )?;
    // The nodes hold the native images; the owned registry goes.
    resources::checked(
        Command::new(repository.join("tools/delivery-owned"))
            .arg("remove-native")
            .arg(&owner)
            .arg(&work),
    )
    .await?;
    ensure!(
        resources::committed_head(repository).await? == head,
        "the source commit changed during the test"
    );
    Ok(())
}
