//! Application preparation preserves SQL, other applications, and running release authority.

use sqlx::{Connection as _, Row as _};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wamn_catalog::{ComponentDescriptor, PackageCoordinate, ServingComponent, ServingManifest};
use wamn_compiler::manifest::PackageManifest;
use wamn_digest::bytes_sha256;
use wamn_environment::environment::{Platform, document::EnvironmentDocument, ensure, expand, sql};

use wamn_test_infrastructure::scratch::ScratchRoot;

const PROJECT_ENV_KEY: &str = "acme--dev--apps";
const STATE_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn connect(url: &str) -> anyhow::Result<sqlx::PgConnection> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

// Artifact transport only. Initialization and activation use the product operations.
struct Registry {
    address: String,
    auth: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
}
impl Registry {
    async fn start(root: &Path, objects: BTreeMap<String, Vec<u8>>) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = format!("localhost:{}", listener.local_addr()?.port());
        let auth = root.join("registry.json");
        std::fs::write(
            &auth,
            serde_json::to_vec(
                &json!({"auths": {&address: {"username":"test", "password":"test"}}}),
            )?,
        )?;
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let body = objects
                    .get(path)
                    .map(Vec::as_slice)
                    .or_else(|| (path == "/v2/").then_some(b"{}"));
                let status = if body.is_some() { 200 } else { 404 };
                let body = body.unwrap_or(b"");
                let header = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nDocker-Content-Digest: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                    bytes_sha256(body)
                );
                if socket.write_all(header.as_bytes()).await.is_ok()
                    && !request.starts_with("HEAD ")
                {
                    let _ = socket.write_all(body).await;
                }
            }
        });
        Ok(Self {
            address,
            auth,
            task,
        })
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn artifact(
    objects: &mut BTreeMap<String, Vec<u8>>,
    repository: &str,
    tag: &str,
    manifest: Vec<u8>,
    blobs: impl IntoIterator<Item = Vec<u8>>,
) {
    objects.insert(format!("/v2/{repository}/manifests/{tag}"), manifest);
    for bytes in blobs {
        objects.insert(
            format!("/v2/{repository}/blobs/{}", bytes_sha256(&bytes)),
            bytes,
        );
    }
}

fn component(
    objects: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
    aliases: &[&str],
) -> anyhow::Result<ServingComponent> {
    let mut resolve = wit_parser::Resolve::new();
    let mut imports = String::new();
    for alias in aliases {
        write!(imports, "import {alias}: http;")?;
    }
    let package = resolve.push_str("http.wit", &format!("package wamn:connection@0.1.0; interface http {{ get: func(); }} world {name} {{ {imports} export run: func(); }}"))?;
    let world = resolve.select_world(&[package], Some(name))?;
    let mut module =
        wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )?;
    let bytes = wit_component::ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    let mut descriptor = ComponentDescriptor::named(name, "1.0.0", bytes_sha256(&bytes));
    descriptor.operations = serde_json::from_value(json!({
        "run": {"input-ports": [], "output-ports": [], "parameters": []}
    }))?;
    descriptor
        .statement_instances
        .insert("state".into(), STATE_DIGEST.into());
    descriptor.connections = aliases
        .iter()
        .map(|alias| wamn_catalog::ComponentConnection {
            store_alias: (*alias).into(),
            requirement_type: wamn_catalog::ComponentConnectionType::Http,
            mode: wamn_catalog::ConnectionMode::Write,
        })
        .collect();
    let admitted = descriptor
        .clone()
        .into_admitted(wamn_catalog::ComponentPackageCoordinate::new(
            PROJECT_ENV_KEY,
            name,
            "1.0.0",
        )?);
    let config =
        wamn_engine::component_artifact::component_artifact_config_bytes(&admitted.component);
    let layer = oci_client::client::ImageLayer::new(
        bytes.clone(),
        wamn_engine::component_artifact::COMPONENT_LAYER_MEDIA_TYPE.into(),
        None,
    );
    let config_layer = oci_client::client::Config::new(
        config.clone(),
        wamn_engine::component_artifact::COMPONENT_CONFIG_MEDIA_TYPE.into(),
        None,
    );
    let image = oci_client::manifest::OciImageManifest::build(&[layer], &config_layer, None);
    artifact(
        objects,
        "components",
        &descriptor.component_digest[7..],
        serde_json::to_vec(&image)?,
        [bytes, config],
    );
    Ok(ServingComponent {
        package_id: name.into(),
        descriptor,
        runtime_schema: None,
        statement_instances: BTreeMap::new(),
    })
}

fn package(
    root: &Path,
    objects: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
    component: Option<&ServingComponent>,
    version: &str,
    extra_sql: &str,
) -> anyhow::Result<PackageManifest> {
    let root = root.join(name);
    std::fs::create_dir_all(root.join("migrations"))?;
    std::fs::create_dir_all(root.join("generated/platform-policy"))?;
    let mut value = json!({"package":{"id":name,"version":version},"required_platform_policy_contract":{"id":"fixture_access","state":"satisfied"},"models":{},"components":{},"connections":{}});
    value["components"] = json!({name:{"connections":["postgres"]}});
    value["models"] = json!({"data":{"schema":name,"table":"data","owner":name,"server_owned_fields":[],"audit_log":{"columns":[],"retention":"none"},"operations":{"get":{"permission":"data.get","result":"one"},"update":{"permission":"data.update","result":"one"}}}});
    std::fs::write(
        root.join("migrations/0001_data.sql"),
        format!(
            "CREATE TABLE {name}.data (id integer GENERATED BY DEFAULT AS IDENTITY CONSTRAINT {name}_data_id_pkey PRIMARY KEY, value text); {extra_sql}"
        ),
    )?;
    if let Some(component) = component {
        value["sequences"] = json!({"document": {"pattern": "DOC-{seq:number}"}});
        value["base_dependencies"] = json!({"state":{"package":"wamn_integration","version":"1.0.0","digest":STATE_DIGEST,"operations":["data.get","data.update"]}});
        for connection in &component.descriptor.connections {
            value["connections"][connection
                .store_alias
                .name()
                .expect("fixture connection has a literal alias")] =
                json!({"interface":"wamn:connection/http@0.1.0","mode":"write"});
        }
        std::fs::create_dir_all(root.join("descriptors"))?;
        std::fs::write(
            root.join("descriptors").join(format!("{name}.json")),
            wamn_digest::canonical_json_bytes(&serde_json::to_value(&component.descriptor)?),
        )?;
    }
    let bytes = serde_json::to_vec(&value)?;
    let manifest = PackageManifest::from_slice(&bytes)?;
    let mut authored = String::new();
    for (key, value) in value.as_object().unwrap() {
        writeln!(authored, "{key} = {value}")?;
    }
    std::fs::write(root.join("wamn.k"), authored)?;
    std::fs::write(root.join("generated/wamn.json"), &bytes)?;
    wamn_test_infrastructure::fixture_digest::write_build_record(&root);
    let packed = wamn_package_control::package_artifact::pack_package(&root)?;
    let tag = packed.tag();
    artifact(
        objects,
        "packages",
        &tag,
        packed.manifest,
        packed
            .layers
            .into_iter()
            .map(|layer| layer.bytes)
            .chain([b"{}".to_vec()]),
    );
    Ok(manifest)
}

fn release(
    objects: &mut BTreeMap<String, Vec<u8>>,
    component: ServingComponent,
    version: &str,
) -> anyhow::Result<ServingManifest> {
    let manifest = ServingManifest {
        format_version: wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        release: wamn_catalog::ServingRelease {
            packages: BTreeSet::from([
                PackageCoordinate::new(&component.package_id, version)?,
                PackageCoordinate::new("wamn_integration", "1.0.0")?,
            ]),
        },
        composed_entry: None,
        components: BTreeSet::from([component]),
        routes: BTreeSet::new(),
        attachments: BTreeMap::new(),
        host_routes: BTreeSet::new(),
    };
    let bytes = manifest.canonical_bytes();
    let (layer, config, image) =
        wamn_runtime::release_manifest_artifact::release_manifest_artifact_layout(&bytes);
    artifact(
        objects,
        "releases",
        &manifest.digest().as_str()[7..],
        serde_json::to_vec(&image)?,
        [layer.data.to_vec(), config.data.to_vec()],
    );
    Ok(manifest)
}

fn document(a: &ServingManifest, b: &ServingManifest) -> anyhow::Result<EnvironmentDocument> {
    let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
    "org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,
    "integrations":{"erp":{"version":1}},"applications":{
        "a":{"revocation_seconds":5,"hostname":"a.example","connections":{"upstream":"erp"}},
        "b":{"revocation_seconds":5,"hostname":"b.example","connections":{"upstream":"erp"}}
    }}))?)?;
    for (name, manifest) in [("a", a), ("b", b)] {
        let application = document.applications.get_mut(name).unwrap();
        application.current = wamn_environment::environment::document::DeclaredRelease::Digest(
            manifest.digest().to_string(),
        );
        application.packages = manifest
            .release
            .packages
            .iter()
            .map(|package| package.package_id().to_owned())
            .collect();
    }
    Ok(document)
}

/// The application data, allocation position, and access that activation must preserve.
#[derive(Debug, PartialEq)]
struct SqlContinuity {
    relations: (sqlx::postgres::types::Oid, sqlx::postgres::types::Oid),
    rows: (i32, i32),
    read: bool,
    update: bool,
    counters: Vec<i64>,
}

async fn continuity(client: &mut sqlx::PgConnection) -> anyhow::Result<SqlContinuity> {
    let (a, b, a_row, b_row, read, update) = sqlx::query_as(
        "SELECT 'a.data'::regclass::oid, 'b.data'::regclass::oid, \
         (SELECT id FROM a.data WHERE value='authored'), \
         (SELECT id FROM b.data WHERE value='other'), \
         has_column_privilege('wamn_app','b.data','value','SELECT'), \
         has_column_privilege('wamn_app','b.data','value','UPDATE')",
    )
    .fetch_one(&mut *client)
    .await?;
    let counters = sqlx::query_scalar(
        "SELECT value FROM platform.number_counter ORDER BY sequence_id,counter,key_value",
    )
    .fetch_all(client)
    .await?;
    Ok(SqlContinuity {
        relations: (a, b),
        rows: (a_row, b_row),
        read,
        update,
        counters,
    })
}

#[tokio::test]
async fn activation_preserves_sql_other_applications_and_running_release() -> anyhow::Result<()> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let scratch = ScratchRoot::create()?;
    let mut objects = BTreeMap::new();
    let old_a = component(&mut objects, "a", &["upstream"])?;
    let b = component(&mut objects, "b", &["upstream"])?;
    let base_package = package(
        scratch.path(),
        &mut objects,
        "wamn_integration",
        None,
        "1.0.0",
        "",
    )?;
    let old_package = package(scratch.path(), &mut objects, "a", Some(&old_a), "1.0.0", "")?;
    let b_package = package(scratch.path(), &mut objects, "b", Some(&b), "1.0.0", "")?;
    let old_a = release(&mut objects, old_a, "1.0.0")?;
    let b = release(&mut objects, b, "1.0.0")?;
    let new_component = component(&mut objects, "a", &["upstream", "extra"])?;
    let new_a = release(&mut objects, new_component, "1.0.0")?;
    let failed_component = component(&mut objects, "a", &["upstream", "extra", "failed"])?;
    let failed_release = release(&mut objects, failed_component, "1.0.0")?;
    let mut missing_objects = objects.clone();
    missing_objects.remove("/v2/packages/manifests/b-1.0.0");
    let missing_root = scratch.path().join("missing-registry");
    std::fs::create_dir(&missing_root)?;
    let missing_registry = Registry::start(&missing_root, missing_objects).await?;
    let registry = Registry::start(scratch.path(), objects).await?;
    let mut postgres = wamn_test_postgres::start(&[("wal_level", "logical")])?;
    let system = postgres.create_database("wamn_system")?;
    let admin_url = url::Url::parse(system.url())?;
    let mut admin = connect(system.url()).await?;
    // CNPG supplies this database owner before the platform SQL operation.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_system NOLOGIN NOINHERIT; ALTER DATABASE wamn_system OWNER TO wamn_system")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    wamn_environment::provision_system::provision_system(
        &wamn_environment::provision_system::ProvisionSystemRequest {
            migration_database_url: Some(
                wamn_control_provision::test_database::platform_migration_input(
                    &system.url(),
                    wamn_control_provision::migrations::MigrationDatabase::System,
                )
                .await
                .unwrap(),
            ),
            system_database_url: system.url().into(),
            platform_domain: "example.invalid".into(),
        },
    )
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::grant_control_surface_sql()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; SET LOCAL ROLE wamn_system; {} INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT;", wamn_control_provision::test_database::seed_org_project_env_sql("acme","apps","dev")))).execute(&mut admin).await.map(drop)?;
    let apply_input = wamn_test_infrastructure::credentials::apply_login(system.url()).await?;
    let mut platform = Platform {
        system_database_url: apply_input,
        target: wamn_release_control::release_chart::Target {
            kubeconfig: PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: PathBuf::new(),
        chart_download: None,
        release_artifact_base: format!("{}/releases", registry.address),
        registry_token_metadata: false,
        registry_auth_file: registry.auth.clone(),
        oci_ca_paths: Vec::new(),
        insecure_registry: true,
        package_artifact_base: Some(format!("{}/packages", registry.address)),
        component_artifact_base: Some(format!("{}/components", registry.address)),
        database_namespace: "platform".into(),
        database_host: None,
        database_port: admin_url.port().unwrap(),
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    let mut document = document(&old_a, &b)?;
    let provisioned = wamn_environment::environment::ensure::provision(
        &platform,
        &document,
        &document.policy,
        system.url(),
        &scratch.path().join("database.json"),
        None,
    )
    .await?;
    let database_name = provisioned.database;
    // CNPG creates this database outside the SQL operation in deployed environments.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&database_name)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;

    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
    let mut tx = admin.begin().await?;
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)?;
    wamn_test_infrastructure::credentials::define(&mut tx, "acme", &json!({"project":"apps","env":"dev","name":"erp","integration_type":"http","auth_type":"none","parameters":{"primary-authority":"https://erp.example/","tls-verification":"verify-authority"}}), &store).await?;
    tx.commit().await?;
    sql::initialize(&platform, &document, system.url(), Some("a"))
        .await
        .context("initialize a")?;
    assert!(
        expand::prepare(
            &platform,
            &document,
            "b",
            b.digest().as_str(),
            &b,
            &[b_package.clone(), base_package.clone()],
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await
        .is_err()
    );
    // Inspect and install B's SQL before selecting or activating B's code.
    let b_current = std::mem::replace(
        &mut document.applications.get_mut("b").unwrap().current,
        wamn_environment::environment::document::DeclaredRelease::None,
    );
    let (candidate_release, candidates) =
        sql::schema::release_candidates(&platform, &document, "b", Some(b.digest().as_str()))
            .await?;
    let state = sql::schema::state(&platform, &document).await?;
    let diff =
        sql::schema::plan(&document, "b", Some(candidate_release), &candidates, &state).await?;
    assert!(diff.packages["b"].installed.is_none());
    assert_eq!(
        sql::schema::state(&platform, &document).await?.packages,
        state.packages
    );
    // Published SQL remains executable after its source directory disappears.
    std::fs::remove_dir_all(scratch.path().join("b"))?;
    sql::schema::apply(&platform, &document, "b", b.digest().as_str()).await?;
    let installed = sql::schema::state(&platform, &document).await?;
    for (id, package) in &diff.packages {
        assert_eq!(
            installed.packages[id].artifact_digest,
            package.source_artifact
        );
        assert_eq!(
            installed.packages[id].coordinate.package_version(),
            package.candidate
        );
    }
    assert_eq!(installed.packages["a"], state.packages["a"]);
    document.applications.get_mut("b").unwrap().current = b_current;
    let mut observer_url = admin_url;
    observer_url.set_path(&database_name);
    let mut observer = connect(observer_url.as_str()).await?;
    // Explicit SQL application grants the new package and retains the existing one.
    for package in [&old_package, &b_package] {
        for model in package.models.values() {
            let relation = format!("{}.{}", model.schema, model.table);
            assert!(
                sqlx::query("SELECT has_column_privilege('wamn_app', $1, 'value', 'SELECT')")
                    .bind(&relation)
                    .fetch_one(&mut observer)
                    .await?
                    .get::<bool, _>(0)
            );
        }
    }

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::ExecutorPlatform,
        ))
            .to_string(),
    ))
    .execute(&mut observer)
    .await
    .map(drop)?;
    // Initialization extends capture from all installed baselines, even when the
    // caller supplies only B's selected document.
    let selected_b =
        wamn_environment::environment::analyse::select_application(&document, Some("b"))?;
    let captured = ensure::installed_manifests(&platform, &selected_b).await?;
    let schemas = captured
        .iter()
        .flat_map(|package| package.models.values().map(|model| model.schema.as_str()))
        .collect::<BTreeSet<_>>();
    let installed_schemas = sqlx::query("SELECT nspname::text FROM pg_namespace n JOIN pg_roles r ON r.oid=n.nspowner WHERE r.rolname LIKE 'wamn_migration_%' AND $1::text IS NOT NULL").bind(&PROJECT_ENV_KEY).fetch_all(&mut observer).await?.into_iter().map(|row| row.get::<String, _>(0)).collect::<BTreeSet<_>>();
    assert_eq!(
        schemas,
        installed_schemas.iter().map(String::as_str).collect()
    );
    let old_packages = [old_package.clone(), base_package.clone()];
    expand::prepare(
        &platform,
        &document,
        "a",
        old_a.digest().as_str(),
        &old_a,
        &old_packages,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    expand::prepare(
        &platform,
        &document,
        "b",
        b.digest().as_str(),
        &b,
        &[b_package, base_package.clone()],
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    for (target_type, target_id, seconds) in [
        (
            "project",
            document.project.as_str(),
            document.revocation_seconds,
        ),
        (
            "application",
            "a",
            document.applications["a"].revocation_seconds,
        ),
        (
            "application",
            "b",
            document.applications["b"].revocation_seconds,
        ),
    ] {
        let row = sqlx::query(
            "SELECT value FROM platform.setting WHERE target_type=$1 AND target_id=$2 AND key='revocation-seconds'",
        ).bind(target_type).bind(target_id).fetch_one(&mut observer).await?;
        assert_eq!(row.get::<serde_json::Value, _>(0).as_u64(), Some(seconds));
    }
    sqlx::raw_sql("INSERT INTO a.data(value) VALUES ('authored'); INSERT INTO b.data(value) VALUES ('other');").execute(&mut observer).await.map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "BEGIN; {} SELECT platform.bump_counter(id, 'number', '') FROM platform.number_sequence; COMMIT;",
        wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning),
    ))).execute(&mut observer).await.map(drop)?;
    let before_sql = continuity(&mut observer).await?;
    sql::initialize(&platform, &document, system.url(), Some("a"))
        .await
        .context("retry initialization")?;

    assert_eq!(before_sql, continuity(&mut observer).await?);
    let principal = |name: &str| wamn_platform_identity::application_subject(PROJECT_ENV_KEY, name);
    let a_user = wamn_platform_identity::resolve_subject(
        &mut admin,
        wamn_platform_identity::PrincipalType::Service,
        &principal("a")?,
    )
    .await?
    .unwrap();
    let b_user = wamn_platform_identity::resolve_subject(
        &mut admin,
        wamn_platform_identity::PrincipalType::Service,
        &principal("b")?,
    )
    .await?
    .unwrap();
    let mounted = wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
        &old_a.canonical_bytes(),
        "running application a",
        wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, "dev")
            .with_applications(
                wamn_environment::environment::analyse::select_application(&document, Some("a"))?
                    .membership(),
            ),
    )?;
    let b_permissions = permissions(&mut observer, b_user.id().to_string().as_str()).await?;
    document.applications.get_mut("a").unwrap().current =
        wamn_environment::environment::document::DeclaredRelease::Digest(
            new_a.digest().as_str().into(),
        );
    document
        .applications
        .get_mut("a")
        .unwrap()
        .connections
        .insert("extra".into(), "erp".into());
    let mut candidate = old_package;
    candidate
        .connections
        .insert("extra".into(), candidate.connections["upstream"].clone());
    candidate
        .base_dependencies
        .get_mut("state")
        .unwrap()
        .operations = vec!["data.update".into()];
    let candidate_packages = [candidate, base_package];
    expand::prepare(
        &platform,
        &document,
        "a",
        new_a.digest().as_str(),
        &new_a,
        &candidate_packages,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    expand::prepare(
        &platform,
        &document,
        "a",
        new_a.digest().as_str(),
        &new_a,
        &candidate_packages,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;

    assert_eq!(before_sql, continuity(&mut observer).await?);
    let b_after = permissions(&mut observer, b_user.id().to_string().as_str()).await?;
    for operation in candidate_packages[1]
        .models
        .values()
        .flat_map(|model| model.operations.values())
    {
        let token = wamn_compiler::manifest::canonical_operation_identity(
            &candidate_packages[1].package,
            &operation.permission,
        )?;
        assert_eq!(
            b_permissions.contains(token.rsplit_once("@").unwrap().0),
            b_after.contains(token.rsplit_once("@").unwrap().0)
        );
    }
    for release in [&old_a, &new_a] {
        for component in &release.components {
            for connection in &component.descriptor.connections {
                assert!(sqlx::query("SELECT 1 FROM catalog.connection_binding WHERE project_env_key=$1 AND manifest_digest=$2 AND component_digest=$3 AND package_id=$4 AND name=$5").bind(&PROJECT_ENV_KEY).bind(&release.digest().as_str()).bind(&component.descriptor.component_digest).bind(&component.package_id).bind(connection.store_alias.name().expect("fixture connection has a literal alias")).fetch_optional(&mut observer).await?.is_some());
            }
        }
    }
    document.applications.get_mut("a").unwrap().current =
        wamn_environment::environment::document::DeclaredRelease::Digest(
            failed_release.digest().as_str().into(),
        );
    document
        .applications
        .get_mut("a")
        .unwrap()
        .connections
        .insert("failed".into(), "erp".into());
    let mut failed_packages = candidate_packages.clone();
    let declaration = failed_packages[0].connections["upstream"].clone();
    failed_packages[0]
        .connections
        .insert("failed".into(), declaration);
    // A shared generated role refuses before changing another principal's authority.
    let mut project = connect(observer_url.as_str()).await?;
    let mut tx = project.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)?;
    wamn_platform_identity::application::mutate_grant(
        &mut tx,
        PROJECT_ENV_KEY,
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::UserRole {
            user_id: b_user.id().to_string().as_str(),
            role: "application-a",
            grant: true,
        },
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;
    tx.commit().await?;
    assert!(
        expand::prepare(
            &platform,
            &document,
            "a",
            failed_release.digest().as_str(),
            &failed_release,
            &failed_packages,
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await
        .is_err()
    );
    assert!(
        sqlx::query("SELECT 1 FROM catalog.connection_requirement WHERE component_digest=$1")
            .bind(
                &failed_release
                    .components
                    .first()
                    .unwrap()
                    .descriptor
                    .component_digest
            )
            .fetch_optional(&mut observer)
            .await?
            .is_none()
    );

    assert_eq!(before_sql, continuity(&mut observer).await?);
    // The failed candidate remains cached, but the runtime resolves the mounted digest.
    let resolved = Arc::clone(mounted.components());
    assert_eq!(
        resolved
            .iter()
            .map(|component| component.component_digest.as_str())
            .collect::<BTreeSet<_>>(),
        old_a
            .components
            .iter()
            .map(|component| component.descriptor.component_digest.as_str())
            .collect(),
    );
    assert!(
        sqlx::query(
            "SELECT 1 FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest=$2"
        )
        .bind(&PROJECT_ENV_KEY)
        .bind(&failed_release.digest().as_str())
        .fetch_optional(&mut observer)
        .await?
        .is_some()
    );
    drop(postgres);
    let caller = permissions(&mut observer, a_user.id().to_string().as_str()).await?;
    assert!(caller.contains("wamn-integration:data/get"));
    assert!(caller.contains("wamn-integration:data/update"));
    let connection = &old_a.components.first().unwrap().descriptor;
    assert!(sqlx::query("SELECT 1 FROM catalog.connection_binding WHERE manifest_digest=$1 AND component_digest=$2 AND name='upstream'").bind(&old_a.digest().as_str()).bind(&connection.component_digest).fetch_optional(&mut observer).await?.is_some());
    // Retention uses the production owner role and retains live releases and unfinished runs.
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut project)
        .await
        .map(drop)?;
    let releases = BTreeMap::from([
        (
            old_a.digest().to_string(),
            (old_a.clone(), old_packages.to_vec()),
        ),
        (
            new_a.digest().to_string(),
            (new_a.clone(), candidate_packages.to_vec()),
        ),
    ]);
    let mut tx = project.begin().await?;
    sqlx::query(wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL)
        .bind(&PROJECT_ENV_KEY)
        .fetch_one(&mut *tx)
        .await?;
    let retained: BTreeSet<String> = sqlx::query_scalar("SELECT manifest_digest FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest<>$2")
        .bind(PROJECT_ENV_KEY).bind(old_a.digest().as_str()).fetch_all(&mut *tx).await?.into_iter().collect();
    let retired = wamn_environment::environment::retention::prune_releases(
        &mut tx,
        PROJECT_ENV_KEY,
        &retained,
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
    )
    .await?;
    assert!(
        wamn_environment::environment::retention::retire_runtime_grants(
            &mut admin,
            &mut tx,
            PROJECT_ENV_KEY,
            &releases,
            &retired,
            &mut wamn_broker::auth_changes::AuthDrops::default()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert!(
        sqlx::query("SELECT 1 FROM catalog.connection_binding WHERE project_env_key=$1 AND manifest_digest=$2").bind(&PROJECT_ENV_KEY).bind(&old_a.digest().as_str()).fetch_optional(&mut observer)
            .await?
            .is_some()
    );
    sqlx::query("INSERT INTO wamn_run.runs (project_env_key, run_id, package_id, environment, status, manifest_digest, wiring_id, wiring_version, wiring_hash, trigger_source, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json) VALUES ($1, 'old-work', 'a', 'dev', 'dispatched', $2, 'work', 1, $2, 'manual', '00000000-0000-0000-0000-000000000001', 'admin', 'manual', '00000000-0000-0000-0000-000000000001', '{}')").bind(&PROJECT_ENV_KEY).bind(&old_a.digest().as_str()).execute(&mut observer).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO wamn_run.runs (project_env_key, run_id, package_id, environment, status, manifest_digest, wiring_id, wiring_version, wiring_hash, trigger_source, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json) VALUES ($1, 'b-work', 'b', 'dev', 'dispatched', $2, 'work', 1, $2, 'manual', '00000000-0000-0000-0000-000000000001', 'admin', 'manual', '00000000-0000-0000-0000-000000000001', '{}')").bind(&PROJECT_ENV_KEY).bind(&b.digest().as_str()).execute(&mut observer).await.map(|result| result.rows_affected())?;
    for (live, pinned) in [(true, true), (false, true), (false, false)] {
        if !pinned {
            sqlx::query("UPDATE wamn_run.runs SET status='completed' WHERE run_id='old-work'")
                .execute(&mut observer)
                .await
                .map(|result| result.rows_affected())?;
        }
        let mut tx = project.begin().await?;
        sqlx::query(wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL)
            .bind(&PROJECT_ENV_KEY)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *tx)
        .await
        .map(drop)?;
        let mut auth = wamn_broker::auth_changes::AuthDrops::default();
        // The failed replacement left a shared role. Restore its exclusive owner before retirement.
        if live {
            wamn_platform_identity::application::mutate_grant(
                &mut tx,
                PROJECT_ENV_KEY,
                wamn_platform_identity::application::GrantAuthority::Provisioning,
                wamn_platform_identity::application::GrantChange::UserRole {
                    user_id: b_user.id().to_string().as_str(),
                    role: "application-a",
                    grant: false,
                },
                &mut auth,
            )
            .await?;
        }
        let mut retained: BTreeSet<String> = sqlx::query_scalar("SELECT manifest_digest FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest<>$2")
            .bind(PROJECT_ENV_KEY).bind(old_a.digest().as_str()).fetch_all(&mut *tx).await?.into_iter().collect();
        if live {
            retained.insert(old_a.digest().to_string());
        }
        let retired = wamn_environment::environment::retention::prune_releases(
            &mut tx,
            PROJECT_ENV_KEY,
            &retained,
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        )
        .await?;
        wamn_environment::environment::retention::retire_runtime_grants(
            &mut admin,
            &mut tx,
            PROJECT_ENV_KEY,
            &releases,
            &retired,
            &mut auth,
        )
        .await?;
        tx.commit().await?;
        assert_eq!(retired.is_empty(), live || pinned);
        let stranded = wamn_environment::environment::drain::stranded(
            observer_url.as_str(),
            PROJECT_ENV_KEY,
            "dev",
            Some(&["a".to_owned()]),
            &BTreeMap::from([(new_a.digest().to_string(), 1)]),
        )
        .await?;
        assert_eq!(
            stranded
                .iter()
                .map(|run| run.run_id.as_str())
                .collect::<Vec<_>>(),
            if pinned { vec!["old-work"] } else { vec![] }
        );
        let caller = permissions(&mut observer, a_user.id().to_string().as_str()).await?;
        assert_eq!(caller.contains("wamn-integration:data/get"), live || pinned);
        assert!(caller.contains("wamn-integration:data/update"));
        let b_after = permissions(&mut observer, b_user.id().to_string().as_str()).await?;
        for operation in candidate_packages[1]
            .models
            .values()
            .flat_map(|model| model.operations.values())
        {
            let token = wamn_compiler::manifest::canonical_operation_identity(
                &candidate_packages[1].package,
                &operation.permission,
            )?;
            assert_eq!(
                b_permissions.contains(token.rsplit_once("@").unwrap().0),
                b_after.contains(token.rsplit_once("@").unwrap().0)
            );
        }

        assert_eq!(before_sql, continuity(&mut observer).await?);
    }
    // A rollback recreates A0's retired bindings and permissions before code can start.
    document.applications.get_mut("a").unwrap().current =
        wamn_environment::environment::document::DeclaredRelease::Digest(
            old_a.digest().to_string(),
        );
    document
        .applications
        .get_mut("a")
        .unwrap()
        .connections
        .remove("extra");
    document
        .applications
        .get_mut("a")
        .unwrap()
        .connections
        .remove("failed");
    expand::prepare(
        &platform,
        &document,
        "a",
        old_a.digest().as_str(),
        &old_a,
        &old_packages,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    assert!(
        sqlx::query("SELECT 1 FROM catalog.connection_binding WHERE project_env_key=$1 AND manifest_digest=$2").bind(&PROJECT_ENV_KEY).bind(&old_a.digest().as_str()).fetch_optional(&mut observer)
            .await?
            .is_some()
    );
    let caller = permissions(&mut observer, a_user.id().to_string().as_str()).await?;
    assert!(caller.contains("wamn-integration:data/get"));

    assert_eq!(before_sql, continuity(&mut observer).await?);
    Ok(())
}

async fn permissions(
    client: &mut sqlx::PgConnection,
    principal: &str,
) -> anyhow::Result<std::collections::BTreeSet<String>> {
    Ok(sqlx::query("SELECT p.permission FROM platform.user_role r JOIN platform.permission p USING(role_name) WHERE r.user_id=$1::text::uuid")
        .bind(principal).fetch_all(client).await?.into_iter().map(|row| row.get(0)).collect())
}
