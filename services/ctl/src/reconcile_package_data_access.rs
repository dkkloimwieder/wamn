//! Apply one generated package data-access overlay after package migration.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::Args;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_schema_control::plan_package_migrations;
use wamn_schema_generator::{
    DATA_ACCESS_OVERLAY_PATH, DATA_ACCESS_ROLE, DataAccessOverlay, DataAccessRelationInventory,
    data_access_schemas, derive_data_access_overlay_from_inventory,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.data-access:' || current_database(), 0))";
const SELECT_APPLIED_SQL: &str = "\
SELECT manifest_sha256 FROM catalog.packages \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3";
// apply-package owns this OID history beside application tables; package ACL
// declarations neither consume nor reconcile control-owned objects.
const CONTROL_OWNED_ENTITY_MAP: &str = "wamn_entities";

/// Post-apply generated ACL reconciliation arguments.
#[derive(Debug, Args)]
pub struct ReconcilePackageDataAccessArgs {
    /// Package root containing wamn.json and generated platform-policy evidence.
    #[arg(long)]
    pub package: PathBuf,

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

/// Reconcile the exact checked-in generated overlay.
pub async fn run(args: ReconcilePackageDataAccessArgs) -> anyhow::Result<()> {
    let (coordinate, outcome) = execute(args).await?;
    println!(
        "reconciled data access for {coordinate}{}",
        if outcome.is_noop() {
            " (already converged)"
        } else {
            ""
        }
    );
    Ok(())
}

/// Reconcile one package and return its observable effect state.
pub async fn reconcile_package_data_access(
    args: ReconcilePackageDataAccessArgs,
) -> anyhow::Result<DataAccessReconcileResult> {
    execute(args).await.map(|(_, outcome)| outcome)
}

async fn execute(
    args: ReconcilePackageDataAccessArgs,
) -> anyhow::Result<(String, DataAccessReconcileResult)> {
    ensure!(!args.tenant.is_empty(), "tenant must not be empty");
    let directory = super::apply_package::read_package_directory(&args.package)?;
    let plan = plan_package_migrations(&directory, None)
        .context("validate package directory before data-access reconciliation")?;
    let application_schemas = data_access_schemas(&directory.manifest_bytes)
        .context("derive package data-access schema set")?;
    let overlay_path = args.package.join(DATA_ACCESS_OVERLAY_PATH);
    let overlay_bytes =
        std::fs::read(&overlay_path).with_context(|| format!("read {}", overlay_path.display()))?;
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

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = reconcile(
        &mut client,
        &args.tenant,
        plan.coordinate.package_id(),
        plan.coordinate.package_version(),
        &directory.manifest_bytes,
        &application_schemas,
        &overlay,
    )
    .await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join data-access database connection")?
            .context("drive data-access database connection")?;
    }
    Ok((coordinate, result?))
}

async fn reconcile(
    client: &mut Client,
    tenant: &str,
    package_id: &str,
    package_version: &str,
    manifest_bytes: &[u8],
    application_schemas: &[String],
    overlay: &DataAccessOverlay,
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
    let applied = tx
        .query_opt(
            SELECT_APPLIED_SQL,
            &[&tenant, &package_id, &package_version],
        )
        .await
        .context("read applied package root")?;
    let Some(applied) = applied else {
        anyhow::bail!(
            "package-data-access-package-not-applied: package={package_id}@{package_version}; remedy=run apply-package against the target"
        );
    };
    let recorded_hash = applied.get::<_, String>(0);
    ensure!(
        recorded_hash == overlay.manifest_sha256(),
        "package-data-access-source-drift: package={package_id}@{package_version}; recorded-sha256={recorded_hash}; presented-sha256={}",
        overlay.manifest_sha256()
    );
    let inventory = relation_inventory(&tx, application_schemas).await?;
    let expected = derive_data_access_overlay_from_inventory(&inventory, manifest_bytes)
        .context("re-derive generated data-access evidence from the applied package")?;
    ensure!(
        &expected == overlay,
        "package-data-access-artifact-drift: package={package_id}@{package_version}; generated evidence does not match the applied manifest and live relation inventory"
    );
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

    let before = direct_acl(&tx, overlay).await?;
    let desired = desired_acl(overlay);
    let before_effective = effective_acl(&tx, overlay).await?;
    let desired_effective = desired_effective_acl(overlay);
    let changed = before != desired || before_effective != desired_effective;
    if changed {
        tx.batch_execute(
            &overlay
                .reconcile_sql()
                .context("render generated data-access reconciliation")?,
        )
        .await
        .context("apply generated data-access reconciliation")?;
    }
    let after = direct_acl(&tx, overlay).await?;
    let after_effective = effective_acl(&tx, overlay).await?;
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
    overlay: &DataAccessOverlay,
) -> anyhow::Result<DirectAcl> {
    let schemas = overlay.schemas();
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
            &[&overlay.role(), &schemas],
        )
        .await
        .context("read direct schema ACL")?
    {
        acl.schema
            .insert((row.get(0), row.get(1), row.get(2), row.get(3)));
    }
    for relation in overlay.relations() {
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
                &[&overlay.role(), &relation.schema(), &relation.table()],
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
                &[&overlay.role(), &relation.schema(), &relation.table()],
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

fn desired_acl(overlay: &DataAccessOverlay) -> DirectAcl {
    let mut desired = DirectAcl::default();
    desired
        .schema
        .extend(overlay.schemas().iter().map(|schema| {
            (
                schema.clone(),
                overlay.role().to_owned(),
                "USAGE".to_owned(),
                false,
            )
        }));
    for relation in overlay.relations() {
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
                    overlay.role().to_owned(),
                    privilege.to_owned(),
                    false,
                )
            }));
        }
        desired
            .column
            .extend(relation.granted_update_fields().map(|field| {
                (
                    relation.schema().to_owned(),
                    relation.table().to_owned(),
                    field.to_owned(),
                    overlay.role().to_owned(),
                    "UPDATE".to_owned(),
                    false,
                )
            }));
    }
    desired
}

async fn effective_acl(
    tx: &Transaction<'_>,
    overlay: &DataAccessOverlay,
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
    for schema in overlay.schemas() {
        for row in tx
            .query(
                "SELECT privilege \
                   FROM pg_catalog.pg_namespace AS namespace \
                   CROSS JOIN unnest($3::text[]) AS privilege \
                  WHERE namespace.nspname = $2 \
                    AND pg_catalog.has_schema_privilege($1, namespace.oid, privilege)",
                &[&overlay.role(), &schema, &schema_privileges],
            )
            .await
            .with_context(|| format!("read effective schema ACL for {schema}"))?
        {
            acl.schema.insert((schema.clone(), row.get(0)));
        }
    }
    for relation in overlay.relations() {
        for row in tx
            .query(
                "SELECT privilege \
                   FROM pg_catalog.pg_class AS relation \
                   JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                   CROSS JOIN unnest($4::text[]) AS privilege \
                  WHERE namespace.nspname = $2 AND relation.relname = $3 \
                    AND pg_catalog.has_table_privilege($1, relation.oid, privilege)",
                &[
                    &overlay.role(),
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
                    &overlay.role(),
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

fn desired_effective_acl(overlay: &DataAccessOverlay) -> EffectiveAcl {
    let mut desired = EffectiveAcl::default();
    desired.schema.extend(
        overlay
            .schemas()
            .iter()
            .map(|schema| (schema.clone(), "USAGE".to_owned())),
    );
    for relation in overlay.relations() {
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
        desired
            .column
            .extend(relation.granted_update_fields().map(|field| {
                (
                    relation.schema().to_owned(),
                    relation.table().to_owned(),
                    field.to_owned(),
                    "UPDATE".to_owned(),
                )
            }));
    }
    desired
}
