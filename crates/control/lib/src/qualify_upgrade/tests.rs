//! Installed-data qualification without a Kubernetes deployment journey.

use super::*;
use wamn_catalog::{ServingComponent, ServingComponentOperation, ServingRelease};
use wamn_test_postgres::{OwnedDatabase, OwnedPostgres};

use crate::apply_package::read_package_directory;

mod overlay;
mod stage;
mod synchronization;
mod whole_row;

const TENANT: &str = "upgrade-proof--fixture--dev";
const ENVIRONMENT: &str = "dev";
const PREDECESSOR_VERSION: &str = "2.1.0";
const CANDIDATE_VERSION: &str = "2.2.0";
const APP_SCHEMA: &str = include_str!("../../../../../deploy/sql/app-schema.sql");

struct Fixture {
    source: Client,
    source_database: OwnedDatabase,
    root: PathBuf,
    suffix: PathBuf,
    manifest: ServingManifest,
    predecessor_release: String,
    server: OwnedPostgres,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The fixture root with a stand-in artifact digest, as a release verb
/// presents only verified artifacts.
fn fixture_root(path: &Path) -> PackageRoot {
    crate::package_artifact::fixture_package_root(path).expect("read the fixture package root")
}

fn fixture_roots(paths: &[PathBuf]) -> Vec<PackageRoot> {
    paths.iter().map(|path| fixture_root(path)).collect()
}

fn apply_request(database_url: &str, package: &Path) -> ApplyPackageRequest {
    ApplyPackageRequest {
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        package: fixture_root(package),
        database_url: database_url.to_owned(),
        org: "upgrade-proof".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    }
}

async fn install(client: &Client) {
    client
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await
        .unwrap();
    client.batch_execute(
        "DO $roles$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN CREATE ROLE wamn_app NOLOGIN; END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN CREATE ROLE wamn_scenario_author NOLOGIN; END IF; \
           EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
         END $roles$;",
    ).await.unwrap();
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .unwrap();
    client.batch_execute(APP_SCHEMA).await.unwrap();
}

async fn fixture(name: &str, whole_row: bool) -> Fixture {
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let source_database = server.create_database("upgrade_source").unwrap();
    let generation_database = server.create_database("upgrade_generation").unwrap();
    let source = connect(source_database.url()).await;
    let generation = connect(generation_database.url()).await;
    install(&source).await;
    install(&generation).await;
    let predecessor = wamn_fixture_package::package_root();
    crate::apply_package::apply_package(apply_request(source_database.url(), &predecessor))
        .await
        .unwrap();
    reconcile_for_upgrade(ReconcilePackageDataAccessRequest {
        packages: vec![fixture_root(&predecessor)],
        database_url: source_database.url().to_owned(),
        org: "upgrade-proof".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    })
    .await
    .unwrap();
    source.batch_execute(
        "BEGIN; \
         SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), \
                set_config('app.operation', 'admin:seed-upgrade-proof', true); \
         INSERT INTO inventory.widget_maker (id,name) VALUES ('00000000-0000-4000-8000-00000000b001','retained maker'); \
         INSERT INTO inventory.widget (code,note,maker_id) VALUES ('priority','retained note','00000000-0000-4000-8000-00000000b001'),('standard',NULL,NULL); \
         INSERT INTO inventory.widget_tag (label) VALUES ('retained tag'); COMMIT;",
    ).await.unwrap();
    let manifest = predecessor_manifest(&predecessor);
    let bytes = manifest.canonical_bytes();
    let (_, digest) = ServingManifest::from_canonical_bytes(&bytes).unwrap();
    source.execute("INSERT INTO catalog.releases (tenant_id,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)", &[&TENANT, &digest.as_str(), &bytes]).await.unwrap();

    let root = std::env::temp_dir().join(format!("wamn-qualify-{name}-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let candidate = root.join("candidate");
    let suffix = wamn_fixture_package::write_upgrade_package(&candidate);
    if whole_row {
        fs::write(
            &suffix,
            "ALTER TABLE inventory.widget ADD COLUMN upgrade_note text;\n",
        )
        .unwrap();
    }
    crate::test_support::compile_package(&candidate);
    apply_qualification_package(apply_request(generation_database.url(), &candidate))
        .await
        .unwrap();
    wamn_schema_generator::materialize_package_verified(generation_database.url(), &candidate)
        .await
        .unwrap();
    wamn_test_infrastructure::fixture_digest::write_receipt(&candidate);
    Fixture {
        source,
        source_database,
        root,
        suffix,
        manifest,
        predecessor_release: digest.as_str().to_owned(),
        server,
    }
}

fn predecessor_manifest(root: &Path) -> ServingManifest {
    wamn_schema_generator::receipt::check_current(root).unwrap();
    let manifest = PackageManifest::from_slice(
        &fs::read(wamn_schema_generator::package_manifest_path(root)).unwrap(),
    )
    .unwrap();
    let statements =
        crate::component_admission::load_package_statement_facts(root, &manifest).unwrap();
    let operations = statements
        .into_iter()
        .map(|(name, statements)| {
            let operation = ServingComponentOperation {
                registered_operation: Some(name.clone()),
                permissions: BTreeSet::from([name.clone()]),
                committed_result_schema: None,
                statements,
            };
            (name, operation)
        })
        .collect();
    ServingManifest::new(
        ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new(
                "platform_fixture",
                PREDECESSOR_VERSION,
            )
            .unwrap()]),
        },
        BTreeSet::from([ServingComponent {
            runtime_schema: None,
            statement_instances: std::collections::BTreeMap::new(),
            package_id: "platform_fixture".to_owned(),
            operations,
            descriptor: wamn_catalog::ComponentDescriptor::named(
                "fixture",
                "0.1.0",
                format!("sha256:{}", "a".repeat(64)),
            ),
        }]),
        BTreeSet::new(),
        BTreeMap::new(),
    )
    .unwrap()
}

#[test]
fn runtime_schema_must_match_the_selected_release_pin() {
    let root = wamn_fixture_package::package_root();
    let directory = read_package_directory(&root).unwrap();
    let mut release = predecessor_manifest(&root);
    assert_eq!(
        runtime_schemas([&directory], &release).unwrap()["platform_fixture"],
        "inventory"
    );
    release.components = release
        .components
        .into_iter()
        .map(|mut component| {
            component.runtime_schema = Some("inventory".into());
            component
        })
        .collect();
    assert_eq!(
        runtime_schemas([&directory], &release).unwrap()["platform_fixture"],
        "inventory"
    );
    release.components = release
        .components
        .into_iter()
        .map(|mut component| {
            component.runtime_schema = Some("different".into());
            component
        })
        .collect();
    assert!(
        runtime_schemas([&directory], &release)
            .unwrap_err()
            .to_string()
            .contains("runtime schema changed")
    );
}

fn runtime() -> BTreeMap<String, String> {
    BTreeMap::from([("platform_fixture".to_owned(), "inventory".to_owned())])
}

fn request(fixture: &Fixture, result: &str) -> QualifyUpgradeRequest {
    let package = fixture_root(&fixture.root.join("candidate"));
    QualifyUpgradeRequest {
        database_url: fixture.source_database.url().to_owned(),
        org: "upgrade-proof".into(),
        project: "fixture".into(),
        environment: ENVIRONMENT.to_owned(),
        predecessor_release: fixture.predecessor_release.clone(),
        package: package.clone(),
        presented_packages: vec![package],
        predecessor_packages: Vec::new(),
        base_component: None,
        result: fixture.root.join(result),
    }
}

async fn state(client: &mut Client) -> (Vec<String>, UpgradePrivileges) {
    let mut rows = Vec::new();
    for table in [
        "inventory.widget",
        "inventory.widget_maker",
        "inventory.widget_tag",
        "catalog.packages",
        "catalog.package_migrations",
        "catalog.package_definition_owners",
        "catalog.releases",
        "catalog.package_upgrade_qualifications",
    ] {
        let records = client
            .query(
                &format!(
                    "SELECT to_jsonb(t)::text || ':' || t.xmin::text FROM {table} AS t ORDER BY 1"
                ),
                &[],
            )
            .await
            .unwrap();
        rows.extend(
            records
                .into_iter()
                .map(|row| format!("{table}:{}", row.get::<_, String>(0))),
        );
    }
    let tx = client.transaction().await.unwrap();
    let privileges = read_upgrade_privileges(&tx, &["inventory".to_owned()])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    (rows, privileges)
}

async fn copied_source(fixture: &mut Fixture) -> scratch::ScratchDatabase {
    let tx = fixture
        .source
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .unwrap();
    let snapshot: String = tx
        .query_one("SELECT pg_export_snapshot()", &[])
        .await
        .unwrap()
        .get(0);
    let privileges = read_upgrade_privileges(&tx, &["inventory".to_owned()])
        .await
        .unwrap();
    let scratch = scratch::copy_database(fixture.source_database.url(), &snapshot)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let mut copy = connect(scratch.url()).await;
    let tx = copy.transaction().await.unwrap();
    restore_upgrade_privileges(&tx, &["inventory".to_owned()], &privileges)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    scratch
}

async fn retained_rows(client: &Client) -> Vec<String> {
    client.query(
        "SELECT identity FROM ( \
           SELECT 'maker:' || (to_jsonb(t) - 'note')::text || ':' || t.xmin::text AS identity FROM inventory.widget_maker t \
           UNION ALL SELECT 'widget:' || to_jsonb(t)::text || ':' || t.xmin::text FROM inventory.widget t \
           UNION ALL SELECT 'tag:' || to_jsonb(t)::text || ':' || t.xmin::text FROM inventory.widget_tag t \
         ) observed ORDER BY identity",
        &[],
    ).await.unwrap().into_iter().map(|row| row.get(0)).collect()
}

#[tokio::test]
async fn qualification_preserves_source_and_apply_requires_unchanged_evidence() {
    let mut fixture = fixture("proof", false).await;
    let original = state(&mut fixture.source).await;
    let result = qualify_upgrade(request(&fixture, "accepted.json"))
        .await
        .unwrap();
    let (evidence, canonical, digest) = read_qualification(&result.result).unwrap();
    assert_eq!(result.sha256, digest);
    assert_eq!(
        evidence.predecessor_package.package_version,
        PREDECESSOR_VERSION
    );
    assert_eq!(
        evidence.candidate_package.package_version,
        CANDIDATE_VERSION
    );
    assert_eq!(evidence.candidate_suffix.len(), 1);
    assert_eq!(evidence.predecessor_privileges, original.1);
    assert!(evidence.post_privileges.column.contains(&(
        "inventory".to_owned(),
        "widget_maker".to_owned(),
        "note".to_owned(),
        "SELECT".to_owned()
    )));
    assert_eq!(state(&mut fixture.source).await, original);

    let prefix = fixture.root.join("candidate/migrations/0001_initial.sql");
    let original_prefix = fs::read(&prefix).unwrap();
    let mut edited = original_prefix.clone();
    edited.push(b'\n');
    fs::write(&prefix, edited).unwrap();
    wamn_test_infrastructure::fixture_digest::write_receipt(&fixture.root.join("candidate"));
    let error = qualify_upgrade(request(&fixture, "prefix.json"))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("prefix"), "{error:#}");
    fs::write(&prefix, original_prefix).unwrap();
    wamn_test_infrastructure::fixture_digest::write_receipt(&fixture.root.join("candidate"));

    assert!(!fixture.root.join("prefix.json").exists());
    assert_eq!(state(&mut fixture.source).await, original);

    let candidate = fixture.root.join("candidate");
    fixture
        .source
        .batch_execute("GRANT SELECT ON inventory.widget_maker TO wamn_app")
        .await
        .unwrap();
    let error = crate::apply_package::apply_qualified_package(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("predecessor privileges changed"),
        "{error:#}"
    );
    let tx = fixture.source.transaction().await.unwrap();
    restore_upgrade_privileges(&tx, &evidence.schemas, &original.1)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(state(&mut fixture.source).await, original);

    let before_rows = retained_rows(&fixture.source).await;
    let applied = crate::apply_package::apply_qualified_package(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
    )
    .await
    .unwrap();
    assert_eq!(applied.migrations_applied, 1);
    assert_eq!(retained_rows(&fixture.source).await, before_rows);
    let persisted = fixture.source.query_one(
        "SELECT canonical_bytes, result_sha256 FROM catalog.package_upgrade_qualifications \
          WHERE tenant_id = $1 AND package_id = 'platform_fixture' AND candidate_package_version = $2",
        &[&TENANT, &CANDIDATE_VERSION],
    ).await.unwrap();
    assert_eq!(persisted.get::<_, Vec<u8>>(0), canonical);
    assert_eq!(persisted.get::<_, String>(1), result.sha256);
    crate::reconcile_package_data_access::reconcile_package_data_access(
        ReconcilePackageDataAccessRequest {
            packages: vec![fixture_root(&candidate)],
            database_url: fixture.source_database.url().to_owned(),
            org: "upgrade-proof".into(),
            project: "fixture".into(),
            environment: "dev".into(),
        },
    )
    .await
    .unwrap();
    let reconciled = state(&mut fixture.source).await;
    assert_eq!(reconciled.1, evidence.post_privileges);
    let replay = crate::apply_package::apply_package(apply_request(
        fixture.source_database.url(),
        &candidate,
    ))
    .await
    .unwrap();
    assert_eq!(replay.migrations_applied, 0);
    assert!(!replay.changed);
    assert_eq!(state(&mut fixture.source).await, reconciled);
}

#[tokio::test]
async fn copied_suffix_retains_rows_and_rolls_back_failed_execution() {
    let mut fixture = fixture("suffix", false).await;
    let original_source = state(&mut fixture.source).await;
    let scratch = copied_source(&mut fixture).await;
    let mut copy = connect(scratch.url()).await;
    let original_copy = state(&mut copy).await;
    let original_rows = retained_rows(&copy).await;
    let package = fixture.root.join("candidate");
    let suffix_ordinal: u32 = fixture
        .suffix
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .split_once('_')
        .unwrap()
        .0
        .parse()
        .unwrap();
    let failure = package.join(format!("migrations/{:04}_failure.sql", suffix_ordinal + 1));
    fs::write(
        &failure,
        "ALTER TABLE inventory.widget_maker ADD COLUMN note text;\n",
    )
    .unwrap();
    wamn_test_infrastructure::fixture_digest::write_receipt(&package);
    let error = apply_qualification_package(apply_request(scratch.url(), &package))
        .await
        .unwrap_err();
    assert!(
        error.chain().any(|error| error
            .downcast_ref::<tokio_postgres::Error>()
            .and_then(tokio_postgres::Error::code)
            == Some(&tokio_postgres::error::SqlState::DUPLICATE_COLUMN)),
        "{error:#}"
    );
    assert_eq!(state(&mut copy).await, original_copy);
    fs::remove_file(failure).unwrap();
    wamn_test_infrastructure::fixture_digest::write_receipt(&package);
    let outcome = apply_qualification_package(apply_request(scratch.url(), &package))
        .await
        .unwrap();
    assert_eq!(outcome.migrations_applied, 1);
    assert_eq!(retained_rows(&copy).await, original_rows);
    let rows = copy.query_one("SELECT (SELECT count(*) FROM inventory.widget), (SELECT count(*) FROM inventory.widget_tag), name, note FROM inventory.widget_maker", &[]).await.unwrap();
    assert_eq!(rows.get::<_, i64>(0), 2);
    assert_eq!(rows.get::<_, i64>(1), 1);
    assert_eq!(rows.get::<_, String>(2), "retained maker");
    assert_eq!(rows.get::<_, Option<String>>(3), None);
    assert_eq!(state(&mut fixture.source).await, original_source);
    drop(copy);
    scratch.finish().await.unwrap();
}

#[tokio::test]
async fn whole_row_failure_is_refused_before_grants_that_would_fix_it() {
    let mut fixture = fixture("whole-row", true).await;
    let original = state(&mut fixture.source).await;
    let error = qualify_upgrade(request(&fixture, "refused.json"))
        .await
        .unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("before candidate grant reconciliation"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("widget/list@{PREDECESSOR_VERSION}"))
            && diagnostic.contains("42501"),
        "{diagnostic}"
    );
    assert!(!fixture.root.join("refused.json").exists());
    assert_eq!(state(&mut fixture.source).await, original);

    let scratch = copied_source(&mut fixture).await;
    let package = fixture.root.join("candidate");
    apply_qualification_package(apply_request(scratch.url(), &package))
        .await
        .unwrap();
    let mut copy = connect(scratch.url()).await;
    plan_predecessor(&mut copy, &fixture.manifest, &runtime())
        .await
        .unwrap_err();
    reconcile_for_upgrade(ReconcilePackageDataAccessRequest {
        packages: vec![fixture_root(&package)],
        database_url: scratch.url().to_owned(),
        org: "upgrade-proof".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    })
    .await
    .unwrap();
    plan_predecessor(&mut copy, &fixture.manifest, &runtime())
        .await
        .unwrap();
    assert_eq!(state(&mut fixture.source).await, original);
    drop(copy);
    scratch.finish().await.unwrap();
}

#[test]
fn the_predecessor_release_must_name_the_installed_versions() {
    let installed = [PackageIdentity {
        package_id: "platform_fixture".to_owned(),
        package_version: PREDECESSOR_VERSION.to_owned(),
        predecessor_version: None,
        artifact_digest: String::new(),
        migrations: Vec::new(),
    }];
    require_installed_release(
        "sha256:a",
        [("platform_fixture", PREDECESSOR_VERSION)],
        &installed,
    )
    .unwrap();
    let older = require_installed_release("sha256:a", [("platform_fixture", "2.0.0")], &installed)
        .unwrap_err()
        .to_string();
    assert_eq!(
        older,
        "predecessor release sha256:a names platform_fixture@2.0.0, but platform_fixture@2.1.0 is installed"
    );
    let missing = require_installed_release("sha256:a", [("other", "1.0.0")], &installed)
        .unwrap_err()
        .to_string();
    assert_eq!(
        missing,
        "predecessor release sha256:a names other@1.0.0, which is not installed"
    );
}

/// `--predecessor-release` names a row of `catalog.releases`. A digest that
/// no row holds refuses before qualification writes evidence or changes the
/// source (docs/plan/platform-deploy.md §13).
#[tokio::test]
async fn a_predecessor_release_outside_catalog_releases_refuses() {
    let mut fixture = fixture("unknown-predecessor", false).await;
    let original = state(&mut fixture.source).await;
    let unknown = format!("sha256:{}", "f".repeat(64));
    let mut qualification = request(&fixture, "unknown-predecessor.json");
    qualification.predecessor_release.clone_from(&unknown);
    let error = qualify_upgrade(qualification).await.unwrap_err();
    assert!(
        format!("{error:#}").contains(&format!("catalog.releases holds no release {unknown}")),
        "{error:#}"
    );
    assert!(!fixture.root.join("unknown-predecessor.json").exists());
    assert_eq!(state(&mut fixture.source).await, original);
}
