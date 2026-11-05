//! Scheduled integration pull and CDC push in the environment kind case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use wamn_catalog::{ComponentPackageScope, PackageCoordinate, WiringDocument};
use wamn_control_provision::events::source_stream_config;
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_dev_control::dev::environment::connect;
use wamn_environment_control::author_workflow::WorkflowTrigger;
use wamn_environment_control::environment::{self, Platform};
use wamn_package_control::component_admission::{AdmitComponentRequest, admit_component};
use wamn_package_control::component_declaration::{
    authored_base_digests, render_declaration_document, render_palette_declaration,
};
use wamn_package_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};
use wamn_platform_identity::auth_changes::AuthChanges;
use wamn_platform_identity::nats::PlatformNats;
use wamn_release_control::publish_release::{PublishReleaseRequest, assemble_local_release};
use wamn_release_control::release_chart::ImageSet;
use wamn_test_infrastructure::event_broker;

use wamn_environment_control::environment::document::DeclaredRelease;

use super::{NAMESPACE, ORG, PROJECT, Run, http_connection, qualification_artifact};

const ENVIRONMENT: &str = "integration";
const CONSUMER: &str = "rest_consumer_fixture";
/// The execution role of the connector's pull: exactly the consumer command
/// its graph calls.
pub const PULL_ROLE: &str = "connector-pull";
/// The wiring of the connector's scheduled pull.
fn pull_wiring(connector: &str) -> &'static str {
    if connector == "netsuite" {
        "netsuite_pull"
    } else {
        "rest_item"
    }
}

/// One owned connection, as the workflow verbs take it.
/// Publish fixture role changes on the scheduler NATS that the hosts watch.
pub async fn auth_changes(run: &Run) -> anyhow::Result<(AuthChanges, Child)> {
    let (connection, forward) = nats_forward(run, "wamn-ctl-nats").await?;
    Ok((AuthChanges::connect(&connection).await?, forward))
}

/// Install the CA, the certificates and the one NATS in the test namespace,
/// with the users that platform apply writes for `environments` of
/// ORG/PROJECT (docs/plan/alignment.md R1-R5).
pub async fn install_one_nats(
    run: &Run,
    repository: &Path,
    environments: &[&str],
) -> anyhow::Result<()> {
    let objects = wamn_environment_control::platform::manifests::one_nats(repository, NAMESPACE)?;
    let (mut certificates, server): (Vec<_>, Vec<_>) = objects.into_iter().partition(|object| {
        matches!(
            object["kind"].as_str(),
            Some("ClusterIssuer" | "Certificate")
        )
    });
    let scopes = environments
        .iter()
        .map(|env| wamn_control_registry::Triple::new(ORG, PROJECT, *env))
        .collect::<Vec<_>>();
    let users = wamn_environment_control::platform::single_namespace_nats(NAMESPACE, ORG, &scopes)?;
    // The namespace may exist already, so it is applied with the certificates.
    certificates.insert(
        0,
        json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": NAMESPACE}}),
    );
    for (name, objects) in [("certificates", certificates), ("users", users)] {
        let file = run.work.join(format!("one-nats-{name}.json"));
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({"apiVersion": "v1", "kind": "List", "items": objects}))?,
        )?;
        run.run("kubectl", &["apply", "-f", &file.to_string_lossy()])
            .await?;
        if name == "certificates" {
            run.run(
                "kubectl",
                &[
                    "wait",
                    "--for=condition=Ready",
                    "clusterissuer/wasmcloud-ca",
                    "--timeout=120s",
                ],
            )
            .await?;
        }
    }
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "wait",
            "--for=condition=Ready",
            "certificate",
            "--all",
            "--timeout=180s",
        ],
    )
    .await?;
    let file = run.work.join("one-nats-server.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({"apiVersion": "v1", "kind": "List", "items": server}))?,
    )?;
    run.run("kubectl", &["apply", "-f", &file.to_string_lossy()])
        .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "rollout",
            "status",
            "statefulset/wamn-nats",
            "--timeout=300s",
        ],
    )
    .await?;
    Ok(())
}

/// The certificate of the Secret `name` in the test namespace, as files of
/// this run.
async fn certificate(
    run: &Run,
    name: &str,
    user: &str,
) -> anyhow::Result<event_broker::Credentials> {
    let secret = run
        .json(
            "kubectl",
            &["-n", NAMESPACE, "get", "secret", name, "-o", "json"],
        )
        .await?;
    let mut files = Vec::new();
    for key in ["ca.crt", "tls.crt", "tls.key"] {
        let file = run.work.join(format!("{name}-{key}"));
        std::fs::write(
            &file,
            STANDARD.decode(secret["data"][key].as_str().context("certificate key")?)?,
        )?;
        files.push(file);
    }
    let [ca, cert, key] =
        <[PathBuf; 3]>::try_from(files).map_err(|_| anyhow::anyhow!("three files"))?;
    // The platform users reply on `_INBOX` (platform/broker.rs platform_users).
    Ok(event_broker::Credentials {
        user: user.to_owned(),
        inbox: "_INBOX".to_owned(),
        ca,
        cert,
        key,
    })
}

/// Forward the one NATS to a local port. The server certificate names
/// 127.0.0.1 for this.
fn forward(run: &Run) -> anyhow::Result<(String, Child)> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let forward = Command::new("kubectl")
        .env("KUBECONFIG", &run.kept.kubeconfig)
        .args([
            "-n",
            NAMESPACE,
            "port-forward",
            "svc/wamn-nats",
            &format!("{port}:4222"),
        ])
        .kill_on_drop(true)
        .spawn()
        .context("forward the one NATS")?;
    Ok((format!("nats://127.0.0.1:{port}"), forward))
}

/// The connection of the platform client whose certificate is the Secret
/// `name`, through a port forward that the caller keeps.
pub async fn nats_forward(run: &Run, name: &str) -> anyhow::Result<(PlatformNats, Child)> {
    let user = name.trim_end_matches("-nats");
    let certificate = certificate(run, name, user).await?;
    let (url, forward) = forward(run)?;
    let connection = certificate.platform_nats(&url);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match connection.connect().await {
            Ok(_) => break,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Ok((connection, forward))
}

/// The platform users the integration check acts as: the provisioning Jobs,
/// the CDC reader and wamn-ctl, through one forward.
pub(super) struct ClusterBroker {
    pub(super) provisioning: event_broker::Credentials,
    pub(super) publisher: event_broker::Credentials,
    pub(super) observer: event_broker::Credentials,
    pub(super) server: String,
    _forward: Child,
}

async fn cluster_broker(run: &Run) -> anyhow::Result<ClusterBroker> {
    let (server, forward) = forward(run)?;
    let broker = ClusterBroker {
        provisioning: certificate(run, "wamn-provisioning-nats", "wamn-provisioning").await?,
        publisher: certificate(run, "wamn-cdc-reader-nats", "wamn-cdc-reader").await?,
        observer: certificate(run, "wamn-ctl-nats", "wamn-ctl").await?,
        server,
        _forward: forward,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match event_broker::connect(&broker.provisioning, &broker.server).await {
            Ok(_) => break,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(broker)
}

pub async fn exercise(
    run: &mut Run,
    repository: &Path,
    platform: &Platform,
    registry: &PackageRegistry,
    images: &ImageSet,
    admin_url: &str,
    connector: &str,
) -> anyhow::Result<()> {
    let scope = wamn_control_registry::Triple::new(ORG, PROJECT, ENVIRONMENT);
    let env_key = wamn_control_registry::project_env_key(ORG, PROJECT, ENVIRONMENT);
    let roots = [
        "apps/platform/wamn_integration",
        "apps/rest_consumer_fixture",
        &format!("apps/{connector}"),
    ]
    .map(|path| repository.join(path));
    let manifests: Vec<wamn_catalog::manifest::PackageManifest> = roots
        .iter()
        .map(|root| {
            wamn_schema_generator::build_record::check_current(root)?;
            Ok(serde_json::from_slice(&std::fs::read(
                wamn_schema_generator::package_manifest_path(root),
            )?)?)
        })
        .collect::<anyhow::Result<_>>()?;
    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let broker = cluster_broker(run).await?;
    let server = broker.server.clone();
    let mut platform = platform.clone();
    platform.events = Some(environment::EventBroker {
        nats: broker.provisioning.platform_nats(&server),
        stream_replicas: 1,
        dup_window_secs: 120,
    });
    if run.reuse {
        // A required setup error can leave the check's environment partly applied.
        let (system, driver) = connect(&crate::observer_url(run, &platform).await?).await?;
        let exists = system
            .query_opt(
                "SELECT 1 FROM registry.project_envs WHERE org=$1 AND project=$2 AND env=$3",
                &[&ORG, &PROJECT, &ENVIRONMENT],
            )
            .await?
            .is_some();
        driver.abort();
        if exists {
            environment::delete::delete(&platform, &scope, true).await?;
        }
    }
    let mut document = fixture_document(connector);
    // The preceding edge paging assertion used the same route. A fresh process
    // gives this independent flow its own page-two refusal and request log.
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "rollout",
            "restart",
            "deployment/http-fixture",
        ],
    )
    .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "rollout",
            "status",
            "deployment/http-fixture",
            "--timeout=120s",
        ],
    )
    .await?;
    let file = run.work.join("integration.k");
    std::fs::write(&file, document.to_kcl())?;
    // Platform install provisions with release=none before it renders the
    // session audience from the environment's allocated instance suffix.
    environment::apply(&platform, &file, &crate::ensure_admin_url(&platform)).await?;
    // Bind only after the connection-free bootstrap. The release below
    // supplies the upstream declaration for the first bound apply.
    super::org_bindings::bind(
        &platform,
        &mut document,
        connector,
        "upstream",
        if connector == "netsuite" {
            "netsuite"
        } else {
            "rest-fixture"
        },
        "http",
        json!({"primary-authority":http_connection::ORIGIN, "tls-verification":"disabled"}),
    )
    .await?;

    http_connection::configure(run, &crate::ensure_admin_url(&platform), ENVIRONMENT).await?;

    let observer = crate::observer_url(run, &platform).await?;
    let (system, system_task) = connect(&observer).await?;
    let suffix: String = system.query_one(
        "SELECT instance_suffix FROM registry.project_envs WHERE org=$1 AND project=$2 AND env=$3",
        &[&ORG, &PROJECT, &ENVIRONMENT],
    ).await?.get(0);
    let issuer = if run.kept.identity_ready {
        // The preceding Epic 4 case installed this issuer in the shared fixture.
        format!("https://integration-session.{NAMESPACE}.svc.cluster.local")
    } else {
        super::session_issuer::deploy(run, repository, &platform).await?
    };
    platform.host_group = Some(event_host_group(run, &platform, &issuer, &suffix)?);
    let mut project_url = url::Url::parse(admin_url)?;
    project_url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(ORG, PROJECT, ENVIRONMENT, &suffix)
    ));
    let bytes = if let Some(bytes) = run.kept.releases.get(connector) {
        bytes.clone()
    } else {
        let bytes = publish(
            run, repository, registry, &roots, &manifests, &env_key, connector,
        )
        .await?;
        run.kept
            .releases
            .insert(connector.to_owned(), bytes.clone());
        bytes
    };
    let pushed = wamn_release_control::publish_release::push_manifest_bytes(
        &bytes,
        &platform.release_artifact_base,
        false,
        &platform.oci_ca_paths,
        &platform.registry_auth_file,
    )
    .await?;
    let digest = pushed.digest.as_str().to_owned();
    let mut releases = registry.clone();
    releases
        .artifact_base
        .clone_from(&platform.release_artifact_base);
    qualification_artifact::push(
        &releases,
        &qualification_artifact::QualificationPayload {
            format: qualification_artifact::QUALIFICATION_PAYLOAD_FORMAT,
            release_digest: digest.clone(),
            image_set: qualification_artifact::image_set_digests(images)?,
            cases: Vec::new(),
        },
        bytes.len(),
    )
    .await?;

    document.release = DeclaredRelease::Digest(digest.clone());
    super::apply_with_diagnostics(&run.work, &platform, &document, "integration").await?;
    let (project, project_task) = connect(project_url.as_str()).await?;
    let report_start = run.report.len();
    let capture_result = async {
    let capture = system
        .query_one(
            "SELECT schema, publication, slot FROM registry.event_readers WHERE org=$1 AND project=$2 AND env=$3",
            &[&ORG, &PROJECT, &ENVIRONMENT],
        )
        .await?;
    let captured: Vec<String> = capture.get(0);
    run.collect("4 integration capture", (|| {
    ensure!(
        captured == [CONSUMER, "wamn_integration"],
        "CDC must capture every installed application schema: {captured:?}"
    );
    Ok(())
    })());
    let publication: String = capture.get(1);
    let slot: String = capture.get(2);
    let publications = project
        .query("SELECT pubname::text FROM pg_publication", &[])
        .await?;
    let slots = project.query(
        "SELECT slot_name::text FROM pg_replication_slots WHERE database = current_database() AND slot_type = 'logical'",
        &[],
    ).await?;
    run.collect("4 integration capture", (|| {
    ensure!(
        publications.len() == 1
            && publications[0].get::<_, String>(0) == publication
            && slots.len() == 1
            && slots[0].get::<_, String>(0) == slot,
        "the environment must have exactly its registered publication and logical slot"
    );
    Ok(())
    })());
    let schemas: Vec<String> = project
        .query_one(
            "SELECT array_agg(n.nspname::text ORDER BY n.nspname) FROM pg_publication_namespace pn \
         JOIN pg_namespace n ON n.oid = pn.pnnspid \
         JOIN pg_publication p ON p.oid = pn.pnpubid WHERE p.pubname = $1",
            &[&publication],
        )
        .await?
        .get(0);
    run.collect("4 integration capture", (|| {
    ensure!(
        schemas == captured,
        "publication schemas differ: {schemas:?}"
    );
    Ok(())
    })());
    // Capture maps identify application entities. History and control tables
    // in the same schemas are explicitly excluded from entity events.
    let tables: Value = project
        .query_one(
            "SELECT jsonb_agg(jsonb_build_object('schema', n.nspname, 'table', c.relname, \
         'identity', c.relreplident, 'published', EXISTS (SELECT FROM pg_publication_tables p \
         WHERE p.pubname = $1 AND p.schemaname = n.nspname AND p.tablename = c.relname)) \
         ORDER BY n.nspname, c.relname) \
         FROM (SELECT relation_oid FROM rest_consumer_fixture.wamn_entities \
               UNION ALL SELECT relation_oid FROM wamn_integration.wamn_entities) e \
         JOIN pg_class c ON c.oid = e.relation_oid JOIN pg_namespace n ON n.oid = c.relnamespace",
            &[&publication],
        )
        .await?
        .get(0);
    run.collect("4 integration capture", (|| {
    ensure!(
        tables
            == json!([
                {"schema":CONSUMER,"table":"item","identity":"f","published":true},
                {"schema":"wamn_integration","table":"log","identity":"f","published":true},
                {"schema":"wamn_integration","table":"setting","identity":"f","published":true}
            ]),
        "every captured table must be published with full replica identity: {tables}"
    );
    Ok(())
    })());
    run.pass(
        "one environment publication and slot capture both application schemas",
        "SELECT registry.event_readers; SELECT pg_publication, pg_publication_namespace, pg_publication_tables, pg_class, pg_replication_slots",
        json!({"schemas":schemas,"publication":publication,"slot":slot,"tables":tables}),
        run.report[report_start..].iter().all(|row| row["result"] != "fail"),
    );
    Ok(())
    }.await;
    run.collect("4 integration capture", capture_result);
    if connector == "netsuite" {
        super::netsuite::seed(&project).await?;
    }
    let mut reader = start_reader(run, repository, &broker, &server, admin_url).await?;
    // The release declares no schedule. The pull is an environment workflow of
    // the connector, authored and enabled after apply
    // (docs/architecture/execution.md#workflows).
    let (auth, _auth_forward) = auth_changes(run).await?;
    let workflows = WorkflowEnvironment {
        system_url: &observer,
        project_url: project_url.as_str(),
        scope: &scope,
        env_key: &env_key,
        digest: &digest,
        applications: &environment::show::serving_applications(&platform, &scope).await?,
        auth: &auth,
    };
    let read = |name: &str| -> anyhow::Result<WiringDocument> {
        let mut value = wamn_schema_generator::operation_reference::read_authored_document(
            &repository.join(format!("apps/{connector}/publication/wirings/{name}.json")),
            wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
        )?;
        if connector == "netsuite" && name == "netsuite_pull" {
            value["nodes"]["page"]["params"]["path-and-query"] =
                json!("/services/rest/query/v1/suiteql?limit=2");
        }
        Ok(serde_json::from_value(value)?)
    };
    let pull = read(pull_wiring(connector))?;
    let push = read(if connector == "netsuite" {
        "netsuite_push"
    } else {
        "rest_item_pushed"
    })?;
    let events = environment::EventBroker {
        nats: broker.provisioning.platform_nats(&server),
        stream_replicas: source.num_replicas,
        dup_window_secs: source.duplicate_window.as_secs(),
    };
    let observed = async {
        enable_trigger(
            &workflows,
            connector,
            &push,
            "connector-push",
            &[],
            WorkflowTrigger::Event {
                source_package: CONSUMER,
                entity: "item",
                ops: &[wamn_event_wire::Op::Insert],
                condition: None,
            },
            Some(&events),
        )
        .await?;

        enable_schedule(
            &workflows,
            connector,
            &pull,
            PULL_ROLE,
            &["rest-consumer-fixture:item/record"],
        )
        .await?;
        if connector == "netsuite" {
            super::netsuite::assertions(run, &project, &workflows).await
        } else {
            let observed = assertions(run, &project, &env_key, &digest, &mut reader).await?;
            disable_schedule(&workflows, connector, &pull.wiring_id).await?;
            assert_schema_triggers(run, &project, &workflows, &events, &broker, &mut reader)
                .await?;
            Ok(observed)
        }
    }
    .await;
    reader.kill().await.context("stop the fixture CDC reader")?;
    reader.wait().await?;
    project_task.abort();
    system_task.abort();
    let observed = observed.with_context(|| {
        format!(
            "CDC reader log: {}",
            std::fs::read_to_string(run.work.join("cdc-reader.log")).unwrap_or_default()
        )
    });
    match observed {
        Ok(observed) => {
            run.pass(
        &format!(
            "{connector} scheduled pull as the workflow user with its role and consumer event push"
        ),
        "author and enable the pull's schedule workflow after apply; SELECT runs, item, setting and command results; GET /recorded",
        observed,
        true,
    );
        }
        Err(error) => run.collect(
            &format!("4 {connector} scheduled pull and events"),
            Err(error),
        ),
    }
    run.retain_failure_diagnostics(connector).await?;
    environment::delete::delete(&platform, &scope, true).await?;
    Ok(())
}

async fn publish(
    run: &Run,
    repository: &Path,
    registry: &PackageRegistry,
    roots: &[PathBuf],
    manifests: &[wamn_catalog::manifest::PackageManifest],
    env_key: &str,
    connector: &str,
) -> anyhow::Result<Vec<u8>> {
    let mut admissions = Vec::new();
    for (root, manifest) in roots.iter().zip(manifests) {
        push_package(&PushPackageRequest {
            package: root.clone(),
            registry: registry.clone(),
            component_artifact_base: format!(
                "{}/wamn/components",
                registry
                    .artifact_base
                    .strip_suffix("/wamn/packages")
                    .context("fixture package registry")?
            ),
        })
        .await?;
        let scope = ComponentPackageScope {
            env_key: env_key.into(),
            package_id: manifest.package.id.clone(),
            package_version: manifest.package.version.clone(),
        };
        let names = if manifest.package.id == connector {
            if connector == "netsuite" {
                vec![connector, "http-request", "http-page", "jsonata"]
            } else {
                vec![connector, "http-page", "jsonata"]
            }
        } else {
            vec![manifest.package.id.as_str()]
        };
        for name in names {
            let declaration = match name {
                "http-request" | "http-page" | "jsonata" => render_palette_declaration(
                    &repository.join(if name == "http-request" {
                        "apps/platform/no-std/http-request/declaration.json.in"
                    } else if name == "http-page" {
                        "apps/platform/no-std/http-page/declaration.json.in"
                    } else {
                        "apps/platform/execution/jsonata/declaration.json.in"
                    }),
                    &scope,
                    None,
                )?,
                _ => render_declaration_document(
                    &root.join(format!("publication/components/{name}.json.in")),
                    env_key,
                )?,
            };
            let file = run.work.join(format!("declaration-{name}.json"));
            std::fs::write(&file, serde_json::to_vec(&declaration)?)?;
            admissions.push(admit_component(AdmitComponentRequest {
                package: root.clone(),
                component_bytes: wamn_schema_generator::output_root(root)
                    .join(format!("components/{name}.wasm")),
                declaration: file,
                admitted_platform_packages: vec![
                    "wamn:node".into(),
                    "wamn:postgres".into(),
                    "wamn:connection".into(),
                ],
                base_digests: authored_base_digests(root)?
                    .into_values()
                    .map(String::from)
                    .collect(),
            })?);
        }
    }
    let request = PublishReleaseRequest {
        applications: [(
            connector.to_owned(),
            wamn_catalog::ApplicationDefinition {
                connections: std::collections::BTreeMap::new(),
                packages: manifests.iter().map(|m| m.package.id.clone()).collect(),
            },
        )]
        .into(),
        application: None,
        database_url: String::new(),
        control_database_url: String::new(),
        org: ORG.into(),
        project: PROJECT.into(),
        environment: ENVIRONMENT.into(),
        verified_publisher_principal: "kind-fixture".into(),
        run_schema: "wamn_run".into(),
        packages: manifests
            .iter()
            .map(|m| PackageCoordinate::new(&m.package.id, &m.package.version))
            .collect::<Result<_, _>>()?,
    };
    Ok(assemble_local_release(&request, roots, &admissions)?
        .published
        .canonical_bytes)
}

fn event_host_group(
    run: &Run,
    platform: &Platform,
    issuer: &str,
    suffix: &str,
) -> anyhow::Result<PathBuf> {
    let group: Value = serde_yaml::from_slice(&std::fs::read(
        platform.host_group.as_ref().context("host group")?,
    )?)?;
    let group = event_host_values(group, issuer, suffix)?;
    let file = run.work.join("integration-host-group.yaml");
    std::fs::write(&file, serde_yaml::to_string(&group)?)?;
    Ok(file)
}

fn binary(repository: &Path, name: &str) -> PathBuf {
    repository
        .join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
        .join("debug")
        .join(name)
}

async fn secret_value(run: &Run, name: &str, key: &str) -> anyhow::Result<Vec<u8>> {
    let value = run
        .json(
            "kubectl",
            &["-n", NAMESPACE, "get", "secret", name, "-o", "json"],
        )
        .await?;
    Ok(STANDARD.decode(value["data"][key].as_str().context("Secret key")?)?)
}

async fn start_reader(
    run: &Run,
    repository: &Path,
    broker: &ClusterBroker,
    server: &str,
    admin_url: &str,
) -> anyhow::Result<Child> {
    let forward = url::Url::parse(admin_url)?;
    let names = [
        wamn_control_provision::workload_secret_name(
            WorkloadRoleFamily::RegistryReader,
            ORG,
            PROJECT,
            ENVIRONMENT,
        ),
        wamn_control_provision::project_env_cdc_secret_name(ORG, PROJECT, ENVIRONMENT),
    ];
    let mut urls = Vec::new();
    for name in names {
        let bytes = secret_value(run, &name, "url").await?;
        let mut url = url::Url::parse(std::str::from_utf8(&bytes)?)?;
        url.set_host(forward.host_str())?;
        url.set_port(forward.port())
            .map_err(|()| anyhow::anyhow!("reader database port"))?;
        urls.push(url.to_string());
    }
    let log = std::fs::File::create(run.work.join("cdc-reader.log"))?;
    let child = Command::new(binary(repository, "wamn-cdc-reader"))
        .env("WAMN_SYSTEM_URL", &urls[0])
        .env("WAMN_CDC_URL", &urls[1])
        .env("WAMN_NATS_URL", server)
        .env("WAMN_NATS_TLS_CA", &broker.publisher.ca)
        .env("WAMN_NATS_TLS_CERT", &broker.publisher.cert)
        .env("WAMN_NATS_TLS_KEY", &broker.publisher.key)
        .args([
            "--org",
            ORG,
            "--project",
            PROJECT,
            "--env",
            ENVIRONMENT,
            "--stream-replicas",
            "1",
            "--dup-window-secs",
            "120",
            "--feedback-secs",
            "1",
            "--slot-poll-secs",
            "0",
        ])
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .context("start production CDC reader")?;
    Ok(child)
}

/// Observe real CDC deliveries before comparing their IDs with workflow runs.
async fn assert_schema_triggers(
    run: &mut Run,
    project: &tokio_postgres::Client,
    workflows: &WorkflowEnvironment<'_>,
    events: &environment::EventBroker,
    broker: &ClusterBroker,
    reader: &mut Child,
) -> anyhow::Result<()> {
    let work = run.work.clone();
    wamn_gate_harness::run_check::check(
        &work,
        "integration_assert_schema_triggers",
        project,
        assert_schema_triggers_inner(run, project, workflows, events, broker, reader),
    )
    .await
}

async fn assert_schema_triggers_inner(
    run: &mut Run,
    project: &tokio_postgres::Client,
    workflows: &WorkflowEnvironment<'_>,
    events: &environment::EventBroker,
    broker: &ClusterBroker,
    reader: &mut Child,
) -> anyhow::Result<()> {
    let triggers = [
        (CONSUMER, "item", "capture-item"),
        ("wamn_integration", "setting", "capture-setting"),
    ];
    for (package, entity, workflow) in triggers {
        let document: WiringDocument = serde_json::from_value(json!({
            "format-version":"0.1", "wiring-id":workflow, "version":1, "entry":"observe",
            "nodes":{"observe":{"component":"jsonata", "interface-version":"0.1.0",
                "operation":"wamn:node/handler@0.1.0", "params":{"expression":"$"}}},
            "edges":[]
        }))?;
        enable_trigger(
            workflows,
            "rest_connector",
            &document,
            workflow,
            &[],
            WorkflowTrigger::Event {
                source_package: package,
                entity,
                ops: &[wamn_event_wire::Op::Update],
                condition: None,
            },
            Some(events),
        )
        .await?;
    }
    let jetstream = async_nats::jetstream::new(
        event_broker::connect(&broker.observer, &events.nats.nats_url).await?,
    );
    let mut stream = jetstream
        .get_stream(wamn_control_provision::event_stream_name(
            ORG,
            PROJECT,
            ENVIRONMENT,
        ))
        .await?;
    let mut sequence = stream.info().await?.state.last_sequence + 1;
    project
        .batch_execute("BEGIN; SET LOCAL app.operation = 'admin:capture-fixture'")
        .await?;
    project
        .execute("SELECT set_config('app.user_id', $1, true)", &[&ENABLER])
        .await?;
    let txid: i64 = project
        .query_one(
            "SELECT pg_current_xact_id()::text::bigint % 4294967296",
            &[],
        )
        .await?
        .get(0);
    ensure!(
        project
            .execute(
                "UPDATE rest_consumer_fixture.item SET name = 'Capture changed' WHERE key = 'one'",
                &[],
            )
            .await?
            == 1,
        "change exactly one consumer item"
    );
    ensure!(project.execute(
        "UPDATE wamn_integration.setting SET value = '\"capture-changed\"'::jsonb WHERE key = 'last_synced_at'", &[],
    ).await? == 1, "change exactly one integration setting");
    project.batch_execute("COMMIT").await?;

    let ids = triggers.map(|(_, _, workflow)| workflow.to_owned());
    let mut deliveries = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        ensure!(
            reader.try_wait()?.is_none(),
            "CDC reader exited; read cdc-reader.log"
        );
        let tail = stream.info().await?.state.last_sequence;
        while sequence <= tail {
            let message = stream.get_raw_message(sequence).await?;
            let envelope: Value = serde_json::from_slice(&message.payload)?;
            if envelope["txid"] == txid {
                let (package, entity, workflow) = triggers
                    .iter()
                    .find(|(package, entity, _)| {
                        envelope["package_id"] == *package && envelope["entity"] == *entity
                    })
                    .context("the changed transaction emits only the two captured entities")?;
                ensure!(envelope["op"] == "update", "expected an update: {envelope}");
                ensure!(
                    message.subject.as_str()
                        == wamn_event_wire::subject(
                            ORG,
                            PROJECT,
                            ENVIRONMENT,
                            entity,
                            wamn_event_wire::Op::Update,
                        ),
                    "the event subject must identify its entity"
                );
                let key = if *package == CONSUMER {
                    "one"
                } else {
                    "last_synced_at"
                };
                ensure!(
                    envelope["new"]["key"] == key,
                    "wrong changed row: {envelope}"
                );
                let lsn = envelope["lsn"].as_u64().context("event LSN")?;
                let delivery = format!(
                    "event:{sequence}:{}",
                    wamn_event_wire::msg_id(PROJECT, ENVIRONMENT, lsn)
                );
                ensure!(
                    deliveries.insert(*workflow, (sequence, delivery)).is_none(),
                    "each row change must produce exactly one delivery"
                );
            }
            sequence += 1;
        }
        let rows: Value = project.query_one(
            "SELECT COALESCE(jsonb_agg(jsonb_build_object('workflow', wiring_id, 'delivery', invoked_by, \
             'status', status, 'trigger', trigger_source, 'fail_type', fail_type, \
             'result', result_json) ORDER BY wiring_id), '[]') \
             FROM wamn_run.runs WHERE env_key = $1 AND package_id = 'rest_connector' AND wiring_id = ANY($2)",
            &[&workflows.env_key, &&ids[..]],
        ).await?.get(0);
        let runs = rows.as_array().context("capture trigger runs")?;
        ensure!(runs.len() <= 2, "duplicate or cross-schema runs: {rows}");
        if deliveries.len() == 2 && runs.len() == 2 {
            let mut settled = true;
            for ((package, entity, workflow), row) in triggers.iter().zip(runs) {
                let (event_sequence, delivery) = &deliveries[workflow];
                ensure!(
                    row["workflow"] == *workflow
                        && row["delivery"] == *delivery
                        && row["trigger"] == "event",
                    "wrong trigger or cross-schema delivery: {rows}"
                );
                ensure!(
                    !matches!(
                        row["status"].as_str(),
                        Some("failed" | "infrastructure-failure")
                    ),
                    "capture workflow failed: {rows}"
                );
                settled &= row["status"] == "completed";
                let durable = format!("entity_{}_{}_{}", package.len(), package, entity);
                let mut consumer = stream
                    .get_consumer::<async_nats::jetstream::consumer::pull::Config>(&durable)
                    .await
                    .map_err(anyhow::Error::from_boxed)?;
                let info = consumer.info().await?;
                settled &= info.ack_floor.stream_sequence >= *event_sequence
                    && info.num_ack_pending == 0
                    && info.num_pending == 0;
            }
            if settled {
                run.pass(
                    "changes in both schemas each reach only their own trigger once",
                    "author and enable two event workflows; UPDATE item; UPDATE setting; read retained CDC deliveries; SELECT wamn_run.runs",
                    json!({"transaction":txid,"deliveries":deliveries,"runs":rows}),
                    true,
                );
                return Ok(());
            }
        }
        ensure!(
            Instant::now() < deadline,
            "schema triggers did not settle in 150 seconds: deliveries={deliveries:?}, runs={rows}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn assertions(
    run: &Run,
    project: &tokio_postgres::Client,
    env_key: &str,
    digest: &str,
    reader: &mut Child,
) -> anyhow::Result<Value> {
    wamn_gate_harness::run_check::check(
        &run.work,
        "integration_assertions",
        project,
        assertions_inner(run, project, env_key, digest, reader),
    )
    .await
}

async fn assertions_inner(
    run: &Run,
    project: &tokio_postgres::Client,
    env_key: &str,
    digest: &str,
    reader: &mut Child,
) -> anyhow::Result<Value> {
    let principal = wamn_project_state::workflow_user_id().to_string();
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        ensure!(
            reader.try_wait()?.is_none(),
            "CDC reader exited; read cdc-reader.log"
        );
        let rows: Value = project.query_one(
            "SELECT jsonb_build_object(\
            'runs', (SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY created_at), '[]') FROM wamn_run.runs r WHERE env_key=$1 AND package_id='rest_connector' AND wiring_id='rest_item'),\
            'items', (SELECT COALESCE(jsonb_agg(jsonb_build_object('key',key,'name',name) ORDER BY key), '[]') FROM rest_consumer_fixture.item i),\
            'last_synced_at', (SELECT value FROM wamn_integration.setting WHERE key='last_synced_at'),\
            'logs', (SELECT COALESCE(jsonb_agg(to_jsonb(l) ORDER BY record_id), '[]') FROM wamn_integration.log l))", &[&env_key],
        ).await?.get(0);
        let runs = rows["runs"].as_array().context("schedule runs")?;
        ensure!(
            runs.iter().all(|r| !matches!(
                r["status"].as_str(),
                Some("failed" | "infrastructure-failure")
            )),
            "integration schedule failed: {rows}"
        );
        let completed: Vec<_> = runs.iter().filter(|r| r["status"] == "completed").collect();
        if !completed.is_empty() && rows["logs"].as_array().context("log rows")?.len() >= 5 {
            ensure!(
                runs.len() == 1 && completed.len() == 1,
                "expected one completed pull: {rows}"
            );
            let scheduled = completed[0];
            let due = &scheduled["input_json"]["due"];
            ensure!(
                scheduled["trigger_source"] == "schedule"
                    && scheduled["invoked_by"] == "schedule"
                    && scheduled["execution_principal_id"] == principal
                    && scheduled["execution_role"] == PULL_ROLE
                    && scheduled["manifest_digest"] == digest
                    && scheduled["idempotency_key"]
                        == format!(
                            "schedule:rest_connector/rest_item:{}",
                            due.as_str().context("schedule due")?
                        )
                    && rows["last_synced_at"] == *due,
                "wrong schedule identity or watermark: {rows}"
            );
            let expected = json!([{"key":"five","name":"Fifth"},{"key":"four","name":"Fourth"},{"key":"one","name":"First"},{"key":"three","name":"Third"},{"key":"two","name":"Second"}]);
            ensure!(rows["items"] == expected, "consumer rows differ: {rows}");
            let provenance: bool = project.query_one("SELECT count(*) = 5 AND bool_and(external_id = key AND external_system_id IS NOT NULL AND external_system_id = (SELECT external_system_id FROM platform.integration WHERE name = 'rest-fixture')) FROM rest_consumer_fixture.item", &[]).await?.get(0);
            ensure!(provenance, "consumer records must retain source provenance");
            let logs = rows["logs"].as_array().unwrap();
            ensure!(
                logs.len() == 5
                    && logs.iter().zip(expected.as_array().unwrap()).all(
                        |(log, item)| log["record_id"] == item["key"]
                            && log["outcome"] == "pushed"
                            && log["delivery_id"].as_str().is_some_and(|id| !id.is_empty())
                    ),
                "logs must contain one pushed row per key: {rows}"
            );
            let requests =
                http_connection::request_records(&http_connection::request_log(run).await?)?;
            let pages: Vec<_> = requests.iter().filter(|r| !r["page"].is_null()).collect();
            ensure!(
                pages.iter().map(|r| r["page"].clone()).collect::<Vec<_>>()
                    == vec![json!(1), json!(2), json!(1), json!(2), json!(3)]
                    && pages
                        .iter()
                        .filter(|r| r["status"] == "429 Too Many Requests")
                        .count()
                        == 1,
                "wrong retry sequence: {pages:?}"
            );
            // All event runs must finish before checking that delivery wrote once.
            let pushed: Value = project.query_one("SELECT COALESCE(jsonb_agg(jsonb_build_object('status', status, 'trigger_source', trigger_source)), '[]') FROM wamn_run.runs WHERE env_key=$1 AND wiring_id='rest_item_pushed'", &[&env_key]).await?.get(0);
            let pushed = pushed.as_array().context("push runs")?;
            if pushed.len() == 5 && pushed.iter().all(|r| r["status"] == "completed") {
                return Ok(json!({"database":rows,"pages":pages,"principal":principal}));
            }
        }
        ensure!(
            Instant::now() < deadline,
            "integration did not complete in 150 seconds: {rows}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub use super::release_documents::workflow::{
    ENABLER, WorkflowEnvironment, admit_workflow, disable_schedule, enable_schedule,
    enable_trigger, event_host_values, fixture_document, next_minute, start_workflow,
};
