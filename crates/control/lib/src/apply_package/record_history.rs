use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
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

/// Each column of one relation in attribute order: its name, whether the
/// primary key holds it, and whether its type is `timestamptz`.
const SELECT_RELATION_COLUMNS_SQL: &str = "\
SELECT a.attname::text, \
       COALESCE(a.attnum = ANY (i.indkey), false), \
       a.atttypid = 'pg_catalog.timestamptz'::pg_catalog.regtype \
  FROM pg_catalog.pg_attribute AS a \
  LEFT JOIN pg_catalog.pg_index AS i ON i.indrelid = a.attrelid AND i.indisprimary \
 WHERE a.attrelid = pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1::text, $2::text)) \
   AND a.attnum > 0 AND NOT a.attisdropped \
 ORDER BY a.attnum";

/// The source text of each named function in `wamn_history`.
const SELECT_HISTORY_FUNCTIONS_SQL: &str = "\
SELECT routine.proname::text, routine.prosrc \
  FROM pg_catalog.pg_proc AS routine \
  JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace \
 WHERE namespace.nspname = 'wamn_history' AND routine.proname = ANY ($1::text[])";

/// The name of a generated function and its identifier as PostgreSQL renders it.
const QUOTE_IDENT_SQL: &str = "SELECT pg_catalog.format('%I', $1::text)";

/// PostgreSQL truncates an object name of 64 bytes or more.
const NAME_BYTE_LIMIT: usize = 64;

/// One column of a logged or stamped relation, read at apply time.
struct RelationColumn {
    name: String,
    primary_key: bool,
    timestamptz: bool,
}

/// The `CREATE OR REPLACE FUNCTION` text of the stamp function of one relation.
///
/// `stamped` lists the selected stamp columns, and `others` every other column.
/// An insert takes fresh stamps. A changed row keeps the created pair and moves
/// the updated pair. A true no-op keeps every old stamp. The change test casts
/// each other column to text, so a change of numeric scale alone is a change,
/// and a type without `=` compares too.
fn stamp_function_sql(name: &str, stamped: &[&str], others: &[&str]) -> String {
    let assign = |source: fn(&str) -> String| {
        stamped.iter().fold(String::new(), |mut text, column| {
            writeln!(
                text,
                "        NEW.{} := {};",
                wamn_pg_core::quote_ident(column),
                source(column)
            )
            .expect("writing to a String cannot fail");
            text
        })
    };
    let fresh = |column: &str| match column {
        "created_by" | "updated_by" => "actor".to_owned(),
        _ => "transaction_timestamp()".to_owned(),
    };
    let moved = |column: &str| match column {
        "updated_at" => "transaction_timestamp()".to_owned(),
        "updated_by" => "actor".to_owned(),
        _ => format!("OLD.{}", wamn_pg_core::quote_ident(column)),
    };
    let kept = |column: &str| format!("OLD.{}", wamn_pg_core::quote_ident(column));
    let insert = assign(fresh);
    let changed_assign = assign(moved);
    let unchanged = assign(kept);
    let changed = changed_test(others);
    format!(
        "CREATE OR REPLACE FUNCTION wamn_history.{name}()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $stamp$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF TG_OP = 'INSERT' THEN
{insert}    ELSIF {changed} THEN
{changed_assign}    ELSE
{unchanged}    END IF;
    RETURN NEW;
END
$stamp$;
REVOKE ALL ON FUNCTION wamn_history.{name}() FROM PUBLIC;"
    )
}

/// The `CREATE OR REPLACE FUNCTION` text of the log function of one relation.
///
/// The entry keys the row by its primary key columns. A relation with no
/// primary key raises `history-key-required`. An update under the same key
/// records the old and the new value of each changed column. An insert, a
/// delete, and a primary key change record the full row. The images keep the
/// `to_jsonb` spelling, and the readers canonicalize it. A key column of type
/// `timestamptz` keeps the canonical spelling, so every entry of a row carries
/// one key text.
fn log_function_sql(name: &str, history: &str, columns: &[RelationColumn]) -> String {
    let all = columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<Vec<_>>();
    let keys = columns
        .iter()
        .filter(|column| column.primary_key)
        .collect::<Vec<_>>();
    let no_key = if keys.is_empty() {
        "    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'history-key-required';\n"
    } else {
        ""
    };
    let key = |row: &str| {
        let members = keys
            .iter()
            .map(|column| {
                let value = format!("{row}.{}", wamn_pg_core::quote_ident(&column.name));
                let value = if column.timestamptz {
                    format!("wamn_history.timestamptz_image({value})")
                } else {
                    value
                };
                format!("{}, {value}", wamn_pg_core::quote_literal(&column.name))
            })
            .collect::<Vec<_>>();
        format!("jsonb_build_object({})", members.join(", "))
    };
    let same_key = if keys.is_empty() {
        "true".to_owned()
    } else {
        keys.iter()
            .map(|column| {
                let column = wamn_pg_core::quote_ident(&column.name);
                format!("to_jsonb(OLD.{column}) = to_jsonb(NEW.{column})")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let diff = all.iter().fold(String::new(), |mut text, column| {
        let name = wamn_pg_core::quote_literal(column);
        let column = wamn_pg_core::quote_ident(column);
        writeln!(
            text,
            "            IF NEW.{column}::text IS DISTINCT FROM OLD.{column}::text THEN
                changed_before := changed_before || jsonb_build_object({name}, OLD.{column});
                changed_after := changed_after || jsonb_build_object({name}, NEW.{column});
            END IF;"
        )
        .expect("writing to a String cannot fail");
        text
    });
    let changed = changed_test(&all);
    let (old_key, new_key) = (key("OLD"), key("NEW"));
    let insert = format!(
        "INSERT INTO {history} (row_key, type, operation, changed_by, changed_at, \
         transaction_id, before, after)"
    );
    format!(
        "CREATE OR REPLACE FUNCTION wamn_history.{name}()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $log$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
    operation text := NULLIF(current_setting('app.operation', true), '');
    changed_before jsonb := '{{}}';
    changed_after jsonb := '{{}}';
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF operation IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'operation-required';
    END IF;
{no_key}    IF TG_OP = 'UPDATE' THEN
        -- A true no-op writes no entry.
        IF NOT {changed} THEN
            RETURN NULL;
        END IF;
        IF {same_key} THEN
{diff}            {insert}
            VALUES ({new_key}, 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        {insert}
        VALUES ({old_key}, 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{{}}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        {insert}
        VALUES ({new_key}, 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{{}}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;
REVOKE ALL ON FUNCTION wamn_history.{name}() FROM PUBLIC;"
    )
}

/// Whether any of `columns` differs between `OLD` and `NEW`, as SQL text.
fn changed_test(columns: &[&str]) -> String {
    if columns.is_empty() {
        return "false".to_owned();
    }
    let tests = columns
        .iter()
        .map(|column| {
            let column = wamn_pg_core::quote_ident(column);
            format!("NEW.{column}::text IS DISTINCT FROM OLD.{column}::text")
        })
        .collect::<Vec<_>>();
    format!("({})", tests.join("\n            OR "))
}

/// The body that PostgreSQL stores for a generated function: the text between
/// its two dollar-quote tags.
fn function_body<'a>(statement: &'a str, tag: &str) -> &'a str {
    let start = statement.find(tag).map_or(0, |start| start + tag.len());
    let end = statement[start..]
        .find(tag)
        .map_or(statement.len(), |end| start + end);
    &statement[start..end]
}

/// The name of the generated function of one relation, refused at 64 bytes or more.
fn generated_function_name(schema: &str, relation: &str, suffix: &str) -> anyhow::Result<String> {
    let name = format!("{schema}__{relation}_{suffix}");
    ensure!(
        name.len() < NAME_BYTE_LIMIT,
        "record-history-function-name-too-long: wamn_history.{name} has {} bytes, and PostgreSQL \
         truncates a name of {NAME_BYTE_LIMIT} bytes or more",
        name.len()
    );
    Ok(name)
}

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
        set_package_owner_role(tx).await?;
        tx.execute(
            "SELECT wamn_history.create_history_table($1, $2, false)",
            &[&model.schema, &model.table],
        )
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
/// The step takes the audit retention lock first, so a retention run never
/// sees a retention change between its read and its delete. It ends with the
/// audit retention grants, which follow the installed log triggers.
pub(super) async fn reconcile_record_history_triggers(
    tx: &Transaction<'_>,
    manifest: &PackageManifest,
    synchronizations: &[super::synchronization::PlannedSynchronization],
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

    let mut functions = Vec::new();
    let mut statements = Vec::new();
    let mut dropped_functions = Vec::new();
    let mut expected = BTreeSet::new();
    for ((model_id, model), relation) in owned.into_iter().zip(&relations) {
        let audit_log = model
            .audit_log
            .as_ref()
            .with_context(|| format!("{model_id} owns its relation and must declare audit_log"))?;
        let stamped = RecordHistoryColumn::ALL
            .into_iter()
            .filter(|column| audit_log.columns.contains(column))
            .map(RecordHistoryColumn::as_str)
            .collect::<Vec<_>>();
        let retention = model.log_retention();
        let (quoted, definitions) = &installed[relation];
        let stamp_name = generated_function_name(&model.schema, &model.table, "stamp")?;
        let log_name = generated_function_name(&model.schema, &model.table, "log")?;
        let (stamp_ident, log_ident) = (
            quote_function_name(tx, &stamp_name).await?,
            quote_function_name(tx, &log_name).await?,
        );
        let columns = if stamped.is_empty() && retention.is_none() {
            Vec::new()
        } else {
            read_relation_columns(tx, &model.schema, &model.table).await?
        };
        let stamp = (!stamped.is_empty()).then(|| {
            let others = columns
                .iter()
                .map(|column| column.name.as_str())
                .filter(|column| !stamped.contains(column))
                .collect::<Vec<_>>();
            (
                stamp_function_sql(&stamp_ident, &stamped, &others),
                "$stamp$",
                format!(
                    "TRIGGER {STAMP_TRIGGER} BEFORE INSERT OR UPDATE ON {quoted} FOR EACH ROW \
                     EXECUTE FUNCTION wamn_history.{stamp_ident}()"
                ),
            )
        });
        let log = retention.map(|retention| {
            let history = format!(
                "{}.{}",
                wamn_pg_core::quote_ident(&model.schema),
                wamn_pg_core::quote_ident(&history_table_name(&model.table))
            );
            (
                log_function_sql(&log_ident, &history, &columns),
                "$log$",
                // The function ignores its argument. The retention run reads it.
                format!(
                    "TRIGGER {LOG_TRIGGER} AFTER INSERT OR DELETE OR UPDATE ON {quoted} FOR EACH \
                     ROW EXECUTE FUNCTION wamn_history.{log_ident}('{}')",
                    retention.replace('\'', "''")
                ),
            )
        });
        let sources = read_history_functions(tx, &[stamp_name.as_str(), log_name.as_str()]).await?;
        for (name, function_name, function_ident, generated) in [
            (STAMP_TRIGGER, &stamp_name, &stamp_ident, stamp),
            (LOG_TRIGGER, &log_name, &log_ident, log),
        ] {
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
    // Each pair already passed typed ownership and exact catalog inspection in
    // this transaction. Include only those exact identities in the complete set.
    for synchronization in synchronizations {
        let declaration = &synchronization.declaration;
        let (_, definitions) = installed
            .get(&(declaration.schema.clone(), declaration.relation.clone()))
            .context("verified synchronization relation is absent from the owned model set")?;
        let definition = definitions
            .get(&declaration.trigger)
            .context("verified synchronization trigger is missing")?;
        expected.insert(definition.clone());
    }
    // A trigger needs its function, and a dropped function needs no trigger.
    for statement in functions
        .iter()
        .chain(&statements)
        .chain(&dropped_functions)
    {
        // The package-owner role owns the relation and the generated
        // functions, so it creates the triggers and the functions.
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
    tx.batch_execute(&wamn_control_provision::sql::grant_executor_history_reads_sql())
        .await
        .context("grant event admission its history reads")?;
    Ok(!functions.is_empty()
        || !statements.is_empty()
        || !dropped_functions.is_empty()
        || grants_changed)
}

/// Each column of one relation, read in the transaction that ran its migrations.
async fn read_relation_columns(
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

/// The function name as `pg_get_triggerdef` renders it, quoted only when needed.
async fn quote_function_name(tx: &Transaction<'_>, name: &str) -> anyhow::Result<String> {
    Ok(tx
        .query_one(QUOTE_IDENT_SQL, &[&name])
        .await
        .with_context(|| format!("quote the function name {name}"))?
        .get(0))
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
    set_package_owner_role(tx).await?;
    tx.batch_execute(&reconcile_audit_retention_grants_sql())
        .await
        .context("reconcile the audit retention grants")?;
    reset_host_role(tx).await?;
    Ok(read_grants().await? != before)
}

#[cfg(test)]
mod tests {
    use super::{
        RelationColumn, changed_test, function_body, generated_function_name, log_function_sql,
        stamp_function_sql,
    };

    fn column(name: &str, primary_key: bool, timestamptz: bool) -> RelationColumn {
        RelationColumn {
            name: name.to_owned(),
            primary_key,
            timestamptz,
        }
    }

    #[test]
    fn a_generated_name_of_64_bytes_is_refused() {
        let relation = "r".repeat(63 - "s__".len() - "_stamp".len());
        assert!(generated_function_name("s", &relation, "stamp").is_ok());
        let error = generated_function_name("s", &format!("{relation}r"), "stamp")
            .expect_err("a 64-byte name is refused");
        assert!(
            error
                .to_string()
                .starts_with("record-history-function-name-too-long")
        );
    }

    #[test]
    fn the_stored_body_is_the_text_between_the_tags() {
        let statement = stamp_function_sql("s__r_stamp", &["updated_at"], &["id"]);
        let body = function_body(&statement, "$stamp$");
        assert!(body.starts_with("\nDECLARE\n"));
        assert!(body.ends_with("\nEND\n"));
        assert!(
            statement.ends_with("REVOKE ALL ON FUNCTION wamn_history.s__r_stamp() FROM PUBLIC;")
        );
    }

    #[test]
    fn the_change_test_compares_each_column_as_text() {
        assert_eq!(changed_test(&[]), "false");
        assert_eq!(
            changed_test(&["a", "b\"c"]),
            "(NEW.\"a\"::text IS DISTINCT FROM OLD.\"a\"::text\n            \
             OR NEW.\"b\"\"c\"::text IS DISTINCT FROM OLD.\"b\"\"c\"::text)"
        );
    }

    #[test]
    fn the_stamp_function_assigns_only_the_selected_columns() {
        let statement = stamp_function_sql("s__r_stamp", &["created_at", "updated_by"], &["id"]);
        let body = function_body(&statement, "$stamp$");
        assert!(body.contains(
            "    IF TG_OP = 'INSERT' THEN\n        NEW.\"created_at\" := transaction_timestamp();\n        NEW.\"updated_by\" := actor;\n"
        ));
        assert!(body.contains(
            "    ELSIF (NEW.\"id\"::text IS DISTINCT FROM OLD.\"id\"::text) THEN\n        NEW.\"created_at\" := OLD.\"created_at\";\n        NEW.\"updated_by\" := actor;\n"
        ));
        assert!(body.contains(
            "    ELSE\n        NEW.\"created_at\" := OLD.\"created_at\";\n        NEW.\"updated_by\" := OLD.\"updated_by\";\n    END IF;"
        ));
        assert!(!body.contains("jsonb"));
        assert!(!body.contains("updated_at"));
    }

    #[test]
    fn the_log_function_keys_by_the_primary_key_and_names_its_history_table() {
        let columns = [
            column("id", true, false),
            column("at", true, true),
            column("note", false, false),
        ];
        let statement = log_function_sql("s__r_log", "\"s\".\"r_history\"", &columns);
        let body = function_body(&statement, "$log$");
        assert!(body.contains(
            "jsonb_build_object('id', NEW.\"id\", 'at', wamn_history.timestamptz_image(NEW.\"at\"))"
        ));
        assert!(body.contains(
            "IF to_jsonb(OLD.\"id\") = to_jsonb(NEW.\"id\") AND to_jsonb(OLD.\"at\") = to_jsonb(NEW.\"at\") THEN"
        ));
        assert!(body.contains("INSERT INTO \"s\".\"r_history\" (row_key, type"));
        assert!(!body.contains("history-key-required"));
        assert!(!body.contains("EXECUTE"));
        assert!(!body.contains("pg_attribute"));
    }

    #[test]
    fn a_log_function_without_a_primary_key_raises_history_key_required() {
        let statement = log_function_sql(
            "s__r_log",
            "\"s\".\"r_history\"",
            &[column("note", false, false)],
        );
        let body = function_body(&statement, "$log$");
        assert!(body.contains("MESSAGE = 'history-key-required';\n    IF TG_OP = 'UPDATE' THEN"));
        assert!(body.contains("        IF true THEN\n"));
    }
}
