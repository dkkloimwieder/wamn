//! Application preparation preserves SQL, other applications, and running release authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wamn_catalog::manifest::PackageManifest;
use wamn_catalog::{ComponentDescriptor, PackageCoordinate, ServingComponent, ServingManifest};
use wamn_environment_control::environment::{
    Platform, document::EnvironmentDocument, ensure, expand, sql,
};
use wamn_package_artifact::sha256_digest;
use wamn_test_infrastructure::scratch::ScratchRoot;

const ENV_KEY: &str = "acme--apps--dev";
const STATE_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, driver) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    tokio::spawn(driver);
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
                    sha256_digest(body)
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
            format!("/v2/{repository}/blobs/{}", sha256_digest(&bytes)),
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
    let mut descriptor = ComponentDescriptor::named(name, "1.0.0", sha256_digest(&bytes));
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
        .into_admitted(wamn_catalog::ComponentPackageScope::new(
            ENV_KEY, name, "1.0.0",
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
        operations: BTreeMap::from([("run".into(), serde_json::from_value(json!({}))?)]),
        statement_instances: BTreeMap::new(),
    })
}

fn package(
    root: &Path,
    objects: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
    component: Option<&ServingComponent>,
) -> anyhow::Result<PackageManifest> {
    let root = root.join(name);
    std::fs::create_dir_all(root.join("migrations"))?;
    std::fs::create_dir_all(root.join("generated/platform-policy"))?;
    let mut value = json!({"package":{"id":name,"version":"1.0.0"},"required_platform_policy_contract":{"id":"fixture_access","state":"satisfied"},"models":{},"components":{},"connections":{}});
    value["components"] = json!({name:{"connections":["postgres"]}});
    value["models"] = json!({"data":{"schema":name,"table":"data","owner":name,"server_owned_fields":[],"audit_log":{"columns":[],"retention":"none"},"operations":{"get":{"permission":"data.get","result":"one"},"update":{"permission":"data.update","result":"one"}}}});
    std::fs::write(
        root.join("migrations/0001_data.sql"),
        format!(
            "CREATE TABLE {name}.data (id integer GENERATED BY DEFAULT AS IDENTITY CONSTRAINT {name}_data_id_pkey PRIMARY KEY, value text);"
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
            wamn_execution_contract::canonical_json_bytes(&serde_json::to_value(
                &component.descriptor,
            )?),
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
    let directory = wamn_package_control::apply_package::read_package_directory(&root)?;
    let columns = BTreeMap::from([(
        "data".into(),
        vec![
            wamn_schema_control::physical_schema::RelationColumn {
                name: "id".into(),
                primary_key: true,
                timestamptz: false,
            },
            wamn_schema_control::physical_schema::RelationColumn {
                name: "value".into(),
                primary_key: false,
                timestamptz: false,
            },
        ],
    )]);
    let physical = wamn_schema_control::physical_schema::PhysicalSchema {
        statements: wamn_schema_control::physical_schema::render(
            &manifest,
            &directory.migrations,
            &columns,
        )?,
        columns,
    };
    std::fs::write(
        root.join("generated/physical-schema.json"),
        serde_json::to_vec(&physical)?,
    )?;
    let overlay = wamn_schema_generator::derive_data_access_overlay_from_relation_fields(
        &[wamn_schema_generator::DataAccessRelationFields::new(
            name,
            "data",
            vec!["id".into(), "value".into()],
        )],
        &bytes,
    )?;
    std::fs::write(
        root.join("generated/platform-policy/data-access.json"),
        overlay.canonical_bytes(),
    )?;
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
) -> anyhow::Result<ServingManifest> {
    let manifest = ServingManifest {
        format_version: wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        release: wamn_catalog::ServingRelease {
            packages: BTreeSet::from([
                PackageCoordinate::new(&component.package_id, "1.0.0")?,
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
        &json!({"org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","integrations":{"erp":{"version":1}},"applications":{
            "a":{"current":a.digest().as_str(),"hostname":"a.example","packages":["a","wamn_integration"],"connections":{"upstream":"erp"}},
            "b":{"current":b.digest().as_str(),"hostname":"b.example","packages":["b","wamn_integration"],"connections":{"upstream":"erp"}}
        }}),
    )?)
}

async fn rows(client: &tokio_postgres::Client, query: &str) -> anyhow::Result<Vec<String>> {
    Ok(client
        .query(query, &[])
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect())
}

#[tokio::test]
async fn activation_preserves_sql_other_applications_and_running_release() -> anyhow::Result<()> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let scratch = ScratchRoot::create()?;
    let mut objects = BTreeMap::new();
    let old_a = component(&mut objects, "a", &["upstream"])?;
    let b = component(&mut objects, "b", &["upstream"])?;
    let base_package = package(scratch.path(), &mut objects, "wamn_integration", None)?;
    let old_package = package(scratch.path(), &mut objects, "a", Some(&old_a))?;
    let b_package = package(scratch.path(), &mut objects, "b", Some(&b))?;
    let old_a = release(&mut objects, old_a)?;
    let b = release(&mut objects, b)?;
    let new_component = component(&mut objects, "a", &["upstream", "extra"])?;
    let new_a = release(&mut objects, new_component)?;
    let failed_component = component(&mut objects, "a", &["upstream", "extra", "failed"])?;
    let failed_release = release(&mut objects, failed_component)?;
    let mut missing_objects = objects.clone();
    missing_objects.remove("/v2/packages/manifests/b-1.0.0");
    let missing_root = scratch.path().join("missing-registry");
    std::fs::create_dir(&missing_root)?;
    let missing_registry = Registry::start(&missing_root, missing_objects).await?;
    let registry = Registry::start(scratch.path(), objects).await?;
    let mut postgres = wamn_test_postgres::start(&[("wal_level", "logical")])?;
    let system = postgres.create_database("wamn_system")?;
    let mut admin = connect(system.url()).await?;
    // CNPG supplies this database owner before the platform SQL operation.
    admin
        .batch_execute(
            "CREATE ROLE wamn_system NOLOGIN; ALTER DATABASE wamn_system OWNER TO wamn_system",
        )
        .await?;
    wamn_environment_control::provision_system::provision_system(
        &wamn_environment_control::provision_system::ProvisionSystemRequest {
            system_database_url: system.url().into(),
            platform_domain: "example.invalid".into(),
        },
    )
    .await?;
    admin
        .batch_execute(&wamn_control_provision::sql::grant_control_surface_sql())
        .await?;
    admin.batch_execute(&format!("BEGIN; SET LOCAL ROLE wamn_system; {} INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT;", wamn_control_provision::test_database::seed_org_project_env_sql("acme","apps","dev"))).await?;
    let admin_url = url::Url::parse(system.url())?;
    let secret = scratch.path().join("apply.json");
    wamn_environment_control::provision_system::emit_apply_credential(
        system.url(),
        admin_url.host_str().unwrap(),
        admin_url.port().unwrap(),
        &secret,
    )
    .await?;
    let secret: serde_json::Value = serde_json::from_slice(&std::fs::read(secret)?)?;
    let platform = Platform {
        system_database_url: secret["stringData"]["url"].as_str().unwrap().into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig: PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: PathBuf::new(),
        release_artifact_base: format!("{}/releases", registry.address),
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
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            wamn_pg_core::quote_ident(&database_name)
        ))
        .await?;

    let store = wamn_test_infrastructure::credentials::LocalCredentials::create()?;
    let tx = admin.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    wamn_test_infrastructure::credentials::define(&tx, "acme", &json!({"project":"apps","env":"dev","name":"erp","integration_type":"http","auth_type":"none","parameters":{"primary-authority":"https://erp.example/","tls-verification":"verify-authority"}}), &store).await?;
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
            &[b_package.clone(), base_package.clone()]
        )
        .await
        .is_err()
    );
    let selected_b =
        wamn_environment_control::environment::analyse::select_application(&document, Some("b"))?;
    assert!(sql::needs_initialization(&platform, &selected_b).await?);
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
    sql::schema::apply(&platform, &document, "b", b.digest().as_str(), system.url()).await?;
    let installed = sql::schema::state(&platform, &document).await?;
    for (id, package) in &diff.packages {
        assert_eq!(
            installed.packages[id].physical_schema_fingerprint,
            package.candidate
        );
    }
    assert_eq!(installed.packages["a"], state.packages["a"]);
    document.applications.get_mut("b").unwrap().current = b_current;
    assert!(!sql::needs_initialization(&platform, &document).await?);
    let mut observer_url = admin_url;
    observer_url.set_path(&database_name);
    let observer = connect(observer_url.as_str()).await?;
    observer
        .batch_execute(&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::ExecutorPlatform,
        ))
        .await?;
    // Initialization extends capture from all installed baselines, even when the
    // caller supplies only B's selected document.
    let selected_b =
        wamn_environment_control::environment::analyse::select_application(&document, Some("b"))?;
    let captured = ensure::installed_manifests(&platform, &selected_b).await?;
    let schemas = captured
        .iter()
        .flat_map(|package| package.models.values().map(|model| model.schema.as_str()))
        .collect::<BTreeSet<_>>();
    let installed_schemas = observer.query(
        "SELECT DISTINCT schema_name FROM catalog.package_definition_owners WHERE env_key=$1 AND definition_type='relation'",
        &[&ENV_KEY],
    ).await?.into_iter().map(|row| row.get::<_, String>(0)).collect::<BTreeSet<_>>();
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
    )
    .await?;
    expand::prepare(
        &platform,
        &document,
        "b",
        b.digest().as_str(),
        &b,
        &[b_package, base_package.clone()],
    )
    .await?;
    observer
        .batch_execute(
            "INSERT INTO a.data(value) VALUES ('authored'); INSERT INTO b.data(value) VALUES ('other');",
        )
        .await?;
    observer.batch_execute(&format!(
        "BEGIN; {} SELECT platform.bump_counter(id, 'number', '') FROM platform.number_sequence; COMMIT;",
        wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning),
    )).await?;
    let mut snapshots = vec![
        "SELECT to_jsonb(t)::text FROM platform.number_sequence t".to_owned(),
        "SELECT to_jsonb(t)::text FROM platform.number_counter t".to_owned(),
        "SELECT to_jsonb(t)::text FROM catalog.packages t".to_owned(),
        "SELECT to_jsonb(t)::text FROM catalog.package_migrations t".to_owned(),
    ];
    let relations = captured
        .iter()
        .flat_map(|package| package.models.values())
        .map(|model| (model.schema.as_str(), model.table.as_str()))
        .collect::<BTreeSet<_>>();
    for (schema, relation) in relations {
        let table = format!(
            "{}.{}",
            wamn_pg_core::quote_ident(schema),
            wamn_pg_core::quote_ident(relation)
        );
        snapshots.push(format!(
            "SELECT jsonb_build_array({}, to_jsonb(t))::text FROM {table} t",
            wamn_pg_core::quote_literal(&table)
        ));
        let sequence: String = observer
            .query_one("SELECT pg_get_serial_sequence($1, 'id')", &[&table])
            .await?
            .get(0);
        snapshots.push(format!(
            "SELECT jsonb_build_array({}, last_value, is_called)::text FROM {sequence}",
            wamn_pg_core::quote_literal(&sequence)
        ));
    }
    let stable_sql = format!("{} ORDER BY 1", snapshots.join(" UNION ALL "));
    let before_sql = rows(&observer, &stable_sql).await?;
    sql::initialize(&platform, &document, system.url(), Some("a"))
        .await
        .context("retry initialization")?;
    assert_eq!(before_sql, rows(&observer, &stable_sql).await?);
    let schema_query = "SELECT row(c.oid,c.relname,c.relkind,c.relowner,c.relacl,a.attname,a.atttypid,a.attnotnull,a.attacl)::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace LEFT JOIN pg_attribute a ON a.attrelid=c.oid WHERE n.nspname IN (SELECT DISTINCT schema_name FROM catalog.package_definition_owners WHERE definition_type='relation') ORDER BY 1";
    let before_schema = rows(&observer, schema_query).await?;
    let stable_b = "SELECT to_jsonb(t)::text FROM platform.permission t WHERE role_name='application-b' UNION ALL SELECT to_jsonb(t)::text FROM platform.user_role t WHERE role_name='application-b' UNION ALL SELECT to_jsonb(t)::text FROM catalog.connection_binding t WHERE package_id='b' ORDER BY 1";
    let before_b = rows(&observer, stable_b).await?;
    let b_schemas: BTreeSet<_> = captured
        .iter()
        .filter(|package| {
            document.applications["b"]
                .packages
                .contains(&package.package.id)
        })
        .flat_map(|package| package.models.values().map(|model| model.schema.as_str()))
        .collect();
    let b_effective_sql = format!(
        "SELECT row(n.nspname,c.relname,a.attname,has_schema_privilege('wamn_app',n.oid,'USAGE'),has_table_privilege('wamn_app',c.oid,'SELECT'),has_table_privilege('wamn_app',c.oid,'UPDATE'),has_column_privilege('wamn_app',c.oid,a.attnum,'SELECT'),has_column_privilege('wamn_app',c.oid,a.attnum,'UPDATE'))::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_attribute a ON a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped WHERE c.relkind IN ('r','p','v') AND n.nspname IN ({}) ORDER BY 1",
        b_schemas
            .into_iter()
            .map(wamn_pg_core::quote_literal)
            .collect::<Vec<_>>()
            .join(","),
    );
    let before_b_effective = rows(&observer, &b_effective_sql).await?;
    let principal = |name: &str| wamn_platform_identity::application_subject(ENV_KEY, name);
    let a_user = wamn_platform_identity::resolve_subject(
        &admin,
        wamn_platform_identity::PrincipalType::Service,
        &principal("a")?,
    )
    .await?
    .unwrap();
    let b_user = wamn_platform_identity::resolve_subject(
        &admin,
        wamn_platform_identity::PrincipalType::Service,
        &principal("b")?,
    )
    .await?
    .unwrap();
    let mounted = wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
        &old_a.canonical_bytes(),
        "running application a",
        wamn_engine::release_manifest::ReleaseScope::new(ENV_KEY, "dev").with_applications(
            wamn_environment_control::environment::analyse::select_application(
                &document,
                Some("a"),
            )?
            .membership(),
        ),
    )?;
    let b_permissions = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &observer,
        b_user.id().as_str(),
        None,
    )
    .await?;
    document.applications.get_mut("a").unwrap().current =
        wamn_environment_control::environment::document::DeclaredRelease::Digest(
            new_a.digest().as_str().into(),
        );
    assert!(!sql::needs_initialization(&platform, &document).await?);
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
    )
    .await?;
    let control_query = "SELECT (to_jsonb(t) - 'created_at' - 'updated_at')::text FROM catalog.connection_requirement t UNION ALL SELECT (to_jsonb(t) - 'created_at' - 'updated_at')::text FROM catalog.connection_binding t UNION ALL SELECT (to_jsonb(t) - 'created_at' - 'updated_at')::text FROM catalog.release_connection t UNION ALL SELECT (to_jsonb(t) - 'created_at' - 'updated_at')::text FROM platform.permission t UNION ALL SELECT (to_jsonb(t) - 'created_at' - 'updated_at')::text FROM platform.user_role t ORDER BY 1";
    let before_retry = rows(&observer, control_query).await?;
    expand::prepare(
        &platform,
        &document,
        "a",
        new_a.digest().as_str(),
        &new_a,
        &candidate_packages,
    )
    .await?;
    assert_eq!(before_retry, rows(&observer, control_query).await?);
    assert_eq!(before_sql, rows(&observer, &stable_sql).await?);
    assert_eq!(before_schema, rows(&observer, schema_query).await?);
    assert_eq!(before_b, rows(&observer, stable_b).await?);
    assert_eq!(before_b_effective, rows(&observer, &b_effective_sql).await?);
    let b_after = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &observer,
        b_user.id().as_str(),
        None,
    )
    .await?;
    for operation in candidate_packages[1]
        .models
        .values()
        .flat_map(|model| model.operations.values())
    {
        let token = wamn_catalog::manifest::canonical_operation_identity(
            &candidate_packages[1].package,
            &operation.permission,
        )?;
        assert_eq!(b_permissions.permits(&token), b_after.permits(&token));
    }
    for release in [&old_a, &new_a] {
        for component in &release.components {
            for connection in &component.descriptor.connections {
                assert!(observer.query_opt("SELECT 1 FROM catalog.connection_binding WHERE env_key=$1 AND manifest_digest=$2 AND component_digest=$3 AND package_id=$4 AND name=$5", &[&ENV_KEY, &release.digest().as_str(), &component.descriptor.component_digest, &component.package_id, &connection.store_alias]).await?.is_some());
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
    let tx = project.transaction().await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    wamn_platform_identity::application::mutate_grant(
        &tx,
        ENV_KEY,
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::UserRole {
            user_id: b_user.id().as_str(),
            role: "application-a",
            grant: true,
        },
        &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
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
            &failed_packages
        )
        .await
        .is_err()
    );
    assert!(
        observer
            .query_opt(
                "SELECT 1 FROM catalog.connection_requirement WHERE component_digest=$1",
                &[&failed_release
                    .components
                    .first()
                    .unwrap()
                    .descriptor
                    .component_digest]
            )
            .await?
            .is_none()
    );
    assert_eq!(before_sql, rows(&observer, &stable_sql).await?);
    assert_eq!(before_schema, rows(&observer, schema_query).await?);
    assert_eq!(before_b_effective, rows(&observer, &b_effective_sql).await?);
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
            ENV_KEY,
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
        observer
            .query_opt(
                "SELECT 1 FROM catalog.releases WHERE env_key=$1 AND manifest_digest=$2",
                &[&ENV_KEY, &failed_release.digest().as_str()],
            )
            .await?
            .is_some()
    );
    drop(postgres);
    let caller = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &observer,
        a_user.id().as_str(),
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
    assert!(observer.query_opt("SELECT 1 FROM catalog.connection_binding WHERE manifest_digest=$1 AND component_digest=$2 AND name='upstream'", &[&old_a.digest().as_str(), &connection.component_digest]).await?.is_some());
    // Retirement uses the production owner role, its lock, and explicit application digests.
    project.batch_execute("SET ROLE wamn_db_owner").await?;
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
    let tx = project.transaction().await?;
    tx.query_one(
        wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL,
        &[&ENV_KEY],
    )
    .await?;
    assert!(
        wamn_environment_control::environment::bindings::contract(
            &tx,
            ENV_KEY,
            "a",
            &document.membership(),
            &[b.digest().as_str()],
            &[],
            &mut wamn_platform_identity::connection_changes::ConnectionDrops::default()
        )
        .await
        .is_err()
    );
    let retired = wamn_environment_control::environment::bindings::contract(
        &tx,
        ENV_KEY,
        "a",
        &document.membership(),
        &[old_a.digest().as_str()],
        &[new_a.digest().as_str()],
        &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
    )
    .await?;
    assert!(
        wamn_environment_control::environment::stage::retire_runtime_grants(
            &admin,
            &tx,
            ENV_KEY,
            &releases,
            &retired,
            &mut wamn_platform_identity::auth_changes::AuthDrops::default()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert!(
        observer
            .query_opt(
                "SELECT 1 FROM catalog.connection_binding WHERE env_key=$1 AND manifest_digest=$2",
                &[&ENV_KEY, &old_a.digest().as_str()]
            )
            .await?
            .is_some()
    );
    observer.execute("INSERT INTO wamn_run.runs(env_key,run_id,package_id,environment,status,manifest_digest,wiring_id,wiring_version,wiring_hash,binding_world_json) VALUES($1,'old-work','a','dev','dispatched',$2,'work',1,$2,'[]')", &[&ENV_KEY, &old_a.digest().as_str()]).await?;
    observer.execute("INSERT INTO wamn_run.runs(env_key,run_id,package_id,environment,status,manifest_digest,wiring_id,wiring_version,wiring_hash,binding_world_json) VALUES($1,'b-work','b','dev','dispatched',$2,'work',1,$2,'[]')", &[&ENV_KEY, &b.digest().as_str()]).await?;
    for (live, pinned) in [(true, true), (false, true), (false, false)] {
        if !pinned {
            observer
                .execute(
                    "UPDATE wamn_run.runs SET status='completed' WHERE run_id='old-work'",
                    &[],
                )
                .await?;
        }
        let tx = project.transaction().await?;
        tx.query_one(
            wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL,
            &[&ENV_KEY],
        )
        .await?;
        tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await?;
        let mut auth = wamn_platform_identity::auth_changes::AuthDrops::default();
        // The failed replacement left a shared role. Restore its exclusive owner before retirement.
        if live {
            wamn_platform_identity::application::mutate_grant(
                &tx,
                ENV_KEY,
                wamn_platform_identity::application::GrantAuthority::Provisioning,
                wamn_platform_identity::application::GrantChange::UserRole {
                    user_id: b_user.id().as_str(),
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
            &tx,
            ENV_KEY,
            "a",
            &document.membership(),
            &[old_digest.as_str()],
            &live_digests,
            &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
        )
        .await?;
        wamn_environment_control::environment::stage::retire_runtime_grants(
            &admin, &tx, ENV_KEY, &releases, &retired, &mut auth,
        )
        .await?;
        tx.commit().await?;
        assert_eq!(retired.is_empty(), live || pinned);
        let stranded = wamn_environment_control::environment::drain::stranded(
            observer_url.as_str(),
            ENV_KEY,
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
            &observer,
            a_user.id().as_str(),
            None,
        )
        .await?;
        assert_eq!(
            caller.permits("wamn-integration:data/get@1.0.0"),
            live || pinned
        );
        assert!(caller.permits("wamn-integration:data/update@1.0.0"));
        let b_after = wamn_runtime::plugins::route_authentication::queued_run_caller(
            &observer,
            b_user.id().as_str(),
            None,
        )
        .await?;
        for operation in candidate_packages[1]
            .models
            .values()
            .flat_map(|model| model.operations.values())
        {
            let token = wamn_catalog::manifest::canonical_operation_identity(
                &candidate_packages[1].package,
                &operation.permission,
            )?;
            assert_eq!(b_permissions.permits(&token), b_after.permits(&token));
        }
        assert_eq!(before_b, rows(&observer, stable_b).await?);
        assert_eq!(before_b_effective, rows(&observer, &b_effective_sql).await?);
        assert_eq!(before_sql, rows(&observer, &stable_sql).await?);
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
    )
    .await?;
    assert!(
        observer
            .query_opt(
                "SELECT 1 FROM catalog.connection_binding WHERE env_key=$1 AND manifest_digest=$2",
                &[&ENV_KEY, &old_a.digest().as_str()]
            )
            .await?
            .is_some()
    );
    let caller = wamn_runtime::plugins::route_authentication::queued_run_caller(
        &observer,
        a_user.id().as_str(),
        None,
    )
    .await?;
    assert!(caller.permits("wamn-integration:data/get@1.0.0"));
    assert_eq!(before_b, rows(&observer, stable_b).await?);
    assert_eq!(before_b_effective, rows(&observer, &b_effective_sql).await?);
    assert_eq!(before_sql, rows(&observer, &stable_sql).await?);
    assert_eq!(before_schema, rows(&observer, schema_query).await?);
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
    admin.batch_execute(&format!(
        "BEGIN; SET LOCAL ROLE wamn_system; {} UPDATE registry.project_envs SET disposable=true WHERE org='acme' AND project='apps' AND env='dev'; COMMIT;",
        wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning),
    )).await?;
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
    assert!(admin.query_one("SELECT pending_database IS NULL FROM registry.project_envs WHERE org='acme' AND project='apps' AND env='dev'", &[]).await?.get::<_, bool>(0));
    let cr = sql::reset::prepare(
        &platform,
        &document,
        "a",
        old_a.digest().as_str(),
        system.url(),
    )
    .await?;
    let mut changed_policy = document.clone();
    changed_policy.policy = "unprovisioned-policy".into();
    let retried = sql::reset::prepare(
        &platform,
        &changed_policy,
        "a",
        old_a.digest().as_str(),
        system.url(),
    )
    .await?;
    assert_eq!(cr["spec"]["name"], retried["spec"]["name"]);
    assert_eq!(retried["spec"]["cluster"]["name"], provisioned.cluster);
    let replacement = cr["spec"]["name"].as_str().unwrap();
    assert_ne!(replacement, database_name);
    // Only CNPG's database creation is represented here. DEP3-11 observes CNPG itself.
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            wamn_pg_core::quote_ident(replacement)
        ))
        .await?;
    let mut replacement_url = url::Url::parse(system.url())?;
    replacement_url.set_path(replacement);
    let candidate_admin = connect(replacement_url.as_str()).await?;
    candidate_admin.batch_execute("CREATE FUNCTION public.interrupt_beta() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN IF EXISTS (SELECT FROM pg_event_trigger_ddl_commands() WHERE object_identity='b.data' AND command_tag='CREATE TABLE') THEN RAISE EXCEPTION 'beta initialization interrupted'; END IF; END $$; CREATE EVENT TRIGGER interrupt_beta ON ddl_command_end EXECUTE FUNCTION public.interrupt_beta()").await?;
    assert!(
        sql::reset::initialize(&platform, &document, system.url())
            .await
            .is_err()
    );
    assert_eq!(before_sql, rows(&observer, &stable_sql).await?);
    assert_eq!(before_schema, rows(&observer, schema_query).await?);
    assert_eq!(
        wamn_environment_control::provision_project_env::read_project_env_instance(
            &platform.system_database_url,
            &document.triple()
        )
        .await?,
        provisioned.instance
    );
    // A committed earlier package survives retry, including rows inserted after it committed.
    candidate_admin.batch_execute("DROP EVENT TRIGGER interrupt_beta; INSERT INTO a.data(id,value) VALUES (123,'committed-before-retry')").await?;
    let first = sql::reset::initialize(&platform, &document, system.url()).await?;
    let second = sql::reset::initialize(&platform, &document, system.url()).await?;
    assert_eq!(first["database"], second["database"]);
    assert_eq!(
        candidate_admin
            .query_one("SELECT value FROM a.data WHERE id=123", &[])
            .await?
            .get::<_, String>(0),
        "committed-before-retry"
    );
    let installed = candidate_admin
        .query(
            "SELECT package_id FROM catalog.packages WHERE env_key=$1",
            &[&ENV_KEY],
        )
        .await?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let active_installed = sql::schema::state(&platform, &document)
        .await?
        .packages
        .into_keys()
        .collect::<BTreeSet<_>>();
    assert_eq!(installed, active_installed);
    assert_eq!(
        wamn_environment_control::provision_project_env::read_project_env_instance(
            &platform.system_database_url,
            &document.triple()
        )
        .await?,
        provisioned.instance
    );
    assert_eq!(before_sql, rows(&observer, &stable_sql).await?);
    let capture = first["capture"].as_str().unwrap();
    candidate_admin.query("SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name=$1", &[&capture]).await?;
    drop(candidate_admin);
    admin
        .batch_execute(&format!(
            "DROP DATABASE {} WITH (FORCE)",
            wamn_pg_core::quote_ident(replacement)
        ))
        .await?;
    drop(project);
    drop(observer);
    admin
        .batch_execute(&format!(
            "DROP DATABASE {} WITH (FORCE)",
            wamn_pg_core::quote_ident(&database_name)
        ))
        .await?;
    Ok(())
}
