//! Converge the generated GuestSql authority union for the installed package set.

pub(super) mod plan;
use plan::{
    desired_acl, desired_effective_acl, render_undeclared_revocation, residue_targets,
    validate_package_set, validate_recorded_manifests,
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context as _, bail, ensure};
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_schema_control::plan_package_migrations;

use wamn_package_control::apply_package::{LOCK_SQL, PackageRoot};
use wamn_schema_generator::{
    DataAccessOverlay, DataAccessRelationFields, EffectiveDataAccess, data_access_schemas,
    derive_effective_data_access, render_effective_data_access_sql,
    validate_data_access_contribution,
};
use wamn_schema_introspection::postgres::DATA_ACCESS_ROLE;

const CLAIM_ENV_KEY_SQL: &str = "SELECT set_config('app.env_key', $1, true)";
const SELECT_INSTALLED_SQL: &str = "\
SELECT package_id, package_version, artifact_digest FROM catalog.packages \
 WHERE env_key = $1 ORDER BY package_id COLLATE \"C\"";
// apply-package owns these OID histories beside application tables. No package
// declaration consumes them, so the declared relation list leaves them out.
// The sweep below still reads them, because every relation in a package-owned
// schema is in scope for revocation.
const CONTROL_OWNED_RELATION_MAPS: [&str; 2] = ["wamn_entities", "wamn_cdc_exclusions"];
// A package-owned schema holds relations no package declares. The declared reads
// bind one relation name at a time, so they never reach those relations and never
// revoke a grant on them. This read asks the server which privileges the App role
// still reaches on every relation in the package-owned schemas. The three branches
// cover table-shaped relations, their columns, and sequences. The schema list is
// the parameter that keeps the sweep inside package-owned ground.
const UNDECLARED_RESIDUE_SQL: &str = "\
SELECT namespace.nspname::text, relation.relname::text, relation.relkind::text, \
       NULL::text, privilege \
  FROM pg_catalog.pg_class AS relation \
  JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
  CROSS JOIN unnest($3::text[]) AS privilege \
 WHERE namespace.nspname = ANY($2::text[]) \
   AND relation.relkind IN ('r', 'p', 'v', 'm', 'f') \
   AND pg_catalog.has_table_privilege($1, relation.oid, privilege) \
UNION ALL \
SELECT namespace.nspname::text, relation.relname::text, relation.relkind::text, \
       attribute.attname::text, privilege \
  FROM pg_catalog.pg_class AS relation \
  JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
  JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
  CROSS JOIN unnest($4::text[]) AS privilege \
 WHERE namespace.nspname = ANY($2::text[]) \
   AND relation.relkind IN ('r', 'p', 'v', 'm', 'f') \
   AND attribute.attnum > 0 AND NOT attribute.attisdropped \
   AND pg_catalog.has_column_privilege($1, relation.oid, attribute.attnum, privilege) \
UNION ALL \
SELECT namespace.nspname::text, relation.relname::text, relation.relkind::text, \
       NULL::text, privilege \
  FROM pg_catalog.pg_class AS relation \
  JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
  CROSS JOIN unnest($5::text[]) AS privilege \
 WHERE namespace.nspname = ANY($2::text[]) \
   AND relation.relkind = 'S' \
   AND pg_catalog.has_sequence_privilege($1, relation.oid, privilege)";

/// Package artifact digest of each package coordinate, keyed by id and version.
/// `None` is a working tree, which only `wamn dev` applies.
type CoordinateHashes = BTreeMap<(String, String), Option<String>>;

/// Inputs of one post-apply generated ACL reconciliation.
#[derive(Debug)]
pub struct ReconcilePackageDataAccessRequest {
    /// Installed package roots containing wamn.json and generated policy
    /// evidence, each with the digest of the verified artifact it was opened
    /// from.
    pub packages: Vec<PackageRoot>,

    /// Owner connection to the target project-environment database.
    pub database_url: String,

    pub org: String,
    pub project: String,
    pub environment: String,
}

impl ReconcilePackageDataAccessRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Reconciled coordinates and the effect state forming the reconciliation closing predicate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataAccessReconcileResult {
    coordinates: Vec<String>,
    changed: bool,
}

impl DataAccessReconcileResult {
    /// Whether the server already held the exact generated direct ACL.
    pub const fn is_noop(&self) -> bool {
        !self.changed
    }

    /// Reconciled package coordinates, `<id>@<version>`, in sorted order.
    pub fn coordinates(&self) -> &[String] {
        &self.coordinates
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DirectAcl {
    schema: BTreeSet<(String, String, String, bool)>,
    table: BTreeSet<(String, String, String, String, bool)>,
    column: BTreeSet<(String, String, String, String, String, bool)>,
}

/// One App privilege the server still reaches on an undeclared relation.
///
/// The parts are the schema, the relation, its `pg_class.relkind`, the column
/// when the privilege is a column privilege, and the privilege name.
type UndeclaredResidue = BTreeSet<(String, String, String, Option<String>, String)>;

const SCHEMA_PRIVILEGES: &[&str] = &["CREATE", "USAGE"];
const TABLE_PRIVILEGES: &[&str] = &[
    "DELETE",
    "INSERT",
    "MAINTAIN",
    "REFERENCES",
    "SELECT",
    "TRIGGER",
    "TRUNCATE",
    "UPDATE",
];
const COLUMN_PRIVILEGES: &[&str] = &["INSERT", "REFERENCES", "SELECT", "UPDATE"];

/// Sorted effective privilege facts; table grants remain distinct from column grants.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EffectiveAcl {
    schema: BTreeSet<(String, String)>,
    table: BTreeSet<(String, String, String)>,
    column: BTreeSet<(String, String, String, String)>,
}

/// Read effective application privileges for reconciliation.
async fn read_effective_privileges(
    tx: &Transaction<'_>,
    role: &str,
    schemas: &[String],
) -> anyhow::Result<EffectiveAcl> {
    let mut acl = EffectiveAcl::default();
    for row in tx
        .query(
            "SELECT n.nspname::text, privilege FROM pg_namespace n \
             CROSS JOIN unnest($3::text[]) AS privilege \
             WHERE n.nspname = ANY($2::text[]) AND has_schema_privilege($1, n.oid, privilege)",
            &[&role, &schemas, &SCHEMA_PRIVILEGES],
        )
        .await
        .context("read effective application schema privileges")?
    {
        acl.schema.insert((row.get(0), row.get(1)));
    }
    for row in tx
        .query(
            "SELECT n.nspname::text, c.relname::text, privilege \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN unnest($3::text[]) AS privilege \
             WHERE n.nspname = ANY($2::text[]) AND c.relkind IN ('r','p','v','m','f') \
               AND has_table_privilege($1, c.oid, privilege)",
            &[&role, &schemas, &TABLE_PRIVILEGES],
        )
        .await
        .context("read effective application table privileges")?
    {
        acl.table.insert((row.get(0), row.get(1), row.get(2)));
    }
    for row in tx
        .query(
            "SELECT n.nspname::text, c.relname::text, a.attname::text, privilege \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_attribute a ON a.attrelid = c.oid \
             CROSS JOIN unnest($3::text[]) AS privilege \
             WHERE n.nspname = ANY($2::text[]) AND c.relkind IN ('r','p','v','m','f') \
               AND a.attnum > 0 AND NOT a.attisdropped \
               AND has_column_privilege($1, c.oid, a.attnum, privilege)",
            &[&role, &schemas, &COLUMN_PRIVILEGES],
        )
        .await
        .context("read effective application column privileges")?
    {
        acl.column
            .insert((row.get(0), row.get(1), row.get(2), row.get(3)));
    }
    Ok(acl)
}

#[derive(Debug)]
struct PresentedPackage {
    coordinate: String,
    package_id: String,
    package_version: String,
    artifact_digest: Option<String>,
    schemas: Vec<String>,
    overlay: DataAccessOverlay,
    directory: wamn_schema_control::PackageDirectory,
}

/// The same parsed package evidence used by validation and local cutover.
#[derive(Debug)]
pub struct PreparedLocalDataAccess(Vec<PresentedPackage>);

#[derive(Clone, Copy)]
enum ReconcileMode<'a> {
    Production,
    Local(&'a str),
}

/// Reconcile the installed package set and return its observable effect state.
pub async fn reconcile_package_data_access(
    request: ReconcilePackageDataAccessRequest,
) -> anyhow::Result<DataAccessReconcileResult> {
    ensure!(
        ![&request.org, &request.project, &request.environment]
            .iter()
            .any(|part| part.is_empty()),
        "org, project and environment must not be empty"
    );
    let packages = read_presented_packages(&request.packages)?;
    execute_prepared(
        &request.database_url,
        &request.env_key(),
        &packages,
        ReconcileMode::Production,
        true,
    )
    .await
}

/// Read the working trees of `wamn dev`, which carry no artifact digest.
pub fn prepare_local(packages: &[PathBuf]) -> anyhow::Result<PreparedLocalDataAccess> {
    let roots = packages
        .iter()
        .map(|path| PackageRoot {
            path: path.clone(),
            artifact_digest: None,
        })
        .collect::<Vec<_>>();
    read_presented_packages(&roots).map(PreparedLocalDataAccess)
}

/// Test again live schema and grants using the already parsed local source inputs.
pub async fn reconcile_local(
    prepared: &PreparedLocalDataAccess,
    database_url: &str,
    env_key: &str,
    environment: &str,
    apply: bool,
) -> anyhow::Result<DataAccessReconcileResult> {
    wamn_runtime::local_application::require_local_target(database_url, env_key, environment)
        .await?;
    execute_prepared(
        database_url,
        env_key,
        &prepared.0,
        ReconcileMode::Local(environment),
        apply,
    )
    .await
}

fn read_presented_packages(roots: &[PackageRoot]) -> anyhow::Result<Vec<PresentedPackage>> {
    ensure!(!roots.is_empty(), "package-data-access-installed-set-empty");
    let mut packages = Vec::with_capacity(roots.len());
    let mut coordinates = BTreeSet::new();
    let mut package_ids = BTreeSet::new();
    for package_root in roots {
        let directory = wamn_package_control::apply_package::read_package_root(package_root)?;
        let plan = plan_package_migrations(&directory, None, &[])
            .context("validate package directory before data-access reconciliation")?;
        let schemas = data_access_schemas(&directory.manifest_bytes)
            .context("derive package data-access schema set")?;
        let overlay_path = wamn_schema_generator::output_root(&package_root.path)
            .join("platform-policy/data-access.json");
        let overlay_bytes = std::fs::read(&overlay_path)
            .with_context(|| format!("read {}", overlay_path.display()))?;
        let overlay = DataAccessOverlay::from_slice(&overlay_bytes)
            .context("parse generated package data-access evidence")?;
        let coordinate = format!(
            "{}@{}",
            plan.coordinate.package_id(),
            plan.coordinate.package_version()
        );
        ensure!(
            overlay.package() == coordinate,
            "package-data-access-coordinate-mismatch: expected {coordinate}, observed {}",
            overlay.package()
        );
        validate_data_access_contribution(&overlay, &directory.manifest_bytes)
            .context("verify generated package data-access contribution")?;
        ensure!(
            coordinates.insert(coordinate.clone()),
            "package-data-access-installed-set-repeats-coordinate: {coordinate}"
        );
        ensure!(
            package_ids.insert(plan.coordinate.package_id().to_owned()),
            "package-data-access-installed-set-repeats-package: {}",
            plan.coordinate.package_id()
        );
        packages.push(PresentedPackage {
            coordinate,
            package_id: plan.coordinate.package_id().to_owned(),
            package_version: plan.coordinate.package_version().to_owned(),
            artifact_digest: plan.artifact_digest,
            schemas,
            overlay,
            directory,
        });
    }
    packages.sort_by(|left, right| left.coordinate.cmp(&right.coordinate));
    Ok(packages)
}

async fn execute_prepared(
    database_url: &str,
    env_key: &str,
    packages: &[PresentedPackage],
    mode: ReconcileMode<'_>,
    apply: bool,
) -> anyhow::Result<DataAccessReconcileResult> {
    ensure!(
        !env_key.is_empty(),
        "org, project and environment must not be empty"
    );
    let (mut client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = reconcile_mode(&mut client, env_key, packages, mode, apply).await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join data-access database connection")?
            .context("drive data-access database connection")?;
    }
    result
}

async fn reconcile_mode(
    client: &mut Client,
    env_key: &str,
    packages: &[PresentedPackage],
    mode: ReconcileMode<'_>,
    apply: bool,
) -> anyhow::Result<DataAccessReconcileResult> {
    let tx = client
        .transaction()
        .await
        .context("begin package data-access reconciliation")?;
    let result = reconcile_in_transaction(&tx, env_key, packages, mode, apply).await?;
    if apply {
        tx.commit()
            .await
            .context("commit package data-access reconciliation")?;
    } else {
        tx.rollback()
            .await
            .context("finish local data-access validation")?;
    }
    Ok(result)
}

/// Reconcile through the existing owner without committing the caller's transaction.
async fn reconcile_in_transaction(
    tx: &Transaction<'_>,
    env_key: &str,
    packages: &[PresentedPackage],
    mode: ReconcileMode<'_>,
    apply: bool,
) -> anyhow::Result<DataAccessReconcileResult> {
    let local = match mode {
        ReconcileMode::Local(environment) => Some(environment),
        ReconcileMode::Production => None,
    };
    tx.query_one(CLAIM_ENV_KEY_SQL, &[&env_key])
        .await
        .context("claim package tenant")?;
    tx.query_one(LOCK_SQL, &[])
        .await
        .context("lock project data-access carrier")?;
    validate_installed_set(tx, env_key, packages, local.is_some()).await?;
    let schemas = packages
        .iter()
        .flat_map(|package| package.schemas.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let relation_fields = load_relation_fields(tx, &schemas).await?;
    let overlays = packages
        .iter()
        .map(|package| package.overlay.clone())
        .collect::<Vec<_>>();
    let effective = derive_effective_data_access(&relation_fields, &overlays)
        .context("derive installed-set data-access authority")?;
    let role = tx
        .query_opt(
            "SELECT rolcanlogin FROM pg_catalog.pg_roles WHERE rolname = $1",
            &[&DATA_ACCESS_ROLE],
        )
        .await
        .context("read stable App ACL role")?;
    let Some(role) = role else {
        anyhow::bail!(
            "package-data-access-role-missing: role={DATA_ACCESS_ROLE}; remedy=prepare the project App role floor"
        );
    };
    ensure!(
        !role.get::<_, bool>(0),
        "package-data-access-role-login-refused: role={DATA_ACCESS_ROLE} must remain NOLOGIN"
    );

    let before = direct_acl(tx, &effective).await?;
    let desired = desired_acl(&effective);
    let before_effective = effective_acl(tx, &effective).await?;
    let desired_effective = desired_effective_acl(&effective);
    let residue = undeclared_residue(tx, &effective).await?;
    let changed = before != desired || before_effective != desired_effective || !residue.is_empty();
    if changed {
        tx.batch_execute(
            &render_effective_data_access_sql(&effective)
                .context("render installed-set data-access reconciliation")?,
        )
        .await
        .context("apply generated data-access reconciliation")?;
        if !residue.is_empty() {
            tx.batch_execute(&render_undeclared_revocation(effective.role(), &residue))
                .await
                .context("revoke App authority on undeclared package relations")?;
        }
    }
    let after = direct_acl(tx, &effective).await?;
    let after_effective = effective_acl(tx, &effective).await?;
    let after_residue = undeclared_residue(tx, &effective).await?;
    ensure!(
        after == desired,
        "package-data-access-postcondition-refused: server ACL differs from generated evidence"
    );
    ensure!(
        after_effective == desired_effective,
        "package-data-access-effective-authority-refused: role={DATA_ACCESS_ROLE}; authority remains outside the generated direct ACL through PUBLIC, ownership, or inherited roles"
    );
    // PostgreSQL never revokes an owner from its own relation, so authority the
    // App role owns survives every reconcile. Two outcomes converge, and the
    // refusal names both: a package declares the relation, or the relation goes.
    ensure!(
        after_residue.is_empty(),
        "package-data-access-undeclared-relation-refused: role={DATA_ACCESS_ROLE}; relations=[{}]; cause=the App role reaches authority no package declares, and an owner never loses a privilege on its own relation; remedy=declare the relation in a package, or drop the relation",
        residue_targets(&after_residue)
    );
    if apply && let Some(environment) = local {
        record_local_manifests(tx, env_key, environment, packages).await?;
    }
    Ok(DataAccessReconcileResult {
        coordinates: packages
            .iter()
            .map(|package| package.coordinate.clone())
            .collect(),
        changed,
    })
}

/// Write the presented manifest digest of every package into the local target comment.
///
/// The comment then names the `wamn.json` the running application came from. This
/// runs in the transaction that commits the grants, so the comment and the
/// grants move together or not at all.
async fn record_local_manifests(
    tx: &Transaction<'_>,
    env_key: &str,
    environment: &str,
    packages: &[PresentedPackage],
) -> anyhow::Result<()> {
    let mut comment =
        wamn_runtime::local_application::read_local_target_comment(tx, env_key, environment)
            .await?;
    for package in packages {
        wamn_package_control::apply_package::record_manifest(
            tx,
            &mut comment,
            &package.coordinate,
            &wamn_package_control::apply_package::manifest_digest(&package.directory),
        )
        .await?;
    }
    Ok(())
}

async fn load_relation_fields(
    tx: &Transaction<'_>,
    schemas: &[String],
) -> anyhow::Result<Vec<DataAccessRelationFields>> {
    tx.query(
        "SELECT namespace.nspname::text, relation.relname::text, \
                array_agg(attribute.attname::text ORDER BY attribute.attname::text COLLATE \"C\") \
           FROM pg_catalog.pg_class AS relation \
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
           JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
          WHERE namespace.nspname = ANY($1::text[]) \
            AND relation.relkind IN ('r', 'v') \
            AND relation.relname <> ALL($2::text[]) \
            AND attribute.attnum > 0 AND NOT attribute.attisdropped \
          GROUP BY namespace.nspname, relation.relname \
          ORDER BY namespace.nspname COLLATE \"C\", relation.relname COLLATE \"C\"",
        &[&schemas, &CONTROL_OWNED_RELATION_MAPS.as_slice()],
    )
    .await
    .context("read live package relation fields")?
    .into_iter()
    .map(|row| {
        Ok(DataAccessRelationFields::new(
            row.try_get::<_, String>(0)
                .context("decode relation schema")?,
            row.try_get::<_, String>(1)
                .context("decode relation table")?,
            row.try_get::<_, Vec<String>>(2)
                .context("decode relation fields")?,
        ))
    })
    .collect()
}

async fn direct_acl(
    tx: &Transaction<'_>,
    effective: &EffectiveDataAccess,
) -> anyhow::Result<DirectAcl> {
    let schemas = effective.schemas();
    let mut acl = DirectAcl::default();
    for row in tx
        .query(
            "SELECT namespace.nspname, \
                    COALESCE(grantee.rolname::text, 'PUBLIC'), \
                    entry.privilege_type, entry.is_grantable \
               FROM pg_catalog.pg_namespace AS namespace \
               CROSS JOIN LATERAL pg_catalog.aclexplode(namespace.nspacl) AS entry \
               LEFT JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = entry.grantee \
              WHERE (entry.grantee = 0 OR grantee.rolname = $1) \
                AND namespace.nspname = ANY($2::text[])",
            &[&effective.role(), &schemas],
        )
        .await
        .context("read direct schema ACL")?
    {
        acl.schema
            .insert((row.get(0), row.get(1), row.get(2), row.get(3)));
    }
    for relation in effective.relations() {
        for row in tx
            .query(
                "SELECT COALESCE(grantee.rolname::text, 'PUBLIC'), \
                        entry.privilege_type, entry.is_grantable \
                   FROM pg_catalog.pg_class AS relation \
                   JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                   CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) AS entry \
                   LEFT JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = entry.grantee \
                  WHERE (entry.grantee = 0 OR grantee.rolname = $1) \
                    AND namespace.nspname = $2 AND relation.relname = $3",
                &[&effective.role(), &relation.schema(), &relation.table()],
            )
            .await
            .with_context(|| {
                format!(
                    "read direct table ACL for {}.{}",
                    relation.schema(),
                    relation.table()
                )
            })?
        {
            acl.table.insert((
                relation.schema().to_owned(),
                relation.table().to_owned(),
                row.get(0),
                row.get(1),
                row.get(2),
            ));
        }
        for row in tx
            .query(
                "SELECT attribute.attname, COALESCE(grantee.rolname::text, 'PUBLIC'), \
                        entry.privilege_type, entry.is_grantable \
                   FROM pg_catalog.pg_class AS relation \
                   JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                   JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
                   CROSS JOIN LATERAL pg_catalog.aclexplode(attribute.attacl) AS entry \
                   LEFT JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = entry.grantee \
                  WHERE (entry.grantee = 0 OR grantee.rolname = $1) \
                    AND namespace.nspname = $2 AND relation.relname = $3 \
                    AND attribute.attnum > 0 AND NOT attribute.attisdropped",
                &[&effective.role(), &relation.schema(), &relation.table()],
            )
            .await
            .with_context(|| {
                format!(
                    "read direct column ACL for {}.{}",
                    relation.schema(),
                    relation.table()
                )
            })?
        {
            acl.column.insert((
                relation.schema().to_owned(),
                relation.table().to_owned(),
                row.get(0),
                row.get(1),
                row.get(2),
                row.get(3),
            ));
        }
    }
    Ok(acl)
}

async fn effective_acl(
    tx: &Transaction<'_>,
    effective: &EffectiveDataAccess,
) -> anyhow::Result<EffectiveAcl> {
    let mut acl = read_effective_privileges(tx, effective.role(), effective.schemas()).await?;
    let declared = effective
        .relations()
        .iter()
        .map(|relation| (relation.schema(), relation.table()))
        .collect::<BTreeSet<_>>();
    acl.table
        .retain(|(schema, table, _)| declared.contains(&(schema.as_str(), table.as_str())));
    acl.column
        .retain(|(schema, table, _, _)| declared.contains(&(schema.as_str(), table.as_str())));
    Ok(acl)
}

/// Read the App authority left on relations no presented package declares.
///
/// The scope is a wall. Only the schemas the presented packages own are read,
/// so a platform schema never enters the sweep. Inside those schemas a relation
/// counts when it carries an ACL a role holds. That means ordinary tables,
/// partitioned tables, views, materialized views and foreign tables, all of
/// which answer `has_table_privilege`, and sequences, which answer
/// `has_sequence_privilege`. Indexes and composite types carry no ACL and
/// PostgreSQL refuses a GRANT that names them, so the sweep leaves them out.
///
/// Only the relations the declared path already converges drop out here. Every
/// relation means every relation, so the applier-owned relation maps stay in
/// scope. Platform roles such as the CDC reader read those maps, and the App
/// role never does, so a revocation there removes nothing that is true. A named
/// exception list grows with the applier and hides the day that stops holding.
async fn undeclared_residue(
    tx: &Transaction<'_>,
    effective: &EffectiveDataAccess,
) -> anyhow::Result<UndeclaredResidue> {
    let schemas = effective.schemas();
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
    let sequence_privileges = vec!["SELECT", "UPDATE", "USAGE"];
    let mut residue = UndeclaredResidue::new();
    for row in tx
        .query(
            UNDECLARED_RESIDUE_SQL,
            &[
                &effective.role(),
                &schemas,
                &table_privileges,
                &column_privileges,
                &sequence_privileges,
            ],
        )
        .await
        .context("read residual App authority on undeclared package relations")?
    {
        let schema = row
            .try_get::<_, String>(0)
            .context("decode residue schema")?;
        let table = row
            .try_get::<_, String>(1)
            .context("decode residue relation")?;
        if effective
            .relations()
            .iter()
            .any(|relation| relation.schema() == schema && relation.table() == table)
        {
            continue;
        }
        residue.insert((schema, table, row.get(2), row.get(3), row.get(4)));
    }
    Ok(residue)
}

async fn validate_installed_set(
    tx: &Transaction<'_>,
    env_key: &str,
    packages: &[PresentedPackage],
    local: bool,
) -> anyhow::Result<()> {
    let installed = tx
        .query(SELECT_INSTALLED_SQL, &[&env_key])
        .await
        .context("read complete installed package set")?
        .into_iter()
        .map(|row| ((row.get(0), row.get(1)), row.get(2)))
        .collect::<CoordinateHashes>();
    let presented = packages
        .iter()
        .map(|package| {
            (
                (package.package_id.clone(), package.package_version.clone()),
                package.artifact_digest.clone(),
            )
        })
        .collect::<CoordinateHashes>();
    validate_package_set(&installed, &presented)?;
    // Published SQL sources retain their exact artifacts. Local code selection
    // does not advance the installed SQL records.
    if !local {
        validate_recorded_manifests(&installed, &presented)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use wamn_schema_control::plan_package_migrations;
    use wamn_schema_generator::validate_data_access_contribution;

    use super::{
        CoordinateHashes, UndeclaredResidue, prepare_local, render_undeclared_revocation,
        validate_package_set, validate_recorded_manifests,
    };

    const DOCK_SHA: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const WMS_SHA: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

    fn coordinates(entries: &[(&str, &str, &str)]) -> CoordinateHashes {
        entries
            .iter()
            .map(|(package_id, package_version, artifact_digest)| {
                (
                    ((*package_id).to_owned(), (*package_version).to_owned()),
                    Some((*artifact_digest).to_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn code_versions_share_one_installed_schema_contribution() {
        let installed = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        let presented = coordinates(&[("dock", "1.1.0", DOCK_SHA)]);
        validate_package_set(&installed, &presented).unwrap();
        // Published schema reconciliation still requires the installed source.
        assert!(validate_recorded_manifests(&installed, &presented).is_err());
    }

    #[test]
    fn missing_or_uninstalled_package_contributions_refuse() {
        let complete = coordinates(&[("dock", "1.0.0", DOCK_SHA), ("wms", "2.0.0", WMS_SHA)]);
        let partial = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        assert!(validate_package_set(&complete, &partial).is_err());
        assert!(validate_package_set(&partial, &complete).is_err());
    }

    /// A coordinate that `wamn dev` applied records no artifact digest, and
    /// reconcile refuses it as every release verb does.
    #[test]
    fn a_coordinate_without_an_artifact_refuses_like_every_release_verb() {
        let installed = CoordinateHashes::from([(("dock".to_owned(), "1.0.0".to_owned()), None)]);
        let presented = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        let refusal = validate_recorded_manifests(&installed, &presented)
            .expect_err("a NULL artifact digest refuses")
            .to_string();
        assert!(
            refusal.starts_with(wamn_schema_control::PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL),
            "{refusal}"
        );
    }

    #[test]
    fn a_presented_root_whose_bytes_moved_under_a_published_version_still_refuses() {
        let installed = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        let presented = coordinates(&[("dock", "1.0.0", WMS_SHA)]);
        validate_package_set(&installed, &presented)
            .expect("the coordinate is covered and applied; only its bytes moved");
        assert!(validate_recorded_manifests(&installed, &presented).is_err());

        // Local cutover retains the owner's exact parsed evidence; removing
        // source files after validation cannot substitute a later manifest/ACL.
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform_fixture");
        let mut original =
            wamn_package_control::apply_package::read_package_directory(&source).unwrap();
        // The copy below is a working tree, with no verified artifact digest.
        original.artifact_digest = None;
        let overlay_bytes = std::fs::read(
            wamn_schema_generator::output_root(&source).join("platform-policy/data-access.json"),
        )
        .unwrap();
        let root =
            std::env::temp_dir().join(format!("wamn-local-grant-evidence-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("wamn.json"), &original.manifest_bytes).unwrap();
        for migration in &original.migrations {
            let path = root.join(&migration.relative_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &migration.bytes).unwrap();
        }
        // A hand-written package keeps its evidence under `generated/`, which
        // the output root resolver reads, so nothing lands outside `root`.
        let path = root.join("generated/platform-policy/data-access.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &overlay_bytes).unwrap();
        let prepared = prepare_local(std::slice::from_ref(&root)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let selected = &prepared.0[0];
        assert_eq!(selected.directory, original);
        assert_eq!(selected.overlay.canonical_bytes(), overlay_bytes);
        validate_data_access_contribution(&selected.overlay, &selected.directory.manifest_bytes)
            .unwrap();
        plan_package_migrations(&selected.directory, None, &[]).unwrap();
    }

    #[test]
    fn a_residual_sequence_privilege_revokes_through_the_sequence_carrier() {
        let residue = UndeclaredResidue::from([(
            "inventory".to_owned(),
            "unconsumed_sequence".to_owned(),
            "S".to_owned(),
            None,
            "USAGE".to_owned(),
        )]);
        assert_eq!(
            render_undeclared_revocation("wamn_app", &residue),
            "REVOKE ALL PRIVILEGES ON SEQUENCE \"inventory\".\"unconsumed_sequence\" \
             FROM PUBLIC, \"wamn_app\";\n"
        );
    }

    #[test]
    fn a_residual_column_privilege_revokes_beside_the_relation_it_sits_on() {
        let residue = UndeclaredResidue::from([
            (
                "inventory".to_owned(),
                "unconsumed_view".to_owned(),
                "v".to_owned(),
                None,
                "SELECT".to_owned(),
            ),
            (
                "inventory".to_owned(),
                "unconsumed_view".to_owned(),
                "v".to_owned(),
                Some("id".to_owned()),
                "SELECT".to_owned(),
            ),
        ]);
        let sql = render_undeclared_revocation("wamn_app", &residue);
        assert!(
            sql.contains(
                "REVOKE ALL PRIVILEGES ON TABLE \"inventory\".\"unconsumed_view\" \
                 FROM PUBLIC, \"wamn_app\";"
            ),
            "the relation revocation is missing: {sql}"
        );
        assert!(
            sql.contains(
                "REVOKE SELECT (\"id\") ON TABLE \"inventory\".\"unconsumed_view\" \
                 FROM PUBLIC, \"wamn_app\";"
            ),
            "the column revocation is missing: {sql}"
        );
    }

    #[test]
    fn an_empty_residue_read_renders_no_revocation_at_all() {
        assert_eq!(
            render_undeclared_revocation("wamn_app", &UndeclaredResidue::new()),
            ""
        );
    }
}
