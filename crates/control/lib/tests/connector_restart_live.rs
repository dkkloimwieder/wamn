//! Real registry pulls and Modbus TCP reads retain the application store across pins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wamn_control::{
    environment::{bindings, document::EnvironmentDocument},
    integration,
};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql, sql};
use wamn_engine::connector::{
    Connector, ConnectorBinding, ConnectorError, ConnectorReader, READER,
};
use wamn_package_artifact::{
    PackageRegistry,
    registry_credentials::{RegistryCredentialSource, read_registry_credentials},
};
use wamn_platform_identity::{
    auth_changes::AuthDrops,
    connections::{self, ConnectionWrite},
};
use wamn_runtime::{
    connectors::{ConnectorSource, Connectors, REFRESH_INTERVAL},
    plugins::wamn_credentials::WamnCredentials,
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
    wasmtime::{Store, component::TypedFunc},
    wit::WitInterface,
};

#[path = "support/local_registry.rs"]
mod local_registry;

mod node {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../execution/workflow/router/wit", world: "node", wasmtime_crate: wash_runtime::wasmtime,
    });
}
use node::wamn::node::types::{Emission, NodeContext, NodeError};
type Run = TypedFunc<(NodeContext, String), (Result<Emission, NodeError>,)>;

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
        "integrations/modbus/Cargo.toml",
        "integrations/modbus/Cargo.lock",
        "integrations/modbus/src/lib.rs",
        "integrations/modbus/wit/world.wit",
        "integrations/modbus/wit/deps/io.wit",
        "integrations/modbus/wit/deps/sockets.wit",
        "crates/platform/runtime/wit/deps/wamn-connector/package.wit",
    ] {
        let target = root.join(file);
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::copy(repository.join(file), target)?;
    }
    Ok(())
}

async fn simulator() -> anyhow::Result<(String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
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
            tokio::spawn(async move {
                if number == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let mut frame = [0; 12];
                stream.read_exact(&mut frame).await.unwrap();
                assert_eq!(&frame[2..], &[0, 0, 0, 6, 1, 3, 0, 7, 0, 1]);
                let mut response = frame[..2].to_vec();
                response.extend([0, 0, 0, 5, 1, 3, 2, 0x12, 0x34]);
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
    async fn load(reader: Arc<ConnectorReader>) -> anyhow::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let package = repository.join("apps/modbus_reader");
        wamn_schema_generator::receipt::check_current(&package)?;
        let bytes = std::fs::read(
            repository.join("apps/target/wamn/modbus_reader/components/modbus_reader.wasm"),
        )?;
        let engine = wamn_engine::build_engine(&[])?;
        let mut declaration = wamn_control::component_declaration::render_declaration_document(
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
            admitted_platform_packages: ["wamn:connector".into()].into(),
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
                    host_interfaces: vec![WitInterface::from(READER)],
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
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let wiring: serde_json::Value = serde_json::from_slice(&std::fs::read(
            repository.join("apps/modbus_reader/publication/wirings/read_register.json"),
        )?)?;
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
            config: wiring["nodes"]["read"]["params"].to_string(),
        };
        let input = json!([{"request_id":"live", "value":{"unit":1,"address":7}}]).to_string();
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

async fn apply(
    client: &mut tokio_postgres::Client,
    document: &EnvironmentDocument,
    packages: &[wamn_schema_generator::PackageManifest],
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    tx.batch_execute(&bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    ))
    .await?;
    bindings::reserve(&tx, document, packages).await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: docker, PostgreSQL 18, wasm32-wasip2"]
async fn published_modbus_pins_restart_only_the_bound_connector() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker"]);
    let _lock = wamn_test_postgres::lock();
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    fixture(scratch.path())?;
    let database = wamn_control_provision::test_database::system();
    let mut admin = connect(database.url()).await?;
    admin
        .batch_execute(&format!(
            "SET ROLE wamn_system; {}",
            wamn_control_provision::test_database::seed_org_project_env_sql("acme", "one", "dev")
        ))
        .await?;
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
    integration::publish(&first, &configuration, database.url()).await?;
    let source_path = scratch.path().join("integrations/modbus/src/lib.rs");
    let source_bytes = std::fs::read_to_string(&source_path)?;
    std::fs::write(
        &source_path,
        format!("{source_bytes}\n// A second published source revision.\n"),
    )?;
    let second = integration::build(scratch.path(), "modbus").await?;
    integration::publish(&second, &configuration, database.url()).await?;
    assert_ne!(first.digest, second.digest);
    let (authority, accepts, simulator_task) = simulator().await?;
    let tx = admin.transaction().await?;
    tx.batch_execute(&bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    ))
    .await?;
    connections::write_connection(
        &tx,
        "acme",
        &ConnectionWrite {
            integration: "modbus".into(),
            external_system: None,
            name: "meter".into(),
            transport: "tcp".into(),
            exchange: "request".into(),
            parameters: json!({"authority":authority}),
            status: "active".into(),
        },
        &mut AuthDrops::default(),
    )
    .await?;
    connections::enable_connection(&tx, "acme", "one", "meter").await?;
    for (scope, name) in [
        ("org", "acme"),
        ("project", "one"),
        ("environment", "one/dev"),
    ] {
        connections::allow_integration(&tx, "acme", scope, name, "modbus").await?;
    }
    tx.commit().await?;
    let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(
        &json!({"org":"acme","project":"one","env":"dev","target":"cloud","release":"none","route_host":"reader.example","policy":"dev","applications":{
            "reader":{"packages":["modbus_reader"],"connectors":{"modbus":first.digest},"connections":{"registers":"meter"}},
            "observer":{"packages":["observer_fixture"],"connectors":{"modbus":first.digest},"connections":{"registers":"meter"}}
        }}),
    )?)?;
    let package = wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(
        &json!({"package":{"id":"modbus_reader","version":"0.1.0"},"required_platform_policy_contract":{"id":"modbus_reader_data_access","state":"satisfied"},"models":{},"components":{},"connections":{"registers":{"integration":"modbus","mode":"read"}}}),
    )?)?;
    let mut observer_package = package.clone();
    observer_package.package.id = "observer_fixture".into();
    let packages = [package, observer_package];
    apply(&mut admin, &document, &packages).await?;
    // A dedicated reader login fixes scope through session_user, independent of GUCs.
    let login = format!("c9_connector_{}", std::process::id());
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
                "SELECT count(*) FROM registry.lookup_connector_bindings('forged','one','dev')",
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
    let credentials = Arc::new(WamnCredentials::from_database(
        Arc::clone(&reader_database),
        "acme",
    ));
    let source = ConnectorSource {
        database: reader_database,
        credentials,
        registry: configuration,
        registry_credentials: RegistryCredentialSource::Fixed(read_registry_credentials(
            &auth,
            &registry.address,
        )?),
        org: "acme".into(),
        project: "one".into(),
        environment: "dev".into(),
    };
    let connectors = Arc::new(Connectors::new(["reader".into(), "observer".into()]));
    connectors.refresh(&source).await?;
    let reader = connectors.reader("reader")?;
    let observer = connectors.reader("observer")?;
    let sibling = observer.store_id("registers").await.unwrap();
    let mut application = Application::load(Arc::clone(&reader)).await?;
    let app_store = application.store.data().active_ctx.store_id.clone();
    assert_eq!(application.read().await?.unwrap().payload, "4660");
    connectors.refresh(&source).await?;
    assert_eq!(
        observer.store_id("registers").await.as_deref(),
        Some(sibling.as_str())
    );
    // Publication alone cannot make a missing artifact replace a serving store.
    let absent = format!("sha256:{}", "f".repeat(64));
    let control = connect(database.url()).await?;
    control.batch_execute("SET ROLE wamn_control").await?;
    control
        .query_one(
            "SELECT registry.publish_integration('modbus',$1)",
            &[&absent],
        )
        .await?;
    document
        .applications
        .get_mut("reader")
        .unwrap()
        .connectors
        .insert("modbus".into(), absent);
    apply(&mut admin, &document, &packages).await?;
    let before_failed_pull = reader.store_id("registers").await;
    assert!(connectors.refresh(&source).await.is_err());
    assert_eq!(reader.store_id("registers").await, before_failed_pull);
    assert_eq!(application.read().await?.unwrap().payload, "4660");
    document
        .applications
        .get_mut("reader")
        .unwrap()
        .connectors
        .insert("modbus".into(), first.digest.clone());
    apply(&mut admin, &document, &packages).await?;
    connectors.refresh(&source).await?;
    let pid = std::process::id();
    let mut measurements = Vec::new();
    let mut previous = reader.store_id("registers").await.unwrap();
    let mut polls = tokio::time::interval_at(
        tokio::time::Instant::now() + REFRESH_INTERVAL,
        REFRESH_INTERVAL,
    );
    polls.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    for (change, pin) in [
        &second.digest,
        &first.digest,
        &second.digest,
        &first.digest,
        &second.digest,
        &first.digest,
    ]
    .into_iter()
    .enumerate()
    {
        document
            .applications
            .get_mut("reader")
            .unwrap()
            .connectors
            .insert("modbus".into(), pin.clone());
        let changed = Instant::now();
        apply(&mut admin, &document, &packages).await?;
        let finished = std::sync::atomic::AtomicBool::new(false);
        let refresh = async {
            let waiting = Instant::now();
            polls.tick().await;
            let poll_wait = waiting.elapsed();
            let result = connectors.refresh(&source).await;
            finished.store(true, Ordering::SeqCst);
            result.map(|()| poll_wait)
        };
        let calls = async {
            let mut unavailable = 0;
            let mut reads = 0;
            let mut first_unavailable = None;
            while !finished.load(Ordering::SeqCst) {
                match application.read().await? {
                    Ok(emission) => {
                        assert_eq!(emission.payload, "4660");
                        reads += 1;
                    }
                    Err(NodeError::Retryable(detail)) => {
                        assert_eq!(detail.message, "unavailable");
                        assert_eq!(detail.code.as_deref(), Some("unavailable"));
                        unavailable += 1;
                        first_unavailable.get_or_insert_with(|| changed.elapsed());
                    }
                    Err(error) => panic!("unexpected read error: {error:?}"),
                }
                tokio::task::yield_now().await;
            }
            anyhow::Ok((unavailable, reads, first_unavailable))
        };
        let (refreshed, calls) = tokio::time::timeout(
            Duration::from_secs(120),
            Box::pin(async { tokio::join!(refresh, calls) }),
        )
        .await?;
        let poll_wait = refreshed?;
        let (unavailable, reads, first_unavailable) = calls?;
        assert_eq!(application.read().await?.unwrap().payload, "4660");
        let elapsed = changed.elapsed();
        assert_eq!(
            observer
                .get("registers".into(), vec![1, 0, 7, 0, 1])
                .await?
                .unwrap(),
            [0x12, 0x34]
        );
        let current = reader.store_id("registers").await.unwrap();
        assert_ne!(previous, current);
        previous = current;
        assert_eq!(application.store.data().active_ctx.store_id, app_store);
        assert_eq!(std::process::id(), pid);
        assert_eq!(
            observer.store_id("registers").await.as_deref(),
            Some(sibling.as_str())
        );
        measurements.push(json!({"change":change,"digest":pin,"pin_to_success_ms":elapsed.as_secs_f64()*1000.0,"poll_wait_ms":poll_wait.as_secs_f64()*1000.0,"unavailable":unavailable,"reads_during_pull":reads,"unavailable_to_success_ms":first_unavailable.map(|start| elapsed.saturating_sub(start).as_secs_f64()*1000.0)}));
    }
    assert!(
        measurements
            .iter()
            .any(|sample| sample["unavailable"].as_u64().unwrap() > 0)
    );
    assert!(matches!(
        reader.get("unknown".into(), vec![]).await?,
        Err(ConnectorError::Unbound)
    ));
    // Build a connector that attempts another port, under the same enforced binding.
    let denied_listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let denied_port = denied_listener.local_addr()?.port();
    let host = authority.rsplit_once(':').unwrap().0;
    let malicious = source_bytes.replace(
        "let authority = wamn::connector::bound::authority();",
        &format!("let authority = String::from(\"{host}:{denied_port}\");"),
    );
    std::fs::write(&source_path, malicious)?;
    let denied = integration::build(scratch.path(), "modbus").await?;
    let denied = Connector::load(
        &std::fs::read(denied.directory.join("connector.wasm"))?,
        ConnectorBinding::new(&authority, None)?,
    )
    .await?;
    let before = accepts.load(Ordering::SeqCst);
    assert!(matches!(
        denied.get("registers".into(), vec![1, 0, 7, 0, 1]).await?,
        Err(ConnectorError::AuthorityDenied)
    ));
    assert_eq!(accepts.load(Ordering::SeqCst), before);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), denied_listener.accept())
            .await
            .is_err()
    );
    let results = std::env::var_os("WAMN_C9_RESULTS")
        .map_or_else(|| scratch.path().join("results"), PathBuf::from);
    std::fs::create_dir_all(&results)?;
    std::fs::write(
        results.join("restart.json"),
        serde_json::to_vec_pretty(
            &json!({"process_id":pid,"application_store":app_store.to_string(),"poll_interval_ms":REFRESH_INTERVAL.as_millis(),"samples":measurements}),
        )?,
    )?;
    simulator_task.abort();
    database.execute(&[&format!(
        "RESET ROLE; DROP OWNED BY {login}; DROP ROLE {login};"
    )])?;
    Ok(())
}
