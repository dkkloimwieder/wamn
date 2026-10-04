//! Inspection of a bounded, ordered batch that updates one owned relation.

use super::{
    Binding, ColumnType, MigrationPolicyError, MigrationPolicyErrorType, Parser, Path,
    StageRelation, lex_statements, policy_error,
};
use std::collections::{BTreeMap, BTreeSet};

/// A single-column, immediate, NOT NULL unique key established from PostgreSQL.
///
/// The caller must establish both relation ownership and the actual constraint.
#[derive(Debug, Clone)]
pub struct StageUniqueKey {
    pub schema: String,
    pub relation: String,
    pub column: String,
}

/// Validate an ordered batch with fixed cursor and completion result columns.
///
/// The two parameters are the JSON cursor and the declared positive batch size.
/// This grammar returns the cursor unchanged. Its predicate must exclude updated
/// rows, and completion requires a later empty batch plus final postconditions.
pub fn validate_stage_batch(
    path: impl AsRef<Path>,
    bytes: &[u8],
    relations: &[StageRelation],
    parameters: &[ColumnType],
    unique_keys: &[StageUniqueKey],
) -> Result<(), MigrationPolicyError> {
    let path = path.as_ref();
    if parameters != [ColumnType::Json, ColumnType::Int32] {
        return Err(policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            None,
            "stage batch parameters must be exactly [Json, Int32]: cursor and batch size",
        ));
    }
    let sql = std::str::from_utf8(bytes).map_err(|_| {
        policy_error(
            MigrationPolicyErrorType::InvalidSql,
            path,
            None,
            "stage batch is not UTF-8 SQL",
        )
    })?;
    let statements = lex_statements(sql, path)?;
    if statements.len() != 1 {
        return Err(policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            None,
            "stage batch requires exactly one statement",
        ));
    }
    // The synthetic CTE has no database schema. It cannot authorize a physical
    // relation, whose name must remain qualified throughout the grammar.
    let mut scoped_relations = relations.to_vec();
    let mut parser = Parser {
        tokens: &statements[0],
        position: 0,
        path,
        relations: &scoped_relations,
        parameters,
        bindings: Vec::new(),
        depth: 0,
        ctes: BTreeMap::new(),
        row_reference: None,
    };
    parser.require_word("with")?;
    parser.require_word("batch")?;
    parser.require_word("as")?;
    parser.require_symbol(b'(')?;
    parser.require_word("select")?;
    let key = parser.identifier()?;
    parser.require_word("from")?;
    let target = parser.relation()?;
    let relation = &relations[target.relation];
    if !unique_keys.iter().any(|candidate| {
        candidate.schema == relation.schema
            && candidate.relation == relation.name
            && candidate.column == key
    }) {
        return Err(parser.error(format!(
            "batch key {}.{}.{key} is not an established immediate NOT NULL unique key",
            relation.schema, relation.name
        )));
    }
    let key_type = relation.columns.get(&key).copied().ok_or_else(|| {
        parser.error(format!(
            "batch key {}.{}.{key} is outside package ownership",
            relation.schema, relation.name
        ))
    })?;
    let target_index = target.relation;
    parser.bindings.push(vec![target]);
    parser.require_word("where")?;
    let predicate = parser.expression()?;
    parser.require_type(&predicate, ColumnType::Boolean)?;
    parser.require_word("order")?;
    parser.require_word("by")?;
    if parser.identifier()? != key {
        return Err(parser.error(format!("stage batch ORDER BY must name unique key {key}")));
    }
    parser.eat_word("asc");
    parser.require_word("limit")?;
    require_parameter(&mut parser, 2)?;
    parser.require_word("for")?;
    parser.require_word("update")?;
    parser.require_symbol(b')')?;
    parser.require_symbol(b',')?;
    parser.require_word("updated")?;
    parser.require_word("as")?;
    parser.require_symbol(b'(')?;
    parser.require_word("update")?;
    let update = parser.relation()?;
    if update.relation != target_index {
        return Err(parser.error("stage batch UPDATE must target the selected owned relation"));
    }
    let update_alias = update.alias.clone();
    if update_alias == "batch" {
        return Err(parser.error("stage batch UPDATE alias must not shadow its batch CTE"));
    }
    let position = parser.position;
    // Establish the CTE's exact column set before inspecting UPDATE expressions.
    drop(parser);
    let batch_index = scoped_relations.len();
    scoped_relations.push(StageRelation {
        schema: String::new(),
        name: "batch".into(),
        columns: BTreeMap::from([(key.clone(), key_type)]),
    });
    let mut parser = Parser {
        tokens: &statements[0],
        position,
        path,
        relations: &scoped_relations,
        parameters,
        bindings: vec![vec![
            update,
            Binding {
                alias: "batch".into(),
                relation: batch_index,
            },
        ]],
        depth: 0,
        ctes: BTreeMap::from([("batch".into(), batch_index)]),
        row_reference: None,
    };
    parser.require_word("set")?;
    let mut assigned = BTreeSet::new();
    loop {
        let field = parser.identifier()?;
        if field == key {
            return Err(parser.error(format!("stage batch must not update its unique key {key}")));
        }
        if !assigned.insert(field.clone()) {
            return Err(parser.error(format!("duplicate stage batch assignment to {field}")));
        }
        let column_type = relation.columns.get(&field).copied().ok_or_else(|| {
            parser.error(format!(
                "batch assignment {}.{}.{field} is outside package ownership",
                relation.schema, relation.name
            ))
        })?;
        parser.require_symbol(b'=')?;
        let value = parser.expression()?;
        parser.require_type(&value, column_type)?;
        if !parser.eat_symbol(b',') {
            break;
        }
    }
    parser.require_word("from")?;
    parser.require_word("batch")?;
    parser.require_word("where")?;
    require_key_reference(&mut parser, &update_alias, &key)?;
    parser.require_symbol(b'=')?;
    require_key_reference(&mut parser, "batch", &key)?;
    parser.require_word("returning")?;
    require_key_reference(&mut parser, &update_alias, &key)?;
    parser.require_symbol(b')')?;
    parser.require_word("select")?;
    require_parameter(&mut parser, 1)?;
    parser.require_symbol(b':')?;
    parser.require_symbol(b':')?;
    parser.require_word("jsonb")?;
    parser.require_word("as")?;
    parser.require_word("next_cursor")?;
    parser.require_symbol(b',')?;
    parser.require_word("not")?;
    parser.require_word("exists")?;
    parser.require_symbol(b'(')?;
    parser.require_word("select")?;
    if parser.digits()? != "1" {
        return Err(parser.error("batch completion requires SELECT 1"));
    }
    parser.require_word("from")?;
    parser.require_word("batch")?;
    parser.require_symbol(b')')?;
    parser.require_word("as")?;
    parser.require_word("complete")?;
    if parser.position != parser.tokens.len() {
        return Err(parser.error(format!("unsupported batch construct {}", parser.current())));
    }
    Ok(())
}

fn require_parameter(
    parser: &mut Parser<'_, '_>,
    expected: usize,
) -> Result<(), MigrationPolicyError> {
    parser.require_symbol(b'$')?;
    if parser.digits()? != expected.to_string() {
        return Err(parser.error(format!("stage batch requires parameter ${expected}")));
    }
    Ok(())
}

fn require_key_reference(
    parser: &mut Parser<'_, '_>,
    relation: &str,
    key: &str,
) -> Result<(), MigrationPolicyError> {
    if parser.identifier()? != relation {
        return Err(parser.error(format!(
            "stage batch requires key reference {relation}.{key}"
        )));
    }
    parser.require_symbol(b'.')?;
    if parser.identifier()? != key {
        return Err(parser.error(format!(
            "stage batch requires key reference {relation}.{key}"
        )));
    }
    Ok(())
}
