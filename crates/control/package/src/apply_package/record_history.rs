use wamn_schema_control::physical_schema::history::function_body;
use wamn_schema_control::physical_schema::{
    RelationColumn, SELECT_RELATION_COLUMNS_SQL, history_table_sql, history_triggers,
};

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_catalog::manifest::PackageManifest;
use wamn_control_provision::AUDIT_RETENTION_ROLE;
use wamn_control_provision::audit_retention::{
    AUDIT_RETENTION_LOCK_SQL, reconcile_audit_retention_grants_sql,
};
use wamn_record_history::history_table_name;

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

/// The source text of each named function in `wamn_history`.
const SELECT_HISTORY_FUNCTIONS_SQL: &str = "\
SELECT routine.proname::text, routine.prosrc \
  FROM pg_catalog.pg_proc AS routine \
  JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace \
 WHERE namespace.nspname = 'wamn_history' AND routine.proname = ANY ($1::text[])";

/// Create the history table of each owned relation whose declaration keeps a log.
///
/// `wamn_history.create_history_table` is the one definition of the table
/// shape. apply-package never drops a history table, so a relation whose
/// retention becomes `none` keeps its table and its entries. The function runs
/// for a present table too, so an older table gains the indexes the shape
/// added since; only a new table counts as a change.
pub(super) async fn create_history_tables(
    tx: &Transaction<'_>,
    manifest: &PackageManifest,
) -> anyhow::Result<bool> {
    // Take the retention lock before CREATE INDEX takes a history-table lock,
    // even when the index already exists. Pruning takes these locks in this order.
    tx.query_one(AUDIT_RETENTION_LOCK_SQL, &[])
        .await
        .context("lock the audit retention source")?;
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
        // The package-owner role owns the relation, so it owns its history table.
        set_package_owner_role(tx, &manifest.package.id).await?;
        tx.batch_execute(&history_table_sql(model))
            .await
            .with_context(|| format!("create history table {}.{history}", model.schema))?;
        reset_host_role(tx).await?;
        changed |= !present;
    }
    Ok(changed)
}

/// Make each owned relation carry exactly the platform triggers that its declaration derives.
///
/// The triggers are derived state, like the operation grants. A declaration
/// that selects no column has no stamp trigger, and a retention of `none` has
/// no log trigger. A trigger that a declaration no longer needs is removed. The
/// log trigger carries the retention as its one argument. One format string
/// gives the text that creates each trigger and the text that PostgreSQL renders for it. The
/// installed triggers of the owned relations are then read as
/// `pg_get_triggerdef` text and compared with the declared text.
///
/// Each trigger runs a function that the step generates for its relation in
/// `wamn_history` (`wamn-c8a5v.2`): `<schema>__<relation>_stamp` and
/// `<schema>__<relation>_log`. The function names the columns, the primary key
/// and the history table as static SQL, so a write reads no catalog and plans
/// no dynamic statement. The step reads the columns and the primary key from
/// the catalog in the transaction that ran the migrations. It replaces a
/// function only when the stored source differs, and it drops the function of a
/// trigger that the declaration no longer needs. Known limitation: a column
/// change that does not run through apply-package leaves the function on the
/// old column list until the next apply. The platform relations keep the shared
/// `wamn_history.stamp_row` and `wamn_history.log_row_change`.
///
/// `create_history_tables` already holds the audit retention lock, so a
/// retention run never sees a retention change between its read and its delete.
/// This step ends with the audit retention grants, which follow the installed log triggers.
pub(super) async fn reconcile_record_history_triggers(
    tx: &Transaction<'_>,
    manifest: &PackageManifest,
) -> anyhow::Result<bool> {
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

    let mut functions = Vec::new();
    let mut statements = Vec::new();
    let mut dropped_functions = Vec::new();
    let mut expected = BTreeSet::new();
    for ((_, model), relation) in owned.into_iter().zip(&relations) {
        let (quoted, definitions) = &installed[relation];
        let columns = read_relation_columns(tx, &model.schema, &model.table).await?;
        let generated = history_triggers(model, &columns)?;
        let names = generated
            .iter()
            .map(|trigger| trigger.function_name.as_str())
            .collect::<Vec<_>>();
        let sources = read_history_functions(tx, &names).await?;
        for trigger in generated {
            let name = trigger.name;
            let function_name = &trigger.function_name;
            let function_ident = &trigger.function_ident;
            let generated = trigger.definition;
            let installed = definitions.get(name);
            let Some((function, tag, definition)) = generated else {
                if installed.is_some() {
                    statements.push(format!("DROP TRIGGER {name} ON {quoted}"));
                }
                if sources.contains_key(function_name) {
                    dropped_functions
                        .push(format!("DROP FUNCTION wamn_history.{function_ident}()"));
                }
                continue;
            };
            if sources.get(function_name).map(String::as_str) != Some(function_body(&function, tag))
            {
                functions.push(function);
            }
            let created = format!("CREATE {definition}");
            if installed != Some(&created) {
                statements.push(format!("CREATE OR REPLACE {definition}"));
            }
            expected.insert(created);
        }
    }
    // A trigger needs its function, and a dropped function needs no trigger.
    for statement in functions
        .iter()
        .chain(&statements)
        .chain(&dropped_functions)
    {
        // The package-owner role owns the relation and the generated
        // functions, so it creates the triggers and the functions.
        set_package_owner_role(tx, &manifest.package.id).await?;
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
    tx.batch_execute(&wamn_control_provision::sql::grant_executor_history_reads_sql())
        .await
        .context("grant event admission its history reads")?;
    Ok(!functions.is_empty()
        || !statements.is_empty()
        || !dropped_functions.is_empty()
        || grants_changed)
}

/// Each column of one relation, read in the transaction that ran its migrations.
pub(super) async fn read_relation_columns(
    tx: &Transaction<'_>,
    schema: &str,
    relation: &str,
) -> anyhow::Result<Vec<RelationColumn>> {
    let rows = tx
        .query(SELECT_RELATION_COLUMNS_SQL, &[&schema, &relation])
        .await
        .with_context(|| format!("read the columns of {schema}.{relation}"))?;
    Ok(rows
        .iter()
        .map(|row| RelationColumn {
            name: row.get(0),
            primary_key: row.get(1),
            timestamptz: row.get(2),
        })
        .collect())
}

/// The stored source of each named `wamn_history` function that exists.
async fn read_history_functions(
    tx: &Transaction<'_>,
    names: &[&str],
) -> anyhow::Result<BTreeMap<String, String>> {
    let rows = tx
        .query(SELECT_HISTORY_FUNCTIONS_SQL, &[&names])
        .await
        .context("read the generated record-history functions")?;
    Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
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
/// The host issues grants across every installed package owner.
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
                        row.get::<_, String>("object_type"),
                        row.get::<_, String>("schema_name"),
                        row.get::<_, String>("object_name"),
                        row.get::<_, String>("privilege_type"),
                    )
                })
                .collect::<Vec<_>>()
        })
    };
    let before = read_grants().await?;
    super::roles::assert_host_role(tx).await?;
    tx.batch_execute(&reconcile_audit_retention_grants_sql())
        .await
        .context("reconcile the audit retention grants")?;
    reset_host_role(tx).await?;
    Ok(read_grants().await? != before)
}
