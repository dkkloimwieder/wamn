//! Converge the generated GuestSql authority union for the installed package set.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::Args;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_schema_control::plan_package_migrations;
use wamn_schema_generator::{
    DATA_ACCESS_OVERLAY_PATH, DATA_ACCESS_ROLE, DataAccessOverlay, DataAccessRelationInventory,
    EffectiveDataAccess, data_access_schemas, derive_effective_data_access,
    render_effective_data_access_sql, validate_data_access_contribution,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.data-access:' || current_database(), 0))";
const SELECT_INSTALLED_SQL: &str = "\
SELECT package_id, package_version, manifest_sha256 FROM catalog.packages \
 WHERE tenant_id = $1 ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\"";
// apply-package owns these OID histories beside application tables. No package
// declaration consumes them, so the declared relation inventory leaves them out.
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

/// Manifest hash of each package coordinate, keyed by id and version.
type CoordinateHashes = BTreeMap<(String, String), String>;

/// Post-apply generated ACL reconciliation arguments.
#[derive(Debug, Args)]
pub struct ReconcilePackageDataAccessArgs {
    /// Installed package roots containing wamn.json and generated policy evidence.
    #[arg(long = "package", required = true)]
    pub packages: Vec<PathBuf>,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,

    /// Tenant owning the already-applied package coordinate.
    #[arg(long)]
    pub tenant: String,
}

/// Effect state forming the reconciliation closing predicate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataAccessReconcileResult {
    changed: bool,
}

impl DataAccessReconcileResult {
    /// Whether the server already held the exact generated direct ACL.
    pub const fn is_noop(self) -> bool {
        !self.changed
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EffectiveAcl {
    schema: BTreeSet<(String, String)>,
    table: BTreeSet<(String, String, String)>,
    column: BTreeSet<(String, String, String, String)>,
}

struct PresentedPackage {
    coordinate: String,
    package_id: String,
    package_version: String,
    manifest_sha256: String,
    schemas: Vec<String>,
    overlay: DataAccessOverlay,
}

/// Reconcile the exact installed set of generated package contributions.
pub async fn run(args: ReconcilePackageDataAccessArgs) -> anyhow::Result<()> {
    let (coordinates, outcome) = execute(args).await?;
    println!(
        "reconciled data access for [{}]{}",
        coordinates.join(", "),
        if outcome.is_noop() {
            " (already converged)"
        } else {
            ""
        }
    );
    Ok(())
}

/// Reconcile the installed package set and return its observable effect state.
pub async fn reconcile_package_data_access(
    args: ReconcilePackageDataAccessArgs,
) -> anyhow::Result<DataAccessReconcileResult> {
    execute(args).await.map(|(_, outcome)| outcome)
}

async fn execute(
    args: ReconcilePackageDataAccessArgs,
) -> anyhow::Result<(Vec<String>, DataAccessReconcileResult)> {
    ensure!(!args.tenant.is_empty(), "tenant must not be empty");
    ensure!(
        !args.packages.is_empty(),
        "package-data-access-installed-set-empty"
    );
    let mut packages = Vec::with_capacity(args.packages.len());
    let mut coordinates = BTreeSet::new();
    let mut package_ids = BTreeSet::new();
    for package_root in &args.packages {
        let directory = super::apply_package::read_package_directory(package_root)?;
        let plan = plan_package_migrations(&directory, None)
            .context("validate package directory before data-access reconciliation")?;
        let schemas = data_access_schemas(&directory.manifest_bytes)
            .context("derive package data-access schema set")?;
        let overlay_path = package_root.join(DATA_ACCESS_OVERLAY_PATH);
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
        ensure!(
            overlay.manifest_sha256() == plan.manifest_sha256,
            "package-data-access-manifest-drift: coordinate={coordinate}; recorded-sha256={}; presented-sha256={}",
            overlay.manifest_sha256(),
            plan.manifest_sha256
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
            manifest_sha256: plan.manifest_sha256,
            schemas,
            overlay,
        });
    }
    packages.sort_by(|left, right| left.coordinate.cmp(&right.coordinate));

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = reconcile(&mut client, &args.tenant, &packages).await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join data-access database connection")?
            .context("drive data-access database connection")?;
    }
    Ok((
        packages
            .into_iter()
            .map(|package| package.coordinate)
            .collect(),
        result?,
    ))
}

async fn reconcile(
    client: &mut Client,
    tenant: &str,
    packages: &[PresentedPackage],
) -> anyhow::Result<DataAccessReconcileResult> {
    let tx = client
        .transaction()
        .await
        .context("begin package data-access reconciliation")?;
    tx.query_one(CLAIM_TENANT_SQL, &[&tenant])
        .await
        .context("claim package tenant")?;
    tx.query_one(LOCK_SQL, &[])
        .await
        .context("lock project data-access carrier")?;
    validate_installed_set(&tx, tenant, packages).await?;
    let schemas = packages
        .iter()
        .flat_map(|package| package.schemas.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let inventory = relation_inventory(&tx, &schemas).await?;
    let overlays = packages
        .iter()
        .map(|package| package.overlay.clone())
        .collect::<Vec<_>>();
    let effective = derive_effective_data_access(&inventory, &overlays)
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

    let before = direct_acl(&tx, &effective).await?;
    let desired = desired_acl(&effective);
    let before_effective = effective_acl(&tx, &effective).await?;
    let desired_effective = desired_effective_acl(&effective);
    let residue = undeclared_residue(&tx, &effective).await?;
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
    let after = direct_acl(&tx, &effective).await?;
    let after_effective = effective_acl(&tx, &effective).await?;
    let after_residue = undeclared_residue(&tx, &effective).await?;
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
    tx.commit()
        .await
        .context("commit package data-access reconciliation")?;
    Ok(DataAccessReconcileResult { changed })
}

async fn validate_installed_set(
    tx: &Transaction<'_>,
    tenant: &str,
    packages: &[PresentedPackage],
) -> anyhow::Result<()> {
    let mut installed = CoordinateHashes::new();
    for row in tx
        .query(SELECT_INSTALLED_SQL, &[&tenant])
        .await
        .context("read complete installed package set")?
    {
        let package_id = row.get::<_, String>(0);
        let package_version = row.get::<_, String>(1);
        ensure!(
            installed
                .insert(
                    (package_id.clone(), package_version.clone()),
                    row.get::<_, String>(2)
                )
                .is_none(),
            "package-data-access-installed-set-repeats-coordinate: {package_id}@{package_version}"
        );
    }
    let presented = packages
        .iter()
        .map(|package| {
            (
                (package.package_id.clone(), package.package_version.clone()),
                package.manifest_sha256.clone(),
            )
        })
        .collect::<CoordinateHashes>();
    validate_presented_lineages(&installed, &presented)
}

/// Refuse unless the presented roots cover every applied package lineage.
///
/// A source tree carries one version of a package at a time, so the applied set
/// is a lineage history rather than a live set. An author who bumps a version,
/// or who reverts a failed bump, leaves a sibling coordinate applied beside the
/// live one. That sibling is the same authority contributor at another point in
/// its lineage, so the presented root speaks for the whole lineage. Only a
/// package with no presented root at all drops a contribution from the union,
/// and only that case refuses. A presented coordinate that never reached Apply
/// also refuses, because its declared relations are not on the server yet.
fn validate_presented_lineages(
    installed: &CoordinateHashes,
    presented: &CoordinateHashes,
) -> anyhow::Result<()> {
    let presented_packages = presented
        .keys()
        .map(|(package_id, _)| package_id.clone())
        .collect::<BTreeSet<_>>();
    let missing = installed
        .keys()
        .map(|(package_id, _)| package_id.clone())
        .filter(|package_id| !presented_packages.contains(package_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let unapplied = presented
        .keys()
        .filter(|coordinate| !installed.contains_key(*coordinate))
        .map(|(package_id, package_version)| format!("{package_id}@{package_version}"))
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty() && unapplied.is_empty(),
        "package-data-access-installed-set-mismatch: missing-packages=[{}]; unapplied-artifacts=[{}]; remedy=present one root for each applied package and apply each presented root",
        missing.join(","),
        unapplied.join(",")
    );
    for ((package_id, package_version), presented_hash) in presented {
        let recorded_hash = installed
            .get(&(package_id.clone(), package_version.clone()))
            .expect("every presented coordinate was proved applied");
        ensure!(
            recorded_hash == presented_hash,
            "package-data-access-source-drift: package={package_id}@{package_version}; recorded-sha256={recorded_hash}; presented-sha256={presented_hash}"
        );
    }
    Ok(())
}

async fn relation_inventory(
    tx: &Transaction<'_>,
    schemas: &[String],
) -> anyhow::Result<Vec<DataAccessRelationInventory>> {
    tx.query(
        "SELECT namespace.nspname::text, relation.relname::text, \
                array_agg(attribute.attname::text ORDER BY attribute.attname::text COLLATE \"C\") \
           FROM pg_catalog.pg_class AS relation \
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
           JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
          WHERE namespace.nspname = ANY($1::text[]) \
            AND relation.relkind = 'r' \
            AND relation.relname <> ALL($2::text[]) \
            AND attribute.attnum > 0 AND NOT attribute.attisdropped \
          GROUP BY namespace.nspname, relation.relname \
          ORDER BY namespace.nspname COLLATE \"C\", relation.relname COLLATE \"C\"",
        &[&schemas, &CONTROL_OWNED_RELATION_MAPS.as_slice()],
    )
    .await
    .context("read live package relation inventory")?
    .into_iter()
    .map(|row| {
        Ok(DataAccessRelationInventory::new(
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

fn desired_acl(effective: &EffectiveDataAccess) -> DirectAcl {
    let mut desired = DirectAcl::default();
    desired
        .schema
        .extend(effective.schemas().iter().map(|schema| {
            (
                schema.clone(),
                effective.role().to_owned(),
                "USAGE".to_owned(),
                false,
            )
        }));
    for relation in effective.relations() {
        for (privilege, fields) in [
            ("SELECT", relation.select_fields()),
            ("INSERT", relation.insert_fields()),
            ("UPDATE", relation.update_fields()),
        ] {
            desired.column.extend(fields.iter().map(|field| {
                (
                    relation.schema().to_owned(),
                    relation.table().to_owned(),
                    field.clone(),
                    effective.role().to_owned(),
                    privilege.to_owned(),
                    false,
                )
            }));
        }
    }
    desired
}

async fn effective_acl(
    tx: &Transaction<'_>,
    effective: &EffectiveDataAccess,
) -> anyhow::Result<EffectiveAcl> {
    let schema_privileges = vec!["CREATE", "USAGE"];
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
    let mut acl = EffectiveAcl::default();
    for schema in effective.schemas() {
        for row in tx
            .query(
                "SELECT privilege \
                   FROM pg_catalog.pg_namespace AS namespace \
                   CROSS JOIN unnest($3::text[]) AS privilege \
                  WHERE namespace.nspname = $2 \
                    AND pg_catalog.has_schema_privilege($1, namespace.oid, privilege)",
                &[&effective.role(), &schema, &schema_privileges],
            )
            .await
            .with_context(|| format!("read effective schema ACL for {schema}"))?
        {
            acl.schema.insert((schema.clone(), row.get(0)));
        }
    }
    for relation in effective.relations() {
        for row in tx
            .query(
                "SELECT privilege \
                   FROM pg_catalog.pg_class AS relation \
                   JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                   CROSS JOIN unnest($4::text[]) AS privilege \
                  WHERE namespace.nspname = $2 AND relation.relname = $3 \
                    AND pg_catalog.has_table_privilege($1, relation.oid, privilege)",
                &[
                    &effective.role(),
                    &relation.schema(),
                    &relation.table(),
                    &table_privileges,
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "read effective table ACL for {}.{}",
                    relation.schema(),
                    relation.table()
                )
            })?
        {
            acl.table.insert((
                relation.schema().to_owned(),
                relation.table().to_owned(),
                row.get(0),
            ));
        }
        for row in tx
            .query(
                "SELECT attribute.attname::text, privilege \
                   FROM pg_catalog.pg_class AS relation \
                   JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                   JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
                   CROSS JOIN unnest($4::text[]) AS privilege \
                  WHERE namespace.nspname = $2 AND relation.relname = $3 \
                    AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                    AND pg_catalog.has_column_privilege($1, relation.oid, attribute.attnum, privilege)",
                &[
                    &effective.role(),
                    &relation.schema(),
                    &relation.table(),
                    &column_privileges,
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "read effective column ACL for {}.{}",
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
            ));
        }
    }
    Ok(acl)
}

fn desired_effective_acl(effective: &EffectiveDataAccess) -> EffectiveAcl {
    let mut desired = EffectiveAcl::default();
    desired.schema.extend(
        effective
            .schemas()
            .iter()
            .map(|schema| (schema.clone(), "USAGE".to_owned())),
    );
    for relation in effective.relations() {
        for (privilege, fields) in [
            ("SELECT", relation.select_fields()),
            ("INSERT", relation.insert_fields()),
            ("UPDATE", relation.update_fields()),
        ] {
            desired.column.extend(fields.iter().map(|field| {
                (
                    relation.schema().to_owned(),
                    relation.table().to_owned(),
                    field.clone(),
                    privilege.to_owned(),
                )
            }));
        }
    }
    desired
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

/// Render the revocation that clears one residue read.
///
/// Revoking a privilege the role never held is not an error in PostgreSQL, so
/// the render stays safe even when the read and the write disagree. The role
/// and PUBLIC both lose the privilege, because PUBLIC reaches the App role.
fn render_undeclared_revocation(role: &str, residue: &UndeclaredResidue) -> String {
    let role = quote_identifier(role);
    let mut targets = BTreeMap::<(&str, &str, &str), BTreeSet<&str>>::new();
    for (schema, table, kind, column, _) in residue {
        let columns = targets
            .entry((schema.as_str(), table.as_str(), kind.as_str()))
            .or_default();
        if let Some(column) = column {
            columns.insert(column.as_str());
        }
    }
    let mut sql = String::new();
    for ((schema, table, kind), columns) in targets {
        let target = format!("{}.{}", quote_identifier(schema), quote_identifier(table));
        // A sequence carries its own privilege vocabulary, and REVOKE refuses to
        // name a sequence as a table.
        let carrier = if kind == "S" { "SEQUENCE" } else { "TABLE" };
        writeln!(
            sql,
            "REVOKE ALL PRIVILEGES ON {carrier} {target} FROM PUBLIC, {role};"
        )
        .expect("writing SQL to a String cannot fail");
        if columns.is_empty() {
            continue;
        }
        // A table-level revocation leaves a direct column grant in place, so the
        // columns the read named lose each column privilege by name.
        let columns = columns
            .into_iter()
            .map(quote_identifier)
            .collect::<Vec<_>>()
            .join(", ");
        for privilege in ["SELECT", "INSERT", "UPDATE", "REFERENCES"] {
            writeln!(
                sql,
                "REVOKE {privilege} ({columns}) ON TABLE {target} FROM PUBLIC, {role};"
            )
            .expect("writing SQL to a String cannot fail");
        }
    }
    sql
}

/// Name each relation a refusal has to report.
fn residue_targets(residue: &UndeclaredResidue) -> String {
    residue
        .iter()
        .map(|(schema, table, ..)| format!("{schema}.{table}"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",")
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::{
        CoordinateHashes, UndeclaredResidue, render_undeclared_revocation,
        validate_presented_lineages,
    };

    const DOCK_SHA: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const WMS_SHA: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

    fn coordinates(entries: &[(&str, &str, &str)]) -> CoordinateHashes {
        entries
            .iter()
            .map(|(package_id, package_version, manifest_sha256)| {
                (
                    ((*package_id).to_owned(), (*package_version).to_owned()),
                    (*manifest_sha256).to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn a_tree_carrying_only_the_earlier_version_reconciles_beside_the_applied_bump() {
        let installed = coordinates(&[("dock", "1.0.0", DOCK_SHA), ("dock", "1.1.0", DOCK_SHA)]);
        let presented = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        validate_presented_lineages(&installed, &presented)
            .expect("a reverted tree recovers beside the coordinate its failed bump applied");
    }

    #[test]
    fn a_tree_carrying_only_the_newer_version_reconciles_beside_the_applied_predecessor() {
        let installed = coordinates(&[("dock", "1.0.0", DOCK_SHA), ("dock", "1.1.0", DOCK_SHA)]);
        let presented = coordinates(&[("dock", "1.1.0", DOCK_SHA)]);
        validate_presented_lineages(&installed, &presented)
            .expect("a bumped tree recovers beside the coordinate it replaces");
    }

    #[test]
    fn an_applied_package_with_no_presented_root_still_refuses_with_a_remedy_the_author_can_run() {
        let installed = coordinates(&[("dock", "1.0.0", DOCK_SHA), ("wms", "2.0.0", WMS_SHA)]);
        let presented = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        let refusal = validate_presented_lineages(&installed, &presented)
            .expect_err("a dropped package contribution must refuse");
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("missing-packages=[wms]"),
            "the refusal did not name the uncovered package: {refusal}"
        );
        assert!(
            refusal.contains("remedy=present one root for each applied package"),
            "the refusal did not name a remedy the author can run: {refusal}"
        );
    }

    #[test]
    fn a_presented_coordinate_that_never_reached_apply_refuses() {
        let installed = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        let presented = coordinates(&[("dock", "1.1.0", DOCK_SHA)]);
        let refusal = validate_presented_lineages(&installed, &presented)
            .expect_err("an unapplied presented coordinate must refuse")
            .to_string();
        assert!(
            refusal.contains("unapplied-artifacts=[dock@1.1.0]"),
            "the refusal did not name the unapplied coordinate: {refusal}"
        );
    }

    #[test]
    fn a_presented_root_whose_bytes_moved_under_a_published_version_still_refuses() {
        let installed = coordinates(&[("dock", "1.0.0", DOCK_SHA)]);
        let presented = coordinates(&[("dock", "1.0.0", WMS_SHA)]);
        let refusal = validate_presented_lineages(&installed, &presented)
            .expect_err("a moved manifest under a published version must refuse")
            .to_string();
        assert!(
            refusal.contains("package-data-access-source-drift: package=dock@1.0.0"),
            "the refusal did not name the immutable coordinate: {refusal}"
        );
    }

    #[test]
    fn a_residual_sequence_privilege_revokes_through_the_sequence_carrier() {
        let residue = UndeclaredResidue::from([(
            "receiving".to_owned(),
            "unconsumed_sequence".to_owned(),
            "S".to_owned(),
            None,
            "USAGE".to_owned(),
        )]);
        assert_eq!(
            render_undeclared_revocation("wamn_app", &residue),
            "REVOKE ALL PRIVILEGES ON SEQUENCE \"receiving\".\"unconsumed_sequence\" \
             FROM PUBLIC, \"wamn_app\";\n"
        );
    }

    #[test]
    fn a_residual_column_privilege_revokes_beside_the_relation_it_sits_on() {
        let residue = UndeclaredResidue::from([
            (
                "receiving".to_owned(),
                "unconsumed_view".to_owned(),
                "v".to_owned(),
                None,
                "SELECT".to_owned(),
            ),
            (
                "receiving".to_owned(),
                "unconsumed_view".to_owned(),
                "v".to_owned(),
                Some("id".to_owned()),
                "SELECT".to_owned(),
            ),
        ]);
        let sql = render_undeclared_revocation("wamn_app", &residue);
        assert!(
            sql.contains(
                "REVOKE ALL PRIVILEGES ON TABLE \"receiving\".\"unconsumed_view\" \
                 FROM PUBLIC, \"wamn_app\";"
            ),
            "the relation revocation is missing: {sql}"
        );
        assert!(
            sql.contains(
                "REVOKE SELECT (\"id\") ON TABLE \"receiving\".\"unconsumed_view\" \
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
