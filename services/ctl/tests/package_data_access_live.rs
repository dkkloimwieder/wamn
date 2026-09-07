//! Disposable-PG18 proof for installed-set generated data authority.

mod support;

use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::reconcile_package_data_access::{self, ReconcilePackageDataAccessArgs};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const APP_SCHEMA: &str = include_str!("../../../deploy/sql/app-schema.sql");
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

fn receiving_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving")
}

fn overlay_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/client_acme_receiving")
}

fn generation_url(admin_url: &str, role: &str) -> String {
    let mut url = Url::parse(admin_url).expect("parse disposable PostgreSQL URL");
    url.set_username(role).expect("set generation role");
    url.set_password(Some(PASSWORD))
        .expect("set generation password");
    url.into()
}

fn reconcile_args(url: &str, packages: Vec<PathBuf>) -> ReconcilePackageDataAccessArgs {
    ReconcilePackageDataAccessArgs {
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
                WHERE namespace.nspname = 'receiving' \
               UNION ALL \
               SELECT 'table:' || relation.relname || ':' || relation.xmin::text \
                 FROM pg_catalog.pg_class AS relation \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                WHERE namespace.nspname = 'receiving' AND relation.relname IN ( \
                    'item', 'location', 'purchase_order', 'purchase_order_line', \
                    'quality_inspection', 'record_receipt_command', 'receipt', 'receipt_line', \
                    'unconsumed_relation', 'unconsumed_sequence', 'unconsumed_view') \
               UNION ALL \
               SELECT 'column:' || relation.relname || ':' || attribute.attname || ':' || attribute.xmin::text \
                 FROM pg_catalog.pg_attribute AS attribute \
                 JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                WHERE namespace.nspname = 'receiving' AND relation.relname IN ( \
                    'item', 'location', 'purchase_order', 'purchase_order_line', \
                    'quality_inspection', 'record_receipt_command', 'receipt', 'receipt_line', \
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
                WHERE namespace.nspname = 'receiving' \
                  AND relation.relname = ANY($3::text[]) \
                  AND pg_catalog.has_table_privilege('wamn_app', relation.oid, privilege) \
               UNION ALL \
               SELECT relation.relname || ':' || attribute.attname || ':' || privilege \
                 FROM pg_catalog.pg_class AS relation \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                 JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
                 CROSS JOIN unnest($2::text[]) AS privilege \
                WHERE namespace.nspname = 'receiving' \
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
#[ignore = "requires disposable PG18 named by WAMN_CTL_PG_URL"]
async fn installed_package_set_unions_a_real_app_generation_and_replays_noop() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping package_data_access_live; WAMN_CTL_PG_URL is unset");
        return;
    };
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
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
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
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install package catalog");
    admin
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install application authorization floor");
    apply_package::run(ApplyPackageArgs {
        package: receiving_package_root(),
        database_url: url.to_string(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply Receiving package before policy");
    apply_package::run(ApplyPackageArgs {
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
            "GRANT DELETE ON TABLE receiving.purchase_order TO wamn_app; \
             GRANT UPDATE (location_code) ON TABLE receiving.location TO wamn_app; \
             GRANT DELETE ON TABLE receiving.quality_inspection TO wamn_app; \
             GRANT SELECT (item_number) ON TABLE receiving.item TO PUBLIC;",
        )
        .await
        .expect("seed direct ACL residue");
    // Negative controls. The admin role mints three relations in the
    // package-owned schema that no package declares, one of each carrier the
    // sweep reads, and grants the App role authority on each. Proving the
    // absence of authority afterwards needs a relation that no package speaks
    // for. Every declared relation fails that test by construction.
    admin
        .batch_execute(
            "CREATE TABLE receiving.unconsumed_relation ( \
                 id uuid PRIMARY KEY DEFAULT gen_random_uuid(), note text); \
             CREATE VIEW receiving.unconsumed_view AS SELECT id FROM receiving.location; \
             CREATE SEQUENCE receiving.unconsumed_sequence; \
             GRANT SELECT ON TABLE receiving.unconsumed_relation TO wamn_app; \
             GRANT UPDATE (note) ON TABLE receiving.unconsumed_relation TO wamn_app; \
             GRANT SELECT ON TABLE receiving.unconsumed_view TO wamn_app; \
             GRANT USAGE ON SEQUENCE receiving.unconsumed_sequence TO wamn_app;",
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
        vec![receiving_package_root()],
    ))
    .await
    .expect_err("one package cannot reconcile a two-package installed set");
    assert!(
        incomplete
            .to_string()
            .contains("package-data-access-installed-set-mismatch"),
        "incomplete installed set did not carry its typed refusal: {incomplete:#}"
    );

    let installed_packages = vec![receiving_package_root(), overlay_package_root()];
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
        .query("SELECT id FROM receiving.location", &[])
        .await
        .expect("generated SELECT field is reachable through a real App generation");
    guest
        .execute(
            "UPDATE receiving.purchase_order \
                SET supplier_id = supplier_id, row_version = row_version \
              WHERE false",
            &[],
        )
        .await
        .expect("generated writable and revision fields are reachable");
    guest
        .query("SELECT id FROM receiving.location FOR KEY SHARE", &[])
        .await
        .expect("generated lock carrier permits the declared row lock");
    guest
        .query(
            "SELECT receipt_id, status, row_version FROM receiving.quality_inspection",
            &[],
        )
        .await
        .expect("overlay SELECT survives beside base authority");
    guest
        .execute(
            "UPDATE receiving.quality_inspection \
                SET status = status, row_version = row_version \
              WHERE false",
            &[],
        )
        .await
        .expect("overlay UPDATE survives beside base authority");
    let delete = guest
        .execute("DELETE FROM receiving.purchase_order WHERE false", &[])
        .await
        .expect_err("residual table DELETE survived reconciliation");
    assert_eq!(
        delete.as_db_error().map(|error| error.code().code()),
        Some("42501")
    );
    // receipt_line is insert-only with select_fields of id alone, so quantity is
    // a real undeclared column. The arm used to name location.location_code,
    // which 0ca9418c declared when it added location.list, so the assertion
    // stated a falsehood from that commit until wamn-10yt.29.
    let undeclared_column = guest
        .query("SELECT quantity FROM receiving.receipt_line", &[])
        .await
        .expect_err("undeclared receipt_line column remained readable");
    assert_eq!(
        undeclared_column
            .as_db_error()
            .map(|error| error.code().code()),
        Some("42501")
    );
    // The arm used to name receiving.item, which both packages declare and whose
    // select fields carry id. It asserted that a declared read fails. The subject
    // is now a relation no package declares, so the arm proves the absence of
    // authority instead of fabricating it.
    for unconsumed in [
        "SELECT id FROM receiving.unconsumed_relation",
        "SELECT note FROM receiving.unconsumed_relation",
        "SELECT id FROM receiving.unconsumed_view",
        "SELECT nextval('receiving.unconsumed_sequence')",
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
            .query(&format!("SELECT * FROM receiving.{control_relation}"), &[])
            .await
            .expect_err("package data authority reached a control-owned relation map");
        assert_eq!(
            denied.as_db_error().map(|error| error.code().code()),
            Some("42501"),
            "{control_relation} became package data authority"
        );
    }
    let overlay_delete = guest
        .execute("DELETE FROM receiving.quality_inspection WHERE false", &[])
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
/// The shipped generated evidence records a manifest hash that its manifest no
/// longer has, because `ce7ffac4` edited every `packages/*/wamn.json` without
/// regenerating `generated/platform-policy/data-access.json`. This helper
/// copies the shipped root and writes the hash the generator writes today. A
/// `bump` also moves the coordinate, which is how an author recovers from a
/// stage that refused an already published version.
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
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
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
    apply_package::run(ApplyPackageArgs {
        package,
        database_url: url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply one package coordinate");
}

#[tokio::test]
#[ignore = "requires disposable PG18 named by WAMN_CTL_PG_URL"]
async fn an_author_recovers_from_a_failed_version_bump_in_either_direction() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping package_data_access_live; WAMN_CTL_PG_URL is unset");
        return;
    };
    let admin = install_lineage_fixture(&url).await;
    let (released, _) = stage_package_root(&receiving_package_root(), "receiving", None);
    let (overlay, _) = stage_package_root(&overlay_package_root(), "overlay", None);
    let (bumped, bumped_sha256) = stage_package_root(
        &receiving_package_root(),
        "receiving-bumped",
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
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving'",
                &[&TENANT],
            )
            .await
            .expect("count the applied Receiving coordinates")
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
            .contains("package-data-access-source-drift: package=wamn_receiving@1.1.0"),
        "the immutable coordinate did not carry its drift refusal: {drift:#}"
    );

    std::fs::remove_dir_all(lineage_fixture_directory())
        .expect("remove the package lineage fixture");
}

#[tokio::test]
#[ignore = "requires disposable PG18 named by WAMN_CTL_PG_URL"]
async fn reconciliation_leaves_every_platform_schema_grant_on_the_app_role_standing() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping package_data_access_live; WAMN_CTL_PG_URL is unset");
        return;
    };
    let admin = install_lineage_fixture(&url).await;
    apply(&url, receiving_package_root()).await;
    apply(&url, overlay_package_root()).await;
    // app_system and catalog are platform schemas. No package manifest names
    // them, so the reconciler never reads a relation there. The floor grants the
    // two schema files install stand outside every package declaration, and so
    // do these two seeded grants.
    admin
        .batch_execute(
            "GRANT DELETE ON TABLE app_system.audit_log TO wamn_app; \
             GRANT UPDATE (tenant_id) ON TABLE catalog.packages TO wamn_app;",
        )
        .await
        .expect("seed App authority inside the platform schemas");
    reconcile_package_data_access::reconcile_package_data_access(reconcile_args(
        &url,
        vec![receiving_package_root(), overlay_package_root()],
    ))
    .await
    .expect("reconcile the installed package set beside platform-schema authority");

    for (relation, privilege) in [
        ("app_system.audit_log", "DELETE"),
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
