//! Exclusive calls through the existing registry, Modbus and application fixtures.

use super::*;
use sqlx::{Connection as _, Row as _};
use wamn_broker::Publisher;
use wamn_broker::connection_changes::ConnectionDrops;
use wamn_runtime::auth_cache::test_util::{nats, until};
use wamn_runtime::resource_lease::ResourceLeases;

struct Host {
    leases: Arc<ResourceLeases>,
    memory: Arc<ConnectionMemory>,
    imports: Arc<IntegrationImports>,
    writer: Arc<IntegrationImports>,
}

struct Fixture {
    admin: sqlx::PgConnection,
    database: wamn_test_postgres::Database,
    project_database: wamn_test_postgres::Database,
    document: EnvironmentDocument,
    packages: Vec<wamn_compiler::manifest::PackageManifest>,
    release: wamn_engine::release_manifest::LoadedRelease,
    credentials: Arc<ConnectionCredentials>,
    postgres: Arc<wamn_runtime::plugins::wamn_postgres::WamnPostgres>,
    reader_url: String,
    registry: PackageRegistry,
    registry_credentials: RegistryCredentialSource,
    _registry: local_registry::LocalRegistry,
    _scratch: wamn_test_infrastructure::scratch::ScratchRoot,
    _store: wamn_test_infrastructure::credentials::MemoryStore,
}

impl Fixture {
    async fn new(authority: &str) -> anyhow::Result<Self> {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        fixture(scratch.path())?;
        let database = wamn_control_provision::test_database::system();
        let project_database = wamn_platform_identity::test_database::project_platform();
        let mut admin = connect(database.url()).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET ROLE wamn_system; BEGIN; {} {} UPDATE registry.meta SET platform_domain='example.invalid'; UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT",
            bind_platform_principal_sql(PlatformComponent::Provisioning),
            wamn_control_provision::test_database::seed_org_project_env_sql("acme", "one", "dev")))).execute(&mut admin).await.map(drop)?;
        let registry = local_registry::LocalRegistry::start("exclusive-resources");
        registry.ready().await;
        let auth = scratch.path().join("auth.json");
        std::fs::write(
            &auth,
            json!({"auths":{&registry.address:{"username":"wamn","password":"wamn"}}}).to_string(),
        )?;
        let configuration = PackageRegistry {
            artifact_base: format!("{}/wamn/components", registry.address),
            credentials: None,
            registry_auth_file: auth.clone(),
            insecure_registry: true,
            oci_ca_paths: vec![],
        };
        let code = integration::build(scratch.path(), "modbus").await?;
        integration::publish(&code, &configuration, database.url(), &integration::Publication {
            version: 2,
            parameter_schema: json!({"type":"object","properties":{"authority":{"type":"string","minLength":1,"immutable":true}},"required":["authority"]}),
            operations: "wamn:modbus/client@1.0.0".into(),
            write_interfaces: vec!["wamn:modbus/write@1.0.0".into()],
        }).await?;
        let store = wamn_test_infrastructure::credentials::MemoryStore::default();
        // Prepare Y as disabled before the scenario starts, without violating uniqueness.
        for name in ["meter", "replacement"] {
            let mut tx = admin.begin().await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                (&bind_platform_principal_sql(PlatformComponent::Provisioning)).to_string(),
            ))
            .execute(&mut *tx)
            .await
            .map(drop)?;
            wamn_test_infrastructure::credentials::define(&mut tx, "acme", &json!({"project":"one","env":"dev","name":name,"integration_type":"modbus","auth_type":"none","parameters":{"authority":authority}}), &store).await?;
            wamn_platform_identity::connections::administration::execute_with_store(
                &mut tx,
                "acme",
                "connection.disable",
                &json!({"project":"one","env":"dev","name":name}).to_string(),
                &mut wamn_broker::auth_changes::AuthDrops::default(),
                &mut ConnectionDrops::default(),
                None,
            )
            .await?;
            tx.commit().await?;
        }
        let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":"acme","project":"one","env":"dev","target":"cloud",
        "integrations":{"meter":{"version":2}},"policy":"dev","revocation_seconds":60,
        "applications": {
            "reader":{"revocation_seconds":5,"hostname": "reader.example","connections":{"registers":"meter"}},
            "writer":{"revocation_seconds":5,"hostname": "reader.example","connections":{"registers":"meter"}}
        }}))?)?;
        for (name, package) in [("reader", "modbus_reader"), ("writer", "observer_fixture")] {
            document
                .applications
                .get_mut(name)
                .unwrap()
                .packages
                .insert(package.to_owned());
        }
        let release = loaded_release(&document, "reader")?;
        document.applications.get_mut("reader").unwrap().current =
            wamn_environment::environment::document::DeclaredRelease::Digest(
                release.release().manifest_digest.to_string(),
            );
        let writer = loaded_release(&document, "writer")?;
        document.applications.get_mut("writer").unwrap().current =
            wamn_environment::environment::document::DeclaredRelease::Digest(
                writer.release().manifest_digest.to_string(),
            );
        let package = wamn_compiler::manifest::PackageManifest::from_slice(&serde_json::to_vec(
            &json!({
                "package":{"id":"modbus_reader","version":"0.1.0"},
                "required_platform_policy_contract":{"id":"modbus_reader_data_access","state":"satisfied"},
                "models":{},"components":{},"connections":{"registers":{"interface":"wamn:modbus/read@1.0.0","mode":"read"}}
            }),
        )?)?;
        let mut writer = package.clone();
        writer.package.id = "observer_fixture".into();
        writer.connections.insert(
            "registers".into(),
            wamn_compiler::manifest::ConnectionDeclaration {
                interface: "wamn:modbus/write@1.0.0".into(),
                mode: wamn_compiler::manifest::ConnectionMode::Write,
            },
        );
        let login = format!("exclusive_{}", database.name());
        database.execute(&[&sql::grant_connection_reader_surface_sql(), &format!(
            "CREATE ROLE {login} LOGIN PASSWORD 'fixture'; GRANT wamn_conn_reader TO {login}; GRANT CONNECT ON DATABASE \"{}\" TO {login}; BEGIN; SET LOCAL ROLE wamn_system; {} INSERT INTO wamn_authority.author_login_projects(login_identity,project_env_key,org_id,project_id,environment) VALUES ('{login}','acme--dev--one','acme','one','dev'); COMMIT;",
            database.name(), bind_platform_principal_sql(PlatformComponent::Provisioning)
        )])?;
        let mut reader_url = url::Url::parse(database.url())?;
        reader_url.set_username(&login).unwrap();
        reader_url.set_password(Some("fixture")).unwrap();
        let credentials = Arc::new(ConnectionCredentials::from_database(
            wamn_postgres::pool(reader_url.as_str(), 4).await?,
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
        let mut fixture = Self {
            admin,
            database,
            project_database,
            document,
            packages: vec![package, writer],
            release,
            credentials,
            postgres,
            reader_url: reader_url.to_string(),
            registry: configuration,
            registry_credentials: RegistryCredentialSource::Fixed(read_registry_credentials(
                &auth,
                &registry.address,
            )?),
            _registry: registry,
            _scratch: scratch,
            _store: store,
        };
        fixture
            .change("meter", "connection.enable", &Publisher::default())
            .await?;
        fixture.bind("meter", &Publisher::default()).await?;
        Ok(fixture)
    }

    async fn change(
        &mut self,
        name: &str,
        operation: &str,
        changes: &Publisher,
    ) -> anyhow::Result<()> {
        let mut tx = self.admin.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&bind_platform_principal_sql(PlatformComponent::Provisioning)).to_string(),
        ))
        .execute(&mut *tx)
        .await
        .map(drop)?;
        let mut drops = ConnectionDrops::default();
        wamn_platform_identity::connections::administration::execute_with_store(
            &mut tx,
            "acme",
            operation,
            &json!({"project":"one","env":"dev","name":name}).to_string(),
            &mut wamn_broker::auth_changes::AuthDrops::default(),
            &mut drops,
            None,
        )
        .await?;
        tx.commit().await?;
        changes.publish(drops).await;
        Ok(())
    }

    async fn bind(&mut self, name: &str, changes: &Publisher) -> anyhow::Result<()> {
        self.document.integrations.clear();
        self.document.integrations.insert(
            name.to_owned(),
            wamn_environment::environment::document::Integration { version: 2 },
        );
        for application in self.document.applications.values_mut() {
            application
                .connections
                .insert("registers".into(), name.into());
        }
        self.release = loaded_release(&self.document, "reader")?;
        self.document
            .applications
            .get_mut("reader")
            .unwrap()
            .current = wamn_environment::environment::document::DeclaredRelease::Digest(
            self.release.release().manifest_digest.to_string(),
        );
        apply(
            &mut self.admin,
            self.database.url(),
            self.project_database.url(),
            &self.document,
            &self.packages,
        )
        .await?;
        let mut drops = ConnectionDrops::default();
        drops.release(
            "acme--dev--one",
            self.release.release().manifest_digest.as_str(),
        );
        changes.publish(drops).await;
        Ok(())
    }

    async fn host(&self, nats: &async_nats::Client) -> anyhow::Result<Host> {
        let leases = Arc::new(ResourceLeases::new(Some(&self.reader_url))?);
        let memory =
            Arc::new(ConnectionMemory::default().with_resource_leases(Arc::clone(&leases)));
        let changes = wamn_runtime::connection_memory::replicate(
            nats,
            Some("acme--dev--one"),
            Arc::clone(&memory),
        );
        until("the exclusive host watches connections", || {
            memory.watched()
        })
        .await;
        let facts = Arc::new(ConnectionFacts::new(
            Arc::clone(&memory),
            changes,
            Arc::clone(&self.credentials),
            Arc::clone(&self.postgres),
            "acme--dev--one",
            "one",
            "dev",
        ));
        let integrations = Integrations::new(
            Arc::clone(&facts),
            &self.release,
            &self.postgres,
            "one",
            self.registry.clone(),
            self.registry_credentials.clone(),
        )
        .await?;
        let writer_release = loaded_release(&self.document, "writer")?;
        let writer = Integrations::new(
            facts,
            &writer_release,
            &self.postgres,
            "one",
            self.registry.clone(),
            self.registry_credentials.clone(),
        )
        .await?;
        Ok(Host {
            leases,
            memory,
            imports: integrations
                .imports(&format!("sha256:{}", "a".repeat(64)), "modbus_reader")?,
            writer: writer.imports(&format!("sha256:{}", "b".repeat(64)), "observer_fixture")?,
        })
    }
}

async fn read(imports: &IntegrationImports, interface: &str) -> anyhow::Result<()> {
    let values = imports
        .call(
            "registers",
            interface,
            "read-registers",
            &[Val::U8(1), Val::U16(7), Val::U16(1)],
        )
        .await?
        .context("exclusive call unavailable")??;
    anyhow::ensure!(
        matches!(values.as_slice(), [Val::Result(Ok(Some(value)))] if matches!(value.as_ref(), Val::List(values) if matches!(values.as_slice(), [Val::U16(0x1234)]))),
        "unexpected exclusive read result: {values:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN, bin:docker, bin:rustc"]
async fn exclusive_calls_wait_serialize_and_transfer_to_replacement() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN, bin:docker, bin:rustc",
    );
    let _lock = wamn_test_postgres::lock();
    let condition = Arc::new(tokio::sync::Semaphore::new(1));
    let (authority, requests, server) =
        simulator_with_condition(Some(Arc::clone(&condition))).await?;
    let mut fixture = Fixture::new(&authority).await?;
    let (_nats, nats) = nats().await;
    let changes = Publisher::new(nats.clone());
    let a = fixture.host(&nats).await?;
    let mut app = Application::load(Arc::clone(&a.imports)).await?;
    assert_eq!(app.read().await?.unwrap().payload, "4660");
    // Two hosts of the same environment share X for the thirty-second refusal.
    let b = fixture.host(&nats).await?;
    let mut other = Application::load(Arc::clone(&b.imports)).await?;
    let started = Instant::now();
    assert!(
        matches!(other.read().await?, Err(NodeError::Retryable(detail)) if detail.code.as_deref()==Some("unavailable"))
    );
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert!(started.elapsed() < Duration::from_secs(35));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    b.leases.shutdown().await;

    // Both declared interfaces share the same connection mutex on host A.
    let before = requests.load(Ordering::SeqCst);
    let reader = Arc::clone(&a.imports);
    let reading = tokio::spawn(async move { read(&reader, "wamn:modbus/read@1.0.0").await });
    until("the reader reaches the resource", || {
        requests.load(Ordering::SeqCst) == before + 1
    })
    .await;
    let writer = Arc::clone(&a.writer);
    let writing = tokio::spawn(async move { read(&writer, "wamn:modbus/write@1.0.0").await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(requests.load(Ordering::SeqCst), before + 1);
    condition.add_permits(1);
    reading.await??;
    until("the writer reaches the resource after the reader", || {
        requests.load(Ordering::SeqCst) == before + 2
    })
    .await;
    assert!(!writing.is_finished());

    // Y becomes active only after X is disabled. X keeps its in-flight call.
    let id = sqlx::query("SELECT c.id FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.name='meter'").fetch_one(&mut fixture.admin).await?.get(0);
    let disabled = Instant::now();
    fixture
        .change("meter", "connection.disable", &changes)
        .await?;
    until("the old host drops X", || {
        a.memory.connection("acme--dev--one", &id).is_err()
    })
    .await;
    assert!(
        matches!(app.read().await?, Err(NodeError::Retryable(detail)) if detail.code.as_deref()==Some("unavailable"))
    );
    assert_eq!(requests.load(Ordering::SeqCst), before + 2);
    fixture
        .change("replacement", "connection.enable", &changes)
        .await?;
    fixture.bind("replacement", &changes).await?;
    let b = fixture.host(&nats).await?;
    let mut replacement = Application::load(Arc::clone(&b.imports)).await?;
    let calling = tokio::spawn(async move { replacement.read().await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!calling.is_finished());
    assert_eq!(requests.load(Ordering::SeqCst), before + 2);
    condition.add_permits(2);
    writing.await??;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(60), calling)
            .await???
            .unwrap()
            .payload,
        "4660"
    );
    assert!(disabled.elapsed() < Duration::from_secs(60));
    assert_eq!(
        a.memory.connection("acme--dev--one", &id).unwrap().status,
        wamn_runtime::connection_memory::ConnectionStatus::Disabled
    );
    a.leases.shutdown().await;
    b.leases.shutdown().await;
    server.abort();
    Ok(())
}
