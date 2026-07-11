//! Row-set → JSON shaping, and one-level relation expansion merge.
//!
//! Shaping is a pure function of the projected column names + the returned
//! cells. Expansion merge takes the primary rows plus a related row-set and
//! attaches each related record under the relation's name — a to-one relation
//! becomes a single embedded object (or `null`), a to-many relation an array.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::router::{Expand, ExpandDir};
use crate::value::SqlValue;

/// Shape a whole row-set into a JSON array of objects keyed by column name.
pub fn shape_rows(columns: &[String], rows: &[Vec<SqlValue>]) -> Vec<Value> {
    rows.iter().map(|r| shape_row(columns, r)).collect()
}

/// Shape a single row into a JSON object keyed by column name. Cells beyond the
/// column list are ignored; missing cells become `null`.
pub fn shape_row(columns: &[String], row: &[SqlValue]) -> Value {
    let mut m = Map::with_capacity(columns.len());
    for (i, col) in columns.iter().enumerate() {
        let v = row.get(i).map(SqlValue::to_json).unwrap_or(Value::Null);
        m.insert(col.clone(), v);
    }
    Value::Object(m)
}

/// A stable grouping key for a shaped JSON scalar (mirrors [`SqlValue::group_key`]).
fn value_key(v: &Value) -> String {
    v.to_string()
}

/// Attach an expansion's related rows onto the already-shaped primary rows.
///
/// `expanded_columns` / `expanded_rows` are the related row-set (as returned by
/// [`crate::Router::build_expand`]). Each related row is grouped by its
/// `ex.match_column` value; each primary row then looks up the group by its own
/// `ex.key_column` value and embeds the result under `ex.name`:
///
/// - [`ExpandDir::ToOne`] → the first match (or `null`);
/// - [`ExpandDir::ToMany`] → the full array (or `[]`).
pub fn attach_expansion(
    primary: &mut [Value],
    ex: &Expand,
    expanded_columns: &[String],
    expanded_rows: &[Vec<SqlValue>],
) {
    let match_idx = expanded_columns.iter().position(|c| c == &ex.match_column);
    let mut groups: HashMap<String, Vec<Value>> = HashMap::new();
    if let Some(mi) = match_idx {
        for row in expanded_rows {
            let Some(cell) = row.get(mi) else { continue };
            if matches!(cell, SqlValue::Null) {
                continue;
            }
            let key = cell.group_key();
            groups
                .entry(key)
                .or_default()
                .push(shape_row(expanded_columns, row));
        }
    }

    for row in primary.iter_mut() {
        let Value::Object(m) = row else { continue };
        let key = m.get(&ex.key_column).map(value_key);
        match ex.dir {
            ExpandDir::ToOne => {
                let embedded = key
                    .and_then(|k| groups.get(&k))
                    .and_then(|g| g.first())
                    .cloned()
                    .unwrap_or(Value::Null);
                m.insert(ex.name.clone(), embedded);
            }
            ExpandDir::ToMany => {
                let embedded = key
                    .and_then(|k| groups.get(&k))
                    .cloned()
                    .unwrap_or_default();
                m.insert(ex.name.clone(), Value::Array(embedded));
            }
        }
    }
}
