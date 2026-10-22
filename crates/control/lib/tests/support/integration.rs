//! Scheduled integration pull and CDC push in the environment kind case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use wamn_catalog::{ComponentPackageScope, PackageCoordinate, WiringDocument};
use wamn_control::component_admission::{AdmitComponentRequest, admit_component};
use wamn_control::component_declaration::{
    authored_base_digests, render_declaration_document, render_palette_declaration,
};
use wamn_control::dev::environment::connect;
use wamn_control::environment::{self, Platform};
use wamn_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};
use wamn_control::publish_release::{
    PublishReleaseRequest, ReleaseWiringTarget, assemble_local_release,
};
use wamn_control::release_chart::ImageSet;
use wamn_control_provision::events::{
    advisory_stream_config, registration_consumers, source_stream_config,
};
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_test_infrastructure::event_broker;

use super::{
    ConnectionDefinition, DeclaredRelease, EnvironmentDocument, NAMESPACE, ORG, POLICY, PROJECT,
    Run, http_connection, qualification_artifact,
};

const ENVIRONMENT: &str = "integration";
const CONNECTOR: &str = "rest_connector";
const CONSUMER: &str = "rest_consumer_fixture";

pub async fn exercise(
    run: &mut Run,
    repository: &Path,
    platform: &Platform,
    registry: &PackageRegistry,
    images: &ImageSet,
    admin_url: &str,
) -> anyhow::Result<()> {
    let scope = wamn_control_registry::Triple::new(ORG, PROJECT, ENVIRONMENT);
    let tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, ENVIRONMENT);
    let roots = [
        "apps/platform/wamn_integration",
        "apps/rest_consumer_fixture",
        "apps/rest_connector",
    ]
    .map(|path| repository.join(path));
    let manifests: Vec<wamn_schema_generator::PackageManifest> = roots
        .iter()
        .map(|root| {
            Ok(serde_json::from_slice(&std::fs::read(
                wamn_schema_generator::package_manifest_path(root),
            )?)?)
        })
        .collect::<anyhow::Result<_>>()?;
    let bytes = publish(run, repository, registry, &roots, &manifests, &tenant).await?;
    let pushed = wamn_control::publish_release::push_manifest_bytes(
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

    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let consumers = registration_consumers(&scope, &tenant, &manifests);
    let broker = event_broker::prepare(
        &run.work,
        &scope,
        &tenant,
        &source,
        &advisory_stream_config(&scope, 1),
        &consumers,
    )?;
    let server = start_broker(run, &broker).await?;
    event_broker::write_binding(&broker, &server, &source)?;
    let mut platform = platform.clone();
    platform.events = Some(environment::EventBroker {
        nats_url: server.clone(),
        nats_username: broker.provisioning.username.clone(),
        nats_password_file: broker.provisioning.password_file.clone(),
        stream_replicas: 1,
        dup_window_secs: 120,
    });
    let mut document = EnvironmentDocument {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENVIRONMENT.into(),
        release: DeclaredRelease::None,
        route_host: "integration.orders.example".into(),
        policy: POLICY.into(),
        connections: BTreeMap::new(),
        floors: BTreeMap::new(),
    };
    document.connections.insert("upstream".into(), ConnectionDefinition::Inline {
        requirement_type: wamn_catalog::RequirementType::Http,
        definition: json!({"primary-authority":http_connection::ORIGIN, "tls-verification":"disabled"}),
    });
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
    environment::apply(&platform, &file).await?;

    let (system, system_task) = connect(&platform.system_database_url).await?;
    let suffix: String = system.query_one(
        "SELECT instance_suffix FROM registry.project_envs WHERE org=$1 AND project=$2 AND env=$3",
        &[&ORG, &PROJECT, &ENVIRONMENT],
    ).await?.get(0);
    let issuer = super::session_issuer::deploy(run, repository, &platform).await?;
    platform.host_group =
        Some(event_host_group(run, &platform, &broker, &server, &issuer, &suffix).await?);
    let mut project_url = url::Url::parse(admin_url)?;
    project_url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(ORG, PROJECT, ENVIRONMENT, &suffix)
    ));
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
    let subject = wamn_platform_identity::application_subject(&tenant, CONNECTOR)?;
    grant(
        run,
        repository,
        &platform,
        project_url.as_str(),
        &tenant,
        &format!("{subject}@{}", super::PLATFORM_DOMAIN),
        &manifest,
    )
    .await?;
    document.release = DeclaredRelease::Digest(digest.clone());
    super::apply_with_diagnostics(&run.work, &platform, &document, "integration").await?;
    let (project, project_task) = connect(project_url.as_str()).await?;
    let version = manifests
        .iter()
        .find(|m| m.package.id == CONNECTOR)
        .context("connector manifest")?
        .package
        .version
        .as_str();
    // As in the HTTP fixture, persist the admitted graphs carried by the release.
    for wiring in &manifest.workflow.wirings {
        let graph = &wiring.graph;
        project.execute(
            "INSERT INTO catalog.wirings (tenant_id, package_id, package_version, wiring_id, version, graph_json, wiring_hash) VALUES ($1,$2,$3,$4,$5,$6::text::jsonb,$7)",
            &[&tenant, &wiring.package_id, &version, &graph.wiring_id, &i32::try_from(graph.version)?, &serde_json::to_string(graph)?, &graph.wiring_hash().as_str()],
        ).await?;
    }
    let principal: String = system
        .query_one(
            "SELECT id::text FROM identity.principals WHERE type='service' AND subject=$1",
            &[&subject],
        )
        .await?
        .get(0);
    let captured: String = system
        .query_one(
            "SELECT schema FROM registry.event_readers WHERE org=$1 AND project=$2 AND env=$3",
            &[&ORG, &PROJECT, &ENVIRONMENT],
        )
        .await?
        .get(0);
    ensure!(
        captured == CONSUMER,
        "CDC must capture only the consumer schema: {captured}"
    );
    let mut reader = start_reader(run, repository, &broker, &server, admin_url).await?;
    let observed = assertions(run, &project, &tenant, &principal, &digest, &mut reader).await;
    reader.kill().await.context("stop the fixture CDC reader")?;
    reader.wait().await?;
    project_task.abort();
    system_task.abort();
    let observed = observed.with_context(|| {
        format!(
            "CDC reader log: {}",
            std::fs::read_to_string(run.work.join("cdc-reader.log")).unwrap_or_default()
        )
    })?;
    run.pass("integration schedule pushes five records and CDC logs each once under its principal",
        "wamn-ctl create-role; grant-permission; grant-role; scheduled rest_item; SELECT runs, item, setting, log; kubectl logs http-fixture",
        observed, true);
    environment::delete::delete(&platform, &scope, true).await?;
    Ok(())
}

async fn publish(
    run: &Run,
    repository: &Path,
    registry: &PackageRegistry,
    roots: &[PathBuf],
    manifests: &[wamn_schema_generator::PackageManifest],
    tenant: &str,
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
            tenant_id: tenant.into(),
            package_id: manifest.package.id.clone(),
            package_version: manifest.package.version.clone(),
        };
        let names = if manifest.package.id == CONNECTOR {
            vec![CONNECTOR, "http-page", "jsonata"]
        } else {
            vec![manifest.package.id.as_str()]
        };
        for name in names {
            let declaration = match name {
                "http-page" | "jsonata" => render_palette_declaration(
                    &repository.join(if name == "http-page" {
                        "apps/platform/no-std/http-page/declaration.json.in"
                    } else {
                        "apps/platform/execution/jsonata/declaration.json.in"
                    }),
                    &scope,
                    None,
                )?,
                _ => render_declaration_document(
                    &root.join(format!("publication/components/{name}.json.in")),
                    tenant,
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
    let connector = manifests
        .iter()
        .find(|m| m.package.id == CONNECTOR)
        .context("connector package")?;
    let scope = ComponentPackageScope {
        tenant_id: tenant.into(),
        package_id: CONNECTOR.into(),
        package_version: connector.package.version.clone(),
    };
    let documents = ["rest_item", "rest_item_pushed"]
        .into_iter()
        .map(|name| {
            let value = wamn_schema_generator::operation_reference::read_authored_document(
                &repository.join(format!(
                    "apps/rest_connector/publication/wirings/{name}.json"
                )),
                wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
            )?;
            Ok((
                scope.clone(),
                serde_json::from_value::<WiringDocument>(value)?,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let request = PublishReleaseRequest {
        database_url: String::new(),
        control_database_url: String::new(),
        org: ORG.into(),
        tenant: tenant.into(),
        environment: ENVIRONMENT.into(),
        verified_publisher_principal: "kind-fixture".into(),
        run_schema: "wamn_run".into(),
        packages: manifests
            .iter()
            .map(|m| PackageCoordinate::new(&m.package.id, &m.package.version))
            .collect::<Result<_, _>>()?,
        wirings: documents
            .iter()
            .map(|(scope, graph)| ReleaseWiringTarget {
                package_id: scope.package_id.clone(),
                package_version: scope.package_version.clone(),
                wiring_id: graph.wiring_id.clone(),
                wiring_version: graph.version,
            })
            .collect(),
    };
    Ok(
        assemble_local_release(&request, roots, &admissions, documents)?
            .published
            .canonical_bytes,
    )
}

async fn start_broker(run: &Run, broker: &event_broker::EventBroker) -> anyhow::Result<String> {
    let name = format!("{}-events", run.cluster);
    run.run(
        "docker",
        &[
            "run",
            "--detach",
            "--name",
            &name,
            "--network",
            "kind",
            "--volume",
            &format!("{}:/fixture/nats.conf:ro", broker.configuration.display()),
            "nats:2.10-alpine",
            "--jetstream",
            "--store_dir",
            "/data",
            "--config",
            "/fixture/nats.conf",
        ],
    )
    .await?;
    let ip = run
        .run(
            "docker",
            &[
                "inspect",
                "--format",
                "{{(index .NetworkSettings.Networks \"kind\").IPAddress}}",
                &name,
            ],
        )
        .await?;
    let server = format!("nats://{}:4222", ip.trim());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match event_broker::connect(&broker.provisioning, &server).await {
            Ok(_) => return Ok(server),
            Err(_) if Instant::now() < deadline => tokio::time::sleep(Duration::from_secs(1)).await,
            Err(error) => return Err(error),
        }
    }
}

async fn event_host_group(
    run: &Run,
    platform: &Platform,
    broker: &event_broker::EventBroker,
    server: &str,
    issuer: &str,
    suffix: &str,
) -> anyhow::Result<PathBuf> {
    let secret = run.work.join("integration-events.json");
    std::fs::write(
        &secret,
        serde_json::to_vec(&json!({
            "apiVersion":"v1", "kind":"Secret", "metadata":{"name":"integration-events","namespace":NAMESPACE},
            "stringData":{"password":std::fs::read_to_string(&broker.runtime.password_file)?, "binding.json":std::fs::read_to_string(&broker.binding)?}
        }))?,
    )?;
    run.run("kubectl", &["apply", "-f", &secret.to_string_lossy()])
        .await?;
    let mut group: Value = serde_yaml::from_slice(&std::fs::read(
        platform.host_group.as_ref().context("host group")?,
    )?)?;
    group["env"]
        .as_array_mut()
        .context("host environment")?
        .extend(
            [
                ("WAMN_SESSION_ISSUER", issuer),
                ("WAMN_SESSION_JWKS_CA", "/integration-session/ca.crt"),
                ("WAMN_SESSION_INSTANCE_SUFFIX", suffix),
                ("WAMN_EVT_NATS_URL", server),
                ("WAMN_EVT_NATS_USERNAME", &broker.runtime.username),
                (
                    "WAMN_EVT_NATS_PASSWORD_FILE",
                    "/integration-events/password",
                ),
                (
                    "WAMN_MAT_NATS_BINDING_FILE",
                    "/integration-events/binding.json",
                ),
                ("WAMN_EVT_ORG", ORG),
                ("WAMN_EVT_PROJECT", PROJECT),
                ("WAMN_EVT_ENV", ENVIRONMENT),
                ("WAMN_EVT_STREAM_REPLICAS", "1"),
                ("WAMN_EVT_DUP_WINDOW_SECS", "120"),
            ]
            .map(|(name, value)| json!({"name":name, "value":value})),
        );
    group["volumes"]
        .as_array_mut()
        .context("host volumes")?
        .extend([
            json!({"name":"integration-events","secret":{"secretName":"integration-events"}}),
            json!({"name":"integration-session","configMap":{"name":"integration-session-ca"}}),
        ]);
    group["volumeMounts"]
        .as_array_mut()
        .context("host mounts")?
        .extend([
            json!({"name":"integration-events","mountPath":"/integration-events","readOnly":true}),
            json!({"name":"integration-session","mountPath":"/integration-session","readOnly":true}),
        ]);
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

/// Write the connector's service row from the release manifest, then grant it
/// the consumer permission. The caller runs this before the release apply, so
/// no host has loaded the release and its schedule cannot tick before the grant.
async fn grant(
    run: &Run,
    repository: &Path,
    platform: &Platform,
    project_url: &str,
    tenant: &str,
    subject: &str,
    manifest: &wamn_catalog::ServingManifest,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    for key in ["ca.crt", "tls.crt", "tls.key"] {
        let bytes = secret_value(run, "wasmcloud-runtime-tls", key).await?;
        let path = run.work.join(format!("auth-{key}"));
        std::fs::write(&path, bytes)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let _forward = Command::new("kubectl")
        .env("KUBECONFIG", run.work.join("kubeconfig"))
        .args([
            "-n",
            NAMESPACE,
            "port-forward",
            "svc/nats",
            &format!("{port}:4222"),
        ])
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let auth = wamn_platform_identity::auth_changes::AuthNats {
        nats_url: format!("nats://127.0.0.1:{port}"),
        nats_tls_ca: run.work.join("auth-ca.crt"),
        nats_tls_cert: run.work.join("auth-tls.crt"),
        nats_tls_key: run.work.join("auth-tls.key"),
        nats_tls_server_name: Some(format!("nats.{NAMESPACE}.svc.cluster.local")),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let changes = loop {
        match wamn_platform_identity::auth_changes::AuthChanges::connect(&auth).await {
            Ok(changes) => break changes,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => return Err(error),
        }
    };
    environment::stage::materialize_application_principals(
        &platform.system_database_url,
        project_url,
        tenant,
        manifest,
        &changes,
    )
    .await?;
    for (verb, extra) in [
        ("create-role", vec![]),
        (
            "grant-permission",
            vec!["--operation", "rest-consumer-fixture:item/record"],
        ),
        ("grant-role", vec!["--user", subject]),
    ] {
        let output = Command::new(binary(repository, "wamn-ctl"))
            .env("WAMN_SYSTEM_ADMIN_URL", &platform.system_database_url)
            .env("WAMN_PG_ADMIN_URL", project_url)
            .env("WAMN_AUTH_NATS_URL", &auth.nats_url)
            .env("WAMN_AUTH_NATS_TLS_CA", &auth.nats_tls_ca)
            .env("WAMN_AUTH_NATS_TLS_CERT", &auth.nats_tls_cert)
            .env("WAMN_AUTH_NATS_TLS_KEY", &auth.nats_tls_key)
            .env(
                "WAMN_AUTH_NATS_TLS_SERVER_NAME",
                auth.nats_tls_server_name.as_ref().unwrap(),
            )
            .args([
                verb,
                "--org",
                ORG,
                "--project",
                PROJECT,
                "--env",
                ENVIRONMENT,
                "--tenant",
                tenant,
                "--role",
                "integration-writer",
            ])
            .args(extra)
            .kill_on_drop(true)
            .output()
            .await?;
        ensure!(
            output.status.success(),
            "{verb} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

async fn start_reader(
    run: &Run,
    repository: &Path,
    broker: &event_broker::EventBroker,
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
        .env("WAMN_EVT_NATS_URL", server)
        .env("WAMN_EVT_NATS_USERNAME", &broker.publisher.username)
        .env(
            "WAMN_EVT_NATS_PASSWORD_FILE",
            &broker.publisher.password_file,
        )
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

async fn assertions(
    run: &Run,
    project: &tokio_postgres::Client,
    tenant: &str,
    principal: &str,
    digest: &str,
    reader: &mut Child,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        ensure!(
            reader.try_wait()?.is_none(),
            "CDC reader exited; read cdc-reader.log"
        );
        let rows: Value = project.query_one(
            "SELECT jsonb_build_object(\
            'runs', (SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY created_at), '[]') FROM wamn_run.runs r WHERE tenant_id=$1 AND package_id='rest_connector' AND wiring_id='rest_item'),\
            'items', (SELECT COALESCE(jsonb_agg(to_jsonb(i) ORDER BY key), '[]') FROM rest_consumer_fixture.item i),\
            'last_synced_at', (SELECT value FROM wamn_integration.setting WHERE key='last_synced_at'),\
            'logs', (SELECT COALESCE(jsonb_agg(to_jsonb(l) ORDER BY record_id), '[]') FROM wamn_integration.log l))", &[&tenant],
        ).await?.get(0);
        let runs = rows["runs"].as_array().context("schedule runs")?;
        ensure!(
            runs.iter().all(|r| !matches!(
                r["status"].as_str(),
                Some("failed" | "infrastructure-failure" | "effect-uncertain")
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
                    && scheduled["service_principal_id"] == principal
                    && scheduled["manifest_digest"] == digest
                    && scheduled["idempotency_key"]
                        == format!(
                            "rest_connector/item:{}",
                            due.as_str().context("schedule due")?
                        )
                    && rows["last_synced_at"] == *due,
                "wrong schedule identity or watermark: {rows}"
            );
            let expected = json!([{"key":"five","name":"Fifth"},{"key":"four","name":"Fourth"},{"key":"one","name":"First"},{"key":"three","name":"Third"},{"key":"two","name":"Second"}]);
            ensure!(rows["items"] == expected, "consumer rows differ: {rows}");
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
            let requests = http_connection::request_records(
                &run.run(
                    "kubectl",
                    &["-n", NAMESPACE, "logs", "deployment/http-fixture"],
                )
                .await?,
            )?;
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
            let pushed: Value = project.query_one("SELECT COALESCE(jsonb_agg(jsonb_build_object('status', status, 'trigger_source', trigger_source)), '[]') FROM wamn_run.runs WHERE tenant_id=$1 AND wiring_id='rest_item_pushed'", &[&tenant]).await?.get(0);
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
