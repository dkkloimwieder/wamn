//! Validated catalog facts for authored query feeds.

use std::collections::{BTreeMap, BTreeSet};

use pg_query::NodeEnum;
use pg_query::protobuf::{JoinType, Node, RangeVar, SelectStmt, SetOperation};
use serde::{Deserialize, Serialize};
use wamn_schema_introspection::ir::{
    CatalogIr, ConstraintType, ForeignKeyColumn, Table, View, ViewColumnOrigin,
};

use crate::{
    AuthoredSql, CustomOperationDeclaration, GenerateError, GenerateErrorType, PackageManifest,
};

/// A schema-qualified table in a feed's read set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedTable {
    pub schema: String,
    pub table: String,
}

/// How a non-root table maps its changes to root keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedDirection {
    ReferencesRoot,
    ReferencedByLink,
}

/// One link, including the base column used for indexed lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedLink {
    pub table: FeedTable,
    pub linking_column: String,
    pub direction: FeedDirection,
    pub foreign_key_table: FeedTable,
    pub foreign_key_columns: Vec<ForeignKeyColumn>,
}

/// The build's compact input to authored feed statement generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedSource {
    pub root: FeedTable,
    pub read_set: Vec<FeedTable>,
    pub join_path: Vec<FeedLink>,
}

#[derive(Debug)]
struct JoinColumn {
    joined: FeedTable,
    referencing: FeedTable,
    column: String,
}

fn table_ref(table: &Table) -> FeedTable {
    FeedTable {
        schema: table.schema().to_owned(),
        table: table.name().to_owned(),
    }
}

fn refusal(operation: &str, reason: impl std::fmt::Display) -> GenerateError {
    GenerateError::new(
        GenerateErrorType::InvalidOperation,
        format!("{operation} feed: {reason}"),
    )
}

fn parse(sql: &str, operation: &str) -> Result<pg_query::ParseResult, GenerateError> {
    pg_query::parse(sql).map_err(|error| refusal(operation, format!("cannot read SELECT: {error}")))
}

fn select(parsed: &pg_query::ParseResult) -> Option<&SelectStmt> {
    let [statement] = parsed.protobuf.stmts.as_slice() else {
        return None;
    };
    match statement.stmt.as_ref()?.node.as_ref()? {
        NodeEnum::SelectStmt(select) => Some(select),
        _ => None,
    }
}

fn text(node: &Node) -> Option<&str> {
    match node.node.as_ref()? {
        NodeEnum::String(value) => Some(&value.sval),
        _ => None,
    }
}

fn column(node: &Node) -> Option<Vec<&str>> {
    let NodeEnum::ColumnRef(column) = node.node.as_ref()? else {
        return None;
    };
    column.fields.iter().map(text).collect()
}

fn alias(relation: &RangeVar) -> &str {
    relation
        .alias
        .as_ref()
        .map_or(&relation.relname, |alias| &alias.aliasname)
}

fn find_table<'a>(catalog: &'a CatalogIr, schema: &str, name: &str) -> Option<&'a Table> {
    catalog
        .tables()
        .iter()
        .find(|table| table.name() == name && (schema.is_empty() || table.schema() == schema))
}

fn foreign_key<'a>(from: &'a Table, to: &Table, field: &str) -> Option<&'a [ForeignKeyColumn]> {
    from.constraints().iter().find_map(|constraint| {
        let ConstraintType::ForeignKey {
            columns,
            referenced_schema,
            referenced_table,
            ..
        } = constraint.constraint_type()
        else {
            return None;
        };
        let [pair] = columns.as_ref() else {
            return None;
        };
        (referenced_schema.as_ref() == to.schema()
            && referenced_table.as_ref() == to.name()
            && pair.column() == field
            && Some(pair.referenced_column()) == crate::key::primary_key(to))
        .then_some(columns.as_ref())
    })
}

fn origin_is(origin: &ViewColumnOrigin, table: &Table, column: &str) -> bool {
    origin.schema == table.schema() && origin.table == table.name() && origin.column == column
}

/// Derive the feed facts after statement types and the view catalog are available.
pub(crate) fn derive(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    sources: &[AuthoredSql<'_>],
    name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<Option<FeedSource>, GenerateError> {
    if operation.feed != Some(true) {
        return Ok(None);
    }
    let fail = |reason: &str| refusal(name, reason);
    let lists = operation
        .lists
        .as_ref()
        .ok_or_else(|| fail("lists must name the root model and one key field"))?;
    let model = lists
        .model
        .as_ref()
        .and_then(|name| manifest.models.get(name))
        .ok_or_else(|| fail("lists must name a declared root model"))?;
    let root = find_table(catalog, &model.schema, &model.table)
        .ok_or_else(|| fail("root table is absent from the catalog"))?;
    let [key] = lists.key_field.as_slice() else {
        return Err(fail("lists needs exactly one key field"));
    };
    if crate::key::primary_key(root) != Some("id") {
        return Err(fail("the root needs a single primary key named id"));
    }
    let [statement] = operation.statements.values().collect::<Vec<_>>()[..] else {
        return Err(fail(
            "an authored feeding read needs exactly one SELECT statement",
        ));
    };
    let sql = sources
        .iter()
        .find(|source| source.path() == statement.path)
        .ok_or_else(|| fail("the authored statement is missing"))?;
    let sql = std::str::from_utf8(sql.bytes()).map_err(|_| fail("the statement is not UTF-8"))?;
    if crate::sql_lex::groups_or_windows(sql.as_bytes()) {
        return Err(fail(
            "the authored statement has GROUP BY or a window function",
        ));
    }
    let parsed = parse(sql, name)?;
    let query = select(&parsed).ok_or_else(|| fail("the authored statement must be one SELECT"))?;
    if !query.group_clause.is_empty() || !query.window_clause.is_empty()
        || parsed.protobuf.nodes().iter().any(|(node, _, _, _)| matches!(node, pg_query::NodeRef::FuncCall(call) if call.over.is_some()))
    {
        return Err(fail("the authored statement has GROUP BY or a window function"));
    }
    if query.limit_count.is_some() || query.limit_offset.is_some() {
        return Err(fail("the authored statement has LIMIT or OFFSET"));
    }
    let relations = parsed
        .protobuf
        .nodes()
        .into_iter()
        .filter_map(|(node, _, _, _)| {
            if let pg_query::NodeRef::RangeVar(relation) = node {
                Some(relation)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for relation in &relations {
        if !catalog.views().iter().any(|view| {
            view.name() == relation.relname
                && (relation.schemaname.is_empty() || view.schema() == relation.schemaname)
        }) {
            return Err(refusal(
                name,
                format!("relation {} is not an authored view", relation.relname),
            ));
        }
    }
    let [relation] = relations.as_slice() else {
        return Err(refusal(
            name,
            format!(
                "the authored statement must read exactly one authored view, found relations {}",
                relations
                    .iter()
                    .map(|relation| relation.relname.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    };
    if !matches!(query.from_clause.as_slice(), [node] if matches!(node.node.as_ref(), Some(NodeEnum::RangeVar(_))))
        || query.with_clause.is_some()
        || query.op != SetOperation::SetopNone as i32
        || parsed
            .protobuf
            .nodes()
            .iter()
            .filter(|(node, _, _, _)| matches!(node, pg_query::NodeRef::SelectStmt(_)))
            .count()
            != 1
    {
        return Err(refusal(
            name,
            format!(
                "relation {} must be the statement's sole FROM relation",
                relation.relname
            ),
        ));
    }
    let view = catalog
        .views()
        .iter()
        .find(|view| {
            view.name() == relation.relname
                && (relation.schemaname.is_empty() || view.schema() == relation.schemaname)
        })
        .expect("view resolved above");
    let key_column = result_column(query, key, alias(relation))
        .and_then(|field| view.columns().iter().find(|column| column.name() == field));
    if key_column
        .and_then(|column| column.origin())
        .is_none_or(|origin| !origin_is(origin, root, "id"))
    {
        return Err(refusal(
            name,
            format!(
                "key field {key} must select the root {}.{}.id",
                root.schema(),
                root.name()
            ),
        ));
    }
    let joins = validate_view(catalog, view, root);
    let used = |table: &Table, from: &Table, column: &str| {
        joins.as_ref().map_or(true, |joins| {
            joins.iter().any(|join| {
                join.joined == table_ref(table)
                    && join.referencing == table_ref(from)
                    && join.column == column
            })
        })
    };
    let mut read_set = Vec::new();
    let mut join_path = Vec::new();
    for base in view.base_tables() {
        let table = find_table(catalog, base.schema(), base.name()).ok_or_else(|| {
            refusal(
                name,
                format!(
                    "view {}.{} reads table {}.{} outside the package catalog",
                    view.schema(),
                    view.name(),
                    base.schema(),
                    base.name()
                ),
            )
        })?;
        if !manifest.models.values().any(|model| {
            model.schema == table.schema()
                && model.table == table.name()
                && model.log_retention().is_some()
        }) {
            return Err(refusal(
                name,
                format!(
                    "table {}.{} needs history with retention",
                    table.schema(),
                    table.name()
                ),
            ));
        }
        read_set.push(table_ref(table));
        if table == root {
            continue;
        }
        let root_links = table.columns().iter().filter_map(|column| {
            if !used(table, table, column.name()) {
                return None;
            }
            foreign_key(table, root, column.name()).map(|pairs| FeedLink {
                table: table_ref(table),
                linking_column: column.name().to_owned(),
                direction: FeedDirection::ReferencesRoot,
                foreign_key_table: table_ref(table),
                foreign_key_columns: pairs.to_vec(),
            })
        });
        let selected_links = view.columns().iter().filter_map(|column| {
            let origin = column.origin()?;
            let from = find_table(catalog, &origin.schema, &origin.table)?;
            if !used(table, from, &origin.column) {
                return None;
            }
            let pairs = foreign_key(from, table, &origin.column)?;
            Some(FeedLink {
                table: table_ref(table),
                linking_column: column.name().to_owned(),
                direction: FeedDirection::ReferencedByLink,
                foreign_key_table: table_ref(from),
                foreign_key_columns: pairs.to_vec(),
            })
        });
        let links = root_links.chain(selected_links).collect::<Vec<_>>();
        let uncovered = joins.as_ref().is_ok_and(|joins| {
            joins.iter().any(|join| {
                join.joined == table_ref(table)
                    && !links.iter().any(|link| {
                        link.foreign_key_table == join.referencing
                            && link
                                .foreign_key_columns
                                .iter()
                                .any(|pair| pair.column() == join.column)
                    })
            })
        });
        if links.is_empty() || uncovered {
            return Err(refusal(
                name,
                format!(
                    "view {}.{} base table {}.{} has no link to the root",
                    view.schema(),
                    view.name(),
                    table.schema(),
                    table.name()
                ),
            ));
        }
        for link in links {
            if !join_path.contains(&link) {
                join_path.push(link);
            }
        }
    }
    joins.map_err(|reason| {
        refusal(
            name,
            format!("view {}.{}: {reason}", view.schema(), view.name()),
        )
    })?;
    Ok(Some(FeedSource {
        root: table_ref(root),
        read_set,
        join_path,
    }))
}

fn result_column<'a>(query: &'a SelectStmt, key: &'a str, relation: &str) -> Option<&'a str> {
    query.target_list.iter().find_map(|node| {
        let NodeEnum::ResTarget(target) = node.node.as_ref()? else {
            return None;
        };
        let value = target.val.as_ref()?;
        if let Some(NodeEnum::ColumnRef(reference)) = value.node.as_ref()
            && reference
                .fields
                .last()
                .is_some_and(|node| matches!(node.node.as_ref(), Some(NodeEnum::AStar(_))))
        {
            return Some(key);
        }
        let fields = column(value)?;
        let field = *fields.last()?;
        let output = if target.name.is_empty() {
            field
        } else {
            &target.name
        };
        (output == key && (fields.len() == 1 || (fields.len() == 2 && fields[0] == relation)))
            .then_some(field)
    })
}

fn validate_view(
    catalog: &CatalogIr,
    view: &View,
    root: &Table,
) -> Result<Vec<JoinColumn>, String> {
    let parsed = pg_query::parse(view.definition())
        .map_err(|error| format!("cannot read the printed definition: {error}"))?;
    let query = select(&parsed).ok_or("the definition must be one SELECT")?;
    if !query.distinct_clause.is_empty() {
        return Err("DISTINCT is not supported".into());
    }
    if crate::sql_lex::groups_or_windows(view.definition().as_bytes()) && query.group_clause.is_empty()
        || !query.window_clause.is_empty()
        || parsed.protobuf.nodes().iter().any(|(node, _, _, _)| matches!(node, pg_query::NodeRef::FuncCall(call) if call.over.is_some()))
    { return Err("window functions are not supported".into()) }
    if query.op != SetOperation::SetopNone as i32 {
        return Err("set operations are not supported".into());
    }
    if view.has_set_returning_functions() {
        return Err("set-returning functions are not supported".into());
    }
    if query.limit_count.is_some() || query.limit_offset.is_some() {
        return Err("LIMIT and OFFSET are not supported".into());
    }
    if query.with_clause.is_some()
        || parsed
            .protobuf
            .nodes()
            .iter()
            .filter(|(node, _, _, _)| matches!(node, pg_query::NodeRef::SelectStmt(_)))
            .count()
            != 1
    {
        return Err("nested SELECT statements are not a feeding view shape".into());
    }
    let [from] = query.from_clause.as_slice() else {
        return Err("FROM must start at the root table".into());
    };
    let grouped = !query.group_clause.is_empty();
    let mut aliases = BTreeMap::new();
    let mut joins = Vec::new();
    view_from(from, catalog, root, grouped, &mut aliases, &mut joins)?;
    if grouped {
        let mut has_root_key = false;
        for group in &query.group_clause {
            let fields = column(group).ok_or("GROUP BY must name plain columns")?;
            let [qualifier, field] = fields.as_slice() else {
                return Err("GROUP BY must name qualified columns".into());
            };
            let table = aliases
                .get(*qualifier)
                .ok_or("GROUP BY names an unknown table")?;
            if *table == root {
                if *field != "id" {
                    return Err("GROUP BY must use only the root primary key and columns of tables referenced by the root".into());
                }
                has_root_key = true;
            } else if !root
                .columns()
                .iter()
                .any(|column| foreign_key(root, table, column.name()).is_some())
            {
                return Err(format!(
                    "GROUP BY table {}.{} is not referenced by the root",
                    table.schema(),
                    table.name()
                ));
            }
        }
        if !has_root_key {
            return Err("GROUP BY must include the root primary key".into());
        }
    }
    Ok(joins)
}

fn view_from<'a>(
    node: &Node,
    catalog: &'a CatalogIr,
    root: &'a Table,
    grouped: bool,
    aliases: &mut BTreeMap<String, &'a Table>,
    joins: &mut Vec<JoinColumn>,
) -> Result<(), String> {
    match node.node.as_ref() {
        Some(NodeEnum::RangeVar(relation)) => {
            let table =
                find_table(catalog, &relation.schemaname, &relation.relname).ok_or_else(|| {
                    format!("FROM relation {} must be a base table", relation.relname)
                })?;
            if table != root {
                return Err("FROM must start at the root table".into());
            }
            aliases.insert(alias(relation).to_owned(), table);
        }
        Some(NodeEnum::JoinExpr(join)) => {
            if !matches!(
                JoinType::try_from(join.jointype),
                Ok(JoinType::JoinInner | JoinType::JoinLeft)
            ) || join.is_natural
                || !join.using_clause.is_empty()
                || join.alias.is_some()
            {
                return Err("joins must use INNER or LEFT JOIN with ON".into());
            }
            view_from(
                join.larg.as_ref().ok_or("JOIN has no left table")?,
                catalog,
                root,
                grouped,
                aliases,
                joins,
            )?;
            let Some(NodeEnum::RangeVar(relation)) =
                join.rarg.as_ref().and_then(|node| node.node.as_ref())
            else {
                return Err("JOIN must name one base table".into());
            };
            let table =
                find_table(catalog, &relation.schemaname, &relation.relname).ok_or_else(|| {
                    format!("joined relation {} must be a base table", relation.relname)
                })?;
            if table == root {
                return Err(format!(
                    "root table {} appears more than once",
                    table.name()
                ));
            }
            let Some(NodeEnum::AExpr(equal)) =
                join.quals.as_ref().and_then(|node| node.node.as_ref())
            else {
                return Err("JOIN ON must be one primary-key equality".into());
            };
            if equal.name.len() != 1 || equal.name.first().and_then(text) != Some("=") {
                return Err("JOIN ON must be one primary-key equality".into());
            }
            let left = equal
                .lexpr
                .as_ref()
                .and_then(|node| column(node))
                .ok_or("JOIN ON must compare plain columns")?;
            let right = equal
                .rexpr
                .as_ref()
                .and_then(|node| column(node))
                .ok_or("JOIN ON must compare plain columns")?;
            let link = |joined: &[&str], referencing: &[&str]| {
                let [joined_alias, joined_column] = joined else {
                    return None;
                };
                let [referencing_alias, referencing_column] = referencing else {
                    return None;
                };
                if *joined_alias != alias(relation) {
                    return None;
                }
                let previous = aliases.get(*referencing_alias)?;
                let to_joined = Some(*joined_column) == crate::key::primary_key(table)
                    && foreign_key(previous, table, referencing_column).is_some();
                let to_root = *previous == root
                    && *referencing_column == "id"
                    && (grouped || Some(*joined_column) == crate::key::primary_key(table))
                    && foreign_key(table, root, joined_column).is_some();
                if to_joined && (!grouped || *previous == root) {
                    Some(JoinColumn {
                        joined: table_ref(table),
                        referencing: table_ref(previous),
                        column: (*referencing_column).to_owned(),
                    })
                } else if to_root {
                    Some(JoinColumn {
                        joined: table_ref(table),
                        referencing: table_ref(table),
                        column: (*joined_column).to_owned(),
                    })
                } else {
                    None
                }
            };
            let Some(link) = link(&left, &right).or_else(|| link(&right, &left)) else {
                return Err(format!(
                    "join to {} must match its primary key to a referencing column{}",
                    table.name(),
                    if grouped {
                        " or its foreign key to the root primary key"
                    } else {
                        ""
                    }
                ));
            };
            joins.push(link);
            aliases.insert(alias(relation).to_owned(), table);
        }
        _ => return Err("FROM functions and subqueries are not a feeding view shape".into()),
    }
    Ok(())
}

/// Warn about missing indexes on base columns used by a feed's links.
pub(crate) fn missing_indexes(catalog: &CatalogIr, name: &str, source: &FeedSource) -> Vec<String> {
    let mut missing = BTreeSet::new();
    for link in &source.join_path {
        let from = &link.foreign_key_table;
        let table = find_table(catalog, &from.schema, &from.table).expect("validated feed table");
        for pair in &link.foreign_key_columns {
            let field = pair.column();
            let indexed = table.indexes().iter().any(|index| {
                index
                    .columns()
                    .first()
                    .is_some_and(|column| column.name() == field)
            }) || table.constraints().iter().any(|constraint| {
                match constraint.constraint_type() {
                    ConstraintType::PrimaryKey { columns } | ConstraintType::Unique { columns } => {
                        columns
                            .first()
                            .is_some_and(|column| column.as_ref() == field)
                    }
                    _ => false,
                }
            });
            if !indexed {
                let index = wamn_schema_introspection::postgres::authored_object_name(
                    &from.table,
                    &[field.to_owned()],
                    "idx",
                );
                missing.insert(format!("{name} linking column {} on {}.{} has no index. Add to the package's migrations: CREATE INDEX {index} ON {}.{} ({field});", link.linking_column, from.schema, from.table, from.schema, from.table));
            }
        }
    }
    missing.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wamn_schema_introspection::ir::{
        Column, ColumnType, Constraint, ForeignKeyAction, Index, IndexColumn, IndexDirection,
        ViewBaseTable, ViewColumn,
    };

    fn table(name: &str, references: &[(&str, &str)]) -> Table {
        let mut constraints =
            vec![Constraint::primary_key(format!("{name}_id_pkey"), ["id"]).unwrap()];
        let mut columns = vec![Column::new("id", ColumnType::Uuid, false, None, None)];
        for (field, target) in references {
            columns.push(Column::new(*field, ColumnType::Uuid, false, None, None));
            constraints.push(
                Constraint::foreign_key(
                    format!("{name}_{field}_fkey"),
                    vec![ForeignKeyColumn::new(*field, "id")],
                    "app",
                    *target,
                    ForeignKeyAction::NoAction,
                    ForeignKeyAction::NoAction,
                )
                .unwrap(),
            );
        }
        Table::new("app", name, columns, constraints, Vec::new())
    }

    fn catalog(definition: &str) -> CatalogIr {
        let columns = [
            ("id", "line", "id"),
            ("order_id", "line", "order_id"),
            ("item_id", "line", "item_id"),
        ]
        .into_iter()
        .map(|(name, table, field)| {
            ViewColumn::new(name, ColumnType::Uuid).with_origin(ViewColumnOrigin {
                schema: "app".into(),
                table: table.into(),
                column: field.into(),
            })
        })
        .collect();
        CatalogIr::new(vec![
            table("line", &[("order_id", "orders"), ("item_id", "item")]),
            table("orders", &[]),
            table("item", &[]),
        ])
        .with_views(vec![
            View::new(
                "app",
                "screen",
                columns,
                ["line", "orders", "item"]
                    .into_iter()
                    .map(|name| ViewBaseTable::new("app", name))
                    .collect(),
            )
            .with_definition(definition.into(), false),
        ])
    }

    fn manifest() -> PackageManifest {
        serde_json::from_value(json!({
            "package": {"id": "fixture", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "fixture", "state": "satisfied"},
            "models": (["line", "orders", "item"].into_iter().map(|name| (name, json!({
                "schema": "app", "table": name, "owner": "fixture", "operations": {},
                "audit_log": {"columns": [], "retention": "P30D"}
            }))).collect::<BTreeMap<_, _>>()),
            "connections": {}, "components": {},
            "custom_operations": {"load": {
                "type": "projection", "visibility": "public", "permission": "load", "feed": true,
                "input": {"fields": []}, "errors": [],
                "lists": {"model": "line", "key_field": "id"},
                "statements": {"load": {"path": "query/load.sql", "fetch": "bounded_list", "row": []}}
            }}
        })).unwrap()
    }

    const RECEIPT: &str = "SELECT line.id, line.order_id, line.item_id
        FROM ((app.line JOIN app.orders ON ((orders.id = line.order_id)))
        JOIN app.item ON ((item.id = line.item_id)))";

    fn derive_sql(
        catalog: &CatalogIr,
        manifest: &PackageManifest,
        sql: &str,
    ) -> Result<FeedSource, GenerateError> {
        derive(
            catalog,
            manifest,
            &[AuthoredSql::new("query/load.sql", sql.as_bytes())],
            "load",
            &manifest.custom_operations["load"],
        )
        .map(Option::unwrap)
    }

    #[test]
    fn printed_receipt_records_aliases_and_index_warnings() {
        let catalog = catalog(RECEIPT);
        let source = derive_sql(
            &catalog,
            &manifest(),
            "SELECT id AS row_id FROM screen s ORDER BY id",
        );
        assert!(source.unwrap_err().context().contains("key field id"));
        let source = derive_sql(
            &catalog,
            &manifest(),
            "SELECT s.id FROM screen s ORDER BY s.id",
        )
        .unwrap();
        assert_eq!(source.root.table, "line");
        assert_eq!(
            source
                .read_set
                .iter()
                .map(|table| table.table.as_str())
                .collect::<Vec<_>>(),
            ["item", "line", "orders"]
        );
        assert_eq!(source.join_path[0].linking_column, "item_id");
        assert_eq!(
            source.join_path[1].foreign_key_columns[0].column(),
            "order_id"
        );
        assert_eq!(missing_indexes(&catalog, "load", &source).len(), 2);
        let line = catalog
            .tables()
            .iter()
            .find(|table| table.name() == "line")
            .unwrap();
        let indexed = Table::new(
            "app",
            "line",
            line.columns().to_vec(),
            line.constraints().to_vec(),
            ["item_id", "order_id"]
                .into_iter()
                .map(|field| {
                    Index::new(
                        format!("line_{field}_idx"),
                        vec![IndexColumn::new(field, IndexDirection::Asc)],
                    )
                    .unwrap()
                })
                .collect(),
        );
        let indexed = CatalogIr::new(vec![indexed, table("item", &[]), table("orders", &[])]);
        assert!(missing_indexes(&indexed, "load", &source).is_empty());
    }

    #[test]
    fn printed_grouped_summary_accepts_only_functionally_dependent_group_columns() {
        let root = table("orders", &[("item_id", "item")]);
        let catalog = CatalogIr::new(vec![
            root.clone(),
            table("line", &[("order_id", "orders")]),
            table("item", &[]),
        ]);
        let definition = "SELECT orders.id, count(line.id) FROM ((app.orders
            JOIN app.item ON ((item.id = orders.item_id)))
            LEFT JOIN app.line ON ((line.order_id = orders.id))) GROUP BY orders.id, item.id";
        let view = View::new("app", "summary", Vec::new(), Vec::new())
            .with_definition(definition.into(), false);
        validate_view(&catalog, &view, &root).unwrap();
        let view = view.with_definition(
            definition.replace("GROUP BY orders.id, item.id", "GROUP BY orders.id, line.id"),
            false,
        );
        assert!(validate_view(&catalog, &view, &root).is_err());
    }

    #[test]
    fn repeated_referenced_tables_keep_each_link() {
        let catalog = catalog(RECEIPT);
        let mut columns = catalog.views()[0].columns().to_vec();
        columns.push(
            ViewColumn::new("other_item_id", ColumnType::Uuid).with_origin(ViewColumnOrigin {
                schema: "app".into(),
                table: "line".into(),
                column: "other_item_id".into(),
            }),
        );
        let view = View::new(
            "app",
            "screen",
            columns,
            catalog.views()[0].base_tables().to_vec(),
        )
        .with_definition(
            "SELECT line.id, line.order_id, line.item_id, line.other_item_id FROM app.line
             JOIN app.orders ON orders.id = line.order_id
             JOIN app.item first_item ON first_item.id = line.item_id
             JOIN app.item other_item ON other_item.id = line.other_item_id"
                .into(),
            false,
        );
        let catalog = CatalogIr::new(vec![
            table(
                "line",
                &[
                    ("order_id", "orders"),
                    ("item_id", "item"),
                    ("other_item_id", "item"),
                ],
            ),
            table("orders", &[]),
            table("item", &[]),
        ])
        .with_views(vec![view]);
        let source = derive_sql(&catalog, &manifest(), "SELECT id FROM screen").unwrap();
        assert_eq!(
            source
                .join_path
                .iter()
                .filter(|link| link.table.table == "item")
                .map(|link| link.linking_column.as_str())
                .collect::<Vec<_>>(),
            ["item_id", "other_item_id"]
        );
    }

    #[test]
    fn refuses_unsafe_printed_view_shapes() {
        let catalog = catalog(RECEIPT);
        let root = catalog
            .tables()
            .iter()
            .find(|table| table.name() == "line")
            .unwrap();
        for definition in [
            "SELECT DISTINCT id FROM app.line",
            "SELECT id, row_number() OVER () FROM app.line",
            "SELECT id FROM app.line UNION SELECT id FROM app.line",
            "SELECT id FROM app.line LIMIT 1",
            "SELECT orders.id FROM app.orders JOIN app.line ON line.order_id = orders.id",
            "SELECT line.id FROM app.line LEFT JOIN app.orders ON orders.id = line.order_id OR true",
            "SELECT line.id FROM app.line, app.orders",
            "SELECT line.id FROM app.line RIGHT JOIN app.orders ON orders.id = line.order_id",
            "SELECT line.id FROM app.line JOIN app.orders ON orders.id <> line.order_id",
            "SELECT line.id FROM app.line JOIN LATERAL generate_series(1, 2) x ON true",
        ] {
            let view = catalog.views()[0]
                .clone()
                .with_definition(definition.into(), false);
            assert!(
                validate_view(&catalog, &view, root).is_err(),
                "{definition}"
            );
        }
        let view = catalog.views()[0]
            .clone()
            .with_definition(RECEIPT.into(), true);
        assert!(
            validate_view(&catalog, &view, root)
                .unwrap_err()
                .contains("set-returning")
        );
    }

    #[test]
    fn authored_statement_refusals_name_the_operation_and_reason() {
        let catalog = catalog(RECEIPT);
        let mut manifest = manifest();
        for (sql, reason) in [
            ("SELECT id FROM line", "relation line"),
            (
                "SELECT s.id FROM screen s JOIN line l ON l.id = s.id",
                "relation line",
            ),
            ("SELECT id FROM screen GROUP BY id", "GROUP BY"),
            ("SELECT id FROM screen LIMIT 1", "LIMIT"),
            ("SELECT id, row_number() OVER () FROM screen", "window"),
        ] {
            let error = derive_sql(&catalog, &manifest, sql).unwrap_err();
            assert!(
                error.context().contains("load") && error.context().contains(reason),
                "{error}"
            );
        }
        manifest
            .models
            .get_mut("item")
            .unwrap()
            .audit_log
            .as_mut()
            .unwrap()
            .retention = "none".into();
        let error = derive_sql(&catalog, &manifest, "SELECT id FROM screen").unwrap_err();
        assert!(
            error
                .context()
                .contains("table app.item needs history with retention")
        );
    }
}
