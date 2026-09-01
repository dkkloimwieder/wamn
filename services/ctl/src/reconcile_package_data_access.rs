//! Converge the generated GuestSql authority union for the installed package set.

use std::collections::{BTreeMap, BTreeSet};
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
// apply-package owns this OID history beside application tables; package ACL
// declarations neither consume nor reconcile control-owned objects.
const CONTROL_OWNED_ENTITY_MAP: &str = "wamn_entities";

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
    let changed = before != desired || before_effective != desired_effective;
    if changed {
        tx.batch_execute(
            &render_effective_data_access_sql(&effective)
                .context("render installed-set data-access reconciliation")?,
        )
        .await
        .context("apply generated data-access reconciliation")?;
    }
    let after = direct_acl(&tx, &effective).await?;
    let after_effective = effective_acl(&tx, &effective).await?;
    ensure!(
        after == desired,
        "package-data-access-postcondition-refused: server ACL differs from generated evidence"
    );
    ensure!(
        after_effective == desired_effective,
        "package-data-access-effective-authority-refused: role={DATA_ACCESS_ROLE}; authority remains outside the generated direct ACL through PUBLIC, ownership, or inherited roles"
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
    let mut installed = BTreeMap::new();
    for row in tx
        .query(SELECT_INSTALLED_SQL, &[&tenant])
        .await
        .context("read complete installed package set")?
    {
        let package_id = row.get::<_, String>(0);
        let package_version = row.get::<_, String>(1);
        let coordinate = format!("{package_id}@{package_version}");
        ensure!(
            installed
                .insert(coordinate.clone(), row.get::<_, String>(2))
                .is_none(),
            "package-data-access-installed-set-repeats-coordinate: {coordinate}"
        );
    }
    let presented = packages
        .iter()
        .map(|package| {
            (
                format!("{}@{}", package.package_id, package.package_version),
                package.manifest_sha256.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let missing = installed
        .keys()
        .filter(|coordinate| !presented.contains_key(*coordinate))
        .cloned()
        .collect::<Vec<_>>();
    let unexpected = presented
        .keys()
        .filter(|coordinate| !installed.contains_key(*coordinate))
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty() && unexpected.is_empty(),
        "package-data-access-installed-set-mismatch: missing-artifacts=[{}]; unexpected-artifacts=[{}]; remedy=present every applied package root",
        missing.join(","),
        unexpected.join(",")
    );
    for (coordinate, recorded_hash) in installed {
        let presented_hash = presented
            .get(&coordinate)
            .expect("installed and presented coordinate sets were proved equal");
        ensure!(
            &recorded_hash == presented_hash,
            "package-data-access-source-drift: package={coordinate}; recorded-sha256={recorded_hash}; presented-sha256={presented_hash}"
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
            AND relation.relname <> $2 \
            AND attribute.attnum > 0 AND NOT attribute.attisdropped \
          GROUP BY namespace.nspname, relation.relname \
          ORDER BY namespace.nspname COLLATE \"C\", relation.relname COLLATE \"C\"",
        &[&schemas, &CONTROL_OWNED_ENTITY_MAP],
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
