//! Real registry pulls and Modbus TCP reads follow the connection facts per call.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql, sql};
use wamn_engine::integration::{Integration, IntegrationBinding, IntegrationImports};
use wamn_environment_control::environment::{bindings, document::EnvironmentDocument};
use wamn_package_artifact::{
    PackageRegistry,
    registry_credentials::{RegistryCredentialSource, read_registry_credentials},
};
use wamn_package_control::integration;
use wamn_runtime::{
    connection_credentials::ConnectionCredentials, connection_facts::ConnectionFacts,
    connection_memory::ConnectionMemory, integrations::Integrations,
};
use wash_runtime::{
    engine::{ctx::SharedCtx, workload::ResolvedWorkload},
    host::{
        HostRef,
        http::{HostHandler, NullServer},
    },
    observability::{MeterKind, Meters},
    plugin::{HostPlugin, PluginBindings},
    types::{Component, Workload},
    wasmtime::{
        Store,
        component::{TypedFunc, Val},
    },
    wit::WitInterface,
};

#[path = "support/local_registry.rs"]
mod local_registry;

#[path = "support/exclusive_resources.rs"]
mod exclusive_resources;

mod node {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../../wit/wamn-node", world: "node", wasmtime_crate: wash_runtime::wasmtime,
    });
}
use node::wamn::node::types::{Emission, NodeContext, NodeError};
type Run = TypedFunc<(NodeContext, String), (Result<Emission, NodeError>,)>;

async fn read_register(instance: &Integration, interface: &str, unit: u8) -> anyhow::Result<u16> {
    let result = instance
        .call(
            interface,
            "read-registers",
            &[Val::U8(unit), Val::U16(7), Val::U16(1)],
        )
        .await?;
    match result.as_slice() {
        [Val::Result(Ok(Some(value)))] => match value.as_ref() {
            Val::List(values) => match values.as_slice() {
                [Val::U16(register)] => Ok(*register),
                _ => anyhow::bail!("unexpected register response"),
            },
            _ => anyhow::bail!("unexpected read response"),
        },
        _ => anyhow::bail!("read failed: {result:?}"),
    }
}

async fn connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, driver) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    tokio::spawn(driver);
    Ok(client)
}

fn fixture(root: &Path) -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for file in [
        "rust-toolchain.toml",
        "tools/guest-rustflags",
        "apps/platform/no-std/guest_runtime.rs",
        "apps/platform/no-std/modbus/Cargo.toml",
        "apps/platform/no-std/modbus/src/lib.rs",
        "apps/platform/no-std/modbus/wit/world.wit",
        "apps/platform/no-std/modbus/wit/deps/io.wit",
        "apps/platform/no-std/modbus/wit/deps/sockets.wit",
        "wit/wamn-integration/package.wit",
    ] {
        let target = root.join(file);
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::copy(repository.join(file), target)?;
    }
    // Keep the fixture independent while inheriting the real workspace identities.
    let workspace = std::fs::read_to_string(repository.join("apps/platform/no-std/Cargo.toml"))?;
    let workspace = workspace
        .lines()
        .map(|line| {
            if line.starts_with("members = ") {
                "members = [\"modbus\"]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.join("apps/platform/no-std/Cargo.toml"), workspace)?;
    std::fs::copy(
        repository.join("apps/platform/no-std/Cargo.lock"),
        root.join("apps/platform/no-std/Cargo.lock"),
    )?;
    Ok(())
}

async fn simulator() -> anyhow::Result<(String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
    simulator_with_gate(None).await
}

async fn simulator_with_gate(
    gate: Option<Arc<tokio::sync::Semaphore>>,
) -> anyhow::Result<(String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
    // Select a private interface without sending a datagram.
    let route = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    route.connect("10.255.255.254:9").await?;
    let ip = route.local_addr()?.ip();
    let listener = tokio::net::TcpListener::bind((ip, 0)).await?;
    let authority = listener.local_addr()?.to_string();
    let count = Arc::new(AtomicUsize::new(0));
    let accepts = Arc::clone(&count);
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let number = accepts.fetch_add(1, Ordering::SeqCst);
            let gate = gate.clone();
            tokio::spawn(async move {
                if number == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let mut frame = [0; 12];
                stream.read_exact(&mut frame).await.unwrap();
                assert_eq!(&frame[2..6], &[0, 0, 0, 6]);
                if let Some(gate) = gate {
                    gate.acquire().await.unwrap().forget();
                }
                if frame[7] == 6 {
                    assert_eq!(&frame[6..], &[17, 6, 0, 7, 0xbe, 0xef]);
                    stream.write_all(&frame).await.unwrap();
                    return;
                }
                assert_eq!(&frame[7..], &[3, 0, 7, 0, 1]);
                let [hi, lo] = if frame[6] == 1 {
                    [0x12, 0x34]
                } else {
                    [0, frame[6]]
                };
                let mut response = frame[..2].to_vec();
                response.extend([0, 0, 0, 5, frame[6], 3, 2, hi, lo]);
                stream.write_all(&response).await.unwrap();
            });
        }
    });
    Ok((authority, count, server))
}

struct Application {
    store: Store<SharedCtx>,
    run: Run,
    _workload: ResolvedWorkload,
    _egress: Arc<dyn HostHandler>,
}

impl Application {
    async fn load(reader: Arc<IntegrationImports>) -> anyhow::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let package = repository.join("apps/modbus_reader");
        wamn_schema_generator::build_record::check_current(&package)?;
        let bytes = std::fs::read(
            repository.join("apps/target/wamn/modbus_reader/components/modbus_reader.wasm"),
        )?;
        let engine = wamn_engine::build_engine(&[])?;
        let mut declaration =
            wamn_package_control::component_declaration::render_declaration_document(
                &package.join("publication/components/modbus_reader.json.in"),
                "acme--one--dev",
            )?;
        declaration["scope"]["package-version"] = json!("0.1.0");
        let operation = declaration["operations"]
            .as_object()
            .context("reader operations missing")?
            .keys()
            .next()
            .context("reader operation missing")?
            .clone();
        let request = wamn_engine::component_admission::ComponentAdmissionRequest {
            declaration: serde_json::from_value(declaration)?,
            admitted_platform_packages: ["wamn:modbus".into()].into(),
            base_digests: std::collections::BTreeSet::new(),
        };
        wamn_engine::component_admission::validate_component_admission(&engine, &bytes, request)?;
        let plugin: Arc<dyn HostPlugin> = reader;
        let plugins = HashMap::from([(plugin.id(), plugin)]);
        let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
        let workload = engine
            .initialize_workload(
                "modbus-reader",
                Workload {
                    namespace: "c9".into(),
                    name: "modbus-reader".into(),
                    components: vec![Component {
                        name: "reader".into(),
                        bytes: bytes.into(),
                        ..Component::default()
                    }],
                    host_interfaces: vec![WitInterface::from("wamn:modbus/read@1.0.0")],
                    annotations: HashMap::new(),
                    service: None,
                    volumes: vec![],
                },
            )?
            .resolve(
                Some(&plugins),
                &PluginBindings::new(),
                &HostRef::from_handler(&egress),
                &Meters::new(MeterKind::Off),
            )
            .await?;
        let id = workload
            .components()
            .read()
            .await
            .keys()
            .next()
            .context("reader component missing")?
            .to_string();
        let mut store = workload.new_store(&id).await?;
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = workload
            .instantiate_pre(&id)
            .await?
            .instantiate_async(&mut store)
            .await?;
        let interface = instance
            .get_export_index(&mut store, None, &operation)
            .context("handler missing")?;
        let run = instance
            .get_export_index(&mut store, Some(&interface), "run-json")
            .context("run missing")?;
        let run = instance.get_typed_func(&mut store, run)?;
        Ok(Self {
            store,
            run,
            _workload: workload,
            _egress: egress,
        })
    }

    async fn read(&mut self) -> anyhow::Result<Result<Emission, NodeError>> {
        self.read_unit(1).await
    }

    async fn read_unit(&mut self, unit: u8) -> anyhow::Result<Result<Emission, NodeError>> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let wiring: serde_json::Value = serde_json::from_slice(&std::fs::read(
            repository.join("apps/modbus_reader/publication/wirings/read_register.json"),
        )?)?;
        let mut config = wiring["nodes"]["read"]["params"].clone();
        config["unit"] = json!(unit);
        let context = NodeContext {
            wiring_id: "read_register".into(),
            wiring_version: 1,
            node_id: "read".into(),
            delivery_id: "live".into(),
            input_port: None,
            occurrence: 0,
            traceparent: None,
            tracestate: None,
            deadline_ms: Some(5000),
            config: config.to_string(),
        };
        let input = json!([{"request_id":"live", "value":{"address":7}}]).to_string();
        let run = self.run;
        let (result,) = self
            .store
            .run_concurrent(async |accessor| run.call_concurrent(accessor, (context, input)).await)
            .await??;
        let mut emission = match result {
            Ok(emission) => emission,
            Err(error) => return Ok(Err(error)),
        };
        let rows: Vec<serde_json::Value> = serde_json::from_str(&emission.payload)?;
        anyhow::ensure!(
            rows.len() == 1 && rows[0]["request_id"] == "live",
            "reader result has the wrong envelope"
        );
        if let Some(error) = rows[0].get("error") {
            anyhow::ensure!(
                error["code"] == "unavailable",
                "unexpected reader error: {error}"
            );
            return Ok(Err(NodeError::Retryable(
                node::wamn::node::types::ErrorDetail {
                    message: "unavailable".into(),
                    code: Some("unavailable".into()),
                },
            )));
        }
        emission.payload = rows[0]["value"]["register"]
            .as_i64()
            .context("register missing from reader result")?
            .to_string();
        Ok(Ok(emission))
    }
}

fn loaded_release(
    document: &EnvironmentDocument,
    application: &str,
) -> anyhow::Result<wamn_engine::release_manifest::LoadedRelease> {
    use std::collections::{BTreeMap, BTreeSet};
    use wamn_catalog::{
        PackageCoordinate, ServingComponent, ServingComponentOperation, ServingManifest,
        ServingRelease,
    };

    let selection = &document.applications[application];
    let version = format!(
        "0.1.{}-{}",
        document.integrations[&selection.connections["registers"]].version,
        selection.connections["registers"]
    );
    let members: Vec<_> = [("modbus_reader", "a"), ("observer_fixture", "b")]
        .into_iter()
        .filter(|(package, _)| selection.packages.contains(*package))
        .collect();
    let manifest = ServingManifest::new(
        ServingRelease {
            packages: members
                .iter()
                .map(|(package, _)| PackageCoordinate::new(*package, &version).unwrap())
                .collect(),
        },
        members
            .into_iter()
            .map(|(package, byte)| ServingComponent {
                runtime_schema: None,
                statement_instances: BTreeMap::new(),
                package_id: package.into(),
                operations: BTreeMap::from([(
                    "wamn:node/handler@0.1.0".into(),
                    ServingComponentOperation {
                        committed_result_schema: None,
                        registered_operation: None,
                        permissions: BTreeSet::new(),
                        statements: BTreeMap::new(),
                    },
                )]),
                descriptor: wamn_catalog::ComponentDescriptor::named(
                    "reader",
                    "0.1.0",
                    format!("sha256:{}", byte.repeat(64)),
                ),
            })
            .collect(),
        BTreeSet::new(),
        BTreeMap::new(),
    )?;
    wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "connection read fixture",
        wamn_engine::release_manifest::ReleaseScope::new("acme--one--dev", "dev")
            .with_applications(
                [(
                    application.to_owned(),
                    document.membership().remove(application).unwrap(),
                )]
                .into(),
            ),
    )
    .map_err(Into::into)
}

async fn apply(
    client: &mut tokio_postgres::Client,
    system_url: &str,
    project_url: &str,
    document: &EnvironmentDocument,
    packages: &[wamn_catalog::manifest::PackageManifest],
) -> anyhow::Result<()> {
    let mut project = connect(project_url).await?;
    project
        .batch_execute("CREATE SCHEMA IF NOT EXISTS wamn_run")
        .await?;
    project
        .batch_execute(&wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"))
        .await?;
    project
        .batch_execute(&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::ExecutorPlatform,
        ))
        .await?;
    // The connection memory reads the release fact as CallableHttp, and the
    // pool probes membership of its ACL role on every new connection.
    project
        .batch_execute(&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
        ))
        .await?;
    let _ = bindings::project_authority(system_url, project_url, document).await?;
    for application in document.applications.keys() {
        let selected = wamn_environment_control::environment::analyse::select_application(
            document,
            Some(application),
        )?;
        let source = client.transaction().await?;
        source.batch_execute("SET LOCAL ROLE wamn_db_owner").await?;
        let bindings = bindings::analyse(&source, &selected, packages).await?;
        source.commit().await?;
        let release = loaded_release(document, application)?;
        let env_key = "acme--one--dev";
        let digest = release.release().manifest_digest.as_str();
        wamn_environment_control::environment::stage::write_release(
            project_url,
            env_key,
            digest,
            release.manifest(),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await?;
        let tx = project.transaction().await?;
        tx.batch_execute(&bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await?;
        for component in &release.manifest().components {
            let package = &component.package_id;
            let manifest = packages
                .iter()
                .find(|manifest| &manifest.package.id == package)
                .context("connection fixture package missing")?;
            let requirement = &manifest.connections["registers"];
            let mode = match requirement.mode {
                wamn_catalog::manifest::ConnectionMode::Read => "read",
                wamn_catalog::manifest::ConnectionMode::Write => "write",
            };
            tx.execute("INSERT INTO catalog.connection_requirement(env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,$3,'registers',$4,$5) ON CONFLICT DO NOTHING", &[&env_key, &component.descriptor.component_digest, package, &requirement.interface, &mode]).await?;
        }
        bindings::write(
            &tx,
            env_key,
            digest,
            &selected,
            &bindings,
            &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: bin:docker, bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN, bin:rustc"]
async fn application_releases_keep_independent_integration_versions() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:docker, bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN, bin:rustc",
    );
    let _lock = wamn_test_postgres::lock();
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    fixture(scratch.path())?;
    let database = wamn_control_provision::test_database::system();
    let project_database = wamn_project_state::test_database::tenant_platform();
    let mut admin = connect(database.url()).await?;
    admin
        .batch_execute(&format!(
            "SET ROLE wamn_system; {}",
            wamn_control_provision::test_database::seed_org_project_env_sql("acme", "one", "dev")
        ))
        .await?;
    admin.batch_execute(&format!("BEGIN; {} UPDATE registry.meta SET platform_domain='example.invalid'; UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning))).await?;
    let registry = local_registry::LocalRegistry::start("modbus-restart");
    registry.ready().await;
    let auth = scratch.path().join("auth.json");
    std::fs::write(
        &auth,
        json!({"auths":{&registry.address:{"username":"wamn","password":"wamn"}}}).to_string(),
    )?;
    let configuration = PackageRegistry {
        artifact_base: format!("{}/wamn/components", registry.address),
        registry_auth_file: auth.clone(),
        insecure_registry: true,
        oci_ca_paths: vec![],
    };
    let first = integration::build(scratch.path(), "modbus").await?;
    integration::publish(&first, &configuration, database.url(), &integration::Publication {version:2, parameter_schema:json!({"type":"object","properties":{"authority":{"type":"string","minLength":1,"immutable":true}},"required":["authority"]}),operations:"wamn:modbus/client@1.0.0".into(),write_interfaces:vec!["wamn:modbus/write@1.0.0".into()]}).await?;
    let source_path = scratch
        .path()
        .join("apps/platform/no-std/modbus/src/lib.rs");
    let source_bytes = std::fs::read_to_string(&source_path)?;
    std::fs::write(
        &source_path,
        source_bytes.replace(
            "super::response_registers(&frame, &response).map_err(transport)?",
            "super::response_registers(&frame, &response).map(|mut registers| { registers[1] = registers[1].wrapping_add(1); registers }).map_err(transport)?",
        ),
    )?;
    let second = integration::build(scratch.path(), "modbus").await?;
    integration::publish(&second, &configuration, database.url(), &integration::Publication {version:3, parameter_schema:json!({"type":"object","properties":{"authority":{"type":"string","minLength":1,"immutable":true}},"required":["authority"]}),operations:"wamn:modbus/client@1.0.0".into(),write_interfaces:vec!["wamn:modbus/write@1.0.0".into()]}).await?;
    assert_ne!(first.digest, second.digest);
    let (authority, accepts, simulator_task) = simulator().await?;
    let (observer_authority, _, observer_task) = simulator().await?;
    let tx = admin.transaction().await?;
    tx.batch_execute(&bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    ))
    .await?;
    let store = wamn_test_infrastructure::credentials::LocalCredentials::create()?;
    for (name, endpoint) in [
        ("meter", &authority),
        ("observer-meter", &observer_authority),
    ] {
        wamn_test_infrastructure::credentials::define(&tx,"acme",&json!({"project":"one","env":"dev","name":name,"integration_type":"modbus","auth_type":"none","parameters":{"authority":endpoint}}),&store).await?;
    }
    tx.commit().await?;
    let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(
        &json!({"org":"acme","project":"one","env":"dev","target":"cloud","integrations":{"meter":{"version":2},"observer-meter":{"version":2}},"policy":"dev",
        "applications": {
            "reader":{"current": format!("sha256:{}","c".repeat(64)), "hostname": "reader.example", "packages":["modbus_reader"],"connections":{"registers":"meter"}},
            "observer":{"current": format!("sha256:{}","c".repeat(64)), "hostname": "reader.example", "packages":["observer_fixture"],"connections":{"registers":"observer-meter"}}
        }}),
    )?)?;
    let selections = document
        .applications
        .keys()
        .map(|application| {
            Ok((
                application.clone(),
                loaded_release(&document, application)?
                    .release()
                    .manifest_digest
                    .to_string(),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for (application, digest) in selections {
        document.applications.get_mut(&application).unwrap().current =
            wamn_environment_control::environment::document::DeclaredRelease::Digest(digest);
    }
    let package = wamn_catalog::manifest::PackageManifest::from_slice(&serde_json::to_vec(
        &json!({"package":{"id":"modbus_reader","version":"0.1.0"},"required_platform_policy_contract":{"id":"modbus_reader_data_access","state":"satisfied"},"models":{},"components":{},"connections":{"registers":{"interface":"wamn:modbus/read@1.0.0","mode":"read"}}}),
    )?)?;
    let mut observer_package = package.clone();
    observer_package.package.id = "observer_fixture".into();
    let packages = [package, observer_package];
    apply(
        &mut admin,
        database.url(),
        project_database.url(),
        &document,
        &packages,
    )
    .await?;
    // A dedicated reader login fixes scope through session_user, independent of GUCs.
    let login = format!("c9_integration_{}", std::process::id());
    database.execute(&[
        &sql::grant_connection_reader_surface_sql(),
        &format!("CREATE ROLE {login} LOGIN PASSWORD 'fixture'; GRANT wamn_conn_reader TO {login}; GRANT CONNECT ON DATABASE \"{}\" TO {login}; BEGIN; SET LOCAL ROLE wamn_system; {} INSERT INTO wamn_authority.author_login_tenants(login_identity,env_key,org_id,project_id,environment) VALUES ('{login}','acme--one--dev','acme','one','dev'); COMMIT;", database.name(), bind_platform_principal_sql(PlatformComponent::Provisioning)),
    ])?;
    let mut reader_url = url::Url::parse(database.url())?;
    reader_url.set_username(&login).unwrap();
    reader_url.set_password(Some("fixture")).unwrap();
    let reader_database = Arc::new(connect(reader_url.as_str()).await?);
    reader_database
        .batch_execute("SET app.org='forged'; SET app.project='forged'")
        .await?;
    assert_eq!(
        reader_database
            .query_one(
                "SELECT count(*) FROM registry.read_connection_fact('forged','one','dev','00000000-0000-0000-0000-000000000001')",
                &[]
            )
            .await?
            .get::<_, i64>(0),
        0
    );
    assert!(
        reader_database
            .query("SELECT * FROM registry.connection_generation", &[])
            .await
            .is_err()
    );
    let credentials = Arc::new(ConnectionCredentials::from_database(
        Arc::clone(&reader_database),
        "acme",
    ));
    let postgres = Arc::new(
        wamn_runtime::plugins::wamn_postgres::WamnPostgres::from_env_for_project(
            "one",
            Some(
                wamn_runtime::plugins::wamn_postgres::ClassCredentials::every_class(
                    project_database.url().to_owned(),
                ),
            ),
        )?,
    );
    let leases = Arc::new(wamn_runtime::resource_lease::ResourceLeases::new(Some(
        reader_url.as_str(),
    ))?);
    let facts = Arc::new(ConnectionFacts::new(
        Arc::new(ConnectionMemory::default().with_resource_leases(Arc::clone(&leases))),
        wamn_platform_identity::connection_changes::ConnectionChanges::default(),
        Arc::clone(&credentials),
        Arc::clone(&postgres),
        "acme--one--dev",
        "one",
        "dev",
    ));
    let imports = async |document: &EnvironmentDocument,
                         application: &str,
                         package: &str,
                         byte: &str|
           -> anyhow::Result<Arc<IntegrationImports>> {
        let release = loaded_release(document, application)?;
        let integrations = Integrations::new(
            Arc::clone(&facts),
            &release,
            &postgres,
            "one",
            configuration.clone(),
            RegistryCredentialSource::Fixed(read_registry_credentials(&auth, &registry.address)?),
        )
        .await?;
        integrations.imports(&format!("sha256:{}", byte.repeat(64)), package)
    };
    let reader = imports(&document, "reader", "modbus_reader", "a").await?;
    let observer = imports(&document, "observer", "observer_fixture", "b").await?;
    let sibling = observer.store_id("registers").await?.unwrap();
    let mut application = Application::load(Arc::clone(&reader)).await?;
    let app_store = Arc::clone(&application.store.data().active_ctx.store_id);
    assert_eq!(application.read().await?.unwrap().payload, "4660");
    assert_eq!(application.read_unit(17).await?.unwrap().payload, "17");
    let writer = reader
        .resolve("registers")
        .await?
        .context("writer binding")?;
    assert_eq!(
        read_register(&writer, "wamn:modbus/write@1.0.0", 17).await?,
        17
    );
    let written = writer
        .call(
            "wamn:modbus/write@1.0.0",
            "write-register",
            &[Val::U8(17), Val::U16(7), Val::U16(0xbeef)],
        )
        .await?;
    assert!(matches!(written.as_slice(), [Val::Result(Ok(None))]));

    assert_eq!(
        observer.store_id("registers").await?.as_deref(),
        Some(sibling.as_str())
    );
    // A missing new digest refuses its first call before the old instance runs.
    let absent = format!("sha256:{}", "f".repeat(64));
    let control = connect(database.url()).await?;
    let parameters: serde_json::Value = control
        .query_one(
            "SELECT parameter_schema FROM registry.integration_type_version WHERE integration_type='modbus' AND version=2",
            &[],
        )
        .await?
        .get(0);
    control.batch_execute("SET ROLE wamn_control").await?;
    control
        .query_one(
            "SELECT registry.publish_integration('modbus',4,$1,$2,'wamn:modbus/client@1.0.0',$3,ARRAY['wamn:modbus/write@1.0.0'])",
            &[
                &absent,
                &parameters,
                &vec![
                    "wamn:modbus/read@1.0.0".to_string(),
                    "wamn:modbus/write@1.0.0".to_string(),
                    "wamn:integration/health@0.1.0".to_string(),
                ],
            ],
        )
        .await?;
    document.integrations.get_mut("meter").unwrap().version = 4;
    apply(
        &mut admin,
        database.url(),
        project_database.url(),
        &document,
        &packages,
    )
    .await?;
    let failed_reader = imports(&document, "reader", "modbus_reader", "a").await?;
    assert!(Application::load(failed_reader).await.is_err());
    assert_eq!(application.read().await?.unwrap().payload, "4660");
    document.integrations.get_mut("meter").unwrap().version = 2;
    apply(
        &mut admin,
        database.url(),
        project_database.url(),
        &document,
        &packages,
    )
    .await?;
    assert_eq!(application.read().await?.unwrap().payload, "4660");
    let before_generation = reader.store_id("registers").await?.unwrap();
    // The engine counts compiles by the digest of the component bytes.
    let code = |built: &integration::BuiltIntegration| -> anyhow::Result<String> {
        Ok(wamn_engine::component_admission::component_digest(
            &std::fs::read(built.directory.join("integration.wasm"))?,
        ))
    };
    let (first_code, second_code) = (code(&first)?, code(&second)?);
    let compiled = wamn_engine::integration::compile_count(&first_code);
    assert_eq!(compiled, 1);
    assert_eq!(wamn_engine::integration::compile_count(&second_code), 0);
    let id = facts.env_connections().await?.connections["meter"];
    let old = facts.connection(&id).await?.unwrap();
    let tx = admin.transaction().await?;
    tx.batch_execute(&bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    ))
    .await?;
    let mut drops = wamn_platform_identity::connection_changes::ConnectionDrops::default();
    wamn_platform_identity::connections::administration::execute_with_store(
        &tx,
        "acme",
        "connection.write",
        &json!({"project":"one","env":"dev","name":"meter","parameters":{"authority":authority}})
            .to_string(),
        &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
        &mut drops,
        Some(&store),
    )
    .await?;
    tx.commit().await?;
    assert!(!drops.is_empty());
    wamn_platform_identity::connection_changes::ConnectionChanges::default()
        .publish(drops)
        .await;
    assert_eq!(application.read().await?.unwrap().payload, "4660");
    assert_eq!(
        facts.connection(&id).await?.unwrap().active_generation,
        old.active_generation + 1
    );
    assert_ne!(
        reader.store_id("registers").await?.unwrap(),
        before_generation
    );
    assert_eq!(
        wamn_engine::integration::compile_count(&first_code),
        compiled
    );
    // A new host loads A1. A0 and B0 retain their own integration versions.
    document.integrations.get_mut("meter").unwrap().version = 3;
    apply(
        &mut admin,
        database.url(),
        project_database.url(),
        &document,
        &packages,
    )
    .await?;
    let upgraded_reader = imports(&document, "reader", "modbus_reader", "a").await?;
    let mut upgraded = Application::load(upgraded_reader).await?;
    assert_eq!(upgraded.read().await?.unwrap().payload, "4661");
    assert_eq!(application.read().await?.unwrap().payload, "4660");
    assert_eq!(wamn_engine::integration::compile_count(&second_code), 1);
    // Rollback reloads A0 without changing B's connection or its resident instance.
    document.integrations.get_mut("meter").unwrap().version = 2;
    apply(
        &mut admin,
        database.url(),
        project_database.url(),
        &document,
        &packages,
    )
    .await?;
    let rollback_reader = imports(&document, "reader", "modbus_reader", "a").await?;
    let mut rollback = Application::load(rollback_reader).await?;
    assert_eq!(rollback.read().await?.unwrap().payload, "4660");
    assert_eq!(
        observer.store_id("registers").await?.as_deref(),
        Some(sibling.as_str())
    );
    let observer_instance = observer
        .resolve("registers")
        .await?
        .context("observer binding")?;
    assert_eq!(
        read_register(&observer_instance, "wamn:modbus/read@1.0.0", 1).await?,
        0x1234
    );
    assert_eq!(application.store.data().active_ctx.store_id, app_store);
    assert!(reader.resolve("unknown").await?.is_none());
    // Build a integration that attempts another port, under the same enforced binding.
    let denied_listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let denied_port = denied_listener.local_addr()?.port();
    let host = authority.rsplit_once(':').unwrap().0;
    let malicious = source_bytes.replace(
        "let authority = wamn::integration::bound::authority();",
        &format!("let authority = String::from(\"{host}:{denied_port}\");"),
    );
    std::fs::write(&source_path, malicious)?;
    let denied = integration::build(scratch.path(), "modbus").await?;
    let denied = Integration::load(
        &std::fs::read(denied.directory.join("integration.wasm"))?,
        IntegrationBinding::new(&authority, None)?,
    )
    .await?;
    let before = accepts.load(Ordering::SeqCst);
    let result = denied
        .call(
            "wamn:modbus/read@1.0.0",
            "read-registers",
            &[Val::U8(1), Val::U16(7), Val::U16(1)],
        )
        .await?;
    assert!(
        matches!(result.as_slice(), [Val::Result(Err(Some(error)))] if matches!(error.as_ref(), Val::Variant(name, _) if name == "authority-denied"))
    );
    assert_eq!(accepts.load(Ordering::SeqCst), before);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), denied_listener.accept())
            .await
            .is_err()
    );
    simulator_task.abort();
    observer_task.abort();
    leases.shutdown().await;
    database.execute(&[&format!(
        "RESET ROLE; DROP OWNED BY {login}; DROP ROLE {login};"
    )])?;
    Ok(())
}
