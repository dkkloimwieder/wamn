//! Run the environment type HTTP release through its render on a native host.

use sqlx::{Connection as _, Row as _};
#[expect(dead_code, reason = "chart tests share these render helpers")]
#[path = "render_fixture.rs"]
pub mod render_fixture;
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
    project: &mut sqlx::PgConnection,
    system: &mut sqlx::PgConnection,
    project_url: &str,
    system_url: &str,
    coordinate: &Triple,
) -> anyhow::Result<Vec<Value>> {
    let project_env_key = wamn_registry::project_env_key(ORG, PROJECT, "http");
    let mut secrets = render_fixture::credential_secrets(coordinate, NAMESPACE);
    for credential in wamn_control_provision::host_credentials::HOST_CREDENTIALS {
        let (client, url) = match credential.database {
            CredentialDatabase::Project => (&mut *project, project_url),
            CredentialDatabase::Control => (&mut *system, system_url),
        };
        let mut database = url
            .parse::<sqlx::postgres::PgConnectOptions>()?
            .get_database()
            .context("database")?
            .to_owned();
        let role = match credential.database {
            CredentialDatabase::Control => wamn_control_provision::system_reader_login(
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
            ),
            CredentialDatabase::Project => workload_login(
                credential.family,
                if credential.family == WorkloadRoleFamily::App {
                    WorkloadRoleIdentity::ProjectEnvKey {
                        project_env_key: &project_env_key,
                        database: &database,
                    }
                } else {
                    WorkloadRoleIdentity::ProjectEnvironment {
                        org: ORG,
                        project: PROJECT,
                        environment: "http",
                        database: &database,
                    }
                },
            )?,
        };
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(
                credential.family,
                &database,
                &role,
                PASSWORD,
            ))
                .to_string(),
        ))
        .execute(&mut *client)
        .await
        .map(drop)?;
        if credential.database == CredentialDatabase::Control {
            let mut tx =
                wamn_environment::provision_project_env::provisioning_transaction(system).await?;
            sqlx::raw_sql("SET LOCAL ROLE wamn_system")
                .execute(&mut *tx)
                .await
                .map(drop)?;
            let mapped: String = sqlx::query(sqlx::AssertSqlSafe(
                (sql::upsert_control_author_project_env_key_mapping_sql()).to_string(),
            ))
            .bind(&role)
            .bind(&project_env_key)
            .bind(&ORG)
            .bind(&PROJECT)
            .bind(&"http")
            .fetch_one(&mut *tx)
            .await?
            .get("project_env_key");
            ensure!(
                mapped == project_env_key,
                "host reader maps to a different project"
            );
            tx.commit().await?;
        }
        let config = secrets
            .iter_mut()
            .find(|item| item["kind"] == "ConfigMap")
            .context("connection configuration")?;
        config["data"][format!("{}.json", credential.family.component_stem())] =
            json!(login(url, &role)?);
    }
    let config = secrets
        .iter_mut()
        .find(|item| item["kind"] == "ConfigMap")
        .unwrap();
    config["data"]["app.json"] =
        json!({"compiled":config["data"]["app.json"],"dynamic":config["data"]["query.json"]})
            .to_string()
            .into();
    Ok(secrets)
}

fn workload(document: &Value, directory: &Path) -> anyhow::Result<String> {
    let spec = &document["spec"]["template"]["spec"];
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
    let component = &spec["components"][0];
    let components = vec![v2::Component {
        name: component["name"].as_str().context("component name")?.into(),
        image: component["image"]
            .as_str()
            .context("entry reference")?
            .into(),
        image_pull_policy: v2::ImagePullPolicy::Never.into(),
        max_concurrency: serde_json::from_value(component["maxConcurrency"].clone())?,
        local_resources: Some(resources(&component["localResources"])?),
        ..Default::default()
    }];
    let name = "flow-http";
    let request = v2::WorkloadStartRequest {
        workload_id: format!("prekind-{name}"),
        workload: Some(v2::Workload {
            namespace: NAMESPACE.into(),
            name: name.into(),
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
    Ok(request.workload_id)
}

fn host_command(
    documents: &[Value],
    secrets: &[Value],
    directory: &Path,
    scheduler: &str,
    http: &str,
    probe: &str,
    files: &str,
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
                    | "WAMN_REGISTRY_TOKEN_METADATA"
                    | "WAMN_COMPONENT_ARTIFACT_BASE"
            ) || name.starts_with("OTEL_")
            {
                continue;
            }
            // The pod mounts the wamn-oauth Secret at this path. The local host
            // reads the same key from a private file in its directory.
            if name == "WAMN_OAUTH_KEY_FILE" {
                let key = secrets
                    .iter()
                    .find(|secret| secret["metadata"]["name"] == "wamn-oauth")
                    .context("wamn-oauth Secret")?["stringData"]["key"]
                    .as_str()
                    .context("OAuth key")?;
                let path = directory.join("oauth-key");
                std::fs::write(&path, key)?;
                std::fs::set_permissions(
                    &path,
                    std::os::unix::fs::PermissionsExt::from_mode(0o600),
                )?;
                command.env(name, &path);
                continue;
            }
            if let Some(key) = value.strip_prefix("/etc/wamn/postgres/") {
                let config = secrets
                    .iter()
                    .find(|item| item["kind"] == "ConfigMap")
                    .context("component configuration")?;
                let path = directory.join(key);
                std::fs::write(
                    &path,
                    config["data"][key].as_str().context("component input")?,
                )?;
                command.env(name, &path);
                continue;
            }
            value.to_owned()
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
                "--oci-ca-path"
                    | "--release-artifact-base"
                    | "--release-manifest-digest"
                    | "--registry-token-metadata"
            )
        {
            continue;
        }
        command.arg(match flag {
            "--scheduler-nats-url" | "--data-nats-url" => format!("{flag}={scheduler}"),
            "--http-addr" => format!("{flag}={http}"),
            "--probe-addr" => format!("{flag}={probe}"),
            "--files-addr" => format!("{flag}={files}"),
            "--oci-cache-dir" => format!("{flag}={}", directory.join("oci").display()),
            "--host-name" => "--host-name=prekind".into(),
            // The chart's host configuration file with the plugins entry (R6).
            "--user-config" => {
                let file = documents
                    .iter()
                    .find(|document| {
                        document["kind"] == "ConfigMap"
                            && document["metadata"]["name"]
                                .as_str()
                                .is_some_and(|name| name.ends_with("-host-config"))
                    })
                    .context("rendered host configuration file")?;
                let path = directory.join("host-config.yaml");
                std::fs::write(
                    &path,
                    file["data"]["config.yaml"]
                        .as_str()
                        .context("host configuration config.yaml")?,
                )?;
                format!("{flag}={}", path.display())
            }
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

async fn outcome(project: &mut sqlx::PgConnection, run: &str) -> anyhow::Result<(String, Value)> {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let row = sqlx::query("SELECT status, result_json FROM wamn_run.runs WHERE run_id=$1")
                .bind(&run)
                .fetch_one(&mut *project)
                .await?;
            let status: String = row.get(0);
            if matches!(
                status.as_str(),
                "completed" | "failed" | "infrastructure-failure"
            ) {
                return Ok((
                    status,
                    row.get::<Option<Value>, _>(1).unwrap_or(Value::Null),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("manual workflow timed out")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: env:WAMN_HOST_BINARY, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_EDGE_DEVICE_COMPONENT, bin:helm"]
async fn prekind_http_host_runs_and_rejects_respond_without_caller() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_HOST_BINARY, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_EDGE_DEVICE_COMPONENT, bin:helm",
    );
    let started = Instant::now();
    let _lock = wamn_test_postgres::lock();
    let work = ScratchRoot::create()?;
    let directory = work.path();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let project_env_key = wamn_registry::project_env_key(ORG, PROJECT, "http");
    let coordinate = Triple::new(ORG, "http", PROJECT);
    let mut database = wamn_test_postgres::database();
    let control = wamn_control_provision::test_database::system();
    let mut project = connect(database.url()).await?;
    let mut system = wamn_postgres::connect(control.url()).await?;

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::set_database_owner_sql(database.name())).to_string(),
    ))
    .execute(&mut project)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::ensure_app_acl_role_sql()).to_string(),
    ))
    .execute(&mut project)
    .await
    .map(drop)?;
    wamn_dev_control::dev::environment::install_journey_platform_base_grants(
        &mut project,
        PLATFORM_DOMAIN,
    )
    .await?;
    wamn_environment::platform_migrations::install_project(&mut project).await?;
    sqlx::query("SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:prekind',false)").bind(&wamn_control_provision::PlatformComponent::Provisioning.principal_id().to_string()).execute(&mut project).await.map(|result| result.rows_affected())?;
    sqlx::query("SELECT set_config('app.project_env_key',$1,false)")
        .bind(&project_env_key)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())?;
    sqlx::raw_sql("INSERT INTO platform.environment(status) VALUES ('active')")
        .execute(&mut project)
        .await
        .map(drop)?;

    let instance: i64 =
        sqlx::query("SELECT oid::bigint FROM pg_database WHERE datname=current_database()")
            .fetch_one(&mut project)
            .await?
            .get(0);
    let marker = wamn_runtime::local_application::local_target_marker(
        &project_env_key,
        "http",
        instance.try_into().unwrap(),
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "COMMENT ON DATABASE \"{}\" IS '{marker}'",
        database.name()
    )))
    .execute(&mut project)
    .await
    .map(drop)?;
    let peer = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", peer.local_addr()?);
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .map(drop)?;
    sqlx::query("SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:prekind',false)").bind(&wamn_control_provision::PlatformComponent::Provisioning.principal_id().to_string()).execute(&mut system).await.map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO registry.orgs(id,placement_type,pool_cluster) VALUES ($1,'pooled','prekind')",
    )
    .bind(&ORG)
    .execute(&mut system)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::raw_sql("UPDATE registry.meta SET platform_domain='example.invalid'")
        .execute(&mut system)
        .await
        .map(drop)?;
    sqlx::query("INSERT INTO registry.projects(org,id) VALUES ($1,$2)")
        .bind(&ORG)
        .bind(&PROJECT)
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())?;
    let policy = wamn_control_provision::EnvPolicy::dev();
    sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::stamp_env_policy_sql()).to_string(),
    ))
    .bind(&ORG)
    .bind(&POLICY)
    .bind(&serde_json::to_string(&policy.recovery_domain)?)
    .bind(&policy.promotion_rank)
    .bind(&policy.instances)
    .bind(&policy.storage)
    .bind(&policy.cpu)
    .bind(&policy.memory)
    .bind(&policy.image)
    .bind(&policy.backup_cadence)
    .bind(&policy.wal_retention)
    .bind(&policy.hibernation)
    .execute(&mut system)
    .await
    .map(|result| result.rows_affected())?;
    let target: String = sqlx::query("SELECT id::text FROM registry.target WHERE name='cloud'")
        .fetch_one(&mut system)
        .await?
        .get(0);
    sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::upsert_project_env_with_policy_sql()).to_string(),
    ))
    .bind(&ORG)
    .bind(&PROJECT)
    .bind(&"http")
    .bind(&"prekind-database")
    .bind(&Some(NAMESPACE))
    .bind(&"aaaaaaa1")
    .bind(&Some(true))
    .bind(&POLICY)
    .bind(&target)
    .fetch_one(&mut system)
    .await?;
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut system)
        .await
        .map(drop)?;
    // C3.5: a credential imports only into a declared, project-enabled connection.
    let mut seed = wamn_postgres::connect(control.url()).await?;

    sqlx::raw_sql("SET ROLE wamn_control")
        .execute(&mut seed)
        .await
        .map(drop)?;
    let mut tx = seed.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)?;
    let credential_store = wamn_test_infrastructure::credentials::MemoryStore::default();
    wamn_test_infrastructure::credentials::define(&mut tx,ORG,&json!({"project":PROJECT,"env":"http","name":"upstream","integration_type":"http","auth_type":"http_basic","parameters":{"primary-authority":origin,"tls-verification":"disabled"},"credential":json!({"username":"fixture","password":"fixture-password"}).to_string()}),&credential_store).await?;
    tx.commit().await?;

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
    let mut client = tokio::time::timeout(Duration::from_secs(10), async {
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
        wamn_broker::events::source_stream_config(&coordinate, 1, Duration::from_secs(120)),
        wamn_broker::events::advisory_stream_config(&coordinate, 1),
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
    let version = &device.facts().target.package_version;
    let device_descriptor = ComponentDescriptor::new(device.facts().clone(), Vec::new());
    let h = release(MANIFEST_H, version, Some(&device_descriptor))?;
    let mut facts = vec![device.facts().clone()];
    let mut descriptors = Vec::new();
    std::fs::write(
        local_component_path(directory, device.component_digest())?,
        std::fs::read(
            std::env::var_os("WAMN_EDGE_DEVICE_COMPONENT").context("WAMN_EDGE_DEVICE_COMPONENT")?,
        )?,
    )?;
    let target = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").context("CARGO_TARGET_DIR")?);
    for name in ["transform", "http-request", "http-page"] {
        let bytes = std::fs::read(
            target
                .join("wasm32-wasip2/debug")
                .join(format!("{}.wasm", name.replace('-', "_"))),
        )?;
        let admitted =
            http_connection::admit_guest(&repository, "1.0.0", "http_fixture", name, &bytes)?;
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
    sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)").bind(&project_env_key).bind(&digest).bind(&published.canonical_bytes).execute(&mut project).await.map(|result| result.rows_affected())?;
    for package in &published.manifest.release.packages {
        sqlx::query("INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( $1,$2,$3,$4)").bind(&project_env_key).bind(&package.package_id()).bind(&package.package_version()).bind(&format!("sha256:{}","a".repeat(64))).execute(&mut project).await.map(|result| result.rows_affected())?;
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
        .connections = [
        "upstream",
        http_connection::OUTBOUND_HANDLE,
        http_connection::API_KEY_HANDLE,
        http_connection::AWS_HANDLE,
    ]
    .map(|name| (name.into(), "upstream".into()))
    .into();
    document.integrations.clear();
    document.integrations.insert(
        "upstream".into(),
        wamn_environment::environment::document::Integration { version: 1 },
    );
    // Match the write mode declared by org_bindings::publish_declarations.
    let package = wamn_compiler::manifest::PackageManifest::from_slice(&serde_json::to_vec(
        &json!({
            "package":{"id":"http_fixture","version":"1.0.0"},
            "required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
            "models":{},"components":{},
            "connections":{"upstream":{"interface":"wamn:connection/http@0.1.0","mode":"write"},"signed-upstream":{"interface":"wamn:connection/http@0.1.0","mode":"write"},"api-key-upstream":{"interface":"wamn:connection/http@0.1.0","mode":"write"},"aws-upstream":{"interface":"wamn:connection/http@0.1.0","mode":"write"}}
        }),
    )?)?;
    // The system base grants seeds the active cloud target used by this document.
    let mut tx =
        wamn_environment::provision_project_env::provisioning_transaction(&mut seed).await?;
    sqlx::raw_sql("SET LOCAL ROLE wamn_db_owner")
        .execute(&mut *tx)
        .await
        .map(drop)?;
    let reserved =
        wamn_environment::environment::bindings::analyse(&mut tx, &document, &[package]).await?;
    tx.commit().await?;
    let _ = wamn_environment::environment::bindings::project_authority(
        control.url(),
        database.url(),
        &document,
    )
    .await?;
    let upstream = reserved.first().context("resolved upstream connection")?;
    let definition = upstream.resolved.as_ref().context("host HTTP fixture")?;
    let mut bindings = Vec::new();
    for component in &published.manifest.components {
        for connection in &component.descriptor.connections {
            let requirement = ComponentConnectionRequirement::new(
                &component.descriptor.component_digest,
                connection
                    .store_alias
                    .name()
                    .expect("fixture connection has a literal alias"),
                ConnectionTypeDescriptor::http_v1(),
            );
            sqlx::query("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,$3,$4,'wamn:connection/http@0.1.0','write') ON CONFLICT DO NOTHING").bind(&project_env_key).bind(&component.descriptor.component_digest).bind(&component.package_id).bind(connection.store_alias.name().expect("fixture connection has a literal alias")).execute(&mut project).await.map(|result| result.rows_affected())?;
            let selection = wamn_runtime::local_application::selection(
                &requirement,
                &upstream.connection_id,
                upstream.generation,
                &definition.definition,
                Some(format!("org:{ORG}:upstream@{}", upstream.generation)),
                false,
            );
            bindings.push(LocalBindingFacts {
                package_id: component.package_id.clone(),
                requirement,
                selection,
            });
        }
    }
    let mut target = wamn_postgres::connect(database.url()).await?;

    let mut tx = target.begin().await?;
    wamn_environment::environment::bindings::write(
        &mut tx,
        &project_env_key,
        digest,
        &document,
        &reserved,
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
    )
    .await?;
    tx.commit().await?;
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
    let mut group = fixture_host_group(&document, "registry.invalid");
    group["env"]
        .as_array_mut()
        .context("environment")?
        .iter_mut()
        .find(|variable| variable["name"] == "WAMN_ALLOWED_HOSTS")
        .context("allowed hosts")?["value"] = json!(origin);
    let group_path = directory.join("http-event-host-group.yaml");
    std::fs::write(&group_path, serde_yaml::to_string(&group)?)?;
    let rendered = render_fixture::environment(&document, &published.manifest, &group_path, 70)?;
    let documents = render_fixture::template(&rendered, NAMESPACE)?;
    let mut secrets = credential_secrets(
        &mut project,
        &mut system,
        database.url(),
        control.url(),
        &coordinate,
    )
    .await?;
    secrets.extend(render_fixture::operator_prerequisites(NAMESPACE));
    secrets.push(render_fixture::prerequisite(
        "apply-registry",
        &["config.json", "ca.crt"],
        NAMESPACE,
    ));
    let mut oauth = render_fixture::prerequisite("wamn-oauth", &["key"], NAMESPACE);
    oauth["stringData"]["key"] = "o".repeat(32).into();
    secrets.push(oauth);
    // cert-manager issues the environment's NATS certificate (R5).
    secrets.push(render_fixture::prerequisite(
        &format!(
            "wamn-nats-{}",
            wamn_registry::project_env_key(
                &coordinate.org,
                &coordinate.project,
                coordinate.env.as_str()
            )
        ),
        &["ca.crt", "tls.crt", "tls.key"],
        NAMESPACE,
    ));
    render_fixture::assert_complete(&documents, &secrets)?;
    let workloads = documents
        .iter()
        .filter(|document| document["kind"] == "WorkloadDeployment")
        .map(|document| workload(document, directory))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let auth = wamn_broker::Publisher::default();
    let environment = integration::WorkflowEnvironment {
        system_url: control.url(),
        project_url: database.url(),
        target: &coordinate,
        project_env_key: &project_env_key,
        digest,
        applications: &document.membership(),
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
            wamn_environment::author_workflow::WorkflowTrigger::Manual,
            None,
        )
        .await?;
    }
    let http = address()?;
    let probe = address()?;
    let files = address()?;
    let mut heartbeats = client
        .subscribe(format!(
            "{}.heartbeat.*",
            wash_runtime::washlet::OPERATOR_API_PREFIX
        ))
        .await?;
    client.flush().await?;
    let mut host = host_command(
        &documents, &secrets, directory, &scheduler, &http, &probe, &files,
    )?
    .spawn()?;
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
            .post(format!(
                "http://{files}/api/{PACKAGE}/files?binding=files&name=prekind.txt"
            ))
            .header("Host", &document.applications[PACKAGE].hostname)
            .header("Content-Type", "text/plain")
            .body("prekind")
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        // This release serves no application host routes, so the native file
        // handler refuses before authentication. The guest answers route-not-found.
        ensure!(
            status == 429
                && serde_json::from_str::<Value>(&body)?["error"] == "route-capacity-exhausted",
            "the file listener must reach the native file handler: {status} {body}"
        );
        let response = reqwest::Client::new()
            .get(format!("http://{http}/no-such-route"))
            .header("Host", &document.applications[PACKAGE].hostname)
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
        for workload in workloads {
            let request = v2::WorkloadStatusRequest {
                workload_id: workload,
            };
            let response: v2::WorkloadStatusResponse = serde_json::from_slice(
                &mut client
                    .request(
                        rpc_subject(&heartbeat.id, "workload.status"),
                        serde_json::to_vec(&request)?.into(),
                    )
                    .await?
                    .payload,
            )?;
            ensure!(
                response.workload_status.is_some_and(
                    |status| status.workload_state == v2::WorkloadState::Running as i32
                ),
                "{} is not RUNNING",
                request.workload_id
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
            let (status, body) = outcome(&mut project, &run).await?;
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
