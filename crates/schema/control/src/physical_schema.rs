//! Ordered installation SQL and its execution owners, independent of executable artifacts.

pub mod history;

use crate::MigrationSource;
use history::{generated_function_name, log_function_sql, stamp_function_sql};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use wamn_catalog::manifest::{ModelDeclaration, PackageManifest, RecordHistoryColumn};
use wamn_digest::Hasher;
use wamn_record_history::{LOG_TRIGGER, STAMP_TRIGGER, history_table_name};

pub const PHYSICAL_SCHEMA_PATH: &str = "generated/physical-schema.json";
const NAME_BYTE_LIMIT: usize = 64;
const HISTORY_SQL: &str = include_str!("../../../../deploy/sql/record-history.sql");

/// Catalog facts needed by the history renderer, in PostgreSQL attribute order.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationColumn {
    pub name: String,
    pub primary_key: bool,
    pub timestamptz: bool,
}

/// A package owner names a package identity, never its version or credentials.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionOwner {
    Platform,
    Package(String),
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalStatement {
    pub owner: ExecutionOwner,
    pub sql: String,
}

/// Immutable evidence and the complete ordered SQL selected for this package.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalSchema {
    pub columns: BTreeMap<String, Vec<RelationColumn>>,
    pub statements: Vec<PhysicalStatement>,
}

impl PhysicalSchema {
    /// Hash each owner and SQL byte in execution order with length delimiters.
    /// A match compares recipes, not the live database or SQL equivalence.
    pub fn fingerprint(&self) -> String {
        let mut hash = Hasher::new();
        for statement in &self.statements {
            let owner = match &statement.owner {
                ExecutionOwner::Platform => "platform",
                ExecutionOwner::Package(package) => package,
            };
            hash.update([u8::from(matches!(
                statement.owner,
                ExecutionOwner::Package(_)
            ))]);
            for bytes in [owner.as_bytes(), statement.sql.as_bytes()] {
                hash.update((bytes.len() as u64).to_be_bytes());
                hash.update(bytes);
            }
        }
        wamn_digest::format_sha256(&hash.finalize())
    }
}

#[derive(Debug)]
pub struct PhysicalSchemaError {
    kind: PhysicalSchemaErrorKind,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

#[derive(Debug)]
enum PhysicalSchemaErrorKind {
    InvalidSql,
    InvalidDefinition,
}

impl PhysicalSchemaError {
    fn invalid(detail: String) -> Self {
        Self {
            kind: PhysicalSchemaErrorKind::InvalidDefinition,
            detail,
            source: None,
        }
    }
    fn parse(source: pg_query::Error) -> Self {
        Self {
            kind: PhysicalSchemaErrorKind::InvalidSql,
            detail: "parse physical SQL".into(),
            source: Some(Box::new(source)),
        }
    }
}
impl fmt::Display for PhysicalSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "physical-schema {:?}: {}", self.kind, self.detail)
    }
}
impl std::error::Error for PhysicalSchemaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
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
        wamn_pg_core::quote_ident(name)
    }
}

/// Resolve the same owner that executes an authored migration statement.
pub fn migration_owner<'a>(
    manifest: &'a PackageManifest,
    raw: &pg_query::protobuf::RawStmt,
) -> &'a str {
    use pg_query::NodeEnum;
    let relation = match raw.stmt.as_ref().and_then(|stmt| stmt.node.as_ref()) {
        Some(NodeEnum::CreateStmt(stmt)) => stmt.relation.as_ref(),
        Some(NodeEnum::AlterTableStmt(stmt)) => stmt.relation.as_ref(),
        Some(NodeEnum::IndexStmt(stmt)) => stmt.relation.as_ref(),
        _ => None,
    };
    relation
        .and_then(|relation| {
            manifest.models.values().find(|model| {
                model.schema == relation.schemaname && model.table == relation.relname
            })
        })
        .map_or(manifest.package.id.as_str(), |model| model.owner.as_str())
}

/// One generated function, its stored body delimiter, and its trigger definition.
#[derive(Debug)]
pub struct HistoryTrigger {
    pub name: &'static str,
    pub function_name: String,
    pub function_ident: String,
    pub definition: Option<(String, &'static str, String)>,
}

/// Render stamp and log functions and triggers for one owned relation.
pub fn history_triggers(
    model: &ModelDeclaration,
    columns: &[RelationColumn],
) -> Result<[HistoryTrigger; 2], PhysicalSchemaError> {
    let audit_log = model.audit_log.as_ref().ok_or_else(|| {
        PhysicalSchemaError::invalid(format!(
            "{}.{} owns its relation and must declare audit_log",
            model.schema, model.table
        ))
    })?;
    let stamped = RecordHistoryColumn::ALL
        .into_iter()
        .filter(|column| audit_log.columns.contains(column))
        .map(RecordHistoryColumn::as_str)
        .collect::<Vec<_>>();
    let quoted = format!(
        "{}.{}",
        quoted_name(&model.schema),
        quoted_name(&model.table)
    );
    let stamp_name = generated_function_name(&model.schema, &model.table, "stamp")?;
    let log_name = generated_function_name(&model.schema, &model.table, "log")?;
    let stamp_ident = quoted_name(&stamp_name);
    let log_ident = quoted_name(&log_name);
    let stamp = (!stamped.is_empty()).then(|| {
        let others = columns.iter().map(|column| column.name.as_str())
            .filter(|column| !stamped.contains(column)).collect::<Vec<_>>();
        (stamp_function_sql(&stamp_ident, &stamped, &others), "$stamp$", format!(
            "TRIGGER {STAMP_TRIGGER} BEFORE INSERT OR UPDATE ON {quoted} FOR EACH ROW EXECUTE FUNCTION wamn_history.{stamp_ident}()"))
    });
    let log = model.log_retention().map(|retention| {
        let history = format!("{}.{}", wamn_pg_core::quote_ident(&model.schema),
            wamn_pg_core::quote_ident(&history_table_name(&model.table)));
        (log_function_sql(&log_ident, &history, columns), "$log$", format!(
            "TRIGGER {LOG_TRIGGER} AFTER INSERT OR DELETE OR UPDATE ON {quoted} FOR EACH ROW EXECUTE FUNCTION wamn_history.{log_ident}({})",
            wamn_pg_core::quote_literal(retention)))
    });
    Ok([
        HistoryTrigger {
            name: STAMP_TRIGGER,
            function_name: stamp_name,
            function_ident: stamp_ident,
            definition: stamp,
        },
        HistoryTrigger {
            name: LOG_TRIGGER,
            function_name: log_name,
            function_ident: log_ident,
            definition: log,
        },
    ])
}

/// The existing SQL helper remains the sole history-table definition.
pub fn history_table_sql(model: &ModelDeclaration) -> String {
    format!(
        "SELECT wamn_history.create_history_table({}, {}, false)",
        wamn_pg_core::quote_literal(&model.schema),
        wamn_pg_core::quote_literal(&model.table)
    )
}

/// Render authored SQL, history tables, functions, and triggers in installation order.
pub fn render(
    manifest: &PackageManifest,
    migrations: &[MigrationSource],
    columns: &BTreeMap<String, Vec<RelationColumn>>,
) -> Result<Vec<PhysicalStatement>, PhysicalSchemaError> {
    let package_statement = |sql| PhysicalStatement {
        owner: ExecutionOwner::Package(manifest.package.id.clone()),
        sql,
    };
    let mut statements = Vec::new();
    // The platform installs this definition before package SQL invokes it.
    if manifest
        .models
        .values()
        .any(|model| model.owner == manifest.package.id && model.log_retention().is_some())
    {
        let start = HISTORY_SQL
            .find("CREATE OR REPLACE FUNCTION wamn_history.create_history_table(")
            .expect("history helper exists");
        let end = HISTORY_SQL[start..]
            .find("\n-- The JSONB image")
            .expect("history helper ends")
            + start;
        statements.push(PhysicalStatement {
            owner: ExecutionOwner::Platform,
            sql: HISTORY_SQL[start..end].trim_end().to_owned(),
        });
    }
    let mut ordered = migrations.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    for migration in ordered {
        let sql = std::str::from_utf8(&migration.bytes).map_err(|source| PhysicalSchemaError {
            kind: PhysicalSchemaErrorKind::InvalidSql,
            detail: format!("{} is not UTF-8 SQL", migration.relative_path),
            source: Some(Box::new(source)),
        })?;
        let parsed = pg_query::parse(sql).map_err(PhysicalSchemaError::parse)?;
        for raw in &parsed.protobuf.stmts {
            let start = usize::try_from(raw.stmt_location)
                .expect("PostgreSQL statement offsets are nonnegative");
            let end = if raw.stmt_len == 0 {
                sql.len()
            } else {
                start
                    + usize::try_from(raw.stmt_len)
                        .expect("PostgreSQL statement lengths are nonnegative")
            };
            statements.push(PhysicalStatement {
                owner: ExecutionOwner::Package(migration_owner(manifest, raw).to_owned()),
                sql: sql[start..end].to_owned(),
            });
        }
    }
    let owned = manifest
        .models
        .iter()
        .filter(|(_, model)| model.owner == manifest.package.id)
        .collect::<Vec<_>>();
    if columns.len() != owned.len() {
        return Err(PhysicalSchemaError::invalid(
            "physical schema columns differ from owned relations".into(),
        ));
    }
    for (_, model) in &owned {
        if model.log_retention().is_some() {
            statements.push(package_statement(history_table_sql(model)));
        }
    }
    let mut triggers = Vec::new();
    for (id, model) in owned {
        let columns = columns.get(id).ok_or_else(|| {
            PhysicalSchemaError::invalid(format!("physical schema lacks columns for {id}"))
        })?;
        for trigger in history_triggers(model, columns)? {
            if let Some((function, _, definition)) = trigger.definition {
                statements.push(package_statement(function));
                triggers.push(package_statement(format!("CREATE OR REPLACE {definition}")));
            }
        }
    }
    statements.extend(triggers);
    Ok(statements)
}

/// Each column of one relation in attribute order: its name, whether the
/// primary key holds it, and whether its type is `timestamptz`.
pub const SELECT_RELATION_COLUMNS_SQL: &str = "\
SELECT a.attname::text, \
       COALESCE(a.attnum = ANY (i.indkey), false), \
       a.atttypid = 'pg_catalog.timestamptz'::pg_catalog.regtype \
  FROM pg_catalog.pg_attribute AS a \
  LEFT JOIN pg_catalog.pg_index AS i ON i.indrelid = a.attrelid AND i.indisprimary \
 WHERE a.attrelid = pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1::text, $2::text)) \
   AND a.attnum > 0 AND NOT a.attisdropped \
 ORDER BY a.attnum";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_sql_and_execution_owners_contribute_to_identity() {
        let mut manifest = PackageManifest::from_slice(
            br#"{
            "package":{"id":"records","version":"1.0.0"},
            "required_platform_policy_contract":{"id":"records_access","state":"satisfied"},
            "models":{"item":{"schema":"records","table":"item","owner":"records",
                "audit_log":{"columns":[],"retention":"unlimited"},
                "operations":{"get":{"permission":"item.get","result":"one"}}}},
            "connections":{},"components":{"records":{"connections":["postgres"]}}
        }"#,
        )
        .unwrap();
        let migrations = [MigrationSource {
            relative_path: "migrations/0001_initial.sql".into(),
            bytes: b"CREATE TABLE records.item (id int PRIMARY KEY, created_at timestamptz);"
                .to_vec(),
        }];
        let mut physical = PhysicalSchema {
            columns: BTreeMap::from([(
                "item".into(),
                vec![
                    RelationColumn {
                        name: "id".into(),
                        primary_key: true,
                        timestamptz: false,
                    },
                    RelationColumn {
                        name: "created_at".into(),
                        primary_key: false,
                        timestamptz: true,
                    },
                ],
            )]),
            statements: Vec::new(),
        };
        physical.statements = render(&manifest, &migrations, &physical.columns).unwrap();
        let original = physical.fingerprint();
        // Retention changes the generated log trigger without authored DDL.
        let audit_log = manifest
            .models
            .get_mut("item")
            .unwrap()
            .audit_log
            .as_mut()
            .unwrap();
        audit_log.retention = "P30D".into();
        physical.statements = render(&manifest, &migrations, &physical.columns).unwrap();
        assert_ne!(physical.fingerprint(), original);
        let generated = physical.fingerprint();
        physical.statements.last_mut().unwrap().owner = ExecutionOwner::Platform;
        assert_ne!(physical.fingerprint(), generated);

        // A column change also changes the generated history function body.
        let changed_owner = physical.fingerprint();
        physical
            .columns
            .get_mut("item")
            .unwrap()
            .push(RelationColumn {
                name: "note".into(),
                primary_key: false,
                timestamptz: false,
            });
        physical.statements = render(&manifest, &migrations, &physical.columns).unwrap();
        assert_ne!(physical.fingerprint(), generated);
        assert_ne!(physical.fingerprint(), changed_owner);
    }

    #[test]
    fn generated_names_refuse_postgres_truncation() {
        let relation = "r".repeat(63 - "s__".len() - "_stamp".len());
        assert!(super::history::generated_function_name("s", &relation, "stamp").is_ok());
        assert!(
            super::history::generated_function_name("s", &format!("{relation}r"), "stamp").is_err()
        );
    }
}
