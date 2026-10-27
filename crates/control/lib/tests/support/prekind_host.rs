//! Run the environment kind HTTP release through its render on a native host.

use std::net::TcpListener;
use std::process::Stdio;

use super::*;
use futures_util::StreamExt as _;
use tokio::io::AsyncWriteExt as _;
use wamn_catalog::{ComponentConnectionRequirement, ComponentDescriptor, ConnectionTypeDescriptor};
use wamn_control_provision::{SystemReader, host_credentials::CredentialDatabase, sql};
use wamn_engine::artifact_source::local_component_path;
use wamn_runtime::local_application::{LOCAL_FACTS_FILE, LocalApplicationFacts, LocalBindingFacts};
use wamn_test_infrastructure::scratch::ScratchRoot;
use wash_runtime::washlet::{rpc_subject, types::v2};

fn address() -> anyhow::Result<String> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.to_string())
}

fn login(url: &str, role: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(url)?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("login role"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("login password"))?;
    Ok(url.to_string())
}

async fn credential_secrets(
    project: &tokio_postgres::Client,
    system: &mut tokio_postgres::Client,
    project_url: &str,
    system_url: &str,
    scope: &Triple,
) -> anyhow::Result<Vec<Value>> {
    let tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, "http");
    let mut secrets = wamn_gate_harness::render_check::credential_secrets(scope, NAMESPACE);
    for credential in wamn_control_provision::host_credentials::HOST_CREDENTIALS {
        let (client, url) = match credential.database {
            CredentialDatabase::Project => (project, project_url),
            CredentialDatabase::Control => (&*system, system_url),
        };
        let database = url
            .parse::<tokio_postgres::Config>()?
            .get_dbname()
            .context("database")?
            .to_owned();
        let role = match credential.database {
            CredentialDatabase::Control => wamn_control_provision::system_reader_generation_role(
                match credential.family {
                    WorkloadRoleFamily::IdentityReader => SystemReader::Identity,
                    WorkloadRoleFamily::ConnectionReader => SystemReader::Connection,
                    WorkloadRoleFamily::RegistryReader => SystemReader::Registry,
                    _ => unreachable!("control credential family"),
                },
                ORG,
                PROJECT,
                "http",
                &database,
                CredentialGeneration::A,
            ),
            CredentialDatabase::Project => workload_generation_role(
                credential.family,
                if credential.family == WorkloadRoleFamily::App {
                    WorkloadRoleScope::Tenant {
                        tenant: &tenant,
                        database: &database,
                    }
                } else {
                    WorkloadRoleScope::ProjectEnvironment {
                        org: ORG,
                        project: PROJECT,
                        environment: "http",
                        database: &database,
                    }
                },
                CredentialGeneration::A,
            )?,
        };
        client
            .batch_execute(&sql::prepare_workload_generation_sql(
                credential.family,
                &database,
                &role,
                PASSWORD,
                "2099-01-01T00:00:00Z",
            ))
            .await?;
        if credential.database == CredentialDatabase::Control {
            let tx = wamn_control::provision_project_env::provisioning_transaction(system).await?;
            tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
            let mapped: String = tx
                .query_one(
                    sql::upsert_control_author_tenant_mapping_sql(),
                    &[&role, &tenant, &ORG, &PROJECT, &"http"],
                )
                .await?
                .get("tenant_id");
            ensure!(mapped == tenant, "host reader maps to a different tenant");
            tx.commit().await?;
        }
        let name =
            wamn_control_provision::workload_secret_name(credential.family, ORG, PROJECT, "http");
        let secret = secrets
            .iter_mut()
            .find(|secret| secret["metadata"]["name"] == name)
            .context("credential Secret")?;
        secret["stringData"]["url"] = json!(login(url, &role)?);
    }
    Ok(secrets)
}

fn workload(document: &Value, directory: &Path, materializer: &Path) -> anyhow::Result<()> {
    let spec = &document["spec"]["template"]["spec"];
    let is_http = spec["components"].is_array();
    let interfaces = spec["hostInterfaces"]
        .as_array()
        .context("host interfaces")?
        .iter()
        .map(|interface| {
            Ok(v2::WitInterface {
                namespace: interface["namespace"]
                    .as_str()
                    .context("interface namespace")?
                    .into(),
                package: interface["package"]
                    .as_str()
                    .context("interface package")?
                    .into(),
                version: interface["version"]
                    .as_str()
                    .context("interface version")?
                    .into(),
                interfaces: serde_json::from_value(interface["interfaces"].clone())?,
                config: interface
                    .get("config")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default(),
                name: interface["name"].as_str().unwrap_or_default().into(),
            })
        })
        .collect::<anyhow::Result<_>>()?;
    let resources = |value: &Value| -> anyhow::Result<v2::LocalResources> {
        Ok(v2::LocalResources {
            config: serde_json::from_value(value["config"].clone())?,
            environment: value
                .pointer("/environment/config")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?
                .unwrap_or_default(),
            ..Default::default()
        })
    };
    let (components, service) = if is_http {
        let component = &spec["components"][0];
        (
            vec![v2::Component {
                name: component["name"].as_str().context("component name")?.into(),
                image: component["image"]
                    .as_str()
                    .context("entry reference")?
                    .into(),
                image_pull_policy: v2::ImagePullPolicy::Never.into(),
                max_concurrency: serde_json::from_value(component["maxConcurrency"].clone())?,
                local_resources: Some(resources(&component["localResources"])?),
                ..Default::default()
            }],
            None,
        )
    } else {
        let bytes = std::fs::read(materializer)?;
        let digest = wamn_engine::component_admission::component_digest(&bytes);
        std::fs::write(local_component_path(directory, &digest)?, bytes)?;
        (
            Vec::new(),
            Some(v2::Service {
                image: digest,
                image_pull_policy: v2::ImagePullPolicy::Never.into(),
                max_restarts: serde_json::from_value(spec["service"]["maxRestarts"].clone())?,
                local_resources: Some(resources(&spec["service"]["localResources"])?),
                ..Default::default()
            }),
        )
    };
    let name = if is_http { "flow-http" } else { "materializer" };
    let request = v2::WorkloadStartRequest {
        workload_id: format!("prekind-{name}"),
        workload: Some(v2::Workload {
            namespace: NAMESPACE.into(),
            name: name.into(),
            service,
            wit_world: Some(v2::WitWorld {
                components,
                host_interfaces: interfaces,
            }),
            ..Default::default()
        }),
    };
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec(&request)?,
    )?;
    Ok(())
}

fn host_command(
    documents: &[Value],
    secrets: &[Value],
    directory: &Path,
    scheduler: &str,
    http: &str,
    probe: &str,
) -> anyhow::Result<Command> {
    let deployment = documents
        .iter()
        .find(|document| {
            document["kind"] == "Deployment"
                && document["spec"]["template"]["spec"]["containers"][0]["name"] == "host"
        })
        .context("rendered host")?;
    let container = &deployment["spec"]["template"]["spec"]["containers"][0];
    let mut command = Command::new(std::env::var_os("WAMN_HOST_BINARY").context("host binary")?);
    command
        .env_clear()
        .env("HOME", directory)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for variable in container["env"].as_array().context("host environment")? {
        let name = variable["name"].as_str().context("variable name")?;
        let value = if let Some(value) = variable["value"].as_str() {
            // Registry credentials and telemetry endpoints belong to the cluster.
            if matches!(
                name,
                "HOME"
                    | "DOCKER_CONFIG"
                    | "WAMN_REGISTRY_AUTH_FILE"
                    | "WAMN_COMPONENT_ARTIFACT_BASE"
            ) || name.starts_with("OTEL_")
            {
                continue;
            }
            match value {
                "/apply-events/password" => directory.join("password").display().to_string(),
                "/apply-events/binding.json" => {
                    directory.join("binding.json").display().to_string()
                }
                _ => value.to_owned(),
            }
        } else if let Some(reference) = variable.pointer("/valueFrom/secretKeyRef") {
            let secret = secrets
                .iter()
                .find(|secret| secret["metadata"]["name"] == reference["name"])
                .context("resolved Secret")?;
            secret["stringData"][reference["key"].as_str().context("Secret key")?]
                .as_str()
                .context("Secret value")?
                .to_owned()
        } else {
            match name {
                "WASMCLOUD_HOST_IP" => "127.0.0.1".into(),
                "WASMCLOUD_HOST_ENVIRONMENT" => NAMESPACE.into(),
                _ => anyhow::bail!("unmapped field reference {name}"),
            }
        };
        command.env(name, value);
    }
    for argument in container["args"].as_array().context("host args")? {
        let argument = argument.as_str().context("host argument")?;
        let flag = argument.split('=').next().expect("argument flag");
        if flag.contains("-tls-")
            || matches!(
                flag,
                "--oci-ca-path" | "--release-artifact-base" | "--release-manifest-digest"
            )
        {
            continue;
        }
        command.arg(match flag {
            "--scheduler-nats-url" | "--data-nats-url" => format!("{flag}={scheduler}"),
            "--http-addr" => format!("{flag}={http}"),
            "--probe-addr" => format!("{flag}={probe}"),
            "--oci-cache-dir" => format!("{flag}={}", directory.join("oci").display()),
            "--host-name" => "--host-name=prekind".into(),
            _ => argument.to_owned(),
        });
    }
    let manifest = std::fs::read(directory.join(wamn_catalog::RELEASE_MANIFEST_FILE_NAME))?;
    let (_, digest) = wamn_catalog::ServingManifest::from_canonical_bytes(&manifest)?;
    command
        .arg("--local-application")
        .arg(directory)
        .arg("--local-application-digest")
        .arg(digest.as_str())
        .arg("--runner=prekind")
        .arg("--wasmtime-cache-dir")
        .arg(directory.join("compiled"));
    command
        .stdout(std::fs::File::create(directory.join("host.log"))?)
        .stderr(Stdio::from(std::fs::File::create(
            directory.join("host.stderr"),
        )?));
    Ok(command)
}

async fn outcome(project: &tokio_postgres::Client, run: &str) -> anyhow::Result<(String, Value)> {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let row = project
                .query_one(
                    "SELECT status, result_json FROM wamn_run.runs WHERE run_id=$1",
                    &[&run],
                )
                .await?;
            let status: String = row.get(0);
            if matches!(
                status.as_str(),
                "completed" | "failed" | "infrastructure-failure" | "effect-uncertain"
            ) {
                return Ok((
                    status,
                    row.get::<_, Option<Value>>(1).unwrap_or(Value::Null),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("manual workflow timed out")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: WAMN_HOST_BINARY, WAMN_NATIVE_C_NATS_BIN, WAMN_FLOW_HTTP_COMPONENT, WAMN_MATERIALIZER_COMPONENT, built edge_device and debug HTTP guests, helm"]
async fn prekind_http_host_runs_and_rejects_respond_without_caller() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_HOST_BINARY",
        "WAMN_NATIVE_C_NATS_BIN",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_MATERIALIZER_COMPONENT",
        "helm",
    ]);
    let started = Instant::now();
    let _lock = wamn_test_postgres::lock();
    let work = ScratchRoot::create()?;
    let directory = work.path();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, "http");
    let scope = Triple::new(ORG, PROJECT, "http");
    let database = wamn_test_postgres::database();
    let control = wamn_control_provision::test_database::system();
    let (project, project_task) = connect(database.url()).await?;
    let (mut system, system_connection) =
        tokio_postgres::connect(control.url(), tokio_postgres::NoTls).await?;
    let system_task = tokio::spawn(system_connection);
    project
        .batch_execute(&sql::set_database_owner_sql(database.name()))
        .await?;
    project
        .batch_execute(&format!(
            "{} {}",
            sql::ensure_app_acl_role_sql(),
            wamn_schema_control::ensure_scenario_author_role_sql(),
        ))
        .await?;
    wamn_control::dev::environment::install_journey_platform_floor(&project, PLATFORM_DOMAIN)
        .await?;
    wamn_control::reconcile_run_plane::reconcile(
        &project,
        &wamn_schema_control::BareSchemaName::new("wamn_run")?,
        true,
    )
    .await?;
    project.execute("SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:prekind',false)", &[&wamn_control_provision::PlatformComponent::Provisioning.principal_id().to_string()]).await?;
    project
        .execute("SELECT set_config('app.tenant',$1,false)", &[&tenant])
        .await?;
    project
        .batch_execute("INSERT INTO platform.environment(status) VALUES ('active')")
        .await?;
    project.execute("INSERT INTO wamn_run.environment_policies(tenant_id,expected_environment,durability_class) VALUES ($1,'http','standard')", &[&tenant]).await?;
    let instance: u32 = project
        .query_one(
            "SELECT oid FROM pg_database WHERE datname=current_database()",
            &[],
        )
        .await?
        .get(0);
    let marker = wamn_runtime::local_application::local_target_marker(&tenant, "http", instance);
    project
        .batch_execute(&format!(
            "COMMENT ON DATABASE \"{}\" IS '{marker}'",
            database.name()
        ))
        .await?;
    let peer = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", peer.local_addr()?);
    system.batch_execute("SET ROLE wamn_system").await?;
    system.execute("SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:prekind',false)", &[&wamn_control_provision::PlatformComponent::Provisioning.principal_id().to_string()]).await?;
    system.execute("INSERT INTO registry.orgs(id,placement_type,pool_cluster) VALUES ($1,'pooled','prekind')", &[&ORG]).await?;
    system
        .execute(
            "INSERT INTO registry.projects(org,id) VALUES ($1,$2)",
            &[&ORG, &PROJECT],
        )
        .await?;
    let policy = wamn_control_registry::EnvPolicy::dev();
    system
        .execute(
            wamn_control_registry::sql::stamp_env_policy_sql(),
            &[
                &ORG,
                &POLICY,
                &serde_json::to_string(&policy.recovery_domain)?,
                &policy.promotion_rank,
                &policy.instances,
                &policy.storage,
                &policy.cpu,
                &policy.memory,
                &policy.image,
                &policy.backup_cadence,
                &policy.wal_retention,
                &policy.hibernation,
                &policy.durability_class.as_sql(),
            ],
        )
        .await?;
    system
        .query_one(
            wamn_control_registry::sql::upsert_project_env_with_policy_sql(),
            &[
                &ORG,
                &PROJECT,
                &"http",
                &"prekind-database",
                &Some(NAMESPACE),
                &"aaaaaaa1",
                &Some(true),
                &POLICY,
            ],
        )
        .await?;
    system.batch_execute("RESET ROLE").await?;
    // C3.5: a credential imports only into a declared, project-enabled connection.
    let (mut seed, seed_connection) =
        tokio_postgres::connect(control.url(), tokio_postgres::NoTls).await?;
    let seed_task = tokio::spawn(seed_connection);
    seed.batch_execute("SET ROLE wamn_control").await?;
    let tx = seed.transaction().await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let mut drops = wamn_platform_identity::auth_changes::AuthDrops::default();
    wamn_platform_identity::connections::write_connection(
        &tx,
        ORG,
        &wamn_platform_identity::connections::ConnectionWrite {
            integration: "http".into(),
            external_system: None,
            name: "upstream".into(),
            transport: "http".into(),
            exchange: "request".into(),
            parameters: json!({"primary-authority":origin,"tls-verification":"disabled"}),
            status: "active".into(),
        },
        &mut drops,
    )
    .await?;
    wamn_platform_identity::connections::enable_connection(&tx, ORG, PROJECT, "upstream").await?;
    for (scope, id) in [
        ("org", ORG.to_owned()),
        ("project", PROJECT.to_owned()),
        ("environment", format!("{PROJECT}/http")),
    ] {
        wamn_platform_identity::connections::allow_integration(&tx, ORG, scope, &id, "http")
            .await?;
    }
    tx.commit().await?;
    wamn_control::connection_configuration::import(control.url(), ORG, &json!({"orgs": {}, "projects": {PROJECT: {"upstream": {"connection": "upstream", "secret": json!({"headers":{"authorization":"Bearer fixture-token"}}).to_string()}}}}).to_string()).await?;

    let scheduler_address = address()?;
    let scheduler = format!("nats://{scheduler_address}");
    let mut nats = Command::new(std::env::var_os("WAMN_NATIVE_C_NATS_BIN").context("NATS binary")?)
        .args([
            "-js",
            "-a",
            "127.0.0.1",
            "-p",
            scheduler_address
                .split(':')
                .next_back()
                .context("NATS port")?,
            "-sd",
        ])
        .arg(directory.join("nats"))
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(directory.join("nats.log"))?)
        .spawn()?;
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            ensure!(
                nats.try_wait()?.is_none(),
                "NATS exited: {}",
                std::fs::read_to_string(directory.join("nats.log"))?
            );
            if let Ok(client) = async_nats::connect(&scheduler).await {
                break Ok::<_, anyhow::Error>(client);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("local NATS did not accept a connection")??;
    let broker = async_nats::jetstream::new(client.clone());
    for stream in [
        wamn_control_provision::events::source_stream_config(&scope, 1, Duration::from_secs(120)),
        wamn_control_provision::events::advisory_stream_config(&scope, 1),
    ] {
        broker.create_stream(stream).await?;
    }

    let peer_task = tokio::spawn(async move {
        let mut records = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = peer.accept().await?;
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await?);
            }
            let head = String::from_utf8(head)?;
            let length: usize = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .context("request length")?
                .1
                .trim()
                .parse()?;
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await?;
            records.push(json!({"head":head,"body":body}));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 8\r\nConnection: close\r\n\r\n{\"id\":1}").await?;
        }
        Ok::<_, anyhow::Error>(records)
    });

    let (device, entry) = fixture_http_entry(directory, &repository)?;
    let version = &device.facts().scope.package_version;
    let device_descriptor = ComponentDescriptor::new(device.facts().clone(), Vec::new());
    let h = release(MANIFEST_H, version, Some(&device_descriptor))?;
    let mut facts = vec![device.facts().clone()];
    wamn_schema_generator::receipt::check_current(&repository.join("apps").join(PACKAGE))?;
    let mut descriptors = Vec::new();
    std::fs::write(
        local_component_path(directory, device.component_digest())?,
        std::fs::read(
            wamn_schema_generator::output_root(&repository.join("apps").join(PACKAGE))
                .join("components/device.wasm"),
        )?,
    )?;
    let target = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").context("CARGO_TARGET_DIR")?);
    for name in ["transform", "http-request", "http-page"] {
        let bytes = std::fs::read(
            target
                .join("wasm32-wasip2/debug")
                .join(format!("{}.wasm", name.replace('-', "_"))),
        )?;
        let admitted = http_connection::admit_guest(&repository, version, PACKAGE, name, &bytes)?;
        std::fs::write(
            local_component_path(directory, &admitted.component.component_digest)?,
            bytes,
        )?;
        descriptors.push(ComponentDescriptor::new(
            admitted.component.clone(),
            admitted.connections,
        ));
        facts.push(admitted.component);
    }
    let bytes = http_connection::release(&h, version, descriptors)?;
    let (manifest, digest) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
    let mut published = PublishedRelease {
        manifest,
        digest,
        canonical_bytes: bytes,
    };
    let entry_path = local_component_path(directory, entry.digest())?;
    std::fs::write(&entry_path, entry.bytes())?;
    record_composed_entry(
        &mut published,
        &entry,
        &format!("file:{}", entry_path.display()),
    )?;
    let digest = published.digest.as_str();
    std::fs::write(
        directory.join(wamn_catalog::RELEASE_MANIFEST_FILE_NAME),
        &published.canonical_bytes,
    )?;
    project.execute("INSERT INTO catalog.releases(tenant_id,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)", &[&tenant,&digest,&published.canonical_bytes]).await?;
    for package in &published.manifest.release.packages {
        project.execute("INSERT INTO catalog.packages(tenant_id,package_id,package_version,artifact_digest) VALUES ($1,$2,$3,$4)", &[&tenant,&package.package_id(),&package.package_version(),&format!("sha256:{}","a".repeat(64))]).await?;
    }
    let mut document = fixture_document("http", "http.orders.example", digest);
    document
        .applications
        .get_mut(PACKAGE)
        .context("application")?
        .packages = [PACKAGE.into(), "http_fixture".into()].into();
    document
        .applications
        .get_mut(PACKAGE)
        .context("application")?
        .connections = [("upstream".into(), "upstream".into())].into();
    // Match the write mode declared by org_bindings::publish_declarations.
    let package =
        wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(&json!({
            "package":{"id":"http_fixture","version":"1.0.0"},
            "required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
            "models":{},"components":{},
            "connections":{"upstream":{"integration":"http","mode":"write"}}
        }))?)?;
    // The system floor seeds the active cloud target used by this document.
    let tx = wamn_control::provision_project_env::provisioning_transaction(&mut seed).await?;
    let reserved = wamn_control::environment::bindings::reserve(&tx, &document, &[package]).await?;
    tx.commit().await?;
    seed_task.abort();
    let upstream = &reserved
        .first()
        .context("reserved upstream connection")?
        .resolved;
    let input = wamn_control::bind_connection::prepare_org_instance(upstream)?;
    let mut bindings = Vec::new();
    for component in &published.manifest.components {
        for connection in &component.descriptor.connections {
            let requirement = ComponentConnectionRequirement::new(
                &component.descriptor.component_digest,
                &connection.store_alias,
                ConnectionTypeDescriptor::http_v1(),
            );
            let id = format!("{}/{}", component.package_id, connection.store_alias);
            wamn_control::bind_connection::prepare_local_instance(
                project.as_ref(),
                &tenant,
                "http",
                &id,
                &input,
            )
            .await?;
            let selection = wamn_runtime::local_application::read_local_binding(
                &project,
                &tenant,
                "http",
                &requirement,
                &id,
            )
            .await?;
            bindings.push(LocalBindingFacts {
                package_id: component.package_id.clone(),
                requirement,
                selection,
            });
        }
    }
    std::fs::write(
        directory.join(LOCAL_FACTS_FILE),
        serde_json::to_vec(&LocalApplicationFacts {
            manifest_digest: published.digest.clone(),
            components: facts,
            requirements: bindings
                .iter()
                .map(|binding| binding.requirement.clone())
                .collect(),
            bindings,
        })?,
    )?;
    let (mut group, event_secret) = event_overlay(
        &document,
        fixture_host_group("registry.invalid"),
        &scheduler,
    )?;
    group["env"]
        .as_array_mut()
        .context("environment")?
        .iter_mut()
        .find(|variable| variable["name"] == "WAMN_ALLOWED_HOSTS")
        .context("allowed hosts")?["value"] = json!(origin);
    for key in ["password", "binding.json"] {
        std::fs::write(
            directory.join(key),
            event_secret["stringData"][key]
                .as_str()
                .context("event Secret")?,
        )?;
    }
    let group_path = directory.join("http-event-host-group.yaml");
    std::fs::write(&group_path, serde_yaml::to_string(&group)?)?;
    let rendered = wamn_gate_harness::render_check::environment(
        &document,
        &published.manifest,
        &group_path,
        70,
    )?;
    let documents = wamn_gate_harness::render_check::template(&rendered, NAMESPACE)?;
    let mut secrets =
        credential_secrets(&project, &mut system, database.url(), control.url(), &scope).await?;
    secrets.extend(wamn_gate_harness::render_check::operator_prerequisites(
        NAMESPACE,
    ));
    secrets.push(wamn_gate_harness::render_check::prerequisite(
        "apply-registry",
        &["config.json", "ca.crt"],
        NAMESPACE,
    ));
    secrets.push(event_secret);
    wamn_gate_harness::render_check::assert_complete(&documents, &secrets)?;
    let materializer = PathBuf::from(
        std::env::var_os("WAMN_MATERIALIZER_COMPONENT").context("materializer bytes")?,
    );
    for document in documents
        .iter()
        .filter(|document| document["kind"] == "WorkloadDeployment")
    {
        workload(document, directory, &materializer)?;
    }
    let auth = wamn_platform_identity::auth_changes::AuthChanges::default();
    let environment = integration::WorkflowEnvironment {
        system_url: control.url(),
        project_url: database.url(),
        scope: &scope,
        tenant: &tenant,
        digest,
        applications: &document.applications,
        auth: &auth,
    };
    let good = http_connection::wiring()?;
    let mut old = serde_json::to_value(&good)?;
    old["wiring-id"] = json!("http-old-respond");
    old["nodes"]["send"]["terminal"] = json!("respond");
    let old = serde_json::from_value(old)?;
    for wiring in [&good, &old] {
        integration::enable_trigger(
            &environment,
            PACKAGE,
            wiring,
            &wiring.wiring_id,
            &[],
            wamn_control::author_workflow::WorkflowTrigger::Manual,
            None,
        )
        .await?;
    }
    let http = address()?;
    let probe = address()?;
    let mut heartbeats = client
        .subscribe(format!(
            "{}.heartbeat.*",
            wash_runtime::washlet::OPERATOR_API_PREFIX
        ))
        .await?;
    client.flush().await?;
    let mut host =
        host_command(&documents, &secrets, directory, &scheduler, &http, &probe)?.spawn()?;
    let result = async {
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                ensure!(
                    host.try_wait()?.is_none(),
                    "host exited: {} {}",
                    std::fs::read_to_string(directory.join("host.log"))?,
                    std::fs::read_to_string(directory.join("host.stderr"))?
                );
                if reqwest::get(format!("http://{probe}/readyz"))
                    .await
                    .is_ok_and(|response| response.status() == 200)
                {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("host readiness timed out")??;
        let response = reqwest::Client::new()
            .get(format!("http://{http}/no-such-route"))
            .header("Host", &document.route_host)
            .send()
            .await?;
        ensure!(
            response.status() == 404 && response.text().await? == ROUTE_NOT_FOUND,
            "HTTP guest did not answer"
        );
        let heartbeat = tokio::time::timeout(Duration::from_secs(30), heartbeats.next())
            .await?
            .context("host heartbeat")?;
        let heartbeat: v2::HostHeartbeat = serde_json::from_slice(&heartbeat.payload)?;
        ensure!(
            heartbeat.environment == NAMESPACE && heartbeat.hostname == "prekind",
            "host identity differs"
        );
        for workload in ["prekind-flow-http", "prekind-materializer"] {
            let response: v2::WorkloadStatusResponse = serde_json::from_slice(
                &client
                    .request(
                        rpc_subject(&heartbeat.id, "workload.status"),
                        serde_json::to_vec(&v2::WorkloadStatusRequest {
                            workload_id: workload.into(),
                        })?
                        .into(),
                    )
                    .await?
                    .payload,
            )?;
            ensure!(
                response.workload_status.is_some_and(
                    |status| status.workload_state == v2::WorkloadState::Running as i32
                ),
                "{workload} is not RUNNING"
            );
        }
        let mut successful_response = String::new();
        for (wiring, expected) in [(&good, "completed"), (&old, "failed")] {
            let run = integration::admit_workflow(
                directory,
                &environment,
                PACKAGE,
                &wiring.wiring_id,
                &json!({"id":1}),
                &wiring.wiring_id,
            )
            .await?;
            let (status, body) = outcome(&project, &run).await?;
            ensure!(status == expected, "{}: {status}: {body}", wiring.wiring_id);
            if expected == "completed" {
                ensure!(
                    body["status"] == 200 && body["body"] == json!({"id":1}),
                    "HTTP result: {body}"
                );
                successful_response = body.to_string();
            } else {
                ensure!(
                    body["error"]["code"] == "respond-without-caller",
                    "old failure differed: {body}"
                );
            }
            println!("{}: {status}: {body}", wiring.wiring_id);
        }
        let records = tokio::time::timeout(Duration::from_secs(10), peer_task).await???;
        for record in records {
            http_connection::assert_observed_request(
                200,
                &successful_response,
                &record.to_string(),
            )?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if host.try_wait()?.is_none() {
        host.kill().await?;
    }
    if nats.try_wait()?.is_none() {
        nats.kill().await?;
    }
    project_task.abort();
    system_task.abort();
    if let Err(error) = &result {
        eprintln!(
            "{error:#}\nhost: {}\nstderr: {}",
            std::fs::read_to_string(directory.join("host.log"))?,
            std::fs::read_to_string(directory.join("host.stderr"))?
        );
    }
    println!(
        "prekind HTTP release {} elapsed {:?}",
        digest,
        started.elapsed()
    );
    result
}
