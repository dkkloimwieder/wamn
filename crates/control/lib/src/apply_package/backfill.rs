//! Commit bounded backfill batches while the predecessor remains installed.

use anyhow::{Context as _, ensure};
use tokio_postgres::Client;
use wamn_schema_control::plan_package_migrations;
use wamn_schema_introspection::{
    ir::ColumnType,
    migration_policy::{StageUniqueKey, validate_stage_batch},
};

use super::{
    ApplicationMode, ApplyOutcome, CLAIM_TENANT_SQL, LOCK_PACKAGE_SQL, PreparedPackage,
    apply_prepared, bind_apply_package_principal, current_package_version, load_applied_package,
    roles, stage, synchronization,
};
use crate::package_upgrade::progress::{self, StageIdentity, StageStatus};

pub(super) async fn apply(
    client: &mut Client,
    tenant: &str,
    package: &PreparedPackage,
    mode: ApplicationMode<'_>,
) -> anyhow::Result<ApplyOutcome> {
    ensure!(
        !matches!(
            mode,
            ApplicationMode::Local(_)
                | ApplicationMode::BackfillCompletion
                | ApplicationMode::Coordinated
        ),
        "backfill requires an installed predecessor and upgrade qualification"
    );
    let declaration = package
        .manifest
        .upgrade_stage
        .as_ref()
        .expect("backfill dispatch has a stage");
    let batch = declaration
        .backfill
        .as_ref()
        .context("backfill stage has no batch SQL")?;
    let package_id = &package.manifest.package.id;
    let version = &package.manifest.package.version;
    let predecessor_version = package
        .manifest
        .package
        .predecessor_version
        .as_deref()
        .context("backfill stage has no predecessor")?;
    let schemas = wamn_schema_generator::data_access_schemas(&package.directory.manifest_bytes)?;
    ensure!(schemas.len() == 1, "backfill requires one runtime schema");
    let schema = &schemas[0];
    let batch_size = i32::try_from(batch.batch_size)?;

    loop {
        let tx = client
            .transaction()
            .await
            .context("begin package backfill batch")?;
        tx.batch_execute(
            "SET LOCAL lock_timeout = '5s'; SET LOCAL standard_conforming_strings = on",
        )
        .await?;
        tx.query_one(CLAIM_TENANT_SQL, &[&tenant]).await?;
        bind_apply_package_principal(&tx).await?;
        tx.query_one(crate::reconcile_package_data_access::LOCK_SQL, &[])
            .await?;
        tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &package_id])
            .await?;

        if load_applied_package(&tx, tenant, package_id, version)
            .await?
            .is_some()
        {
            let outcome = apply_prepared(&tx, tenant, package, mode).await?;
            tx.commit().await?;
            return Ok(outcome);
        }
        ensure!(
            current_package_version(&tx, tenant, package_id)
                .await?
                .as_deref()
                == Some(predecessor_version),
            "backfill requires installed predecessor {package_id}@{predecessor_version}"
        );
        let predecessor = load_applied_package(&tx, tenant, package_id, predecessor_version)
            .await?
            .context("backfill predecessor records are missing")?;
        let plan = plan_package_migrations(&package.directory, Some(&predecessor))?;
        ensure!(
            plan.pending.is_empty(),
            "backfill stage cannot carry a DDL suffix; apply an expand version first"
        );
        synchronization::verify_inherited(
            &tx,
            tenant,
            package_id,
            &package.manifest,
            &package.migration_policy.synchronizations,
            &std::collections::BTreeSet::new(),
        )
        .await?;
        let accepted = match mode {
            ApplicationMode::Production(supplied) => {
                crate::package_upgrade::require_application(
                    &tx,
                    tenant,
                    &package.root,
                    &package.directory,
                    &plan,
                    supplied,
                    false,
                )
                .await?
            }
            ApplicationMode::Qualification => None,
            ApplicationMode::Local(_)
            | ApplicationMode::BackfillCompletion
            | ApplicationMode::Coordinated => {
                unreachable!("mode checked before transaction")
            }
        };
        let artifact = crate::package_artifact::package_artifact_digest(&package.root)?;
        let identity = StageIdentity {
            tenant_id: tenant.to_owned(),
            package_id: package_id.clone(),
            package_version: version.clone(),
            predecessor_version: predecessor_version.to_owned(),
            package_artifact_digest: artifact.clone(),
            // Qualification uses an owned disposable copy before final
            // evidence exists, so the artifact digest stands for both.
            predecessor_release_digest: accepted.as_ref().map_or_else(
                || artifact.clone(),
                |accepted| accepted.predecessor_release_digest().to_owned(),
            ),
            evidence_digest: accepted.as_ref().map_or_else(
                || artifact.clone(),
                |accepted| accepted.qualification_sha256().to_owned(),
            ),
        };
        progress::require_no_other_stage(&tx, tenant, package_id, version, &artifact).await?;
        let recorded = progress::open(&tx, &identity, &batch.initial_cursor).await?;
        ensure!(
            recorded.status == StageStatus::InProgress,
            "backfill progress is complete but the package version is absent"
        );
        if recorded.completed_batches == 0 {
            stage::conditions(
                &tx,
                tenant,
                package_id,
                schema,
                "preconditions",
                &declaration.preconditions,
            )
            .await?;
        }
        let relations = stage::owned_relations(&tx, tenant, package_id, schema).await?;
        let mut keys = Vec::new();
        for row in tx
            .query(
                "SELECT n.nspname,c.relname,a.attname FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON c.oid=i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid AND a.attnum=i.indkey[0] \
             WHERE n.nspname=$1 AND i.indisunique AND i.indisvalid AND i.indisready \
               AND i.indimmediate AND i.indnkeyatts=1 AND i.indpred IS NULL \
               AND i.indexprs IS NULL AND a.attnotnull",
                &[&schema],
            )
            .await?
        {
            keys.push(StageUniqueKey {
                schema: row.try_get(0)?,
                relation: row.try_get(1)?,
                column: row.try_get(2)?,
            });
        }
        validate_stage_batch(
            "upgrade_stage.backfill.sql",
            batch.sql.as_bytes(),
            &relations,
            &[ColumnType::Json, ColumnType::Int32],
            &keys,
        )?;
        let search_path = format!("pg_catalog, {}, pg_temp", wamn_pg_core::quote_ident(schema));
        tx.query_one("SELECT set_config('search_path',$1,true)", &[&search_path])
            .await?;
        tx.batch_execute("SET LOCAL lock_timeout = '5s'").await?;
        roles::set_package_owner_role(&tx).await?;
        let statement = tx
            .prepare_typed(
                &batch.sql,
                &[
                    tokio_postgres::types::Type::JSONB,
                    tokio_postgres::types::Type::INT4,
                ],
            )
            .await?;
        let rows = tx
            .query(&statement, &[&recorded.cursor, &batch_size])
            .await
            .with_context(|| {
                format!(
                    "backfill {package_id}@{version} stopped; retained cursor {}",
                    recorded.cursor
                )
            })?;
        roles::reset_host_role(&tx).await?;
        ensure!(
            rows.len() == 1 && rows[0].len() == 2,
            "backfill must return one next_cursor/complete row; retained cursor {}",
            recorded.cursor
        );
        let cursor: serde_json::Value = rows[0].try_get("next_cursor")?;
        let complete: bool = rows[0].try_get("complete")?;
        progress::advance(&tx, &identity, &cursor).await?;
        if complete {
            let outcome =
                apply_prepared(&tx, tenant, package, ApplicationMode::BackfillCompletion).await?;
            if let Some(accepted) = accepted {
                crate::package_upgrade::persist(&tx, &accepted, &artifact).await?;
            }
            progress::complete(&tx, &identity).await?;
            tx.commit()
                .await
                .context("commit completed backfill and installed version")?;
            return Ok(outcome);
        }
        tx.commit()
            .await
            .context("commit backfill data and cursor")?;
    }
}
