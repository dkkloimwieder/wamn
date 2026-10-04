//! Row-only synchronization assignments with explicit writable-column authority.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{
    Binding, MigrationPolicyError, MigrationPolicyErrorType, Parser, StageRelation, lex_statements,
    policy_error,
};

/// Inspect a closed synchronization body against one owned row and writable fields.
///
/// The caller must exclude identity, platform, and other server-owned targets
/// from `writable_columns`. The surrounding routine must separately establish
/// SECURITY INVOKER. Success does not authorize routine creation or execution.
pub fn validate_stage_trigger_body(
    path: impl AsRef<Path>,
    body: &str,
    relation: &StageRelation,
    writable_columns: &[String],
) -> Result<(), MigrationPolicyError> {
    let path = path.as_ref();
    let statements = lex_statements(body, path)?;
    if statements.len() < 3 {
        return Err(policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            None,
            "synchronization body requires BEGIN, NEW assignments, RETURN NEW, and END",
        ));
    }
    let assignment_count = statements.len() - 2;
    let mut assigned = BTreeSet::new();
    for (index, tokens) in statements.iter().enumerate() {
        let mut parser = Parser {
            tokens,
            position: 0,
            path,
            relations: std::slice::from_ref(relation),
            parameters: &[],
            bindings: vec![vec![Binding {
                alias: "new".into(),
                relation: 0,
            }]],
            depth: 0,
            ctes: BTreeMap::new(),
            row_reference: Some("new"),
        };
        let result = match index.cmp(&assignment_count) {
            Ordering::Less => assignment(&mut parser, index == 0, writable_columns, &mut assigned),
            Ordering::Equal => parser
                .require_word("return")
                .and_then(|()| parser.require_word("new")),
            Ordering::Greater => parser.require_word("end"),
        };
        result.map_err(|mut error| {
            error.statement_index = Some(index + 1);
            error
        })?;
        if parser.position != tokens.len() {
            let mut error = parser.error(format!(
                "unsupported synchronization construct {}",
                parser.current()
            ));
            error.statement_index = Some(index + 1);
            return Err(error);
        }
    }
    Ok(())
}

fn assignment(
    parser: &mut Parser<'_, '_>,
    first: bool,
    writable_columns: &[String],
    assigned: &mut BTreeSet<String>,
) -> Result<(), MigrationPolicyError> {
    if first {
        parser.require_word("begin")?;
    }
    parser.require_word("new")?;
    parser.require_symbol(b'.')?;
    let target = parser.identifier()?;
    if !writable_columns.contains(&target) {
        return Err(parser.error(format!(
            "synchronization target NEW.{target} is not an explicitly writable package column"
        )));
    }
    let column_type = parser.relations[0]
        .columns
        .get(&target)
        .copied()
        .ok_or_else(|| {
            parser.error(format!(
                "synchronization target NEW.{target} is outside package ownership"
            ))
        })?;
    if !assigned.insert(target.clone()) {
        return Err(parser.error(format!(
            "duplicate synchronization assignment to NEW.{target}"
        )));
    }
    parser.require_symbol(b':')?;
    parser.require_symbol(b'=')?;
    let value = parser.expression()?;
    // Name unapproved operators before reporting any consequent result-type error.
    if parser.position != parser.tokens.len() {
        return Err(parser.error(format!(
            "unsupported synchronization construct {}",
            parser.current()
        )));
    }
    parser.require_type(&value, column_type)
}
