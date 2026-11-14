//! Application preparation preserves SQL, other applications, and running release authority.

use sqlx::{Connection as _, Row as _};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wamn_catalog::{ComponentDescriptor, PackageCoordinate, ServingComponent, ServingManifest};
use wamn_compiler::manifest::PackageManifest;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentityType};
use wamn_digest::bytes_sha256;
use wamn_environment_control::environment::{
    Platform, document::EnvironmentDocument, ensure, expand, sql,
};

use wamn_environment_control::provision_project_env::{finish_database, require_connected};
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
            value["connections"][&connection.store_alias] =
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
    EnvironmentDocument::from_json(&serde_json::to_vec(
        &json!({"org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,"integrations":{"erp":{"version":1}},"applications":{
            "a":{"current":a.digest().as_str(),"revocation_seconds":5,"hostname":"a.example","packages":["a","wamn_integration"],"connections":{"upstream":"erp"}},
            "b":{"current":b.digest().as_str(),"revocation_seconds":5,"hostname":"b.example","packages":["b","wamn_integration"],"connections":{"upstream":"erp"}}
        }}),
    )?)
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
    wamn_environment_control::provision_system::provision_system(
        &wamn_environment_control::provision_system::ProvisionSystemRequest {
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
    let provisioned = wamn_environment_control::environment::ensure::provision(
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
        wamn_environment_control::environment::document::DeclaredRelease::None,
    );
    let (candidate_release, candidates) =
        sql::schema::release_candidates(&platform, &document, "b", Some(b.digest().as_str()))
            .await?;
    let state = sql::schema::state(&platform, &document).await?;
    let diff = sql::schema::plan(&document, "b", Some(candidate_release), &candidates, &state)?;
    assert_eq!(
        diff.database_instance.as_deref(),
        Some(database_name.as_str())
    );
    assert!(diff.packages["b"].installed.is_none());
    assert_eq!(
        sql::schema::state(&platform, &document).await?.packages,
        state.packages
    );
    // Published SQL remains executable after its source directory disappears.
    std::fs::remove_dir_all(scratch.path().join("b"))?;
    sql::schema::apply(
        &platform,
        &document,
        "b",
        b.digest().as_str(),
        system.url(),
        None,
    )
    .await?;
    let installed = sql::schema::state(&platform, &document).await?;
    for (id, package) in &diff.packages {
        assert_eq!(
            installed.packages[id].artifact_digest,
            package.source_artifact
        );
        assert_eq!(
            installed.packages[id].physical_schema_fingerprint,
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
        wamn_environment_control::environment::analyse::select_application(&document, Some("b"))?;
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
                wamn_environment_control::environment::analyse::select_application(
                    &document,
                    Some("a"),
                )?
                .membership(),
            ),
    )?;
    let b_permissions = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &mut observer,
        b_user.id().to_string().as_str(),
        None,
    )
    .await?;
    document.applications.get_mut("a").unwrap().current =
        wamn_environment_control::environment::document::DeclaredRelease::Digest(
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
    let b_after = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &mut observer,
        b_user.id().to_string().as_str(),
        None,
    )
    .await?;
    for operation in candidate_packages[1]
        .models
        .values()
        .flat_map(|model| model.operations.values())
    {
        let token = wamn_compiler::manifest::canonical_operation_identity(
            &candidate_packages[1].package,
            &operation.permission,
        )?;
        assert_eq!(b_permissions.permits(&token), b_after.permits(&token));
    }
    for release in [&old_a, &new_a] {
        for component in &release.components {
            for connection in &component.descriptor.connections {
                assert!(sqlx::query("SELECT 1 FROM catalog.connection_binding WHERE project_env_key=$1 AND manifest_digest=$2 AND component_digest=$3 AND package_id=$4 AND name=$5").bind(&PROJECT_ENV_KEY).bind(&release.digest().as_str()).bind(&component.descriptor.component_digest).bind(&component.package_id).bind(&connection.store_alias).fetch_optional(&mut observer).await?.is_some());
            }
        }
    }
    document.applications.get_mut("a").unwrap().current =
        wamn_environment_control::environment::document::DeclaredRelease::Digest(
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
    let postgres = wamn_runtime::plugins::wamn_postgres::WamnPostgres::new(
        wamn_runtime::plugins::wamn_postgres::WamnPostgresConfig {
            credentials: Some(
                wamn_runtime::plugins::wamn_postgres::ClassCredentials::every_class(
                    observer_url.to_string(),
                ),
            ),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
            row_limit: 100,
        },
    )?;
    let resolved = postgres
        .resolve_release_components(
            wamn_runtime::plugins::wamn_postgres::DEFAULT_PROJECT,
            PROJECT_ENV_KEY,
            "dev",
            mounted.release().manifest_digest.as_str(),
        )
        .await?;
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
    let caller = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &mut observer,
        a_user.id().to_string().as_str(),
        None,
    )
    .await?;
    wamn_engine::router_delivery::authorize_registered_operation(
        Some(&caller),
        Some("wamn-integration:data/get@1.0.0"),
    )?;
    wamn_engine::router_delivery::authorize_registered_operation(
        Some(&caller),
        Some("wamn-integration:data/update@1.0.0"),
    )?;
    let connection = &old_a.components.first().unwrap().descriptor;
    assert!(sqlx::query("SELECT 1 FROM catalog.connection_binding WHERE manifest_digest=$1 AND component_digest=$2 AND name='upstream'").bind(&old_a.digest().as_str()).bind(&connection.component_digest).fetch_optional(&mut observer).await?.is_some());
    // Retirement uses the production owner role, its lock, and explicit application digests.
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
    assert!(
        wamn_environment_control::environment::bindings::contract(
            &mut tx,
            PROJECT_ENV_KEY,
            "a",
            &document.membership(),
            &[b.digest().as_str()],
            &[],
            &mut wamn_broker::connection_changes::ConnectionDrops::default()
        )
        .await
        .is_err()
    );
    let retired = wamn_environment_control::environment::bindings::contract(
        &mut tx,
        PROJECT_ENV_KEY,
        "a",
        &document.membership(),
        &[old_a.digest().as_str()],
        &[new_a.digest().as_str()],
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
    )
    .await?;
    assert!(
        wamn_environment_control::environment::stage::retire_runtime_grants(
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
    sqlx::query("INSERT INTO wamn_run.runs(project_env_key,run_id,package_id,environment,status,manifest_digest,wiring_id,wiring_version,wiring_hash,binding_world_json) VALUES($1,'old-work','a','dev','dispatched',$2,'work',1,$2,'[]')").bind(&PROJECT_ENV_KEY).bind(&old_a.digest().as_str()).execute(&mut observer).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO wamn_run.runs(project_env_key,run_id,package_id,environment,status,manifest_digest,wiring_id,wiring_version,wiring_hash,binding_world_json) VALUES($1,'b-work','b','dev','dispatched',$2,'work',1,$2,'[]')").bind(&PROJECT_ENV_KEY).bind(&b.digest().as_str()).execute(&mut observer).await.map(|result| result.rows_affected())?;
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
        let old_digest = old_a.digest();
        let new_digest = new_a.digest();
        let live_digests = if live {
            vec![old_digest.as_str(), new_digest.as_str()]
        } else {
            vec![new_digest.as_str()]
        };
        let retired = wamn_environment_control::environment::bindings::contract(
            &mut tx,
            PROJECT_ENV_KEY,
            "a",
            &document.membership(),
            &[old_digest.as_str()],
            &live_digests,
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        )
        .await?;
        wamn_environment_control::environment::stage::retire_runtime_grants(
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
        let stranded = wamn_environment_control::environment::drain::stranded(
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
        let caller = wamn_runtime::plugins::route_authentication::queued_run_caller(
            &mut observer,
            a_user.id().to_string().as_str(),
            None,
        )
        .await?;
        assert_eq!(
            caller.permits("wamn-integration:data/get@1.0.0"),
            live || pinned
        );
        assert!(caller.permits("wamn-integration:data/update@1.0.0"));
        let b_after = wamn_runtime::plugins::route_authentication::queued_run_caller(
            &mut observer,
            b_user.id().to_string().as_str(),
            None,
        )
        .await?;
        for operation in candidate_packages[1]
            .models
            .values()
            .flat_map(|model| model.operations.values())
        {
            let token = wamn_compiler::manifest::canonical_operation_identity(
                &candidate_packages[1].package,
                &operation.permission,
            )?;
            assert_eq!(b_permissions.permits(&token), b_after.permits(&token));
        }

        assert_eq!(before_sql, continuity(&mut observer).await?);
    }
    // A rollback recreates A0's retired bindings and permissions before code can start.
    document.applications.get_mut("a").unwrap().current =
        wamn_environment_control::environment::document::DeclaredRelease::Digest(
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
    let caller = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &mut observer,
        a_user.id().to_string().as_str(),
        None,
    )
    .await?;
    assert!(caller.permits("wamn-integration:data/get@1.0.0"));

    assert_eq!(before_sql, continuity(&mut observer).await?);
    // Preparation never changes active SQL or release selections.
    document.applications.get_mut("a").unwrap().current =
        wamn_environment_control::environment::document::DeclaredRelease::Digest(
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
    assert!(
        sql::reset::prepare(
            &platform,
            &document,
            "a",
            old_a.digest().as_str(),
            system.url()
        )
        .await
        .is_err()
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "BEGIN; SET LOCAL ROLE wamn_system; {} UPDATE registry.environments SET disposable=true WHERE org='acme' AND project='apps' AND env='dev'; COMMIT;",
        wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning),
    ))).execute(&mut admin).await.map(drop)?;
    let mut unavailable = platform.clone();
    unavailable.package_artifact_base = Some(format!("{}/packages", missing_registry.address));
    unavailable.registry_auth_file = missing_registry.auth.clone();
    // Alpha resolves, but beta's retained SQL is unavailable. No target is allocated.
    assert!(
        sql::reset::prepare(
            &unavailable,
            &document,
            "a",
            old_a.digest().as_str(),
            system.url()
        )
        .await
        .is_err()
    );
    assert!(sqlx::query("SELECT pending_database IS NULL FROM registry.environment_projects WHERE org='acme' AND project='apps' AND env='dev'").fetch_one(&mut admin).await?.get::<bool, _>(0));

    assert_eq!(before_sql, continuity(&mut observer).await?);
    let mut omitted = document.clone();
    omitted.applications.remove("b");
    assert!(
        sql::reset::prepare_retained(&platform, &omitted, system.url())
            .await
            .is_err()
    );
    sql::reset::prepare(
        &platform,
        &document,
        "a",
        old_a.digest().as_str(),
        system.url(),
    )
    .await?;
    let pending: serde_json::Value = sqlx::query("SELECT pending_database FROM registry.environment_projects WHERE org='acme' AND project='apps' AND env='dev'").fetch_one(&mut admin).await?.get(0);
    let mut changed_policy = document.clone();
    changed_policy.policy = "unprovisioned-policy".into();
    sql::reset::prepare(
        &platform,
        &changed_policy,
        "a",
        old_a.digest().as_str(),
        system.url(),
    )
    .await?;
    let retried: serde_json::Value = sqlx::query("SELECT pending_database FROM registry.environment_projects WHERE org='acme' AND project='apps' AND env='dev'").fetch_one(&mut admin).await?.get(0);
    assert_eq!(pending, retried);
    let installed_before = sql::schema::state(&platform, &document).await?;
    let old_identity = installed_before.initialization_identity;
    let expected_packages = installed_before
        .packages
        .into_keys()
        .collect::<BTreeSet<_>>();
    let expected = sql::ExpectedDatabase::read(&platform, &document).await?;
    // Keep the product-issued credentials across CNPG's same-name recreation.
    let grant = |row: sqlx::postgres::PgRow| -> (String, String, String, String) {
        (
            row.get("object_type"),
            row.get("schema_name"),
            row.get("object_name"),
            row.get("privilege_type"),
        )
    };
    let mut credential_tests = Vec::new();
    for family in WorkloadRoleFamily::project_env_families()
        .filter(|family| family.identity_type() != WorkloadRoleIdentityType::Control)
    {
        let mut project_admin = url::Url::parse(system.url())?;
        project_admin.set_path(&database_name);
        let credential_url = wamn_test_infrastructure::credentials::database_login(
            system.url(),
            Some(project_admin.as_str()),
            &document.org,
            &document.project,
            &document.env,
            family,
        )
        .await?;
        let grants = sqlx::query(sqlx::AssertSqlSafe(
            (wamn_control_provision::sql::role_database_grants_sql()).to_string(),
        ))
        .bind(&family.acl_role())
        .fetch_all(&mut observer)
        .await?
        .into_iter()
        .map(grant)
        .collect::<BTreeSet<_>>();
        credential_tests.push((credential_url, family, grants));
    }
    assert!(
        sql::reset::initialize(&platform, &document, system.url())
            .await
            .is_err()
    );
    assert!(sql::reset::needs_recreation(&platform, &document).await?);
    // PostgreSQL observes reconstruction. DEP3-11 observes CNPG absent/present itself.
    drop(project);
    drop(observer);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP DATABASE {} WITH (FORCE)",
            postgres_protocol::escape::escape_identifier(&database_name),
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
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
    let mut project_admin_url = url::Url::parse(system.url())?;
    project_admin_url.set_path(&database_name);
    let mut candidate_admin = connect(project_admin_url.as_str()).await?;
    sqlx::raw_sql("CREATE FUNCTION public.interrupt_beta() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN IF EXISTS (SELECT FROM pg_event_trigger_ddl_commands() WHERE object_identity='b.data' AND command_tag='CREATE TABLE') THEN RAISE EXCEPTION 'beta initialization interrupted'; END IF; END $$; CREATE EVENT TRIGGER interrupt_beta ON ddl_command_end EXECUTE FUNCTION public.interrupt_beta()").execute(&mut candidate_admin).await.map(drop)?;
    assert!(
        sql::reset::initialize(&platform, &document, system.url())
            .await
            .is_err()
    );
    assert!(!sql::reset::needs_recreation(&platform, &document).await?);
    // A committed earlier package survives retry, including rows inserted after it committed.
    sqlx::raw_sql("DROP EVENT TRIGGER interrupt_beta; INSERT INTO a.data(id,value) VALUES (123,'committed-before-retry')").execute(&mut candidate_admin).await.map(drop)?;
    sql::reset::initialize(&platform, &document, system.url()).await?;
    sql::reset::initialize(&platform, &document, system.url()).await?;
    // SQL restoration uses the installed substrate image. Credential grants
    // belong to reconnect and cannot depend on that image's control code.
    for (url, _, _) in &credential_tests {
        let error = connect(url)
            .await
            .err()
            .context("credential still needs reconnection")?;
        assert_eq!(
            error
                .downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error)
                .and_then(|error| error.code())
                .as_deref(),
            Some("42501")
        );
    }
    ensure::restore_project_database_access(&platform, &document, project_admin_url.as_str())
        .await?;
    for (url, family, expected_grants) in &credential_tests {
        let mut workload = connect(url).await?;
        assert!(
            !sqlx::query("SELECT rolsuper FROM pg_roles WHERE rolname=current_user")
                .fetch_one(&mut workload)
                .await?
                .get::<bool, _>(0)
        );
        let grants = sqlx::query(sqlx::AssertSqlSafe(
            (wamn_control_provision::sql::role_database_grants_sql()).to_string(),
        ))
        .bind(&family.acl_role())
        .fetch_all(&mut workload)
        .await?
        .into_iter()
        .map(grant)
        .collect::<BTreeSet<_>>();
        assert_eq!(&grants, expected_grants);
    }
    assert_eq!(
        sqlx::query("SELECT value FROM a.data WHERE id=123")
            .fetch_one(&mut candidate_admin)
            .await?
            .get::<String, _>(0),
        "committed-before-retry"
    );
    let installed = sqlx::query("SELECT package_id FROM catalog.packages WHERE project_env_key=$1")
        .bind(&PROJECT_ENV_KEY)
        .fetch_all(&mut candidate_admin)
        .await?
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect::<BTreeSet<_>>();
    assert_eq!(installed, expected_packages);
    let state = sql::schema::state(&platform, &document).await?;
    assert_eq!(
        state.database_instance.as_deref(),
        Some(database_name.as_str())
    );
    assert_ne!(state.initialization_identity, old_identity);
    assert_eq!(
        state.initialization_identity.as_deref(),
        pending["initialization_identity"].as_str()
    );
    let mut system_writer = connect(&platform.system_database_url).await?;
    let role =
        sqlx::query("SELECT current_user, rolsuper FROM pg_roles WHERE rolname=current_user")
            .fetch_one(&mut system_writer)
            .await?;
    assert_eq!(role.get::<String, _>(0), "wamn_apply");
    assert!(!role.get::<bool, _>(1));
    assert!(
        !sqlx::query("SELECT has_table_privilege(current_user, 'registry.capture_gap', 'SELECT')")
            .fetch_one(&mut system_writer)
            .await?
            .get::<bool, _>(0)
    );
    let capture = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &provisioned.instance,
    );
    let manifests = ensure::installed_manifests(&platform, &document).await?;
    let schemas = manifests
        .iter()
        .flat_map(|manifest| manifest.models.values().map(|model| model.schema.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    wamn_environment_control::enable_cdc_project_env::record_event_reader(
        &platform.system_database_url,
        &document.triple(),
        &schemas,
        &capture,
        None,
    )
    .await?;
    // The existing recovery owner connects through the administrator, then
    // executes registry reads and writes as the non-superuser wamn_system owner.
    sql::reset::reconnect_capture_gap(&platform, &document, system.url()).await?;
    let mut capture_owner = connect(system.url()).await?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut capture_owner)
        .await
        .map(drop)?;
    let role =
        sqlx::query("SELECT current_user, rolsuper FROM pg_roles WHERE rolname=current_user")
            .fetch_one(&mut capture_owner)
            .await?;
    assert_eq!(role.get::<String, _>(0), "wamn_system");
    assert!(!role.get::<bool, _>(1));
    let mut tx = wamn_environment_control::provision_project_env::provisioning_transaction(
        &mut capture_owner,
    )
    .await?;
    let start: chrono::DateTime<chrono::Utc> = sqlx::query("SELECT now()")
        .fetch_one(&mut *tx)
        .await?
        .get(0);
    sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::insert_capture_gap_sql()).to_string(),
    ))
    .bind(&document.org)
    .bind(&document.project)
    .bind(&document.env)
    .bind(&capture)
    .bind(&Option::<String>::None)
    .bind(&start)
    .bind(&"missing")
    .bind(&"0/0")
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    sql::reset::reconnect_capture_gap(&platform, &document, system.url()).await?;
    sql::reset::reconnect_capture_gap(&platform, &document, system.url()).await?;
    assert!(
        sqlx::query(sqlx::AssertSqlSafe(
            (wamn_registry::sql::select_capture_gap_sql()).to_string()
        ))
        .bind(&document.org)
        .bind(&document.project)
        .bind(&document.env)
        .fetch_one(&mut capture_owner)
        .await?
        .get::<bool, _>(1)
    );

    // A CNPG publication can already exist after recreation while grants on
    // the newly created maps still need restoration by the CDC SQL owner.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::replication_login_sql(
            &capture,
            "reset-replication-password",
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::create_publication_schemas_sql(&capture, &schemas))
            .to_string(),
    ))
    .execute(&mut candidate_admin)
    .await
    .map(drop)?;
    assert!(ensure::cdc_in_place(&mut candidate_admin, &capture, &schemas).await?);
    let maps = schemas
        .iter()
        .flat_map(|schema| {
            wamn_compiler::manifest::CONTROL_OWNED_RELATION_TABLES
                .map(|table| format!("{schema}.{table}"))
        })
        .collect::<Vec<_>>();
    for map in &maps {
        assert!(
            !sqlx::query("SELECT has_table_privilege($1,$2,'SELECT')")
                .bind(&capture)
                .bind(map)
                .fetch_one(&mut candidate_admin)
                .await?
                .get::<bool, _>(0)
        );
    }
    wamn_environment_control::enable_cdc_project_env::apply_cdc_sql(
        project_admin_url.as_str(),
        &schemas,
        &capture,
        &database_name,
    )
    .await?;
    let mut replication_url = project_admin_url.clone();
    replication_url.set_username(&capture).unwrap();
    replication_url
        .set_password(Some("reset-replication-password"))
        .unwrap();
    let mut reader = connect(replication_url.as_str()).await?;
    let role =
        sqlx::query("SELECT current_user, rolsuper FROM pg_roles WHERE rolname=current_user")
            .fetch_one(&mut reader)
            .await?;
    assert_eq!(role.get::<String, _>(0), capture);
    assert!(!role.get::<bool, _>(1));
    for map in &maps {
        sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM {map} LIMIT 1")))
            .fetch_all(&mut reader)
            .await?;
    }

    assert!(
        require_connected(&mut system_writer, &document.triple())
            .await
            .is_err()
    );
    assert!(expected.lock(&platform, &document, false).await.is_err());
    assert_eq!(
        sql::schema::status(&mut platform, &document)
            .await?
            .pending_database
            .as_ref(),
        Some(&pending)
    );
    let mut superseded = pending.clone();
    superseded["initialization_identity"] = "stale".into();
    assert!(
        finish_database(&mut system_writer, &document.triple(), &superseded)
            .await
            .is_err()
    );
    finish_database(&mut system_writer, &document.triple(), &pending).await?;
    assert!(expected.lock(&platform, &document, true).await.is_err());
    require_connected(&mut system_writer, &document.triple()).await?;
    assert!(
        sql::schema::status(&mut platform, &document)
            .await?
            .pending_database
            .is_none()
    );
    let mut stopped = document.clone();
    for selection in stopped.applications.values_mut() {
        selection.current = wamn_environment_control::environment::document::DeclaredRelease::None;
    }
    sql::reset::prepare_retained(&platform, &stopped, system.url()).await?;
    let retained: serde_json::Value = sqlx::query("SELECT pending_database FROM registry.environment_projects WHERE org='acme' AND project='apps' AND env='dev'").fetch_one(&mut admin).await?.get(0);
    assert_ne!(
        retained["initialization_identity"],
        pending["initialization_identity"]
    );
    assert_eq!(retained["inputs"], pending["inputs"]);
    sqlx::query(
        "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name=$1",
    )
    .bind(&capture)
    .fetch_all(&mut candidate_admin)
    .await?;
    drop(reader);
    drop(candidate_admin);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP DATABASE {} WITH (FORCE)",
            postgres_protocol::escape::escape_identifier(&database_name),
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    Ok(())
}

#[tokio::test]
async fn reset_replays_recorded_project_sql_and_refuses_unreconstructable_history()
-> anyhow::Result<()> {
    use wamn_control_provision::migrations::MigrationDatabase;
    use wamn_environment_control::{platform_migrations, provision_system};
    use wamn_schema_control::MigrationSource;

    let scratch = ScratchRoot::create()?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let system = postgres.create_database("wamn_system")?;
    let mut admin = wamn_postgres::connect(system.url()).await?;

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_system NOLOGIN NOINHERIT; ALTER DATABASE wamn_system OWNER TO wamn_system")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    provision_system::provision_system(&provision_system::ProvisionSystemRequest {
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
    })
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::seed_org_project_env_sql(
            "reset",
            "substrate",
            "dev",
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let platform = Platform {
        system_database_url: system.url().into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig: PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: PathBuf::new(),
        release_artifact_base: String::new(),
        registry_token_metadata: false,
        registry_auth_file: scratch.path().join("auth.json"),
        oci_ca_paths: Vec::new(),
        insecure_registry: false,
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    let document = EnvironmentDocument::from_json(br#"{"org":"reset","project":"substrate","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,"applications":{}}"#)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {} UPDATE registry.environments SET disposable=true WHERE org='reset' AND project='substrate' AND env='dev'; COMMIT;", wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning)))).execute(&mut admin).await.map(drop)?;
    let name =
        wamn_control_provision::project_env_database_name("reset", "substrate", "dev", "aaaaaaa1");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&name)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let database = postgres.database(&name)?;
    ensure::ensure_project_database(&platform, &document, &[], &mut admin, system.url()).await?;
    let mut project = wamn_postgres::connect(database.url()).await?;

    let mut files = platform_migrations::embedded_sources(MigrationDatabase::Project);
    for (label, sql) in [
        (
            "create",
            "CREATE TABLE platform.reset_probe(value int); INSERT INTO platform.reset_probe VALUES (7);",
        ),
        ("upgrade", "UPDATE platform.reset_probe SET value=value+1;"),
    ] {
        files.push(MigrationSource {
            relative_path: format!("migrations/project/{:04}_{label}.sql", files.len() + 1),
            bytes: sql.as_bytes().to_vec(),
        });
    }
    let lock = wamn_environment_control::environment::lock::LifecycleLock::acquire(
        system.url(),
        &document.triple(),
    )
    .await?;
    platform_migrations::apply(
        &mut project,
        platform_migrations::MigrationTarget::Project(&lock),
        &files,
        None,
    )
    .await?;
    files.push(MigrationSource {
        relative_path: format!("migrations/project/{:04}_pending.sql", files.len() + 1),
        bytes: b"UPDATE platform.reset_probe SET value=value+1;".to_vec(),
    });
    let before = wamn_package_control::schema_baseline::observe(
        &mut project,
        MigrationDatabase::Project,
        &files,
    )
    .await?;
    assert_eq!(
        sqlx::query("SELECT value FROM platform.reset_probe")
            .fetch_one(&mut project)
            .await?
            .get::<i32, _>(0),
        8
    );
    for missing in [true, false] {
        let mut unavailable = files.clone();
        if missing {
            unavailable.truncate(files.len() - 2);
        } else {
            unavailable[0].bytes.push(b' ');
        }
        assert!(
            sql::reset::prepare_retained_with_migrations(
                &platform,
                &document,
                system.url(),
                &unavailable
            )
            .await
            .is_err()
        );
        assert_eq!(
            wamn_package_control::schema_baseline::observe(
                &mut project,
                MigrationDatabase::Project,
                &files
            )
            .await?,
            before
        );
        assert_eq!(
            sqlx::query("SELECT value FROM platform.reset_probe")
                .fetch_one(&mut project)
                .await?
                .get::<i32, _>(0),
            8
        );
        assert!(sqlx::query("SELECT pending_database IS NULL FROM registry.environment_projects WHERE org='reset' AND project='substrate' AND env='dev'").fetch_one(&mut admin).await?.get::<bool, _>(0));
    }
    sql::reset::prepare_retained_with_migrations(&platform, &document, system.url(), &files)
        .await?;
    let pending: serde_json::Value = sqlx::query("SELECT pending_database FROM registry.environment_projects WHERE org='reset' AND project='substrate' AND env='dev'").fetch_one(&mut admin).await?.get(0);
    // CNPG owns deletion/recreation. This case observes the SQL restoration boundary only.
    drop(project);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP DATABASE {} WITH (FORCE)",
            postgres_protocol::escape::escape_identifier(&name)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&name)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    lock.release().await;
    // An unavailable retry image refuses even before initialization SQL creates a relation.
    let mut unavailable = files.clone();
    unavailable[0].bytes.push(b' ');
    assert!(
        sql::reset::initialize_with_migrations(&platform, &document, system.url(), &unavailable)
            .await
            .is_err()
    );
    let mut project = wamn_postgres::connect(database.url()).await?;

    assert!(
        sqlx::query("SELECT to_regclass('platform.schema_baseline') IS NULL")
            .fetch_one(&mut project)
            .await?
            .get::<bool, _>(0)
    );
    sql::reset::initialize_with_migrations(&platform, &document, system.url(), &files).await?;
    sql::reset::initialize_with_migrations(&platform, &document, system.url(), &files).await?;
    let restored = wamn_package_control::schema_baseline::observe(
        &mut project,
        MigrationDatabase::Project,
        &files,
    )
    .await?;
    assert_eq!(restored.installed, before.installed);
    assert_eq!(restored.pending, before.pending);
    assert_eq!(
        restored.initialization.as_ref().unwrap().sha256,
        before.initialization.as_ref().unwrap().sha256
    );
    assert_eq!(
        restored.initialization.as_ref().unwrap().installed_at,
        pending["initialization_identity"].as_str().unwrap()
    );
    assert_ne!(
        restored.initialization.as_ref().unwrap().installed_at,
        before.initialization.as_ref().unwrap().installed_at
    );
    assert_eq!(
        sqlx::query("SELECT value FROM platform.reset_probe")
            .fetch_one(&mut project)
            .await?
            .get::<i32, _>(0),
        8
    );
    // A real nontransactional failure leaves a started record and blocks a new reset.
    let lock = wamn_environment_control::environment::lock::LifecycleLock::acquire(
        system.url(),
        &document.triple(),
    )
    .await?;
    files.last_mut().unwrap().bytes = b"-- wamn:no-transaction\nCREATE INDEX CONCURRENTLY reset_probe_idx ON platform.absent_reset_probe(value);".to_vec();
    let mut executor = wamn_postgres::connect(database.url()).await?;

    assert!(
        platform_migrations::apply(
            &mut executor,
            platform_migrations::MigrationTarget::Project(&lock),
            &files,
            None
        )
        .await
        .is_err()
    );
    let uncertain = wamn_package_control::schema_baseline::observe(
        &mut project,
        MigrationDatabase::Project,
        &files,
    )
    .await?;
    let error =
        sql::reset::prepare_retained_with_migrations(&platform, &document, system.url(), &files)
            .await
            .unwrap_err();
    assert!(error.to_string().contains("--resolve"));
    assert_eq!(
        wamn_package_control::schema_baseline::observe(
            &mut project,
            MigrationDatabase::Project,
            &files
        )
        .await?,
        uncertain
    );
    assert_eq!(
        uncertain
            .uncertain
            .iter()
            .map(|file| &file.relative_path)
            .collect::<Vec<_>>(),
        files
            .last()
            .map(|file| &file.relative_path)
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        sqlx::query("SELECT value FROM platform.reset_probe")
            .fetch_one(&mut project)
            .await?
            .get::<i32, _>(0),
        8
    );
    lock.release().await;
    drop(executor);
    drop(project);
    drop(admin);
    Ok(())
}
