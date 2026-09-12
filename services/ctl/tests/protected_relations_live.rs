//! Checks database shapes and author privileges in fresh PostgreSQL 18.
//!
//! Set `WAMN_CTL_PG_URL` to a superuser URL for a disposable database.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};

use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_ROLE, IDENTITY_ISSUER_TABLES, grant_identity_issuer_surface_sql,
};
use wamn_control_provision::sql;
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::reconcile_run_plane;
use wamn_schema_control::BareSchemaName;

const SYSTEM_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const OPS_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/ops-schema.sql");
const CONTROL_PORTABLE_STORE_SQL: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;
const APP_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/app-schema.sql");
const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");
const AUTHOR_SQL_ROLES: [&str; 2] = ["wamn_app", "wamn_control_author"];

#[derive(Debug, PartialEq, Eq)]
struct PortableFingerprint {
    columns: Vec<String>,
    constraints: Vec<String>,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ctl crate is under services/ctl")
        .to_path_buf()
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn prepare_scratch_database(client: &Client) {
    client
        .batch_execute(&format!(
            "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DROP SCHEMA IF EXISTS audit_app CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
                 CREATE ROLE wamn_system LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_system LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_run_projection_writer') THEN \
                 CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_ops') THEN \
                 CREATE ROLE wamn_ops NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_ops NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_control_author') THEN \
                 CREATE ROLE wamn_control_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_control_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
             END $$; \
             REVOKE wamn_scenario_author FROM wamn_app; \
             DO $$ BEGIN \
               EXECUTE format('REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM wamn_effect_writer', current_database()); \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM wamn_run_projection_writer', current_database()); \
               EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_app', current_database()); \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); \
             END $$;"
        ))
        .await
        .expect("prepare disposable database roles");
}

async fn install_control_database(client: &Client) {
    let system_install = format!(
        "SET ROLE wamn_system;\n{SYSTEM_SCHEMA_SQL}\n{CONTROL_PORTABLE_STORE_SQL}\n\
         {OPS_SCHEMA_SQL}\nRESET ROLE;"
    );
    client
        .batch_execute(&system_install)
        .await
        .expect("install canonical control-plane schema");
    client
        .batch_execute(&grant_identity_issuer_surface_sql())
        .await
        .expect("install canonical identity issuer key-table authority");
    // Check the identity issuer privileges, including SELECT and no TRUNCATE.
    for table in IDENTITY_ISSUER_TABLES {
        let relation = format!("identity.{table}");
        let privileges: bool = client
            .query_one(
                "SELECT has_table_privilege($1, $2, 'SELECT') \
                    AND has_table_privilege($1, $2, 'INSERT') \
                    AND has_table_privilege($1, $2, 'UPDATE') \
                    AND has_table_privilege($1, $2, 'DELETE') \
                    AND NOT has_table_privilege($1, $2, 'TRUNCATE')",
                &[&IDENTITY_ISSUER_ROLE, &relation],
            )
            .await
            .expect("read identity issuer key-table privileges")
            .get(0);
        assert!(
            privileges,
            "identity issuer privilege boundary drifted on {relation}"
        );
    }
}

async fn install_project_database(client: &Client, url: &str, repository: &Path) {
    // Package DDL runs as the production database owner, not the administrator.
    client
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .expect("ensure the canonical project database owner");
    let database: String = client
        .query_one("SELECT current_database()", &[])
        .await
        .expect("read the disposable project database name")
        .get(0);
    client
        .batch_execute(&sql::set_database_owner_sql(&database))
        .await
        .expect("assign the canonical project database owner before package installation");
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE",
        )
        .await
        .expect("remove same-qualified control portable schemas before project install");
    client
        .batch_execute(APP_SCHEMA_SQL)
        .await
        .expect("install canonical application schema");

    let schema = BareSchemaName::new("wamn_run").expect("canonical run schema name");
    reconcile_run_plane::reconcile(client, &schema, true)
        .await
        .expect("install canonical project schemas through reconciler");
    // R55 (wamn-0h0g.12.177). The install above is the FRESH path — the control
    // schemas were dropped a few lines up, so every convergence arm sits behind a
    // presence probe that is false. This test reads the reconciled database,
    // so the reconciler must be shown CONVERGED and not
    // merely exited-zero: a second pass over the same database plans nothing.
    let converged = reconcile_run_plane::reconcile(client, &schema, true)
        .await
        .expect("second reconcile over the installed project schemas");
    assert!(
        converged.is_noop(),
        "the project install did not converge; the database still requires changes: {:#?}",
        converged.actions
    );
    // …and the post-check named the PROJECT record, on a database that carried
    // the CONTROL plane's same-qualified schemas moments ago.
    let mut at_target = converged.at_target.clone();
    at_target.sort();
    assert_eq!(
        at_target,
        [
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
            "effect_attempts",
            "environment_policies",
            "operator_run_actions",
            "run_queue",
            "runs",
        ],
        "the converged post-check did not name exactly the project run-plane record"
    );

    apply_package::run(ApplyPackageArgs {
        package: repository.join("apps/wamn_receiving"),
        database_url: url.to_string(),
        tenant: "protected-relation-audit".to_string(),
    })
    .await
    .expect("apply the package-owned Receiving migration stream");

    // The management surface revokes and regrants across `catalog` as well as
    // the run schema, so it must follow the catalog install, not precede it.
    client
        .batch_execute(&sql::grant_management_admitter_surface_sql("wamn_run"))
        .await
        .expect("install canonical management-admission authority");
}

/// Portable relations the control store installs that the project plane also
/// installs, so both copies must carry an identical column and constraint shape.
const SHARED_PORTABLE_RELATIONS: [&str; 5] = [
    "catalog.packages",
    "catalog.package_migrations",
    "catalog.effective_releases",
    "catalog.effective_release_packages",
    "catalog.effective_release_heads",
];

/// Portable relations both planes install whose shapes DELIBERATELY diverge by
/// one column, ruled 2026-09-09 on wamn-10yt.52.
///
/// # What the retired arm asserted
///
/// These two used to sit in [`SHARED_PORTABLE_RELATIONS`], where
/// `assert_eq!(control_shared_fingerprints, project_shared_fingerprints,
/// "control copies drifted from the still-authoritative project column/constraint
/// shapes")` required their control and project copies to be IDENTICAL in every column
/// (number, name, type, nullability, default) and every non-trigger constraint.
/// That equality is GONE for these two relations and is not asserted anywhere
/// else. The control copies key by `environment_instance` and the project copies
/// do not, because a project database is dropped and cloned before every
/// development run and has no second creation to distinguish.
///
/// # What replaces it
///
/// [`assert_diverges_by_environment_instance`] below, which is narrower than the
/// old arm but not nothing: both copies must still exist, and the control copy's
/// columns must be the project copy's plus EXACTLY `environment_instance`. Any
/// other drift between the planes still fails, so the loss is confined to the
/// constraint shapes the added key column necessarily moves.
const DIVERGED_PORTABLE_RELATIONS: [&str; 2] = [
    "catalog.component_library",
    "catalog.connection_requirements",
];

/// The one column the diverged relations are permitted to differ by.
const DIVERGENCE_COLUMN: &str = "environment_instance";

/// Portable relations that live only in the control plane.
/// `install_project_database` DROPs the `catalog` and `wamn_run` schemas
/// project-side and no project installer recreates these, so they have no
/// project fingerprint by design. Comparing them across planes is what
/// manufactured the seven-relation drift this list retires.
const CONTROL_ONLY_PORTABLE_RELATIONS: [&str; 5] = [
    "catalog.authoring_command_audit",
    "catalog.deployment_attestations",
    // The projected environment identity is a CONTROL fact: the project plane
    // has no `registry.project_envs` to project from, and its own catalog is
    // recreated per run (wamn-10yt.38).
    "catalog.tenant_environments",
    "wamn_run.gate_reports",
    "wamn_authority.author_login_tenants",
];

async fn portable_fingerprints(
    client: &Client,
    relations: &[&str],
) -> BTreeMap<String, PortableFingerprint> {
    let mut fingerprints = relations
        .iter()
        .copied()
        .map(|relation| {
            (
                relation.to_string(),
                PortableFingerprint {
                    columns: Vec::new(),
                    constraints: Vec::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for row in client
        .query(
            "SELECT n.nspname || '.' || c.relname, \
                    a.attnum::text || ':' || a.attname || ':' || \
                    pg_catalog.format_type(a.atttypid,a.atttypmod) || ':' || \
                    a.attnotnull::text || ':' || \
                    COALESCE(pg_get_expr(d.adbin,d.adrelid,true),'-') \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid=a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             LEFT JOIN pg_catalog.pg_attrdef d \
               ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
             WHERE a.attnum > 0 AND NOT a.attisdropped \
               AND n.nspname || '.' || c.relname = ANY($1) \
             ORDER BY 1,a.attnum",
            &[&relations],
        )
        .await
        .expect("read shared portable columns")
    {
        let relation: String = row.get(0);
        fingerprints
            .get_mut(&relation)
            .expect("shared relation")
            .columns
            .push(row.get(1));
    }
    for row in client
        .query(
            "SELECT n.nspname || '.' || c.relname, \
                    con.contype::text || ':' || pg_get_constraintdef(con.oid,true) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class c ON c.oid=con.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             WHERE n.nspname || '.' || c.relname = ANY($1) \
               AND con.contype <> 't' \
             ORDER BY 1,2",
            &[&relations],
        )
        .await
        .expect("read shared portable constraints")
    {
        let relation: String = row.get(0);
        fingerprints
            .get_mut(&relation)
            .expect("shared relation")
            .constraints
            .push(row.get(1));
    }
    fingerprints
}

/// Column NAMES of one fingerprint, in catalog order.
///
/// The raw fingerprint entries lead with `attnum`, which shifts for every column
/// after an inserted one, so a divergence test has to compare names rather than
/// those strings.
fn column_names(fingerprint: &PortableFingerprint) -> Vec<String> {
    fingerprint
        .columns
        .iter()
        .map(|column| {
            column
                .split(':')
                .nth(1)
                .expect("a column fingerprint carries attnum:attname:...")
                .to_owned()
        })
        .collect()
}

/// Show the control copy is the project copy plus exactly one key column.
///
/// This is the narrowed successor to the cross-plane equality
/// [`DIVERGED_PORTABLE_RELATIONS`] documents. Both copies must exist, and the
/// only permitted difference is [`DIVERGENCE_COLUMN`]: a control copy that grew
/// a SECOND column the project plane lacks, or lost one it has, still fails.
fn assert_diverges_by_environment_instance(
    control: &BTreeMap<String, PortableFingerprint>,
    project: &BTreeMap<String, PortableFingerprint>,
) {
    for relation in DIVERGED_PORTABLE_RELATIONS {
        let control_columns = column_names(
            control
                .get(relation)
                .unwrap_or_else(|| panic!("{relation} has a control fingerprint")),
        );
        let project_columns = column_names(
            project
                .get(relation)
                .unwrap_or_else(|| panic!("{relation} has a project fingerprint")),
        );
        assert!(
            !control_columns.is_empty() && !project_columns.is_empty(),
            "{relation} must be installed in BOTH planes: control={control_columns:?} \
             project={project_columns:?}"
        );
        assert!(
            control_columns.iter().any(|name| name == DIVERGENCE_COLUMN),
            "{relation} control copy must carry {DIVERGENCE_COLUMN}: {control_columns:?}"
        );
        assert!(
            !project_columns.iter().any(|name| name == DIVERGENCE_COLUMN),
            "{relation} project copy must NOT carry {DIVERGENCE_COLUMN}: {project_columns:?}"
        );
        let without_instance = control_columns
            .iter()
            .filter(|name| name.as_str() != DIVERGENCE_COLUMN)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            without_instance, project_columns,
            "{relation} may diverge across planes by {DIVERGENCE_COLUMN} and nothing else"
        );
    }
}

async fn assert_author_sql_boundaries(client: &Client, control_database: bool) {
    client
        .batch_execute("BEGIN READ ONLY")
        .await
        .expect("start read-only catalog snapshot");
    let read_only: bool = client
        .query_one(
            "SELECT current_setting('transaction_read_only') = 'on'",
            &[],
        )
        .await
        .expect("verify read-only catalog snapshot")
        .get(0);
    assert!(read_only);

    let rows = client
        .query(
            "WITH mutation_roles AS ( \
               SELECT c.oid AS relation, c.relowner AS role_oid \
               FROM pg_catalog.pg_class c WHERE c.relkind IN ('r','p') \
               UNION \
               SELECT c.oid, acl.grantee FROM pg_catalog.pg_class c \
               CROSS JOIN LATERAL pg_catalog.aclexplode( \
                   COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl \
               WHERE c.relkind IN ('r','p') \
                 AND acl.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE') \
               UNION \
               SELECT c.oid, acl.grantee FROM pg_catalog.pg_class c \
               JOIN pg_catalog.pg_attribute att ON att.attrelid=c.oid \
                 AND att.attnum > 0 AND NOT att.attisdropped AND att.attacl IS NOT NULL \
               CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl \
               WHERE c.relkind IN ('r','p') AND acl.privilege_type IN ('INSERT','UPDATE') \
             ) \
             SELECT n.nspname || '.' || c.relname, granted_role.rolname, \
                    c.relrowsecurity AND c.relforcerowsecurity \
                    AND EXISTS (SELECT FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid) \
             FROM mutation_roles mutation \
             JOIN pg_catalog.pg_class c ON c.oid=mutation.relation \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_roles granted_role ON granted_role.oid=mutation.role_oid \
             WHERE granted_role.rolname::text = ANY($1) ORDER BY 1,2",
            &[&AUTHOR_SQL_ROLES.as_slice()],
        )
        .await
        .expect("read author SQL mutation privileges");
    let mut control_author_relations = BTreeSet::new();
    for row in rows {
        let relation: String = row.get(0);
        let role: String = row.get(1);
        let forced_row_security: bool = row.get(2);
        assert!(
            forced_row_security,
            "{relation} grants author SQL mutation without forced row security"
        );
        if role == "wamn_control_author" {
            control_author_relations.insert(relation);
        }
    }
    let expected = if control_database {
        BTreeSet::from([
            "catalog.authoring_command_audit".to_string(),
            "wamn_run.gate_reports".to_string(),
        ])
    } else {
        BTreeSet::new()
    };
    assert_eq!(control_author_relations, expected);
    client
        .batch_execute("ROLLBACK")
        .await
        .expect("finish read-only catalog snapshot");
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn protected_relations_match_reconciled_postgres() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let repository = repository();
    let client = connect(&url).await;
    let version: String = client
        .query_one("SELECT current_setting('server_version_num')", &[])
        .await
        .expect("read protected-relation probe server version")
        .get(0);
    let version: u32 = version.parse().expect("server_version_num is numeric");
    assert!(
        (180_000..190_000).contains(&version),
        "protected-relation derivation requires PostgreSQL 18, found {version}"
    );
    prepare_scratch_database(&client).await;
    install_control_database(&client).await;
    let control_shared_fingerprints =
        portable_fingerprints(&client, &SHARED_PORTABLE_RELATIONS).await;
    let control_diverged_fingerprints =
        portable_fingerprints(&client, &DIVERGED_PORTABLE_RELATIONS).await;
    let control_only_fingerprints =
        portable_fingerprints(&client, &CONTROL_ONLY_PORTABLE_RELATIONS).await;
    assert!(
        control_only_fingerprints
            .values()
            .all(|fingerprint| !fingerprint.columns.is_empty()),
        "control-only portable relations must exist in the control plane: {control_only_fingerprints:?}"
    );
    assert_author_sql_boundaries(&client, true).await;
    install_project_database(&client, &url, &repository).await;
    let project_shared_fingerprints =
        portable_fingerprints(&client, &SHARED_PORTABLE_RELATIONS).await;
    assert_eq!(
        control_shared_fingerprints, project_shared_fingerprints,
        "control copies drifted from the still-authoritative project column/constraint shapes"
    );
    // The two relations this equality no longer covers, and the narrower rule
    // that replaced it for them (wamn-10yt.52).
    assert_diverges_by_environment_instance(
        &control_diverged_fingerprints,
        &portable_fingerprints(&client, &DIVERGED_PORTABLE_RELATIONS).await,
    );
    let project_side_control_only =
        portable_fingerprints(&client, &CONTROL_ONLY_PORTABLE_RELATIONS).await;
    assert!(
        project_side_control_only
            .values()
            .all(|fingerprint| fingerprint.columns.is_empty()),
        "control-only portable relations must not be installed project-side: \
         {project_side_control_only:?}"
    );
    assert_author_sql_boundaries(&client, false).await;
}
