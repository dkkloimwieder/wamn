//! The parsed check on the SQL a package declares for its guests.
//!
//! An application guest runs only statements declared at build: the host
//! resolves a statement path in the admitted set and runs those exact bytes
//! (`wamn:postgres/statements`). So admission, not the host, is the authority
//! on what that SQL may do (`docs/plan/platform-config.md`, Numbering). This
//! check parses each statement with PostgreSQL's own grammar (`pg_query`) and
//! walks the whole parse tree, so a name in a subquery, a CTE, a default or a
//! cast is seen, and quoting, schema qualification, case and comments change
//! nothing.
//!
//! It refuses:
//! - SQL that does not parse, or that is not exactly one statement;
//! - a `U&` identifier or string, whose escapes can spell a name;
//! - a `DO` block or a function body, where PL/pgSQL `EXECUTE` runs a string;
//! - the SQL `EXECUTE` of a prepared statement;
//! - a session write: `SET`, `RESET` or `set_config`;
//! - `pg_logical_emit_message`, which could forge a `wamn.*` causation frame;
//! - `platform.bump_counter`, `platform.assign_counter`, `platform.set_counter`;
//! - `query_to_xml*` and `cursor_to_xml*`, which run a query held in a string;
//! - an insert, update, delete, merge, truncate or copy of a `platform.*`
//!   relation.
//!
//! The names are matched in every identifier AND every string constant of the
//! tree, as whole identifiers and ignoring case. That over-refuses a statement
//! that only names one in a literal, which is fail-closed.

use std::fmt;

use pg_query::protobuf::Token;
use serde_json::Value;

/// Why a declared statement is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RefusalKind {
    /// The SQL does not parse with PostgreSQL's grammar.
    Unparsed,
    /// The SQL holds no statement or more than one.
    NotOneStatement,
    /// A `U&` identifier or string.
    UnicodeEscape,
    /// A `DO` block or a function body.
    CodeBlock,
    /// The SQL `EXECUTE` of a prepared statement.
    DynamicExecute,
    /// `SET`, `RESET` or `set_config`.
    SessionWrite,
    /// `pg_logical_emit_message`.
    LogicalMessage,
    /// A platform counter function.
    CounterFunction,
    /// `query_to_xml*` or `cursor_to_xml*`.
    XmlQuery,
    /// A write of a `platform.*` relation.
    PlatformWrite,
}

/// A declared statement that admission refuses.
#[derive(Debug)]
pub(super) struct DeclaredStatementRefusal {
    kind: RefusalKind,
    detail: String,
}

impl DeclaredStatementRefusal {
    fn new(kind: RefusalKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    #[cfg(test)]
    pub(super) const fn kind(&self) -> RefusalKind {
        self.kind
    }
}

impl fmt::Display for DeclaredStatementRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.detail)
    }
}

/// The platform functions that write `number_counter`. Only the numbering
/// renderer of the host calls them, on its own connection access.
const COUNTER_FUNCTIONS: [&str; 3] = ["bump_counter", "assign_counter", "set_counter"];

/// Statement nodes refused wherever they occur in the tree.
const REFUSED_NODES: [(&str, RefusalKind); 4] = [
    ("DoStmt", RefusalKind::CodeBlock),
    ("CreateFunctionStmt", RefusalKind::CodeBlock),
    ("ExecuteStmt", RefusalKind::DynamicExecute),
    ("VariableSetStmt", RefusalKind::SessionWrite),
];

/// Write nodes and the field that holds the relation each one writes.
const WRITE_NODES: [(&str, &str); 6] = [
    ("InsertStmt", "relation"),
    ("UpdateStmt", "relation"),
    ("DeleteStmt", "relation"),
    ("MergeStmt", "relation"),
    ("CopyStmt", "relation"),
    ("TruncateStmt", "relations"),
];

/// Refuse `sql` unless it is one statement that passes every check above.
pub(super) fn check_declared_statement(sql: &str) -> Result<(), DeclaredStatementRefusal> {
    let unparsed = |error: pg_query::Error| {
        DeclaredStatementRefusal::new(RefusalKind::Unparsed, error.to_string())
    };
    let scanned = pg_query::scan(sql).map_err(unparsed)?;
    for token in &scanned.tokens {
        if matches!(
            Token::try_from(token.token),
            Ok(Token::Uident | Token::Usconst | Token::Uescape)
        ) {
            return Err(DeclaredStatementRefusal::new(
                RefusalKind::UnicodeEscape,
                format!("a U& identifier or string at byte {}", token.start),
            ));
        }
    }
    let parsed = pg_query::parse(sql).map_err(unparsed)?;
    if parsed.protobuf.stmts.len() != 1 {
        return Err(DeclaredStatementRefusal::new(
            RefusalKind::NotOneStatement,
            format!("{} statements, not one", parsed.protobuf.stmts.len()),
        ));
    }
    let tree = serde_json::to_value(&parsed.protobuf)
        .map_err(|error| DeclaredStatementRefusal::new(RefusalKind::Unparsed, error.to_string()))?;
    check_value(&tree)
}

/// Walk every object, array and string of the parse tree.
fn check_value(value: &Value) -> Result<(), DeclaredStatementRefusal> {
    match value {
        Value::String(text) => check_text(text),
        Value::Array(items) => items.iter().try_for_each(check_value),
        Value::Object(fields) => {
            for (key, field) in fields {
                if let Some((node, kind)) = REFUSED_NODES.iter().find(|(node, _)| node == key) {
                    return Err(DeclaredStatementRefusal::new(*kind, format!("a {node}")));
                }
                if let Some((node, relation)) = WRITE_NODES.iter().find(|(node, _)| node == key) {
                    check_write(node, field.get(*relation))?;
                }
                check_value(field)?;
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
    }
}

/// Refuse a write whose target relation is in the `platform` schema.
fn check_write(node: &str, relation: Option<&Value>) -> Result<(), DeclaredStatementRefusal> {
    let mut targets = Vec::new();
    match relation {
        Some(Value::Array(items)) => targets.extend(
            items
                .iter()
                .filter_map(|item| item.get("node").and_then(|node| node.get("RangeVar"))),
        ),
        Some(range) => targets.push(range),
        None => {}
    }
    for target in targets {
        let schema = target.get("schemaname").and_then(Value::as_str);
        if schema.is_some_and(|schema| schema.eq_ignore_ascii_case("platform")) {
            return Err(DeclaredStatementRefusal::new(
                RefusalKind::PlatformWrite,
                format!("a {node} of a platform.* relation"),
            ));
        }
    }
    Ok(())
}

/// Refuse one identifier or string constant that names a refused function.
fn check_text(text: &str) -> Result<(), DeclaredStatementRefusal> {
    let lower = text.to_ascii_lowercase();
    let refused = [
        (&COUNTER_FUNCTIONS[..], false, RefusalKind::CounterFunction),
        (&["set_config"][..], false, RefusalKind::SessionWrite),
        (
            &["pg_logical_emit_message"][..],
            true,
            RefusalKind::LogicalMessage,
        ),
        (
            &["query_to_xml", "cursor_to_xml"][..],
            true,
            RefusalKind::XmlQuery,
        ),
    ];
    for (names, prefix, kind) in refused {
        if let Some(name) = names
            .iter()
            .find(|name| names_identifier(&lower, name, prefix))
        {
            return Err(DeclaredStatementRefusal::new(kind, format!("names {name}")));
        }
    }
    Ok(())
}

/// True if `name` occurs in `text` with no identifier character before it,
/// and, unless `prefix`, none after it.
fn names_identifier(text: &str, name: &str, prefix: bool) -> bool {
    let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + name.len()..].chars().next();
        !before.is_some_and(identifier) && (prefix || !after.is_some_and(identifier))
    })
}

#[cfg(test)]
mod tests;
