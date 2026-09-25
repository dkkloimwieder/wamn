use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_catalog::{VERSION_BUMP_TRIGGER, VERSION_NOTE_TRIGGER};
use wamn_control_provision::AUDIT_RETENTION_ROLE;
use wamn_control_provision::audit_retention::{
    AUDIT_RETENTION_LOCK_SQL, reconcile_audit_retention_grants_sql,
};
use wamn_record_history::{LOG_TRIGGER, STAMP_TRIGGER, history_table_name};
use wamn_schema_generator::{PackageManifest, RecordHistoryColumn};

use super::definition_ownership::SELECT_RELATION_PRESENT_SQL;
use super::roles::{reset_host_role, set_package_owner_role};

/// Each non-internal trigger of the named relations, as the server renders it.
///
/// Every named relation yields a row with its quoted name, and a relation with
/// triggers yields one row for each trigger.
const SELECT_RELATION_TRIGGERS_SQL: &str = "\
SELECT owned.schema_name, owned.relation_name, owned.quoted, \
       installed.tgname::text, pg_catalog.pg_get_triggerdef(installed.oid) \
  FROM (SELECT schema_name, relation_name, \
               pg_catalog.format('%I.%I', schema_name, relation_name) AS quoted \
          FROM unnest($1::text[], $2::text[]) AS named (schema_name, relation_name)\
       ) AS owned \
  LEFT JOIN pg_catalog.pg_trigger AS installed \
    ON installed.tgrelid = pg_catalog.to_regclass(owned.quoted) AND NOT installed.tgisinternal";

/// Create the history table of each owned relation whose declaration keeps a log.
///
/// `wamn_history.create_history_table` is the one definition of the table
/// shape. apply-package never drops a history table, so a relation whose
/// retention becomes `none` keeps its table and its entries.
pub(super) async fn create_history_tables(
    tx: &Transaction<'_>,
    manifest: &PackageManifest,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for model in manifest
        .models
        .values()
        .filter(|model| model.owner == manifest.package.id && model.log_retention().is_some())
    {
        let history = history_table_name(&model.table);
        let present = tx
            .query_one(SELECT_RELATION_PRESENT_SQL, &[&model.schema, &history])
            .await
            .with_context(|| format!("read history table {}.{history}", model.schema))?
            .get::<_, bool>(0);
        if present {
            continue;
        }
        // The package-owner role owns the relation, so it owns its history table.
        set_package_owner_role(tx).await?;
        tx.execute(
            "SELECT wamn_history.create_history_table($1, $2, false)",
            &[&model.schema, &model.table],
        )
        .await
        .with_context(|| format!("create history table {}.{history}", model.schema))?;
        reset_host_role(tx).await?;
        changed = true;
    }
    Ok(changed)
}

/// Make each owned relation carry exactly the platform triggers that its declaration derives.
///
/// The triggers are derived state, like the operation grants. A declaration
/// that selects no column has no stamp trigger, and a retention of `none` has
/// no log trigger. A trigger that a declaration no longer needs is removed. The
/// log trigger carries the retention as its one argument. Every owned relation
/// also carries the two model version triggers of
/// `deploy/sql/model-versions.sql`. One format string gives the text that
/// creates each trigger and the text that PostgreSQL renders for it. The
/// installed triggers of the owned relations are then read as
/// `pg_get_triggerdef` text and compared with the declared text.
///
/// The step takes the audit retention lock first, so a retention run never
/// sees a retention change between its read and its delete. It ends with the
/// audit retention grants, which follow the installed log triggers.
pub(super) async fn reconcile_record_history_triggers(
    tx: &Transaction<'_>,
    manifest: &PackageManifest,
) -> anyhow::Result<bool> {
    tx.query_one(AUDIT_RETENTION_LOCK_SQL, &[])
        .await
        .context("lock the audit retention source")?;
    let owned = manifest
        .models
        .iter()
        .filter(|(_, model)| model.owner == manifest.package.id)
        .collect::<Vec<_>>();
    let relations = owned
        .iter()
        .map(|(_, model)| (model.schema.clone(), model.table.clone()))
        .collect::<Vec<_>>();
    let installed = read_relation_triggers(tx, &relations).await?;

    let mut statements = Vec::new();
    let mut expected = BTreeSet::new();
    for ((model_id, model), relation) in owned.into_iter().zip(&relations) {
        let audit_log = model
            .audit_log
            .as_ref()
            .with_context(|| format!("{model_id} owns its relation and must declare audit_log"))?;
        let columns = RecordHistoryColumn::ALL
            .into_iter()
            .filter(|column| audit_log.columns.contains(column))
            .map(|column| format!("'{}'", column.as_str()))
            .collect::<Vec<_>>();
        let (quoted, definitions) = &installed[relation];
        let stamp = (!columns.is_empty()).then(|| {
            format!(
                "TRIGGER {STAMP_TRIGGER} BEFORE INSERT OR UPDATE ON {quoted} FOR EACH ROW \
                 EXECUTE FUNCTION wamn_history.stamp_row({})",
                columns.join(", ")
            )
        });
        let log = model.log_retention().map(|retention| {
            format!(
                "TRIGGER {LOG_TRIGGER} AFTER INSERT OR DELETE OR UPDATE ON {quoted} FOR EACH ROW \
                 EXECUTE FUNCTION wamn_history.log_row_change('{}')",
                retention.replace('\'', "''")
            )
        });
        let note = format!(
            "TRIGGER {VERSION_NOTE_TRIGGER} AFTER INSERT OR DELETE OR UPDATE OR TRUNCATE ON {quoted} \
             FOR EACH STATEMENT EXECUTE FUNCTION wamn_cache.note_change()"
        );
        let bump = format!(
            "CONSTRAINT TRIGGER {VERSION_BUMP_TRIGGER} AFTER INSERT OR DELETE OR UPDATE ON {quoted} \
             DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wamn_cache.bump_changed()"
        );
        for (name, definition) in [
            (STAMP_TRIGGER, stamp),
            (LOG_TRIGGER, log),
            (VERSION_NOTE_TRIGGER, Some(note)),
            (VERSION_BUMP_TRIGGER, Some(bump)),
        ] {
            let installed = definitions.get(name);
            let Some(definition) = definition else {
                if installed.is_some() {
                    statements.push(format!("DROP TRIGGER {name} ON {quoted}"));
                }
                continue;
            };
            let created = format!("CREATE {definition}");
            if installed != Some(&created) {
                // PostgreSQL cannot replace a constraint trigger in place.
                if definition.starts_with("CONSTRAINT") {
                    if installed.is_some() {
                        statements.push(format!("DROP TRIGGER {name} ON {quoted}"));
                    }
                    statements.push(created.clone());
                } else {
                    statements.push(format!("CREATE OR REPLACE {definition}"));
                }
            }
            expected.insert(created);
        }
    }
    for statement in &statements {
        // The package-owner role owns the relation, so it creates the trigger.
        set_package_owner_role(tx).await?;
        tx.batch_execute(statement)
            .await
            .with_context(|| format!("reconcile a record-history trigger: {statement}"))?;
        reset_host_role(tx).await?;
    }

    let observed = read_relation_triggers(tx, &relations)
        .await?
        .into_values()
        .flat_map(|(_, definitions)| definitions.into_values())
        .collect::<BTreeSet<_>>();
    ensure!(
        observed == expected,
        "record-history-trigger-mismatch: the owned relations carry {observed:?}, but the declarations derive {expected:?}"
    );
    let grants_changed = reconcile_audit_retention_grants(tx).await?;
    Ok(!statements.is_empty() || grants_changed)
}

/// Read the non-internal triggers of each relation as `pg_get_triggerdef` renders them.
///
/// The result maps each relation to its name as PostgreSQL quotes it and to
/// its trigger definitions by trigger name.
async fn read_relation_triggers(
    tx: &Transaction<'_>,
    relations: &[(String, String)],
) -> anyhow::Result<BTreeMap<(String, String), (String, BTreeMap<String, String>)>> {
    let (schemas, tables): (Vec<&str>, Vec<&str>) = relations
        .iter()
        .map(|(schema, table)| (schema.as_str(), table.as_str()))
        .unzip();
    let rows = tx
        .query(SELECT_RELATION_TRIGGERS_SQL, &[&schemas, &tables])
        .await
        .context("read the installed record-history triggers")?;
    let mut triggers = BTreeMap::<_, (String, BTreeMap<_, _>)>::new();
    for row in rows {
        let (_, definitions) = triggers
            .entry((row.get(0), row.get(1)))
            .or_insert_with(|| (row.get(2), BTreeMap::new()));
        if let Some(name) = row.get::<_, Option<String>>(3) {
            definitions.insert(name, row.get(4));
        }
    }
    Ok(triggers)
}

/// Grant the audit retention role exactly its privileges on the history tables
/// whose log trigger carries `P<n>D`, and revoke every other privilege.
///
/// The package-owner role owns the history tables, so it issues the grants.
/// The result reports whether the grants of the role changed.
async fn reconcile_audit_retention_grants(tx: &Transaction<'_>) -> anyhow::Result<bool> {
    let read_grants = || async {
        tx.query(
            wamn_control_provision::sql::role_database_grants_sql(),
            &[&AUDIT_RETENTION_ROLE],
        )
        .await
        .context("read the audit retention grants")
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    (
                        row.get::<_, String>("object_kind"),
                        row.get::<_, String>("schema_name"),
                        row.get::<_, String>("object_name"),
                        row.get::<_, String>("privilege_type"),
                    )
                })
                .collect::<Vec<_>>()
        })
    };
    let before = read_grants().await?;
    set_package_owner_role(tx).await?;
    tx.batch_execute(&reconcile_audit_retention_grants_sql())
        .await
        .context("reconcile the audit retention grants")?;
    reset_host_role(tx).await?;
    Ok(read_grants().await? != before)
}
