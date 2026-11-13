//! PostgreSQL syntax facts used by compilation and release effect review.

pub mod admission;

use crate::manifest::FilterMatch;
use pg_query::{NodeEnum, NodeRef};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationAccess {
    pub select_fields: BTreeSet<String>,
    pub insert_fields: BTreeSet<String>,
    pub update_fields: BTreeSet<String>,
    pub lock: bool,
    pub delete: bool,
}

fn parse(sql: &[u8]) -> Result<pg_query::ParseResult, &'static str> {
    pg_query::parse(std::str::from_utf8(sql).map_err(|_| "SQL is not UTF-8")?)
        .map_err(|_| "invalid PostgreSQL statement")
}

fn strings(nodes: &[pg_query::protobuf::Node]) -> Vec<&str> {
    nodes
        .iter()
        .filter_map(|node| match node.node.as_ref()? {
            NodeEnum::String(value) => Some(value.sval.as_str()),
            _ => None,
        })
        .collect()
}

// pg_query's convenience walk omits returning lists, VALUES and several
// expression children. Include them so effect facts cover the whole statement.
fn nodes(parsed: &pg_query::ParseResult) -> Vec<NodeRef<'_>> {
    fn append<'a>(out: &mut Vec<NodeRef<'a>>, node: &'a pg_query::protobuf::Node) {
        if let Some(node) = &node.node {
            out.extend(node.nodes().into_iter().map(|(node, _, _, _)| node));
        }
    }
    let mut result = parsed
        .protobuf
        .nodes()
        .into_iter()
        .map(|(node, _, _, _)| node)
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < result.len() {
        match result[index] {
            NodeRef::SelectStmt(statement) => {
                for node in &statement.values_lists {
                    append(&mut result, node);
                }
                for node in statement
                    .limit_count
                    .iter()
                    .chain(statement.limit_offset.iter())
                {
                    append(&mut result, node);
                }
                for node in &statement.window_clause {
                    append(&mut result, node);
                }
            }
            NodeRef::InsertStmt(statement) => {
                for node in &statement.returning_list {
                    append(&mut result, node);
                }
            }
            NodeRef::UpdateStmt(statement) => {
                for node in &statement.returning_list {
                    append(&mut result, node);
                }
            }
            NodeRef::DeleteStmt(statement) => {
                for node in &statement.returning_list {
                    append(&mut result, node);
                }
            }
            NodeRef::OnConflictClause(clause) => {
                if let Some(infer) = &clause.infer {
                    result.push(NodeRef::InferClause(infer));
                }
            }
            NodeRef::FuncCall(call) => {
                if let Some(filter) = &call.agg_filter {
                    append(&mut result, filter);
                }
                for node in &call.agg_order {
                    append(&mut result, node);
                }
                if let Some(window) = &call.over {
                    result.push(NodeRef::WindowDef(window));
                }
            }
            NodeRef::WindowDef(window) => {
                for node in window.partition_clause.iter().chain(&window.order_clause) {
                    append(&mut result, node);
                }
                for node in window.start_offset.iter().chain(window.end_offset.iter()) {
                    append(&mut result, node);
                }
            }
            NodeRef::CallStmt(statement) => {
                if let Some(call) = &statement.funccall {
                    result.push(NodeRef::FuncCall(call));
                }
            }
            NodeRef::AArrayExpr(array) => {
                for node in &array.elements {
                    append(&mut result, node);
                }
            }
            NodeRef::AIndirection(expression) => {
                if let Some(arg) = &expression.arg {
                    append(&mut result, arg);
                }
                for node in &expression.indirection {
                    append(&mut result, node);
                }
            }
            NodeRef::AIndices(indices) => {
                for node in indices.lidx.iter().chain(indices.uidx.iter()) {
                    append(&mut result, node);
                }
            }
            NodeRef::NamedArgExpr(expression) => {
                if let Some(arg) = &expression.arg {
                    append(&mut result, arg);
                }
            }
            NodeRef::CollateClause(expression) => {
                if let Some(arg) = &expression.arg {
                    append(&mut result, arg);
                }
            }
            NodeRef::GroupingFunc(expression) => {
                for node in &expression.args {
                    append(&mut result, node);
                }
            }
            NodeRef::CaseExpr(expression) => {
                if let Some(arg) = &expression.arg {
                    append(&mut result, arg);
                }
            }
            NodeRef::SubLink(link) => {
                if let Some(expression) = &link.testexpr {
                    append(&mut result, expression);
                }
            }
            _ => {}
        }
        index += 1;
    }
    result
}

/// Routine names at every expression position, including VALUES and RETURNING.
pub fn called_routines(sql: &[u8]) -> Result<BTreeSet<String>, &'static str> {
    fn visit(value: &serde_json::Value, names: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(name) = fields.get("funcname").and_then(serde_json::Value::as_array) {
                    let parts = name
                        .iter()
                        .filter_map(|part| part.get("node")?.get("String")?.get("sval")?.as_str())
                        .collect::<Vec<_>>();
                    if !parts.is_empty() {
                        names.insert(parts.join("."));
                    }
                }
                for value in fields.values() {
                    visit(value, names);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, names);
                }
            }
            _ => {}
        }
    }
    // The complete protobuf tree covers syntax that the convenience node walk omits.
    let tree = serde_json::to_value(parse(sql)?.protobuf)
        .map_err(|_| "cannot read the SQL syntax tree")?;
    let mut names = BTreeSet::new();
    visit(&tree, &mut names);
    Ok(names)
}

pub fn contains_schema_qualified_reference(sql: &[u8], schema: &str) -> bool {
    parse(sql).is_ok_and(|parsed| {
        nodes(&parsed).iter().any(|node| match node {
            NodeRef::RangeVar(relation) => relation.schemaname == schema,
            NodeRef::FuncCall(call) => {
                let names = strings(&call.funcname);
                names.len() > 1 && names[0] == schema
            }
            NodeRef::ColumnRef(column) => {
                let names = strings(&column.fields);
                names.len() > 2 && names[0] == schema
            }
            _ => false,
        })
    })
}

/// Relation effects and named constraints from one parsed statement.
#[derive(Debug)]
pub struct StatementAnalysis {
    pub access: BTreeMap<String, RelationAccess>,
    pub deletes: BTreeSet<String>,
    pub constraints: BTreeSet<String>,
}

pub fn analyze(
    sql: &[u8],
    relations: &BTreeMap<String, BTreeSet<String>>,
) -> Result<StatementAnalysis, &'static str> {
    let parsed = parse(sql)?;
    let nodes = nodes(&parsed);
    if nodes
        .iter()
        .any(|node| matches!(node, NodeRef::MergeStmt(_)))
    {
        return Err("MERGE effects are not supported");
    }
    let deletes = nodes
        .iter()
        .filter_map(|node| match node {
            NodeRef::DeleteStmt(statement) => statement
                .relation
                .as_ref()
                .map(|relation| relation.relname.clone()),
            _ => None,
        })
        .collect();
    let constraints = nodes
        .iter()
        .filter_map(|node| match node {
            NodeRef::InferClause(clause) if !clause.conname.is_empty() => {
                Some(clause.conname.clone())
            }
            _ => None,
        })
        .collect();
    let access = relation_effects(&parsed, &nodes, relations)?;
    Ok(StatementAnalysis {
        access,
        deletes,
        constraints,
    })
}

pub fn relation_access(
    sql: &[u8],
    relations: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, RelationAccess>, &'static str> {
    analyze(sql, relations).map(|analysis| analysis.access)
}

fn relation_effects(
    parsed: &pg_query::ParseResult,
    nodes: &[NodeRef<'_>],
    relations: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, RelationAccess>, &'static str> {
    let mut access = BTreeMap::<String, RelationAccess>::new();
    let resolve = |name: &str| {
        parsed
            .aliases
            .get(name)
            .filter(|table| relations.contains_key(*table))
            .or_else(|| {
                parsed
                    .tables
                    .iter()
                    .any(|(table, _)| table == name)
                    .then(|| relations.get_key_value(name).map(|(name, _)| name))
                    .flatten()
            })
    };
    for (table, _) in &parsed.tables {
        if parsed.cte_names.contains(table) {
            continue;
        }
        if !relations.contains_key(table) {
            return Err("SQL references an undeclared relation");
        }
        access.entry(table.clone()).or_default();
    }
    for node in nodes {
        match node {
            NodeRef::InsertStmt(statement) => {
                let table = &statement
                    .relation
                    .as_ref()
                    .ok_or("INSERT has no target")?
                    .relname;
                let fields = relations
                    .get(table)
                    .ok_or("INSERT references an undeclared relation")?;
                let entry = access.entry(table.clone()).or_default();
                if statement.cols.is_empty() {
                    entry.insert_fields.extend(fields.iter().cloned());
                }
                for column in &statement.cols {
                    if let Some(NodeEnum::ResTarget(target)) = &column.node {
                        if !fields.contains(&target.name) {
                            return Err("INSERT names an unknown column");
                        }
                        entry.insert_fields.insert(target.name.clone());
                    }
                }
                if let Some(conflict) = &statement.on_conflict_clause {
                    for target in &conflict.target_list {
                        if let Some(NodeEnum::ResTarget(target)) = &target.node {
                            entry.update_fields.insert(target.name.clone());
                        }
                    }
                }
            }
            NodeRef::UpdateStmt(statement) => {
                let table = &statement
                    .relation
                    .as_ref()
                    .ok_or("UPDATE has no target")?
                    .relname;
                let fields = relations
                    .get(table)
                    .ok_or("UPDATE references an undeclared relation")?;
                let entry = access.entry(table.clone()).or_default();
                for column in &statement.target_list {
                    if let Some(NodeEnum::ResTarget(target)) = &column.node {
                        if !fields.contains(&target.name) {
                            return Err("UPDATE names an unknown column");
                        }
                        entry.update_fields.insert(target.name.clone());
                    }
                }
            }
            NodeRef::DeleteStmt(statement) => {
                let table = &statement
                    .relation
                    .as_ref()
                    .ok_or("DELETE has no target")?
                    .relname;
                if !relations.contains_key(table) {
                    return Err("DELETE references an undeclared relation");
                }
                access.entry(table.clone()).or_default().delete = true;
            }
            NodeRef::ColumnRef(column) => {
                let names = strings(&column.fields);
                let star = column
                    .fields
                    .iter()
                    .any(|field| matches!(field.node, Some(NodeEnum::AStar(_))));
                if let [qualifier, field] = names.as_slice() {
                    if let Some(table) = resolve(qualifier) {
                        if !relations[table].contains(*field) {
                            return Err("SQL names an unknown column");
                        }
                        access
                            .entry(table.clone())
                            .or_default()
                            .select_fields
                            .insert((*field).to_owned());
                    }
                } else if let [field] = names.as_slice() {
                    if let Some(table) = resolve(field) {
                        access
                            .entry(table.clone())
                            .or_default()
                            .select_fields
                            .extend(relations[table].iter().cloned());
                    } else if !star {
                        for (table, entry) in &mut access {
                            if relations[table].contains(*field) {
                                entry.select_fields.insert((*field).to_owned());
                            }
                        }
                    }
                }
                if star {
                    for (table, entry) in &mut access {
                        if names.is_empty()
                            || names.first().and_then(|name| resolve(name)) == Some(table)
                        {
                            entry.select_fields.extend(relations[table].iter().cloned());
                        }
                    }
                }
            }
            NodeRef::SelectStmt(statement) if !statement.locking_clause.is_empty() => {
                let selected = statement
                    .from_clause
                    .iter()
                    .filter_map(|node| node.node.as_ref())
                    .flat_map(NodeEnum::nodes)
                    .filter_map(|(node, _, _, _)| match node {
                        NodeRef::RangeVar(relation) => Some(relation),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                for clause in &statement.locking_clause {
                    let Some(NodeEnum::LockingClause(clause)) = &clause.node else {
                        continue;
                    };
                    for relation in &selected {
                        let alias = relation
                            .alias
                            .as_ref()
                            .map_or(relation.relname.as_str(), |alias| alias.aliasname.as_str());
                        let locks = clause.locked_rels.is_empty() || clause.locked_rels.iter().any(|node| matches!(&node.node, Some(NodeEnum::RangeVar(target)) if target.relname == alias));
                        if locks && let Some(entry) = access.get_mut(&relation.relname) {
                            entry.lock = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(access)
}

pub fn groups_or_windows(sql: &[u8]) -> bool {
    parse(sql).is_ok_and(|parsed| {
        nodes(&parsed).iter().any(|node| match node {
            NodeRef::SelectStmt(statement) => {
                !statement.group_clause.is_empty() || !statement.window_clause.is_empty()
            }
            NodeRef::FuncCall(call) => call.over.is_some(),
            _ => false,
        })
    })
}

pub fn filter_predicate(sql: &[u8], parameter: u32) -> Option<(Option<FilterMatch>, usize)> {
    fn uses(node: &NodeEnum, parameter: u32, aliases: &BTreeSet<&str>) -> bool {
        node.nodes().iter().any(|(node, _, _, _)| match node {
            NodeRef::ParamRef(value) => u32::try_from(value.number).ok() == Some(parameter),
            NodeRef::ColumnRef(column) => strings(&column.fields)
                .first()
                .is_some_and(|name| aliases.contains(name)),
            _ => false,
        })
    }
    let parsed = parse(sql).ok()?;
    let nodes = nodes(&parsed);
    let location = nodes
        .iter()
        .filter_map(|node| match node {
            NodeRef::ParamRef(value) if u32::try_from(value.number).ok() == Some(parameter) => {
                usize::try_from(value.location).ok()
            }
            _ => None,
        })
        .min()?;
    let line = sql[..location].split(|byte| *byte == b'\n').count();
    // Each range alias belongs to its SELECT. Generated filters reuse the
    // alias in sibling EXISTS predicates, so aliases cannot be global.
    for node in &nodes {
        let NodeRef::SelectStmt(select) = node else {
            continue;
        };
        let aliases = select
            .from_clause
            .iter()
            .filter_map(|node| node.node.as_ref())
            .flat_map(NodeEnum::nodes)
            .filter_map(|(node, _, _, _)| match node {
                NodeRef::RangeFunction(range)
                    if range
                        .functions
                        .iter()
                        .filter_map(|node| node.node.as_ref())
                        .any(|node| uses(node, parameter, &BTreeSet::new())) =>
                {
                    range.alias.as_ref().map(|alias| alias.aliasname.as_str())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if aliases.is_empty() {
            continue;
        }
        let Some(predicate) = select
            .where_clause
            .as_ref()
            .and_then(|node| node.node.as_ref())
        else {
            continue;
        };
        let predicate_nodes = predicate.nodes();
        let nested = predicate_nodes
            .iter()
            .filter_map(|(node, _, _, _)| match node {
                NodeRef::SelectStmt(select) => Some(select),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (node, _, _, _) in &predicate_nodes {
            let NodeRef::FuncCall(call) = node else {
                continue;
            };
            if nested.iter().any(|select| {
                select.target_list.iter().chain(&select.from_clause).chain(&select.group_clause).chain(&select.sort_clause)
                    .chain(select.where_clause.as_deref()).chain(select.having_clause.as_deref())
                    .chain(select.with_clause.iter().flat_map(|with| &with.ctes))
                    .filter_map(|node| node.node.as_ref()).flat_map(NodeEnum::nodes)
                    .any(|(node, _, _, _)| matches!(node, NodeRef::FuncCall(inner) if std::ptr::eq(inner, *call)))
            }) { continue; }
            if call
                .args
                .iter()
                .filter_map(|node| node.node.as_ref())
                .any(|node| uses(node, parameter, &aliases))
            {
                match strings(&call.funcname).last().copied() {
                    Some("strpos") => return Some((Some(FilterMatch::Contains), line)),
                    Some("starts_with") => return Some((Some(FilterMatch::Prefix), line)),
                    _ => {}
                }
            }
        }
    }
    let aliases = BTreeSet::new();
    let mut mode = None;
    for node in &nodes {
        match node {
            NodeRef::FuncCall(call)
                if call
                    .args
                    .iter()
                    .filter_map(|node| node.node.as_ref())
                    .any(|node| uses(node, parameter, &aliases)) =>
            {
                match strings(&call.funcname).last().copied() {
                    Some("strpos") => return Some((Some(FilterMatch::Contains), line)),
                    Some("starts_with") => return Some((Some(FilterMatch::Prefix), line)),
                    _ => {}
                }
            }
            NodeRef::TypeCast(cast)
                if cast
                    .arg
                    .as_ref()
                    .and_then(|node| node.node.as_ref())
                    .is_some_and(|node| uses(node, parameter, &aliases)) =>
            {
                if cast
                    .type_name
                    .as_ref()
                    .and_then(|ty| strings(&ty.names).last().copied())
                    == Some("bool")
                {
                    mode = Some(FilterMatch::IsNull);
                }
            }
            NodeRef::AExpr(expression) => {
                let right = expression
                    .rexpr
                    .as_ref()
                    .and_then(|node| node.node.as_ref());
                let left = expression
                    .lexpr
                    .as_ref()
                    .and_then(|node| node.node.as_ref());
                if expression.kind == pg_query::protobuf::AExprKind::AexprIn as i32
                    && right.is_some_and(|node| uses(node, parameter, &aliases))
                {
                    mode = Some(FilterMatch::Exact);
                }
                // A JSONB value is a range only when SQL extracts an actual bound.
                if strings(&expression.name).as_slice() == ["->>"]
                    && left.is_some_and(|node| uses(node, parameter, &aliases))
                    && matches!(right, Some(NodeEnum::AConst(value)) if matches!(&value.val, Some(pg_query::protobuf::a_const::Val::Sval(value)) if matches!(value.sval.as_str(), "min" | "max")))
                {
                    mode = Some(FilterMatch::Range);
                }
            }
            NodeRef::SubLink(link)
                if link.sub_link_type == pg_query::protobuf::SubLinkType::AnySublink as i32
                    && link
                        .subselect
                        .as_ref()
                        .and_then(|node| node.node.as_ref())
                        .is_some_and(|node| uses(node, parameter, &aliases)) =>
            {
                mode = Some(FilterMatch::Exact);
            }
            _ => {}
        }
    }
    Some((mode, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_follow_range_function_values_and_extract_real_bounds() {
        for (sql, expected) in [
            (
                "SELECT id FROM item WHERE EXISTS (SELECT FROM jsonb_array_elements_text($2::jsonb) AS filter(value) WHERE strpos(part_code, filter.value) > 0)",
                Some(FilterMatch::Contains),
            ),
            (
                "SELECT id FROM item WHERE EXISTS (SELECT FROM jsonb_array_elements_text($2::jsonb) AS filter(value) WHERE starts_with(part_code, filter.value))",
                Some(FilterMatch::Prefix),
            ),
            (
                "SELECT id FROM item WHERE EXISTS (SELECT FROM jsonb_array_elements_text($1::jsonb) AS filter(value) WHERE starts_with(part_code, filter.value)) AND EXISTS (SELECT FROM jsonb_array_elements_text($2::jsonb) AS filter(value) WHERE strpos(part_code, filter.value) > 0)",
                Some(FilterMatch::Contains),
            ),
            ("SELECT $2::jsonb", None),
            (
                "SELECT id FROM item WHERE weight >= ($2::jsonb->>'min')::numeric",
                Some(FilterMatch::Range),
            ),
        ] {
            assert_eq!(filter_predicate(sql.as_bytes(), 2).unwrap().0, expected);
        }
    }

    #[test]
    fn merge_refuses_until_every_branch_has_effect_facts() {
        assert!(
            analyze(
                b"MERGE INTO item USING source ON item.id = source.id WHEN MATCHED THEN DELETE",
                &BTreeMap::from([
                    ("item".into(), BTreeSet::from(["id".into()])),
                    ("source".into(), BTreeSet::from(["id".into()]))
                ])
            )
            .is_err()
        );
    }
}
