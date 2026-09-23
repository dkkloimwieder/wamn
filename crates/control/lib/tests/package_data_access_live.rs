//! Disposable-PG18 test for installed-set generated data authority.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::reconcile_package_data_access::{self, ReconcilePackageDataAccessRequest};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_test_infrastructure::locked_database;

const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const OVERLAY_EVIDENCE_PATH: &str = "generated/platform-policy/data-access.json";
const TENANT: &str = "package-data-access-live";
const PASSWORD: &str = "package-data-access-live-password";
/// The relation maps apply-package writes beside the application tables.
const APPLIER_OWNED_RELATIONS: [&str; 2] = ["wamn_entities", "wamn_cdc_exclusions"];

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(connection);
    client
}

fn fixture_package_root() -> PathBuf {
    wamn_fixture_package::package_root()
}

fn overlay_package_root() -> PathBuf {
    wamn_fixture_package::overlay_root()
}

fn generation_url(admin_url: &str, role: &str) -> String {
    let mut url = Url::parse(admin_url).expect("parse disposable PostgreSQL URL");
    url.set_username(role).expect("set generation role");
    url.set_password(Some(PASSWORD))
        .expect("set generation password");
    url.into()
}

fn reconcile_args(url: &str, packages: Vec<PathBuf>) -> ReconcilePackageDataAccessRequest {
    ReconcilePackageDataAccessRequest {
        packages,
        database_url: url.to_owned(),
        tenant: TENANT.to_owned(),
    }
}

async fn acl_identity(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT identity FROM ( \
               SELECT 'schema:' || namespace.nspname || ':' || namespace.xmin::text AS identity \
                 FROM pg_catalog.pg_namespace AS namespace \
                WHERE namespace.nspname = 'inventory' \
               UNION ALL \
               SELECT 'table:' || relation.relname || ':' || relation.xmin::text \
                 FROM pg_catalog.pg_class AS relation \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                WHERE namespace.nspname = 'inventory' AND relation.relname IN ( \
                    'widget', 'widget_command', 'widget_maker', \
                    'unconsumed_relation', 'unconsumed_sequence', 'unconsumed_view') \
               UNION ALL \
               SELECT 'column:' || relation.relname || ':' || attribute.attname || ':' || attribute.xmin::text \
                 FROM pg_catalog.pg_attribute AS attribute \
                 JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                WHERE namespace.nspname = 'inventory' AND relation.relname IN ( \
                    'widget', 'widget_command', 'widget_maker', \
                    'unconsumed_relation', 'unconsumed_sequence', 'unconsumed_view') \
                  AND attribute.attnum > 0 AND NOT attribute.attisdropped \
             ) AS observed ORDER BY identity COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read ACL-bearing catalog identities")
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

/// Every privilege the App role reaches on the applier-owned relation maps.
///
/// This is a measurement, not an assumption. The maps carry no seeded grant, so
/// an empty answer before reconciliation states that no guest path reads them.
/// A non-empty answer is a finding about the applier, and the assertion that
/// reads it says so.
async fn applier_owned_authority(client: &Client) -> Vec<String> {
    let table_privileges = vec![
        "DELETE",
        "INSERT",
        "MAINTAIN",
        "REFERENCES",
        "SELECT",
        "TRIGGER",
        "TRUNCATE",
        "UPDATE",
    ];
    let column_privileges = vec!["INSERT", "REFERENCES", "SELECT", "UPDATE"];
    client
        .query(
            "SELECT held FROM ( \
               SELECT relation.relname || ':' || privilege AS held \
                 FROM pg_catalog.pg_class AS relation \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                 CROSS JOIN unnest($1::text[]) AS privilege \
                WHERE namespace.nspname = 'inventory' \
                  AND relation.relname = ANY($3::text[]) \
                  AND pg_catalog.has_table_privilege('wamn_app', relation.oid, privilege) \
               UNION ALL \
               SELECT relation.relname || ':' || attribute.attname || ':' || privilege \
                 FROM pg_catalog.pg_class AS relation \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                 JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
                 CROSS JOIN unnest($2::text[]) AS privilege \
                WHERE namespace.nspname = 'inventory' \
                  AND relation.relname = ANY($3::text[]) \
                  AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                  AND pg_catalog.has_column_privilege( \
                        'wamn_app', relation.oid, attribute.attnum, privilege) \
             ) AS observed ORDER BY held COLLATE \"C\"",
            &[
                &table_privileges,
                &column_privileges,
                &APPLIER_OWNED_RELATIONS.as_slice(),
            ],
        )
        .await
        .expect("read App authority on the applier-owned relation maps")
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn installed_package_set_unions_a_real_app_generation_and_replays_noop() {
    let url = locked_database::database(wamn_test_postgres::database);
    let admin = connect(&url).await;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .expect("read disposable database")
        .get(0);
    let generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("App generation accepts tenant scope");
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS inventory CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{generation}') THEN \
                 EXECUTE format('DROP OWNED BY %I', '{generation}'); \
                 EXECUTE format('DROP ROLE %I', '{generation}'); \
               END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 EXECUTE 'DROP OWNED BY wamn_app'; \
                 EXECUTE 'DROP ROLE wamn_app'; \
               END IF; \
               CREATE ROLE wamn_app NOLOGIN; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
             END $reset$;"
        ))
        .await
        .expect("reset package data-access fixture");
    admin
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .expect("ensure the production package-owner role");
    admin
        .batch_execute(
            "DO $grant$ BEGIN \
               EXECUTE format(\
                 'GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()\
               ); \
             END $grant$;",
        )
        .await
        .expect("grant the package-owner role its production-equivalent database authority");
    admin
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install package catalog");
    admin
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install application authorization floor");
    apply_package::apply_package(ApplyPackageRequest {
        package: fixture_package_root(),
        database_url: url.to_string(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply the fixture package before policy");
    apply_package::apply_package(ApplyPackageRequest {
        package: overlay_package_root(),
        database_url: url.to_string(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply client overlay package before policy");
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            &database,
            &generation,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await
        .expect("prepare production App generation");
    admin
        .batch_execute(
            "ALTER TABLE inventory.widget_maker ADD COLUMN undeclared_note text; \
             GRANT DELETE ON TABLE inventory.widget_maker TO wamn_app; \
             GRANT UPDATE (name) ON TABLE inventory.widget_maker TO wamn_app; \
             GRANT SELECT (undeclared_note) ON TABLE inventory.widget_maker TO wamn_app; \
             GRANT SELECT (name) ON TABLE inventory.widget_maker TO PUBLIC;",
        )
        .await
        .expect("seed direct ACL residue");
    // Negative controls. The admin role mints three relations in the
    // package-owned schema that no package declares, one of each carrier the
    // sweep reads, and grants the App role authority on each. Showing the
    // absence of authority afterwards needs a relation that no package speaks
    // for. Every declared relation fails that test by construction.
    admin
        .batch_execute(
            "CREATE TABLE inventory.unconsumed_relation ( \
                 id uuid PRIMARY KEY DEFAULT gen_random_uuid(), note text); \
             CREATE VIEW inventory.unconsumed_view AS SELECT id FROM inventory.widget_maker; \
             CREATE SEQUENCE inventory.unconsumed_sequence; \
             GRANT SELECT ON TABLE inventory.unconsumed_relation TO wamn_app; \
             GRANT UPDATE (note) ON TABLE inventory.unconsumed_relation TO wamn_app; \
             GRANT SELECT ON TABLE inventory.unconsumed_view TO wamn_app; \
             GRANT USAGE ON SEQUENCE inventory.unconsumed_sequence TO wamn_app;",
        )
        .await
        .expect("seed authority on relations no package declares");
    // The applier-owned relation maps carry no seed. The sweep reaches them like
    // every other relation in the schema, and this reading states what it finds
    // there before it runs.
    assert_eq!(
        applier_owned_authority(&admin).await,
        Vec::<String>::new(),
        "a guest path already reaches an applier-owned relation map, which is a \
         finding about apply-package rather than a reason to exclude the map"
    );

    let incomplete = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![fixture_package_root()],
    ))
    .await
    .expect_err("one package cannot reconcile a two-package installed set");
    assert!(
        incomplete
            .to_string()
            .contains("package-data-access-installed-set-mismatch"),
        "incomplete installed set did not carry its typed refusal: {incomplete:#}"
    );

    let installed_packages = vec![fixture_package_root(), overlay_package_root()];
    let first_effect = reconcile_package_data_access::reconcile_package_data_access(
        reconcile_args(&url, installed_packages.clone()),
    )
    .await
    .expect("apply installed-set data-access union");
    assert!(
        !first_effect.is_noop(),
        "residual ACL did not require repair"
    );
    assert_eq!(
        applier_owned_authority(&admin).await,
        Vec::<String>::new(),
        "reconciliation left App authority on an applier-owned relation map"
    );
    let guest = connect(&generation_url(&url, &generation)).await;
    guest
        .query("SELECT id FROM inventory.widget_maker", &[])
        .await
        .expect("generated SELECT field is reachable through a real App generation");
    guest
        .execute(
            "UPDATE inventory.widget \
                SET maker_id = maker_id, edit_version = edit_version \
              WHERE false",
            &[],
        )
        .await
        .expect("generated writable and revision fields are reachable");
    guest
        .query("SELECT id FROM inventory.widget FOR KEY SHARE", &[])
        .await
        .expect("generated lock carrier permits the declared row lock");
    guest
        .query("SELECT overlay_note FROM inventory.widget", &[])
        .await
        .expect("overlay SELECT survives beside base authority");
    let delete = guest
        .execute("DELETE FROM inventory.widget_maker WHERE false", &[])
        .await
        .expect_err("residual table DELETE survived reconciliation");
    assert_eq!(
        delete.as_db_error().map(|error| error.code().code()),
        Some("42501")
    );
    // undeclared_note is added to the live relation by the seeding above and
    // no manifest declares it, so it is a real undeclared column.
    let undeclared_column = guest
        .query("SELECT undeclared_note FROM inventory.widget_maker", &[])
        .await
        .expect_err("the undeclared column remained readable");
    assert_eq!(
        undeclared_column
            .as_db_error()
            .map(|error| error.code().code()),
        Some("42501")
    );
    // The arm used to name a relation both packages declare, and whose
    // select fields carry id. It asserted that a declared read fails. The subject
    // is now a relation no package declares, so the arm shows the absence of
    // authority instead of fabricating it.
    for unconsumed in [
        "SELECT id FROM inventory.unconsumed_relation",
        "SELECT note FROM inventory.unconsumed_relation",
        "SELECT id FROM inventory.unconsumed_view",
        "SELECT nextval('inventory.unconsumed_sequence')",
    ] {
        let denied = guest
            .query(unconsumed, &[])
            .await
            .expect_err("seeded authority survived on a relation no package declares");
        assert_eq!(
            denied.as_db_error().map(|error| error.code().code()),
            Some("42501"),
            "{unconsumed} kept its seeded authority"
        );
    }
    for control_relation in ["wamn_entities", "wamn_cdc_exclusions"] {
        let denied = guest
            .query(&format!("SELECT * FROM inventory.{control_relation}"), &[])
            .await
            .expect_err("package data authority reached a control-owned relation map");
        assert_eq!(
            denied.as_db_error().map(|error| error.code().code()),
            Some("42501"),
            "{control_relation} became package data authority"
        );
    }
    let overlay_delete = guest
        .execute("DELETE FROM inventory.widget_maker WHERE false", &[])
        .await
        .expect_err("overlay residue survived installed-set reconciliation");
    assert_eq!(
        overlay_delete
            .as_db_error()
            .map(|error| error.code().code()),
        Some("42501")
    );

    let first = acl_identity(&admin).await;
    let again = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        installed_packages,
    ))
    .await
    .expect("replay installed-set data-access union");
    assert!(again.is_noop(), "replay did not report convergence");
    assert_eq!(
        acl_identity(&admin).await,
        first,
        "replay rewrote ACL state"
    );
}

fn lineage_fixture_directory() -> PathBuf {
    std::env::temp_dir().join("wamn-ctl-package-lineage-live")
}

fn manifest_sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
    )
}

/// Stage one package root under the lineage fixture directory.
///
/// This helper copies the shipped root and writes the hash of the staged
/// manifest, because a `bump` rewrites that manifest and the evidence must
/// follow the copy rather than the shipped file. A `bump` also moves the
/// coordinate, which is how an author recovers from a stage that refused an
/// already published version.
fn stage_package_root(source: &Path, name: &str, bump: Option<(&str, &str)>) -> (PathBuf, String) {
    let root = lineage_fixture_directory().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("migrations")).expect("create staged migrations directory");
    std::fs::create_dir_all(root.join("generated/platform-policy"))
        .expect("create staged evidence directory");
    for entry in std::fs::read_dir(source.join("migrations")).expect("read package migrations") {
        let entry = entry.expect("read one package migration entry");
        std::fs::copy(
            entry.path(),
            root.join("migrations").join(entry.file_name()),
        )
        .expect("copy one package migration");
    }

    let manifest_bytes = std::fs::read(source.join("wamn.json")).expect("read package manifest");
    let manifest_bytes = match bump {
        None => manifest_bytes,
        Some((version, predecessor)) => {
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&manifest_bytes).expect("parse package manifest");
            let package = manifest
                .get_mut("package")
                .and_then(serde_json::Value::as_object_mut)
                .expect("package manifest carries a package identity");
            package.insert("version".to_owned(), serde_json::json!(version));
            package.insert(
                "predecessor_version".to_owned(),
                serde_json::json!(predecessor),
            );
            wamn_execution_contract::canonical_json_bytes(&manifest)
        }
    };
    let staged_sha256 = manifest_sha256(&manifest_bytes);
    std::fs::write(root.join("wamn.json"), &manifest_bytes).expect("write staged manifest");

    let mut overlay: serde_json::Value = serde_json::from_slice(
        &std::fs::read(source.join(OVERLAY_EVIDENCE_PATH)).expect("read package evidence"),
    )
    .expect("parse package evidence");
    let overlay_object = overlay
        .as_object_mut()
        .expect("package evidence is an object");
    if let Some((version, _)) = bump {
        let package: String = overlay_object
            .get("package")
            .and_then(serde_json::Value::as_str)
            .expect("package evidence names its coordinate")
            .split('@')
            .next()
            .expect("a coordinate always carries a package id")
            .to_owned();
        overlay_object.insert(
            "package".to_owned(),
            serde_json::json!(format!("{package}@{version}")),
        );
    }
    overlay_object.insert(
        "manifest_sha256".to_owned(),
        serde_json::json!(staged_sha256),
    );
    std::fs::write(
        root.join(OVERLAY_EVIDENCE_PATH),
        wamn_execution_contract::canonical_json_bytes(&overlay),
    )
    .expect("write staged evidence");
    (root, staged_sha256)
}

async fn install_lineage_fixture(url: &str) -> Client {
    let admin = connect(url).await;
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS inventory CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 EXECUTE 'DROP OWNED BY wamn_app'; \
                 EXECUTE 'DROP ROLE wamn_app'; \
               END IF; \
               CREATE ROLE wamn_app NOLOGIN; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
             END $reset$;",
        )
        .await
        .expect("reset package lineage fixture");
    admin
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .expect("ensure the production package-owner role");
    admin
        .batch_execute(
            "DO $grant$ BEGIN \
               EXECUTE format(\
                 'GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()\
               ); \
             END $grant$;",
        )
        .await
        .expect("grant the package-owner role its production-equivalent database authority");
    admin
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install package catalog");
    admin
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install application authorization floor");
    admin
}

async fn apply(url: &str, package: PathBuf) {
    apply_package::apply_package(ApplyPackageRequest {
        package,
        database_url: url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply one package coordinate");
}

#[tokio::test]
async fn an_author_recovers_from_a_failed_version_bump_in_either_direction() {
    let url = locked_database::database(wamn_test_postgres::database);
    let admin = install_lineage_fixture(&url).await;
    let (released, _) = stage_package_root(&fixture_package_root(), "fixture", None);
    let (overlay, _) = stage_package_root(&overlay_package_root(), "overlay", None);
    let (bumped, bumped_sha256) = stage_package_root(
        &fixture_package_root(),
        "fixture-bumped",
        Some(("1.1.0", "1.0.0")),
    );
    apply(&url, released.clone()).await;
    apply(&url, overlay.clone()).await;

    // The failed run applied the bumped coordinate and then refused later on.
    // Both coordinates of one package are now applied, and no source tree can
    // present them together.
    apply(&url, bumped.clone()).await;
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM catalog.packages \
                  WHERE tenant_id = $1 AND package_id = 'platform_fixture'",
                &[&TENANT],
            )
            .await
            .expect("count the applied fixture coordinates")
            .get::<_, i64>(0),
        2,
        "the failed bump did not leave two applied coordinates"
    );

    let reverted = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![released.clone(), overlay.clone()],
    ))
    .await
    .expect("a tree carrying only the earlier version reconciles beside the applied bump");
    assert!(
        !reverted.is_noop(),
        "the reverted tree did not converge the generated authority"
    );

    let forward = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![bumped.clone(), overlay.clone()],
    ))
    .await
    .expect("a tree carrying only the newer version reconciles beside the applied predecessor");
    assert!(
        forward.is_noop(),
        "the same authority union changed under the newer coordinate"
    );

    // A published version stays immutable. Moving the bytes under an applied
    // coordinate is still refused, and the refusal names that coordinate.
    let mut moved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bumped.join("wamn.json")).expect("read manifest"))
            .expect("parse manifest");
    moved
        .get_mut("package")
        .and_then(serde_json::Value::as_object_mut)
        .expect("the staged manifest carries a package identity")
        .insert("predecessor_version".to_owned(), serde_json::json!("0.9.0"));
    let moved_bytes = wamn_execution_contract::canonical_json_bytes(&moved);
    let moved_sha256 = manifest_sha256(&moved_bytes);
    assert_ne!(
        moved_sha256, bumped_sha256,
        "the manifest bytes did not move"
    );
    std::fs::write(bumped.join("wamn.json"), &moved_bytes).expect("move the manifest bytes");
    let mut evidence: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bumped.join(OVERLAY_EVIDENCE_PATH)).expect("read staged evidence"),
    )
    .expect("parse staged evidence");
    evidence
        .as_object_mut()
        .expect("package evidence is an object")
        .insert(
            "manifest_sha256".to_owned(),
            serde_json::json!(moved_sha256),
        );
    std::fs::write(
        bumped.join(OVERLAY_EVIDENCE_PATH),
        wamn_execution_contract::canonical_json_bytes(&evidence),
    )
    .expect("regenerate evidence for the moved manifest");
    let drift = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![bumped.clone(), overlay.clone()],
    ))
    .await
    .expect_err("moved bytes reconciled under an already applied version");
    assert!(
        drift
            .to_string()
            .contains("package-data-access-source-drift: package=platform_fixture@1.1.0"),
        "the immutable coordinate did not carry its drift refusal: {drift:#}"
    );

    std::fs::remove_dir_all(lineage_fixture_directory())
        .expect("remove the package lineage fixture");
}

#[tokio::test]
async fn reconciliation_leaves_every_platform_schema_grant_on_the_app_role_standing() {
    let url = locked_database::database(wamn_test_postgres::database);
    let admin = install_lineage_fixture(&url).await;
    apply(&url, fixture_package_root()).await;
    apply(&url, overlay_package_root()).await;
    // app_system and catalog are platform schemas. No package manifest names
    // them, so the reconciler never reads a relation there. The floor grants the
    // two schema files install stand outside every package declaration, and so
    // does this seeded grant.
    admin
        .batch_execute("GRANT UPDATE (tenant_id) ON TABLE catalog.packages TO wamn_app;")
        .await
        .expect("seed App authority inside the platform schemas");
    reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![fixture_package_root(), overlay_package_root()],
    ))
    .await
    .expect("reconcile the installed package set beside platform-schema authority");

    for (relation, privilege) in [
        ("app_system.users", "SELECT"),
        ("app_system.configurations", "UPDATE"),
        ("catalog.packages", "SELECT"),
    ] {
        assert!(
            admin
                .query_one(
                    "SELECT pg_catalog.has_table_privilege('wamn_app', $1, $2)",
                    &[&relation, &privilege],
                )
                .await
                .expect("read platform-schema table authority")
                .get::<_, bool>(0),
            "reconciliation revoked {privilege} on {relation}"
        );
    }
    assert!(
        admin
            .query_one(
                "SELECT pg_catalog.has_column_privilege( \
                   'wamn_app', 'catalog.packages', 'tenant_id', 'UPDATE')",
                &[],
            )
            .await
            .expect("read platform-schema column authority")
            .get::<_, bool>(0),
        "reconciliation revoked the seeded platform-schema column grant"
    );
}

/// Stage the fixture application, whose `widget` relation logs.
fn stage_logged_fixture() -> PathBuf {
    let (root, _) = stage_package_root(&fixture_package_root(), "fixture-logged", None);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("wamn.json")).expect("read manifest"))
            .expect("parse manifest");
    assert_eq!(
        manifest["models"]["widget"]["audit_log"],
        serde_json::json!({"columns": ["created_at"], "retention": "P30D"}),
        "the relation logs and stamps its one reserved column"
    );
    root
}

/// Level-2 spec test 12 and ruling 28: a logged relation with no stamp columns
/// writes history entries through the production App role, and the reconciler
/// keeps the derived history insert grant.
#[tokio::test]
async fn a_logged_relation_writes_history_through_the_reconciled_app_role() {
    let url = locked_database::database(wamn_test_postgres::database);
    let admin = install_lineage_fixture(&url).await;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .expect("read disposable database")
        .get(0);
    let package = stage_logged_fixture();
    apply(&url, package.clone()).await;
    let generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("App generation accepts tenant scope");
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            &database,
            &generation,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await
        .expect("prepare production App generation");
    let first = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![package.clone()],
    ))
    .await
    .expect("reconcile the logged package");
    assert!(!first.is_noop(), "the logged package needed its grants");

    let history_privileges = || async {
        admin
            .query(
                "SELECT held FROM ( \
                   SELECT attribute.attname::text || ':' || privilege AS held \
                     FROM pg_catalog.pg_attribute AS attribute \
                     CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'REFERENCES']) \
                          AS privilege \
                    WHERE attribute.attrelid = 'inventory.widget_history'::regclass \
                      AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                      AND pg_catalog.has_column_privilege( \
                            'wamn_app', attribute.attrelid, attribute.attnum, privilege) \
                 ) AS observed ORDER BY held COLLATE \"C\"",
                &[],
            )
            .await
            .expect("read App authority on the history table")
            .into_iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>()
    };
    let expected_privileges = [
        "after:INSERT",
        "before:INSERT",
        "changed_at:INSERT",
        "changed_by:INSERT",
        "kind:INSERT",
        "operation:INSERT",
        "row_key:INSERT",
        "transaction_id:INSERT",
    ];
    assert_eq!(history_privileges().await, expected_privileges);

    admin
        .batch_execute(
            "BEGIN; \
             SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), \
                    set_config('app.operation', 'admin:seed-history-fixture', true); \
             INSERT INTO inventory.widget_maker (id, name) \
               VALUES ('00000000-0000-4000-8000-00000000b004', 'history-maker'); \
             INSERT INTO inventory.widget (id, code, maker_id) \
               VALUES ('00000000-0000-4000-8000-00000000b003', 'priority', \
                       '00000000-0000-4000-8000-00000000b004'); \
             COMMIT;",
        )
        .await
        .expect("seed the logged relation as the fixture principal");

    let guest = connect(&generation_url(&url, &generation)).await;
    guest
        .batch_execute(
            "BEGIN; \
             SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f2', true), \
                    set_config('app.operation', 'platform-fixture:widget/update@1.0.0', true); \
             UPDATE inventory.widget SET note = 'logged' \
              WHERE id = '00000000-0000-4000-8000-00000000b003'; \
             COMMIT;",
        )
        .await
        .expect("the App role writes a logged relation with its reconciled grants");
    let entries = admin
        .query(
            "SELECT kind, operation, changed_by::text, row_key::text, before::text, after::text \
               FROM inventory.widget_history ORDER BY position",
            &[],
        )
        .await
        .expect("read the history entries")
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
                row.get::<_, String>(4),
                row.get::<_, String>(5),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        2,
        "one entry for each row change: {entries:?}"
    );
    assert_eq!(
        (
            entries[0].0.as_str(),
            entries[0].1.as_str(),
            entries[0].4.as_str()
        ),
        ("insert", "admin:seed-history-fixture", "{}")
    );
    assert_eq!(
        entries[1],
        (
            "update".to_owned(),
            "platform-fixture:widget/update@1.0.0".to_owned(),
            "00000000-0000-4000-8000-0000000000f2".to_owned(),
            r#"{"id": "00000000-0000-4000-8000-00000000b003"}"#.to_owned(),
            r#"{"note": null}"#.to_owned(),
            r#"{"note": "logged"}"#.to_owned(),
        )
    );
    let stamps = admin
        .query_one(
            "SELECT count(*) FROM pg_catalog.pg_attribute \
              WHERE attrelid = 'inventory.widget'::regclass \
                AND attname IN ('created_at', 'created_by', 'updated_at', 'updated_by') \
                AND NOT attisdropped",
            &[],
        )
        .await
        .expect("read stamp columns")
        .get::<_, i64>(0);
    assert_eq!(
        stamps, 1,
        "the logged relation carries the one stamp column its audit_log declares"
    );

    for statement in [
        "SELECT kind FROM inventory.widget_history",
        "UPDATE inventory.widget_history SET kind = kind",
        "DELETE FROM inventory.widget_history",
    ] {
        let denied = guest
            .execute(statement, &[])
            .await
            .expect_err("the App role reached a history entry outside the log trigger");
        assert_eq!(
            denied.as_db_error().map(|error| error.code().code()),
            Some("42501"),
            "{statement}"
        );
    }

    let again = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![package],
    ))
    .await
    .expect("replay the logged package reconciliation");
    assert!(again.is_noop(), "the reconciler changed the history grant");
    assert_eq!(history_privileges().await, expected_privileges);

    std::fs::remove_dir_all(lineage_fixture_directory().join("fixture-logged"))
        .expect("remove the logged package fixture");
}

/// The full comment of the connected database.
async fn database_comment(client: &Client) -> Option<String> {
    client
        .query_one(
            "SELECT pg_catalog.shobj_description(oid, 'pg_database') \
               FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()",
            &[],
        )
        .await
        .expect("read the database comment")
        .get(0)
}

/// The manifest hashes the local target comment records.
async fn recorded_manifests(client: &Client) -> BTreeMap<String, String> {
    let comment = database_comment(client)
        .await
        .expect("the target carries a comment");
    wamn_runtime::local_application::parse_local_target_comment(&comment)
        .expect("parse the local target comment")
        .manifests
}

/// Change the staged manifest bytes without changing what the package declares.
fn change_manifest_bytes(root: &Path) -> String {
    let path = root.join("wamn.json");
    let mut bytes = std::fs::read(&path).expect("read the staged manifest");
    bytes.push(b'\n');
    std::fs::write(&path, &bytes).expect("write the changed manifest");
    let changed = manifest_sha256(&bytes);
    let mut evidence: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join(OVERLAY_EVIDENCE_PATH)).expect("read staged evidence"),
    )
    .expect("parse staged evidence");
    evidence
        .as_object_mut()
        .expect("package evidence is an object")
        .insert("manifest_sha256".to_owned(), serde_json::json!(changed));
    std::fs::write(
        root.join(OVERLAY_EVIDENCE_PATH),
        wamn_execution_contract::canonical_json_bytes(&evidence),
    )
    .expect("regenerate evidence for the changed manifest");
    changed
}

/// Owner ruling 8 of wamn-ri4b: the local grant reconcile at Activate records
/// the presented manifest hash of each package in the local target comment, and
/// a changed wamn.json at an applied coordinate no longer refuses there.
#[tokio::test]
async fn the_local_grant_reconcile_records_the_presented_manifest_hash() {
    const ENVIRONMENT: &str = "development";
    let url = locked_database::database(wamn_test_postgres::database);
    let admin = install_lineage_fixture(&url).await;
    let (fixture, first) = stage_package_root(&fixture_package_root(), "fixture-local", None);
    let (overlay, overlay_sha256) =
        stage_package_root(&overlay_package_root(), "overlay-local", None);
    apply(&url, fixture.clone()).await;
    apply(&url, overlay.clone()).await;

    let instance: u32 = admin
        .query_one(
            "SELECT oid FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()",
            &[],
        )
        .await
        .expect("read the database instance")
        .get(0);
    let marker =
        wamn_runtime::local_application::local_target_marker(TENANT, ENVIRONMENT, instance);
    admin
        .batch_execute(&format!(
            "DO $comment$ BEGIN \
               EXECUTE format('COMMENT ON DATABASE %I IS %L', current_database(), {}::text); \
             END $comment$;",
            wamn_pg_core::quote_literal(&marker)
        ))
        .await
        .expect("mark the local target");

    let roots = vec![fixture.clone(), overlay.clone()];
    let reconcile = |apply: bool| {
        let prepared = reconcile_package_data_access::prepare_local(&roots)
            .expect("prepare the presented local packages");
        let url = url.to_string();
        async move {
            reconcile_package_data_access::reconcile_local(
                &prepared,
                &url,
                TENANT,
                ENVIRONMENT,
                apply,
            )
            .await
        }
    };

    // A validating reconcile rolls back, so it records nothing.
    reconcile(false)
        .await
        .expect("validate the local grants before the cutover");
    assert_eq!(database_comment(&admin).await, Some(marker.clone()));

    reconcile(true).await.expect("apply the local grants");
    assert_eq!(
        recorded_manifests(&admin).await,
        BTreeMap::from([
            ("platform_fixture@1.0.0".to_owned(), first.clone()),
            ("platform_fixture_overlay@1.0.0".to_owned(), overlay_sha256),
        ]),
        "the comment does not name the presented manifest of each package"
    );

    // An operations-only wamn.json edit at an applied coordinate reconciles
    // with no Migrate, and the comment follows the presented manifest.
    let second = change_manifest_bytes(&fixture);
    assert_ne!(second, first, "the manifest bytes did not move");
    reconcile(true)
        .await
        .expect("the local reconcile takes a changed manifest at an applied coordinate");
    assert_eq!(
        recorded_manifests(&admin)
            .await
            .get("platform_fixture@1.0.0"),
        Some(&second),
        "the comment kept a manifest hash the application no longer came from"
    );
    assert_eq!(
        admin
            .query_one(
                "SELECT manifest_sha256 FROM catalog.packages \
                  WHERE tenant_id = $1 AND package_id = 'platform_fixture' \
                    AND package_version = '1.0.0'",
                &[&TENANT],
            )
            .await
            .expect("read the immutable package row")
            .get::<_, String>(0),
        first,
        "catalog.packages keeps the first recorded manifest hash"
    );

    // The production path still refuses the same moved bytes.
    let drift = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        roots.clone(),
    ))
    .await
    .expect_err("moved bytes reconciled under an applied coordinate");
    assert!(
        drift
            .to_string()
            .contains("package-data-access-source-drift: package=platform_fixture@1.0.0"),
        "the production path did not refuse the moved bytes: {drift:#}"
    );

    for root in &roots {
        std::fs::remove_dir_all(root).expect("remove the local reconcile fixture");
    }
}
