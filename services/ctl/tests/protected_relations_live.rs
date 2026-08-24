//! Generates and verifies the protected-relation authority table from a fresh
//! PostgreSQL 18 database.
//!
//! Set `WAMN_CTL_PG_URL` to a superuser URL for a disposable database. Set
//! `WAMN_UPDATE_PROTECTED_RELATIONS=1` only when intentionally regenerating
//! `architecture/protected-writes.json`.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, NoTls};

use wamn_control_provision::sql;
use wamn_ctl::migrate_catalog::{self, MigrateCatalogArgs};
use wamn_ctl::reconcile_run_plane;
use wamn_schema_control::BareSchemaName;

const MANIFEST_PATH: &str = "architecture/state-owners.json";
const TABLE_PATH: &str = "architecture/protected-writes.json";
const SYSTEM_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const OPS_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/ops-schema.sql");
const CONTROL_PORTABLE_STORE_SQL: &str =
    include_str!("../../../deploy/sql/control-portable-store.sql");
const APP_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/app-schema.sql");
const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");
const WITNESS_SCHEMA: &str = "audit_app";
const WITNESS_ENTITY: &str = "audit_records";
const AUTHOR_SQL_EXPOSURE: &str = "author SQL, RLS-bounded";
const AUTHOR_SQL_ROLES: [&str; 2] = ["wamn_app", "wamn_control_author"];

#[derive(Debug, Deserialize)]
struct OwnershipManifest {
    schema_version: String,
    canonical_sources: Vec<CanonicalSource>,
    objects: Vec<OwnershipEntry>,
    families: Vec<OwnershipFamily>,
    #[serde(flatten)]
    ignored: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CanonicalSource {
    path: String,
    scope: String,
    #[serde(flatten)]
    ignored: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OwnershipEntry {
    id: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
    schema_source: String,
    #[serde(flatten)]
    ignored: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OwnershipFamily {
    pattern: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
    schema_source: String,
    #[serde(flatten)]
    ignored: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug)]
struct DeclaredRelation {
    scope: String,
    relation: String,
    physical_relation: String,
    ops: bool,
    installer: String,
    owner: String,
}

#[derive(Clone, Debug)]
struct RelationBase {
    physical_owner: String,
    rls_enabled: bool,
    rls_forced: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct PortableFingerprint {
    columns: Vec<String>,
    constraints: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProtectedRelationTable {
    schema_version: String,
    rows: Vec<ProtectedRelationRow>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProtectedRelationRow {
    scope: String,
    relation: String,
    ops: bool,
    installer: String,
    owner: String,
    mechanisms: Vec<String>,
    roles: Vec<ExecutingRole>,
    #[serde(rename = "author-reachable")]
    author_reachable: String,
    guards: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecutingRole {
    role: String,
    basis: String,
    operations: Vec<String>,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ctl crate is under services/ctl")
        .to_path_buf()
}

fn sole_installer(relation: &str, migration_owners: &[String]) -> String {
    assert_eq!(
        migration_owners.len(),
        1,
        "{relation} must declare exactly one installer"
    );
    migration_owners[0].clone()
}

fn is_ops_artifact(schema_source: &str, scopes: &BTreeMap<String, String>) -> bool {
    scopes
        .get(schema_source)
        .unwrap_or_else(|| panic!("undeclared canonical source {schema_source}"))
        == "production-control-database-ops"
}

fn declared_relations(repository: &Path) -> Vec<DeclaredRelation> {
    let source = std::fs::read_to_string(repository.join(MANIFEST_PATH))
        .expect("read state ownership manifest");
    let manifest: OwnershipManifest =
        serde_json::from_str(&source).expect("parse state ownership manifest");
    assert_eq!(manifest.schema_version, "0.1");
    assert!(
        !manifest.ignored.is_empty(),
        "manifest scope must be present"
    );
    let source_scopes = manifest
        .canonical_sources
        .into_iter()
        .map(|source| {
            let _ = source.ignored;
            (source.path, source.scope)
        })
        .collect::<BTreeMap<_, _>>();

    let mut relations = manifest
        .objects
        .into_iter()
        .map(|entry| {
            let _ = &entry.ignored;
            DeclaredRelation {
                scope: source_scopes
                    .get(&entry.schema_source)
                    .expect("declared canonical source")
                    .clone(),
                installer: sole_installer(&entry.id, &entry.migration_owners),
                physical_relation: entry.id.clone(),
                relation: entry.id,
                ops: is_ops_artifact(&entry.schema_source, &source_scopes),
                owner: entry.semantic_owner,
            }
        })
        .collect::<Vec<_>>();
    relations.extend(manifest.families.into_iter().map(|family| {
        let _ = &family.ignored;
        let physical_relation = family
            .pattern
            .replace("<app_schema>", WITNESS_SCHEMA)
            .replace("<catalog_entity_name>", WITNESS_ENTITY);
        DeclaredRelation {
            scope: source_scopes
                .get(&family.schema_source)
                .expect("declared canonical source")
                .clone(),
            installer: sole_installer(&family.pattern, &family.migration_owners),
            physical_relation,
            relation: family.pattern,
            ops: is_ops_artifact(&family.schema_source, &source_scopes),
            owner: family.semantic_owner,
        }
    }));
    relations
        .sort_by(|left, right| (&left.scope, &left.relation).cmp(&(&right.scope, &right.relation)));
    relations
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
}

async fn install_project_database(client: &Client, url: &str, repository: &Path) {
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS wamn_run CASCADE",
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

    let catalog_path = std::env::temp_dir().join(format!(
        "wamn-protected-relations-{}.catalog.json",
        std::process::id()
    ));
    let catalog = serde_json::json!({
        "schema-version": "0.1",
        "catalog-id": "protected-relation-audit",
        "version": 1,
        "entities": [{
            "id": "audit-records",
            "name": WITNESS_ENTITY,
            "fields": [{
                "id": "value",
                "name": "value",
                "type": { "kind": "text" }
            }]
        }],
        "relations": []
    });
    std::fs::write(
        &catalog_path,
        serde_json::to_vec_pretty(&catalog).expect("serialize witness catalog"),
    )
    .expect("write witness catalog");
    let migrate_result = migrate_catalog::run(MigrateCatalogArgs {
        admin_database_url: url.to_string(),
        tenant: "protected-relation-audit".to_string(),
        environment: "dev".to_string(),
        schema: WITNESS_SCHEMA.to_string(),
        target: catalog_path.clone(),
        base: None,
        dry_run: false,
        skip_reconcile_replica_identity: true,
    })
    .await;
    let _ = std::fs::remove_file(&catalog_path);
    migrate_result.expect("install canonical application-family witnesses");

    // The management surface revokes and regrants across `catalog` as well as
    // the run schema, so it must follow the catalog install, not precede it.
    client
        .batch_execute(&sql::grant_management_admitter_surface_sql("wamn_run"))
        .await
        .expect("install canonical management-admission authority");

    assert!(repository.join(MANIFEST_PATH).is_file());
}

async fn shared_portable_fingerprints(client: &Client) -> BTreeMap<String, PortableFingerprint> {
    const RELATIONS: [&str; 17] = [
        "catalog.catalogs",
        "catalog.flow_artifacts",
        "catalog.execution_bundles",
        "catalog.releases",
        "catalog.release_flows",
        "catalog.catalog_heads",
        "catalog.flow_drafts",
        "catalog.validated_flow_drafts",
        "catalog.release_exposure_manifests",
        "catalog.release_sources",
        "catalog.release_attachments",
        "catalog.connection_requirements",
        "catalog.draft_safe_connection_grants",
        "catalog.authoring_command_audit",
        "wamn_run.authoring_test_run_reservations",
        "wamn_run.authoring_test_case_runs",
        "wamn_run.authoring_test_reports",
    ];
    let mut fingerprints = RELATIONS
        .into_iter()
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
    let relations: &[&str] = &RELATIONS;
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
               AND NOT (n.nspname='catalog' \
                        AND c.relname='draft_safe_connection_grants' \
                        AND con.contype='f') \
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

fn normalize_catalog_text(relation: &str, value: &str) -> String {
    let value = if relation.starts_with("<app_schema>.") {
        value.replace(WITNESS_SCHEMA, "<app_schema>")
    } else {
        value.to_string()
    };
    if relation == "<app_schema>.<catalog_entity_name>" {
        value.replace(WITNESS_ENTITY, "<catalog_entity_name>")
    } else {
        value
    }
}

fn constraint_kind(value: &str) -> &'static str {
    match value {
        "c" => "check",
        "f" => "foreign-key",
        "p" => "primary-key",
        "u" => "unique",
        "x" => "exclude",
        other => panic!("unexpected constraint kind {other}"),
    }
}

fn foreign_key_action(value: &str) -> &'static str {
    match value {
        "a" => "no-action",
        "r" => "restrict",
        "c" => "cascade",
        "n" => "set-null",
        "d" => "set-default",
        other => panic!("unexpected foreign-key action {other}"),
    }
}

fn trigger_mode(value: &str) -> &'static str {
    match value {
        "O" => "origin",
        "D" => "disabled",
        "R" => "replica",
        "A" => "always",
        other => panic!("unexpected trigger mode {other}"),
    }
}

fn is_author_sql_role(role: &str) -> bool {
    AUTHOR_SQL_ROLES.contains(&role)
}

async fn generate_rows(
    client: &Client,
    declarations: Vec<DeclaredRelation>,
) -> Vec<ProtectedRelationRow> {
    let physical_to_declared = declarations
        .iter()
        .map(|entry| (entry.physical_relation.clone(), entry.relation.clone()))
        .collect::<BTreeMap<_, _>>();

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

    let base_rows = client
        .query(
            "SELECT n.nspname || '.' || c.relname, owner.rolname, \
                    c.relrowsecurity, c.relforcerowsecurity \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid=c.relowner \
             WHERE c.relkind IN ('r','p') \
               AND n.nspname IN ('registry','provisioning','identity','catalog', \
                                  'app_system','wamn_run','wamn_authority','audit_app') \
             ORDER BY 1",
            &[],
        )
        .await
        .expect("read protected relation bases");
    let mut bases = BTreeMap::new();
    for row in base_rows {
        let physical: String = row.get(0);
        if physical_to_declared.contains_key(&physical) {
            assert!(
                bases
                    .insert(
                        physical,
                        RelationBase {
                            physical_owner: row.get(1),
                            rls_enabled: row.get(2),
                            rls_forced: row.get(3),
                        },
                    )
                    .is_none()
            );
        }
    }
    assert_eq!(
        bases.keys().cloned().collect::<BTreeSet<_>>(),
        physical_to_declared.keys().cloned().collect(),
        "scratch database does not materialize every declared relation"
    );

    let mut role_operations = BTreeMap::<(String, String, String), BTreeSet<String>>::new();
    for (physical, base) in &bases {
        let relation = physical_to_declared
            .get(physical)
            .expect("declared physical relation");
        role_operations
            .entry((
                relation.clone(),
                base.physical_owner.clone(),
                "owner".to_string(),
            ))
            .or_default()
            .extend(["delete", "insert", "truncate", "update"].map(str::to_string));
    }
    let table_acl_rows = client
        .query(
            "SELECT n.nspname || '.' || c.relname, \
                    CASE acl.grantee WHEN 0 THEN 'PUBLIC' ELSE pg_get_userbyid(acl.grantee) END, \
                    lower(acl.privilege_type), owner.rolname \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid=c.relowner \
             CROSS JOIN LATERAL pg_catalog.aclexplode( \
                 COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl \
             WHERE c.relkind IN ('r','p') \
               AND acl.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE') \
             ORDER BY 1,2,3",
            &[],
        )
        .await
        .expect("read table mutation grants");
    for row in table_acl_rows {
        let physical: String = row.get(0);
        let Some(relation) = physical_to_declared.get(&physical) else {
            continue;
        };
        let role: String = row.get(1);
        let operation: String = row.get(2);
        let physical_owner: String = row.get(3);
        if role != physical_owner {
            role_operations
                .entry((relation.clone(), role, "grant".to_string()))
                .or_default()
                .insert(operation);
        }
    }
    let column_acl_rows = client
        .query(
            "SELECT n.nspname || '.' || c.relname, att.attname, \
                    CASE acl.grantee WHEN 0 THEN 'PUBLIC' ELSE pg_get_userbyid(acl.grantee) END, \
                    lower(acl.privilege_type), owner.rolname \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid=c.relowner \
             JOIN pg_catalog.pg_attribute att ON att.attrelid=c.oid \
               AND att.attnum > 0 AND NOT att.attisdropped AND att.attacl IS NOT NULL \
             CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl \
             WHERE c.relkind IN ('r','p') AND acl.privilege_type IN ('INSERT','UPDATE') \
             ORDER BY 1,2,3,4",
            &[],
        )
        .await
        .expect("read column mutation grants");
    for row in column_acl_rows {
        let physical: String = row.get(0);
        let Some(relation) = physical_to_declared.get(&physical) else {
            continue;
        };
        let column: String = row.get(1);
        let role: String = row.get(2);
        let operation: String = row.get(3);
        let physical_owner: String = row.get(4);
        if role != physical_owner {
            role_operations
                .entry((relation.clone(), role, "grant".to_string()))
                .or_default()
                .insert(format!("{operation}({column})"));
        }
    }

    let mut mechanisms = declarations
        .iter()
        .map(|entry| {
            (
                entry.relation.clone(),
                BTreeSet::from(["direct-dml".to_string()]),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut guards = BTreeMap::<String, BTreeSet<String>>::new();
    for declaration in &declarations {
        let base = bases
            .get(&declaration.physical_relation)
            .expect("declared relation base");
        guards
            .entry(declaration.relation.clone())
            .or_default()
            .insert(format!(
                "row-security:enabled={};forced={}",
                base.rls_enabled, base.rls_forced
            ));
    }

    let policy_rows = client
        .query(
            "SELECT n.nspname || '.' || c.relname, p.polname, \
                    CASE p.polcmd WHEN 'r' THEN 'select' WHEN 'a' THEN 'insert' \
                      WHEN 'w' THEN 'update' WHEN 'd' THEN 'delete' ELSE 'all' END, \
                    p.polpermissive, \
                    ARRAY(SELECT CASE role_oid WHEN 0 THEN 'PUBLIC' \
                                      ELSE pg_get_userbyid(role_oid) END \
                          FROM unnest(p.polroles) role_oid ORDER BY 1), \
                    pg_get_expr(p.polqual,p.polrelid,true), \
                    pg_get_expr(p.polwithcheck,p.polrelid,true) \
             FROM pg_catalog.pg_policy p \
             JOIN pg_catalog.pg_class c ON c.oid=p.polrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             ORDER BY 1,2",
            &[],
        )
        .await
        .expect("read row-security policies");
    let mut policy_count = BTreeMap::<String, usize>::new();
    for row in policy_rows {
        let physical: String = row.get(0);
        let Some(relation) = physical_to_declared.get(&physical) else {
            continue;
        };
        let name: String = row.get(1);
        let command: String = row.get(2);
        let permissive: bool = row.get(3);
        let roles: Vec<String> = row.get(4);
        let using_expression: Option<String> = row.get(5);
        let check_expression: Option<String> = row.get(6);
        *policy_count.entry(relation.clone()).or_default() += 1;
        guards.entry(relation.clone()).or_default().insert(
            normalize_catalog_text(
                relation,
                &format!(
                    "policy:{name};command={command};permissive={permissive};roles={};using={};check={}",
                    roles.join(","),
                    using_expression.as_deref().unwrap_or("-"),
                    check_expression.as_deref().unwrap_or("-")
                ),
            ),
        );
    }

    let constraint_rows = client
        .query(
            "SELECT nc.nspname || '.' || child.relname, con.conname, \
                    con.contype::text, con.condeferrable, con.condeferred, con.convalidated, \
                    pg_get_constraintdef(con.oid,true), \
                    CASE WHEN parent.oid IS NULL THEN NULL ELSE np.nspname || '.' || parent.relname END, \
                    con.confupdtype::text, con.confdeltype::text \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class child ON child.oid=con.conrelid \
             JOIN pg_catalog.pg_namespace nc ON nc.oid=child.relnamespace \
             LEFT JOIN pg_catalog.pg_class parent ON parent.oid=con.confrelid \
             LEFT JOIN pg_catalog.pg_namespace np ON np.oid=parent.relnamespace \
             WHERE con.contype IN ('p','u','c','f','x') \
             ORDER BY 1,2",
            &[],
        )
        .await
        .expect("read relation constraints and cascades");
    for row in constraint_rows {
        let physical: String = row.get(0);
        let Some(relation) = physical_to_declared.get(&physical) else {
            continue;
        };
        let name: String = row.get(1);
        let kind: String = row.get(2);
        let deferrable: bool = row.get(3);
        let deferred: bool = row.get(4);
        let validated: bool = row.get(5);
        let definition: String = row.get(6);
        guards.entry(relation.clone()).or_default().insert(
            normalize_catalog_text(
                relation,
                &format!(
                    "constraint:{name};kind={};deferrable={deferrable};deferred={deferred};validated={validated};definition={definition}",
                    constraint_kind(&kind)
                ),
            ),
        );
        if kind == "f" {
            let parent: String = row.get::<_, Option<String>>(7).expect("foreign-key parent");
            let on_update: String = row.get(8);
            let on_delete: String = row.get(9);
            for (event, action) in [("update", on_update), ("delete", on_delete)] {
                let action = foreign_key_action(&action);
                if !matches!(action, "no-action" | "restrict") {
                    mechanisms
                        .entry(relation.clone())
                        .or_default()
                        .insert(normalize_catalog_text(
                            relation,
                            &format!(
                                "foreign-key:{event}:{action};from={parent};constraint={name}"
                            ),
                        ));
                }
            }
        }
    }

    let unique_index_rows = client
        .query(
            "SELECT n.nspname || '.' || c.relname, idx.relname, i.indisvalid, i.indisready, \
                    pg_get_indexdef(i.indexrelid,0,true) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON c.oid=i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_class idx ON idx.oid=i.indexrelid \
             LEFT JOIN pg_catalog.pg_constraint con ON con.conindid=i.indexrelid \
             WHERE i.indisunique AND con.oid IS NULL \
             ORDER BY 1,2",
            &[],
        )
        .await
        .expect("read standalone unique indexes");
    for row in unique_index_rows {
        let physical: String = row.get(0);
        let Some(relation) = physical_to_declared.get(&physical) else {
            continue;
        };
        let name: String = row.get(1);
        let valid: bool = row.get(2);
        let ready: bool = row.get(3);
        let definition: String = row.get(4);
        guards
            .entry(relation.clone())
            .or_default()
            .insert(normalize_catalog_text(
                relation,
                &format!("unique-index:{name};valid={valid};ready={ready};definition={definition}"),
            ));
    }

    let trigger_rows = client
        .query(
            "SELECT n.nspname || '.' || c.relname, t.tgname, t.tgenabled::text, \
                    pg_get_triggerdef(t.oid,true), fnn.nspname || '.' || p.proname, \
                    pg_get_function_identity_arguments(p.oid), owner.rolname, \
                    p.prosecdef, p.proconfig \
             FROM pg_catalog.pg_trigger t \
             JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_proc p ON p.oid=t.tgfoid \
             JOIN pg_catalog.pg_namespace fnn ON fnn.oid=p.pronamespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid=p.proowner \
             WHERE NOT t.tgisinternal \
             ORDER BY 1,2",
            &[],
        )
        .await
        .expect("read triggers and function owners");
    for row in trigger_rows {
        let physical: String = row.get(0);
        let Some(relation) = physical_to_declared.get(&physical) else {
            continue;
        };
        let name: String = row.get(1);
        let mode: String = row.get(2);
        let definition: String = row.get(3);
        let function: String = row.get(4);
        let arguments: String = row.get(5);
        let function_owner: String = row.get(6);
        let security_definer: bool = row.get(7);
        let configuration: Option<Vec<String>> = row.get(8);
        guards.entry(relation.clone()).or_default().insert(
            normalize_catalog_text(
                relation,
                &format!(
                    "trigger:{name};mode={};function={function}({arguments});function-owner={function_owner};security-definer={security_definer};config={};definition={definition}",
                    trigger_mode(&mode),
                    configuration.unwrap_or_default().join(",")
                ),
            ),
        );
    }
    client
        .batch_execute("ROLLBACK")
        .await
        .expect("finish read-only catalog snapshot");

    let mut roles_by_relation = BTreeMap::<String, Vec<ExecutingRole>>::new();
    for ((relation, role, basis), operations) in role_operations {
        roles_by_relation
            .entry(relation)
            .or_default()
            .push(ExecutingRole {
                role,
                basis,
                operations: operations.into_iter().collect(),
            });
    }
    for roles in roles_by_relation.values_mut() {
        roles.sort_by(|left, right| (&left.role, &left.basis).cmp(&(&right.role, &right.basis)));
    }

    declarations
        .into_iter()
        .map(|declaration| {
            let roles = roles_by_relation
                .remove(&declaration.relation)
                .expect("every relation has its physical owner role");
            let author_reachable = if roles.iter().any(|role| is_author_sql_role(&role.role)) {
                let base = bases
                    .get(&declaration.physical_relation)
                    .expect("author-exposed relation base");
                assert!(
                    base.rls_enabled
                        && base.rls_forced
                        && policy_count
                            .get(&declaration.relation)
                            .copied()
                            .unwrap_or(0)
                            > 0,
                    "{} grants author SQL mutation without forced row security",
                    declaration.relation
                );
                AUTHOR_SQL_EXPOSURE.to_string()
            } else {
                "no".to_string()
            };
            ProtectedRelationRow {
                scope: declaration.scope,
                relation: declaration.relation.clone(),
                ops: declaration.ops,
                installer: declaration.installer,
                owner: declaration.owner,
                mechanisms: mechanisms
                    .remove(&declaration.relation)
                    .expect("relation mechanisms")
                    .into_iter()
                    .collect(),
                roles,
                author_reachable,
                guards: guards
                    .remove(&declaration.relation)
                    .expect("relation guards")
                    .into_iter()
                    .collect(),
            }
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn protected_relations_match_reconciled_postgres() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let repository = repository();
    let client = connect(&url).await;
    prepare_scratch_database(&client).await;
    let declarations = declared_relations(&repository);
    install_control_database(&client).await;
    let control_portable_fingerprints = shared_portable_fingerprints(&client).await;
    let mut rows = generate_rows(
        &client,
        declarations
            .iter()
            .filter(|entry| entry.scope.starts_with("production-control-database"))
            .cloned()
            .collect(),
    )
    .await;
    install_project_database(&client, &url, &repository).await;
    let project_portable_fingerprints = shared_portable_fingerprints(&client).await;
    assert_eq!(
        control_portable_fingerprints, project_portable_fingerprints,
        "control copies drifted from the still-authoritative project column/constraint shapes"
    );
    rows.extend(
        generate_rows(
            &client,
            declarations
                .into_iter()
                .filter(|entry| entry.scope == "production-project-database")
                .collect(),
        )
        .await,
    );
    rows.sort_by(|left, right| (&left.scope, &left.relation).cmp(&(&right.scope, &right.relation)));
    let actual = ProtectedRelationTable {
        schema_version: "0.1".to_string(),
        rows,
    };
    let actual_json = format!(
        "{}\n",
        serde_json::to_string_pretty(&actual).expect("serialize protected relation table")
    );
    let table_path = repository.join(TABLE_PATH);
    if std::env::var_os("WAMN_UPDATE_PROTECTED_RELATIONS").is_some() {
        std::fs::write(&table_path, actual_json).expect("write protected relation table");
        return;
    }
    let expected_source = std::fs::read_to_string(&table_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", table_path.display()));
    let expected: ProtectedRelationTable =
        serde_json::from_str(&expected_source).expect("parse protected relation table");
    assert_eq!(
        actual, expected,
        "regenerate with WAMN_UPDATE_PROTECTED_RELATIONS=1"
    );
}
