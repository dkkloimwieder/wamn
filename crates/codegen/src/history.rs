//! Compiler-owned record history SQL.

mod sql;

use crate::build_database;
use crate::sql_quote::{escape_identifier, escape_literal};
use anyhow::{Context as _, ensure};
use sql::{generated_function_name, log_function_sql, stamp_function_sql};
use std::collections::BTreeMap;
use wamn_component_policy::PackageManifest;
use wamn_package::manifest::source_name::SourceName;
use wamn_package::manifest::{ModelDeclaration, RecordHistoryColumn};
use wamn_record_history::{LOG_TRIGGER, STAMP_TRIGGER, history_table_name};

const NAME_BYTE_LIMIT: usize = 64;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationColumn {
    pub name: String,
    pub primary_key: bool,
    pub timestamptz: bool,
}

/// PostgreSQL quotes identifiers unless they are safe unreserved lower-case names.
fn quoted_name(name: &str) -> String {
    use pg_query::protobuf::KeywordKind;
    let safe = name.bytes().enumerate().all(|(i, b)| {
        b.is_ascii_lowercase() || b == b'_' || (i > 0 && (b.is_ascii_digit() || b == b'$'))
    });
    if !name.is_empty()
        && safe
        && pg_query::scan(name).is_ok_and(|scan| {
            scan.tokens.len() == 1
                && matches!(
                    scan.tokens[0].keyword_kind(),
                    KeywordKind::NoKeyword | KeywordKind::UnreservedKeyword
                )
        })
    {
        name.to_owned()
    } else {
        escape_identifier(name)
    }
}

/// One generated function and its trigger definition.
#[derive(Debug)]
struct HistoryTrigger {
    pub name: &'static str,
    pub function_ident: String,
    pub definition: Option<(String, String)>,
}

/// Render stamp and log functions and triggers for one owned relation.
fn history_triggers(
    model: &ModelDeclaration,
    columns: &[RelationColumn],
) -> anyhow::Result<[HistoryTrigger; 2]> {
    let audit_log = model.audit_log.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "{}.{} owns its relation and must declare audit_log",
            model.schema,
            model.table
        )
    })?;
    let stamped = RecordHistoryColumn::ALL
        .into_iter()
        .filter(|column| audit_log.columns.contains(column))
        .map(RecordHistoryColumn::as_str)
        .collect::<Vec<_>>();
    let quoted = format!(
        "{}.{}",
        quoted_name(model.schema.as_str()),
        quoted_name(model.table.as_str())
    );
    let stamp_name = generated_function_name(model.schema.as_str(), model.table.as_str(), "stamp")?;
    let log_name = generated_function_name(model.schema.as_str(), model.table.as_str(), "log")?;
    let stamp_ident = quoted_name(&stamp_name);
    let log_ident = quoted_name(&log_name);
    let stamp = (!stamped.is_empty()).then(|| {
        let others = columns.iter().map(|column| column.name.as_str())
            .filter(|column| !stamped.contains(column)).collect::<Vec<_>>();
        (stamp_function_sql(&stamp_ident, &stamped, &others), format!(
            "TRIGGER {STAMP_TRIGGER} BEFORE INSERT OR UPDATE ON {quoted} FOR EACH ROW EXECUTE FUNCTION wamn_history.{stamp_ident}()"))
    });
    let log = model.log_retention().map(|retention| {
        let history = format!("{}.{}", escape_identifier(model.schema.as_str()),
            escape_identifier(&history_table_name(model.table.as_str())));
        (log_function_sql(&log_ident, &history, columns), format!(
            "TRIGGER {LOG_TRIGGER} AFTER INSERT OR DELETE OR UPDATE ON {quoted} FOR EACH ROW EXECUTE FUNCTION wamn_history.{log_ident}({})",
            escape_literal(retention)))
    });
    Ok([
        HistoryTrigger {
            name: STAMP_TRIGGER,
            function_ident: stamp_ident,
            definition: stamp,
        },
        HistoryTrigger {
            name: LOG_TRIGGER,
            function_ident: log_ident,
            definition: log,
        },
    ])
}

/// The existing SQL helper remains the sole history-table definition.
pub(crate) fn history_table_sql(model: &ModelDeclaration) -> String {
    format!(
        "SELECT wamn_history.create_history_table({}, {}, false)",
        escape_literal(model.schema.as_str()),
        escape_literal(model.table.as_str())
    )
}

/// Render a full history change. Disabled triggers are removed by this migration.
pub(crate) async fn render(
    root: &std::path::Path,
    manifest: &PackageManifest,
    columns: &BTreeMap<SourceName, Vec<RelationColumn>>,
) -> anyhow::Result<String> {
    let mut statements = retired_triggers(root, manifest).await?;
    for (id, model) in manifest
        .models
        .iter()
        .filter(|(_, model)| model.owner == manifest.package.id)
    {
        if model.log_retention().is_some() {
            statements.push(history_table_sql(model));
        }
        let columns = columns
            .get(id)
            .with_context(|| format!("history columns missing for {id}"))?;
        for trigger in history_triggers(model, columns)? {
            statements.push(format!(
                "DROP TRIGGER IF EXISTS {} ON {}.{}",
                trigger.name,
                escape_identifier(model.schema.as_str()),
                escape_identifier(model.table.as_str())
            ));
            if let Some((function, definition)) = trigger.definition {
                statements.push(function);
                statements.push(format!("CREATE {definition}"));
            } else {
                statements.push(format!(
                    "DROP FUNCTION IF EXISTS wamn_history.{}()",
                    trigger.function_ident
                ));
            }
        }
    }
    Ok(statements.join(";\n") + ";\n")
}

/// Preserve generated migrations in the source tree; append each changed history recipe.
/// The next build and published artifact carry every earlier migration unchanged.
pub(crate) async fn migrations(
    root: &std::path::Path,
    sql: &str,
) -> anyhow::Result<BTreeMap<std::path::PathBuf, Vec<u8>>> {
    let authored = root.join("migrations");
    let authored = if authored.is_dir() {
        build_database::migration_directory(&authored)
            .await?
            .iter()
            .map(|migration| migration.version)
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    let recipe = sql;
    let sql = format!(
        "{}{}\n{recipe}",
        crate::migrations::INPUT_VERSIONS_PREFIX,
        serde_json::to_string(&authored)?
    );
    let mut files = BTreeMap::new();
    let highest = build_database::package_migrations(root)
        .await?
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0);
    let mut latest = None;
    for directory in ["migrations", "history"] {
        let path = root.join(directory);
        if !path.is_dir() {
            continue;
        }
        let sources = build_database::migration_directory(&path).await?;
        for migration in &sources {
            if directory == "history" {
                latest = Some(migration.sql.as_str().to_owned());
            }
        }
        if directory == "history" {
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "sql")
                {
                    files.insert(
                        std::path::Path::new(directory).join(entry.file_name()),
                        std::fs::read(entry.path())?,
                    );
                }
            }
        }
    }
    if latest.as_deref() != Some(sql.as_str())
        && (!authored.is_empty() || !files.is_empty() || recipe != ";\n")
    {
        let version = highest
            .checked_add(1)
            .context("migration version exhausted")?;
        files.insert(
            std::path::PathBuf::from(format!("history/{version:04}_record_history.sql")),
            sql.into_bytes(),
        );
    }
    ensure!(
        files.keys().all(|path| path.starts_with("history")),
        "history migration escaped its directory"
    );
    Ok(files)
}

/// Remove triggers for declarations retired since an earlier immutable migration.
async fn retired_triggers(
    root: &std::path::Path,
    manifest: &PackageManifest,
) -> anyhow::Result<Vec<String>> {
    let directory = root.join("history");
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let previous = build_database::migration_directory(&directory).await?;
    let mut retired = std::collections::BTreeSet::new();
    for migration in &previous {
        for raw in pg_query::parse(migration.sql.as_str())?.protobuf.stmts {
            let Some(pg_query::NodeEnum::CreateTrigStmt(trigger)) =
                raw.stmt.and_then(|node| node.node)
            else {
                continue;
            };
            let Some(relation) = trigger.relation else {
                continue;
            };
            if manifest.models.values().any(|model| {
                model.owner == manifest.package.id
                    && model.schema.as_str() == relation.schemaname
                    && model.table.as_str() == relation.relname
            }) {
                continue;
            }
            let suffix = match trigger.trigname.as_str() {
                STAMP_TRIGGER => "stamp",
                LOG_TRIGGER => "log",
                _ => continue,
            };
            let function =
                generated_function_name(&relation.schemaname, &relation.relname, suffix)?;
            retired.insert((
                relation.schemaname,
                relation.relname,
                trigger.trigname,
                function,
            ));
        }
    }
    let mut statements = Vec::new();
    for (schema, table, trigger, function) in retired {
        let relation = format!(
            "{}.{}",
            escape_identifier(&schema),
            escape_identifier(&table)
        );
        let drop = format!(
            "DROP TRIGGER IF EXISTS {} ON {relation}",
            escape_identifier(&trigger)
        );
        statements.push(format!("DO $history$ BEGIN IF to_regclass({}) IS NOT NULL THEN EXECUTE {}; END IF; END $history$",
            escape_literal(&relation), escape_literal(&drop)));
        statements.push(format!(
            "DROP FUNCTION IF EXISTS wamn_history.{}()",
            escape_identifier(&function)
        ));
    }
    Ok(statements)
}
