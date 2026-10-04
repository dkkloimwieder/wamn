//! Install only the closed, paired declaration of an owned synchronization trigger.

use std::collections::BTreeSet;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_schema_control::SqlStatement;
use wamn_schema_generator::{
    PackageManifest, RecordHistoryColumn, TombstoneColumn, UpgradeStagePhase,
};
use wamn_schema_introspection::migration_policy::{
    DefinitionAction, DefinitionType, StageRelation, StageSynchronization,
    inspect_stage_synchronization, validate_stage_trigger_body,
};
use wamn_schema_introspection::postgres::{
    StageSynchronizationExpectation, verify_stage_synchronizations,
};

use super::definition_ownership::{PlannedDefinitionMutation, model_for_relation};
use super::{roles, stage};

#[derive(Debug)]
pub(super) struct PlannedSynchronization {
    pub relative_path: String,
    pub declaration: StageSynchronization,
}

pub(super) fn require_pending_phase(
    manifest: &PackageManifest,
    declarations: &[PlannedSynchronization],
    pending_paths: &BTreeSet<&str>,
) -> anyhow::Result<()> {
    for declaration in declarations {
        if pending_paths.contains(declaration.relative_path.as_str()) {
            ensure!(
                manifest
                    .upgrade_stage
                    .as_ref()
                    .is_some_and(|stage| stage.phase == UpgradeStagePhase::Expand),
                "migration {} introduces synchronization {}.{} outside an expand stage",
                declaration.relative_path,
                declaration.declaration.schema,
                declaration.declaration.trigger
            );
        }
    }
    Ok(())
}

pub(super) async fn install(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    manifest: &PackageManifest,
    planned: &PlannedSynchronization,
    statement: &SqlStatement,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    let declaration = &planned.declaration;
    tx.batch_execute("SET LOCAL lock_timeout = '5s'").await?;
    // The relation lock freezes the column facts used to admit the trigger body.
    tx.batch_execute(&format!(
        "LOCK TABLE {}.{} IN SHARE ROW EXCLUSIVE MODE",
        wamn_pg_core::quote_ident(&declaration.schema),
        wamn_pg_core::quote_ident(&declaration.relation),
    ))
    .await
    .with_context(|| {
        format!(
            "lock synchronization relation for {}; stop and resume",
            planned.relative_path
        )
    })?;
    let relation = owned_relation(tx, tenant, package, declaration, mutations).await?;
    let columns = writable_columns(tx, tenant, package, manifest, declaration, mutations).await?;
    let inspected = inspect_stage_synchronization(
        &planned.relative_path,
        statement.sql.as_bytes(),
        &relation,
        &columns,
    )?;
    ensure!(
        inspected == *declaration,
        "synchronization declaration changed before execution"
    );
    require_absent(tx, tenant, declaration).await?;
    let previous: String = tx.query_one("SHOW search_path", &[]).await?.try_get(0)?;
    let search_path = format!(
        "pg_catalog, {}, pg_temp",
        wamn_pg_core::quote_ident(&declaration.schema)
    );
    tx.batch_execute("SET LOCAL standard_conforming_strings = on; SET LOCAL lock_timeout = '5s'")
        .await?;
    tx.query_one("SELECT set_config('search_path',$1,true)", &[&search_path])
        .await?;
    roles::set_package_owner_role(tx).await?;
    tx.batch_execute(&statement.sql).await.with_context(|| {
        format!(
            "install synchronization {}; stop and resume after correcting the cause",
            planned.relative_path
        )
    })?;
    roles::reset_host_role(tx).await?;
    let expected = StageSynchronizationExpectation {
        definition: inspected,
        writable_columns: columns,
        owner_role: "wamn_db_owner".to_owned(),
    };
    verify_stage_synchronizations(tx, &[expected]).await?;
    for (section, name) in [
        ("synchronization_function", &declaration.function),
        ("synchronization_trigger", &declaration.trigger),
    ] {
        tx.execute(
            "INSERT INTO catalog.package_definition_owners \
             (tenant_id,schema_name,relation_name,definition_type,definition_name,owner_package_id) \
             VALUES ($1,$2,$3,$4,$5,$6)",
            &[&tenant, &declaration.schema, &declaration.relation, &section, name, &package],
        ).await.context("record immutable synchronization definition ownership")?;
    }
    tx.query_one("SELECT set_config('search_path',$1,true)", &[&previous])
        .await?;
    Ok(())
}

/// Recheck every inherited synchronization before package or batch SQL can fire it.
pub(super) async fn verify_inherited(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    manifest: &PackageManifest,
    declarations: &[PlannedSynchronization],
    pending_paths: &BTreeSet<&str>,
) -> anyhow::Result<()> {
    if declarations.is_empty() && manifest.upgrade_stage.is_none() {
        return Ok(());
    }
    require_known_triggers(tx, tenant, package, declarations, pending_paths).await?;
    let mut expected = Vec::new();
    for planned in declarations {
        if pending_paths.contains(planned.relative_path.as_str()) {
            continue;
        }
        let declaration = &planned.declaration;
        for (section, name) in [
            ("synchronization_function", &declaration.function),
            ("synchronization_trigger", &declaration.trigger),
        ] {
            let owned: bool = tx
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM catalog.package_definition_owners \
                 WHERE tenant_id=$1 AND schema_name=$2 AND relation_name=$3 \
                   AND definition_type=$4 AND definition_name=$5 AND owner_package_id=$6)",
                    &[
                        &tenant,
                        &declaration.schema,
                        &declaration.relation,
                        &section,
                        name,
                        &package,
                    ],
                )
                .await?
                .try_get(0)?;
            ensure!(
                owned,
                "inherited {section} {}.{name} has no exact ownership record for package {package}",
                declaration.schema
            );
        }
        let relation = owned_relation(tx, tenant, package, declaration, &[]).await?;
        let columns = writable_columns(tx, tenant, package, manifest, declaration, &[]).await?;
        validate_stage_trigger_body(
            &planned.relative_path,
            &declaration.body,
            &relation,
            &columns,
        )?;
        expected.push(StageSynchronizationExpectation {
            definition: declaration.clone(),
            writable_columns: columns,
            owner_role: "wamn_db_owner".to_owned(),
        });
    }
    verify_stage_synchronizations(tx, &expected).await?;
    Ok(())
}

/// Freeze trigger sets while preserving concurrent serving writes.
async fn require_known_triggers(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    declarations: &[PlannedSynchronization],
    pending_paths: &BTreeSet<&str>,
) -> anyhow::Result<()> {
    let owned = tx
        .query(
            "SELECT n.nspname,c.relname FROM catalog.package_definition_owners o \
         JOIN pg_catalog.pg_namespace n ON n.nspname=o.schema_name \
         JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname=o.relation_name \
         WHERE o.tenant_id=$1 AND o.owner_package_id=$2 AND o.definition_type='relation' \
           AND c.relkind='r' ORDER BY n.nspname,c.relname",
            &[&tenant, &package],
        )
        .await
        .context("read owned relations before synchronization verification")?;
    // Lock trigger-free tables too, so a trigger cannot appear after an empty query.
    for row in &owned {
        let schema: &str = row.get(0);
        let relation: &str = row.get(1);
        tx.batch_execute("SET LOCAL lock_timeout = '5s'").await?;
        tx.batch_execute(&format!(
            "LOCK TABLE {}.{} IN ROW EXCLUSIVE MODE",
            wamn_pg_core::quote_ident(schema),
            wamn_pg_core::quote_ident(relation),
        ))
        .await
        .with_context(|| {
            format!("lock owned relation {schema}.{relation} before stage SQL; stop and resume")
        })?;
    }
    for row in &owned {
        let schema: &str = row.get(0);
        let relation: &str = row.get(1);
        let triggers = tx
            .query(
                "SELECT t.tgname FROM pg_catalog.pg_trigger t \
             JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             WHERE n.nspname=$1 AND c.relname=$2 AND NOT t.tgisinternal",
                &[&schema, &relation],
            )
            .await
            .context("read locked package trigger set")?;
        for trigger in triggers {
            let name: &str = trigger.get(0);
            let platform = [
                wamn_record_history::STAMP_TRIGGER,
                wamn_record_history::LOG_TRIGGER,
                wamn_catalog::VERSION_NOTE_TRIGGER,
                wamn_catalog::VERSION_BUMP_TRIGGER,
            ]
            .contains(&name);
            let inherited = declarations.iter().any(|planned| {
                !pending_paths.contains(planned.relative_path.as_str())
                    && planned.declaration.schema == schema
                    && planned.declaration.relation == relation
                    && planned.declaration.trigger == name
            });
            ensure!(
                platform || inherited,
                "unlisted trigger {schema}.{relation}.{name} is refused before package SQL"
            );
        }
    }
    Ok(())
}

async fn owned_relation(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    declaration: &StageSynchronization,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<StageRelation> {
    let mut relation = stage::owned_relations(tx, tenant, package, &declaration.schema)
        .await?
        .into_iter()
        .find(|relation| relation.name == declaration.relation)
        .with_context(|| {
            format!(
                "synchronization relation {}.{} is not owned by package {package}",
                declaration.schema, declaration.relation
            )
        })?;
    let mut owned_fields = tx
        .query(
            "SELECT definition_name FROM catalog.package_definition_owners \
         WHERE tenant_id=$1 AND schema_name=$2 AND relation_name=$3 \
           AND definition_type='field' AND owner_package_id=$4",
            &[
                &tenant,
                &declaration.schema,
                &declaration.relation,
                &package,
            ],
        )
        .await?
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    for planned in mutations {
        let mutation = &planned.mutation;
        if mutation.action() == DefinitionAction::Add
            && mutation.definition_type() == DefinitionType::Field
            && mutation.schema() == declaration.schema
            && mutation.relation() == declaration.relation
        {
            owned_fields.insert(mutation.definition().to_owned());
        }
    }
    relation
        .columns
        .retain(|name, _| owned_fields.contains(name));
    Ok(relation)
}

async fn require_absent(
    tx: &Transaction<'_>,
    tenant: &str,
    declaration: &StageSynchronization,
) -> anyhow::Result<()> {
    let exists: bool = tx.query_one(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p \
           JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
           WHERE n.nspname=$2 AND p.proname=$4) \
         OR EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t \
           JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid \
           JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
           WHERE n.nspname=$2 AND c.relname=$3 AND t.tgname=$5) \
         OR EXISTS (SELECT 1 FROM catalog.package_definition_owners \
           WHERE tenant_id=$1 AND schema_name=$2 \
             AND ((definition_type='synchronization_function' AND definition_name=$4) \
               OR (definition_type='synchronization_trigger' AND relation_name=$3 AND definition_name=$5)))",
        &[&tenant, &declaration.schema, &declaration.relation, &declaration.function, &declaration.trigger],
    ).await?.try_get(0)?;
    ensure!(
        !exists,
        "synchronization {}.{} / {} already exists or has immutable ownership history",
        declaration.schema,
        declaration.function,
        declaration.trigger
    );
    Ok(())
}

async fn writable_columns(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    manifest: &PackageManifest,
    declaration: &StageSynchronization,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<Vec<String>> {
    let added = mutations
        .iter()
        .filter_map(|planned| {
            let mutation = &planned.mutation;
            (mutation.action() == DefinitionAction::Add
                && mutation.definition_type() == DefinitionType::Field
                && mutation.schema() == declaration.schema
                && mutation.relation() == declaration.relation)
                .then(|| mutation.definition().to_owned())
        })
        .collect::<Vec<_>>();
    let mut excluded = ["id", "tenant_id", "row_version"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    excluded.extend(
        RecordHistoryColumn::ALL
            .into_iter()
            .map(|column| column.as_str().to_owned()),
    );
    excluded.extend(
        TombstoneColumn::ALL
            .into_iter()
            .map(|column| column.as_str().to_owned()),
    );
    if let Some(model) = model_for_relation(manifest, &declaration.schema, &declaration.relation) {
        excluded.extend(model.server_owned_fields.iter().cloned());
    }
    let rows = tx.query(
        "SELECT a.attname FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid=a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
         WHERE n.nspname=$2 AND c.relname=$3 AND c.relkind='r' \
           AND a.attnum>0 AND NOT a.attisdropped AND a.attgenerated='' AND a.attidentity='' \
           AND NOT a.attname=ANY($6::text[]) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index i WHERE i.indrelid=c.oid AND i.indisprimary AND a.attnum=ANY(i.indkey)) \
           AND (a.attname=ANY($5::text[]) OR EXISTS (SELECT 1 FROM catalog.package_definition_owners o \
             WHERE o.tenant_id=$1 AND o.schema_name=n.nspname AND o.relation_name=c.relname \
               AND o.definition_type='field' AND o.definition_name=a.attname AND o.owner_package_id=$4)) \
         ORDER BY a.attnum",
        &[&tenant, &declaration.schema, &declaration.relation, &package, &added, &excluded],
    ).await.context("read synchronization writable package fields")?;
    rows.iter()
        .map(|row| row.try_get(0).map_err(Into::into))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{PlannedSynchronization, require_pending_phase};
    use std::collections::BTreeSet;
    use wamn_schema_generator::{PackageManifest, UpgradeStagePhase};
    use wamn_schema_introspection::migration_policy::inspect_stage_synchronization_declaration;

    #[test]
    fn only_expand_can_introduce_a_synchronization_pair() {
        let mut manifest = PackageManifest::from_slice(br#"{
            "package":{"id":"fixture","version":"2.0.0","predecessor_version":"1.0.0"},
            "models":{},"components":{},"connections":["main"],
            "required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
            "upgrade_stage":{"phase":"expand","preconditions":{"ready":"SELECT true"},"postconditions":{"done":"SELECT true"}}
        }"#).unwrap();
        let path = "migrations/0003_synchronize.sql";
        let declaration = inspect_stage_synchronization_declaration(path, b"CREATE FUNCTION inventory.sync_note() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER AS $$ BEGIN NEW.new_note := NEW.note; RETURN NEW; END $$; CREATE TRIGGER sync_note BEFORE INSERT OR UPDATE ON inventory.widget FOR EACH ROW EXECUTE FUNCTION inventory.sync_note();").unwrap();
        let declarations = [PlannedSynchronization {
            relative_path: path.to_owned(),
            declaration,
        }];
        let pending = BTreeSet::from([path]);
        require_pending_phase(&manifest, &declarations, &pending).unwrap();
        manifest.upgrade_stage.as_mut().unwrap().phase = UpgradeStagePhase::Backfill;
        assert!(
            require_pending_phase(&manifest, &declarations, &pending)
                .unwrap_err()
                .to_string()
                .contains(path)
        );
        require_pending_phase(&manifest, &declarations, &BTreeSet::new()).unwrap();
        manifest.upgrade_stage = None;
        assert!(require_pending_phase(&manifest, &declarations, &pending).is_err());
        require_pending_phase(&manifest, &declarations, &BTreeSet::new()).unwrap();
    }
}
