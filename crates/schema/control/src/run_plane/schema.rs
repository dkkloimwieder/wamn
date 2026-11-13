//! Read the declared SQL schema and validate project schema names.

use std::collections::BTreeSet;
use std::fmt;

use postgres_protocol::escape::escape_identifier;

use super::RUN_PLANE_FILES;

/// A checked schema name usable in quoted SQL and bare DDL rewrites.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BareSchemaName(String);

/// Why a value cannot be used as a bare project schema name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidBareSchemaName {
    reason: &'static str,
}

impl InvalidBareSchemaName {
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for InvalidBareSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for InvalidBareSchemaName {}

impl BareSchemaName {
    /// Check a schema name before it reaches generated SQL.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidBareSchemaName> {
        let value = value.into();
        let mut bytes = value.bytes();
        let reason = if value.is_empty() {
            "identifier is empty"
        } else if value.as_bytes().contains(&0) {
            "identifier contains NUL"
        } else if value.len() > 63 {
            "identifier exceeds PostgreSQL's 63-byte limit"
        } else if !matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            "schema name must match the lowercase bare identifier syntax [a-z_][a-z0-9_]*"
        } else {
            return Ok(Self(value));
        };
        Err(InvalidBareSchemaName { reason })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn quoted(&self) -> String {
        escape_identifier(&self.0)
    }
}

impl fmt::Display for BareSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A present index is STALE when the record definition names a record column of
/// its table that the live definition does not (word-boundary token match, so
/// `run_id` never matches inside `event_root_run_id`). This is deliberately the
/// narrow, real drift class — notably the pre-E4 `run_queue_claimable` without
/// the ordering columns. It does not compare other definition differences.
pub(super) fn index_definition_stale(
    file: &str,
    table: &str,
    record_stmt: &str,
    live_def: &str,
) -> bool {
    // Unit observations intentionally use the schema-of-record statement as
    // the live definition. PostgreSQL's `pg_indexes` rendering is checked
    // below; the record itself is already canonical by construction.
    if live_def == record_stmt {
        return false;
    }

    let live = live_def.split_whitespace().collect::<Vec<_>>().join(" ");
    let record_tokens = ident_tokens(record_stmt);
    let live_tokens = ident_tokens(&live);
    record_columns(file, "wamn_run", table)
        .iter()
        .any(|(col, _)| record_tokens.contains(col.as_str()) && !live_tokens.contains(col.as_str()))
}

/// Identifier-ish tokens of a SQL string: maximal `[A-Za-z0-9_]+` runs.
fn ident_tokens(sql: &str) -> BTreeSet<&str> {
    sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .collect()
}

pub(super) use postgres_protocol::escape::escape_identifier as quote_ident;

pub(super) fn record_table_names() -> BTreeSet<String> {
    RUN_PLANE_FILES
        .iter()
        .flat_map(|file| record_tables(file, "wamn_run"))
        .collect()
}

pub(super) fn normalize_observed_schema(definition: &str, schema: &BareSchemaName) -> String {
    definition
        .replace(
            &format!(
                "SET search_path TO 'pg_catalog', '{}', 'pg_temp'",
                schema.as_str()
            ),
            "SET search_path TO 'pg_catalog', 'wamn_run', 'pg_temp'",
        )
        .replace(&format!("{}.", schema.as_str()), "wamn_run.")
}

// ---------------------------------------------------------------------------
// Record parsing: slice the deploy/sql sources per table. The files follow the
// repo layout convention (one `CREATE TABLE <q>.<t> (` per table, one column
// per definition start line, full-line `--` comments, statements after the
// table body up to the next CREATE TABLE belong to that table's section); the
// tests below pin the parse against all three shipped files so a layout change
// fails here, not silently.
// ---------------------------------------------------------------------------

/// The canonical deploy DDL rewrite from the `wamn_run` schema to the target
/// project schema (the project-environment provisioning convention, relocated here
/// as the single owner). The dot-anchored replace leaves prose mentions like
/// `wamn_run_store` untouched. [`BareSchemaName`] makes the unquoted
/// interpolation requirement explicit in the API.
pub fn rewrite_schema(ddl: &str, schema: &BareSchemaName) -> String {
    ddl.replace(
        "SET search_path = pg_catalog, wamn_run, pg_temp",
        &format!("SET search_path = pg_catalog, {schema}, pg_temp"),
    )
    .replace("wamn_run.", &format!("{schema}."))
    // The guarded form FIRST: `SCHEMA wamn_run` is not a substring of it, so
    // missing it left `CREATE SCHEMA IF NOT EXISTS wamn_run` unrewritten (the
    // pre-wamn-1wdq bug: publish --runstate silently created a stray
    // `wamn_run` schema on the target DB while publish pre-created the real
    // target — caught by this verb's from-zero gate leg).
    .replace(
        "SCHEMA IF NOT EXISTS wamn_run",
        &format!("SCHEMA IF NOT EXISTS {schema}"),
    )
    .replace("SCHEMA wamn_run", &format!("SCHEMA {schema}"))
}

/// Every `CREATE TABLE <qualifier>.<name>` in `src`, in file order.
pub(super) fn record_tables(src: &str, qualifier: &str) -> Vec<String> {
    let head = format!("CREATE TABLE {qualifier}.");
    src.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix(&head)?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The schema declaration/grant prefix only. Helper functions are reconciled
/// independently, so a missing table can never replay a plain `CREATE
/// FUNCTION` against an already-present helper.
pub(super) fn schema_header_section(src: &str, qualifier: &str) -> String {
    let function_head = format!("CREATE FUNCTION {qualifier}.");
    src.lines()
        .take_while(|line| !line.trim().starts_with(&function_head))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One table's section: from its `CREATE TABLE` line up to (excluding) the next
/// `CREATE TABLE <qualifier>.`, independently reconciled run-plane helper
/// function, or EOF — the table body plus its indexes, RLS enablement, policy,
/// triggers, and grants. Catalog helpers remain part of their table sections;
/// run-plane helpers are reconciled independently before missing table sections
/// execute. Leading comment banners belong to the PREVIOUS section (they are
/// comments; nothing is lost).
pub(super) fn table_section(src: &str, qualifier: &str, table: &str) -> String {
    let head = format!("CREATE TABLE {qualifier}.{table} (");
    let any_head = format!("CREATE TABLE {qualifier}.");
    let function_head = format!("CREATE FUNCTION {qualifier}.");
    let replace_function_head = format!("CREATE OR REPLACE FUNCTION {qualifier}.");
    let mut out = Vec::new();
    let mut in_section = false;
    for line in src.lines() {
        let t = line.trim();
        if !in_section {
            if t.starts_with(&head) {
                in_section = true;
                out.push(line);
            }
            continue;
        }
        if t.starts_with(&any_head)
            || (qualifier == "wamn_run"
                && (t.starts_with(&function_head) || t.starts_with(&replace_function_head)))
        {
            break;
        }
        if t == "-- BEGIN POST-TABLE CONSTRAINTS" {
            break;
        }
        // CATALOG_SCHEMA_SQL closes its own transaction (wamn-jnms). That
        // terminator belongs to the file, not to the last table, and a repair
        // action that carried it would commit the caller's batch early.
        if t == "COMMIT;" {
            break;
        }
        out.push(line);
    }
    assert!(
        !out.is_empty(),
        "record parse: no section for {qualifier}.{table} — schema-of-record layout changed"
    );
    out.join("\n")
}

pub(super) fn table_section_carries_trigger(table: &str, trigger: &str) -> bool {
    RUN_PLANE_FILES.iter().any(|file| {
        record_tables(file, "wamn_run")
            .iter()
            .any(|name| name == table)
            && table_section(file, "wamn_run", table).contains(&format!("CREATE TRIGGER {trigger}"))
    })
}

/// The column definitions of `CREATE TABLE <qualifier>.<table> ( … )` in `src`:
/// `(name, full definition)` pairs, in record order, constraints and comments
/// skipped. Parenthesis-depth aware so a multi-line definition (the `runs`
/// status CHECK) parses whole; definitions are whitespace-collapsed for direct
/// use in `ALTER TABLE … ADD COLUMN`.
pub(super) fn record_columns(src: &str, qualifier: &str, table: &str) -> Vec<(String, String)> {
    const CONSTRAINT_KEYWORDS: [&str; 5] = ["PRIMARY", "FOREIGN", "CONSTRAINT", "CHECK", "UNIQUE"];
    let head = format!("CREATE TABLE {qualifier}.{table} (");
    let mut cols = Vec::new();
    let mut in_table = false;
    let mut depth: i32 = 0;
    let mut item: Option<(bool, Vec<String>)> = None; // (is_column, lines)
    let flush = |item: &mut Option<(bool, Vec<String>)>, cols: &mut Vec<(String, String)>| {
        if let Some((true, lines)) = item.take() {
            let def = lines
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let def = def.strip_suffix(',').unwrap_or(&def).to_string();
            let name = def
                .split_whitespace()
                .next()
                .expect("non-empty column definition")
                .to_string();
            cols.push((name, def));
        }
    };
    for line in src.lines() {
        let t = line.trim();
        if !in_table {
            if t.starts_with(&head) {
                in_table = true;
            }
            continue;
        }
        if depth == 0 && item.is_none() && t.starts_with(')') {
            break; // end of the table body
        }
        if t.is_empty() || t.starts_with("--") {
            continue;
        }
        if item.is_none() {
            let tok = t.split_whitespace().next().unwrap_or_default();
            let is_column = !CONSTRAINT_KEYWORDS.contains(&tok);
            item = Some((is_column, Vec::new()));
        }
        if let Some((_, lines)) = &mut item {
            lines.push(t.to_string());
        }
        for c in t.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 && t.ends_with(',') {
            depth = 0;
            flush(&mut item, &mut cols);
        }
        if depth < 0 {
            // The body's closing `)` rode the last item's line; flush and stop.
            flush(&mut item, &mut cols);
            break;
        }
    }
    flush(&mut item, &mut cols);
    assert!(
        !cols.is_empty(),
        "record parse: no columns for {qualifier}.{table} — schema-of-record layout changed"
    );
    cols
}

/// Every `CREATE [UNIQUE] INDEX <name> ON <qualifier>.<table> …;` statement in
/// `src`: `(index name, table, full statement)`.
pub(super) fn index_statements(src: &str, qualifier: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in src.lines() {
        let t = line.trim();
        match &mut current {
            None if t.starts_with("CREATE INDEX ") || t.starts_with("CREATE UNIQUE INDEX ") => {
                current = Some(vec![t.to_string()]);
            }
            None => continue,
            Some(lines) => lines.push(t.to_string()),
        }
        if t.ends_with(';') {
            let stmt = current.take().expect("complete statement").join(" ");
            let stmt = stmt.strip_suffix(';').unwrap_or(&stmt).to_string();
            let mut words = stmt.split_whitespace().skip_while(|w| *w != "INDEX");
            words.next(); // "INDEX"
            let name = words.next().expect("index name").to_string();
            let mut words = stmt.split_whitespace().skip_while(|w| *w != "ON");
            words.next(); // "ON"
            let table = words
                .next()
                .expect("index table")
                .trim_start_matches(&format!("{qualifier}."))
                .to_string();
            out.push((name, table, stmt));
        }
    }
    out
}
