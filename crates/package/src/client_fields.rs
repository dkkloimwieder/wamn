//! Projects declared field presence, null values and nested shapes.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn field(path: String, type_name: String, required: bool, nullable: bool) -> FieldIr {
    FieldIr {
        path,
        type_name,
        required,
        nullable,
        revision: crate::manifest::Revision::default(),
        values: Vec::new(),
        children: Vec::new(),
        minimum: None,
        maximum: None,
        label: None,
        visible: None,
        description: None,
        references: None,
        column: None,
    }
}

/// Read the record one declared field names, when it names one.
fn reference(declared: &Value) -> Option<ReferenceIr> {
    let model = declared
        .get("references")?
        .get("model")?
        .as_str()?
        .to_owned();
    Some(ReferenceIr {
        model,
        narrowed_by: declared
            .get("references")
            .and_then(|reference| reference.get("narrowed_by"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Read the two authored text members of one declared field.
fn text(declared: &Value) -> (Option<String>, Option<String>) {
    let member = |key: &str| declared.get(key).and_then(Value::as_str).map(str::to_owned);
    (member("label"), member("description"))
}

fn values(value: Option<&Value>) -> Vec<String> {
    let mut values: Vec<_> = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    values.sort();
    values.dedup();
    values
}

fn insert(tree: &mut Vec<FieldIr>, leaf: FieldIr, segments: &[&str], prefix: &str) {
    let Some((segment, rest)) = segments.split_first() else {
        return;
    };
    if rest.is_empty() {
        // A leaf path ending in `[]` declares repeated scalars: an array
        // whose one child is the element, at the same path.
        let leaf = if segment.ends_with("[]") {
            let mut array = field(leaf.path.clone(), "array".into(), leaf.required, false);
            array.children.push(leaf);
            array
        } else {
            leaf
        };
        tree.push(leaf);
        tree.sort_by(|a, b| a.path.cmp(&b.path));
        tree.dedup();
        return;
    }
    let path = if prefix.is_empty() {
        (*segment).to_owned()
    } else {
        format!("{prefix}.{segment}")
    };
    let index = tree
        .iter()
        .position(|item| item.path == path)
        .unwrap_or_else(|| {
            tree.push(field(
                path.clone(),
                if segment.ends_with("[]") {
                    "array"
                } else {
                    "object"
                }
                .into(),
                true,
                false,
            ));
            tree.len() - 1
        });
    insert(&mut tree[index].children, leaf, rest, &path);
    tree.sort_by(|a, b| a.path.cmp(&b.path));
}

pub fn fields_of(contract: &Value) -> Vec<FieldIr> {
    let mut tree = Vec::new();
    for declared in contract
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(path) = declared.get("path").and_then(Value::as_str) else {
            continue;
        };
        let mut leaf = field(
            path.to_owned(),
            declared
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("opaque")
                .to_owned(),
            declared
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            declared
                .get("nullable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
        leaf.values = values(declared.get("values"));
        leaf.revision = declared
            .get("revision")
            .and_then(|revision| serde_json::from_value(revision.clone()).ok())
            .unwrap_or_default();
        (leaf.label, leaf.description) = text(declared);
        leaf.visible = declared.get("visible").and_then(Value::as_bool);
        leaf.references = reference(declared);
        insert(&mut tree, leaf, &path.split('.').collect::<Vec<_>>(), "");
    }
    tree
}

pub fn input_fields_of(contract: &Value) -> Vec<FieldIr> {
    let mut tree = fields_of(contract);
    if !tree.is_empty() {
        // The custom input contract's line bound owns value.line[].
        if let Some(value) = tree.iter_mut().find(|field| field.path == "value")
            && let Some(line) = value
                .children
                .iter_mut()
                .find(|field| field.path == "value.line[]")
            && let Some(bounds) = contract.get("line")
        {
            line.minimum = bounds.get("minimum").and_then(Value::as_u64);
            line.maximum = bounds.get("maximum").and_then(Value::as_u64);
            // The line bound is the only declaration of the repeated group,
            // so it carries that group's text as well as its bounds.
            (line.label, line.description) = text(bounds);
        }
        return tree;
    }
    if let Some(members) = contract.as_object() {
        for (name, member) in members {
            // `pagination` states the cursor protocol, and its `type` names
            // that protocol (`keyset`), not an input.
            if name == "pagination" {
                continue;
            }
            let Some(type_name) = member.get("type").and_then(Value::as_str) else {
                continue;
            };
            let mut declared = field(
                name.clone(),
                type_name.to_owned(),
                member
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                member
                    .get("nullable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            declared.values = values(member.get("values"));
            declared.revision = member
                .get("revision")
                .and_then(|revision| serde_json::from_value(revision.clone()).ok())
                .unwrap_or_default();
            (declared.label, declared.description) = text(member);
            declared.visible = member.get("visible").and_then(Value::as_bool);
            declared.references = reference(member);
            tree.push(declared);
        }
    }
    for writable in contract
        .get("writable_fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = writable
            .get("path")
            .or_else(|| writable.get("field"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let mut declared = field(
            name.to_owned(),
            writable
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("opaque")
                .to_owned(),
            // An input that the operation refuses when omitted is required.
            writable.get("omitted").and_then(Value::as_str) == Some("invalid_input"),
            writable.get("explicit_null").and_then(Value::as_str) == Some("accepted"),
        );
        declared.values = values(writable.get("values"));
        (declared.label, declared.description) = text(writable);
        declared.visible = writable.get("visible").and_then(Value::as_bool);
        declared.references = reference(writable);
        declared.column = writable
            .get("field")
            .and_then(Value::as_str)
            .map(str::to_owned);
        insert(
            &mut tree,
            declared,
            &name.split('.').collect::<Vec<_>>(),
            "",
        );
    }
    let mut sent_any = false;
    for filter in contract
        .get("filters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(type_name)) = (
            filter.get("field").and_then(Value::as_str),
            filter.get("type").and_then(Value::as_str),
        ) else {
            continue;
        };
        // A list is a repeated value, a range an object of two optional
        // bounds, and an is-null filter one boolean. A band that has no
        // default is always sent, with its minimum.
        let required = filter.get("required").and_then(Value::as_bool) == Some(true);
        let sent = required && filter.get("default").is_none();
        let (path, declared) = match filter.get("binding").and_then(Value::as_str) {
            Some("json_array") => {
                let path = format!("filter.{name}[]");
                let mut repeated = field(path.clone(), "array".into(), false, false);
                let mut item = field(path.clone(), type_name.into(), true, false);
                (item.label, item.description) = text(filter);
                item.visible = filter.get("visible").and_then(Value::as_bool);
                repeated.children.push(item);
                (path, repeated)
            }
            Some("json_range") => {
                let path = format!("filter.{name}");
                let mut range = field(path.clone(), "object".into(), sent, false);
                (range.label, range.description) = text(filter);
                range.visible = filter.get("visible").and_then(Value::as_bool);
                for bound in ["max", "min"] {
                    range.children.push(field(
                        format!("{path}.{bound}"),
                        type_name.into(),
                        required && bound == "min",
                        false,
                    ));
                }
                (path, range)
            }
            Some("json_boolean") => {
                let path = format!("filter.{name}");
                let mut empty = field(path.clone(), "boolean".into(), false, false);
                (empty.label, empty.description) = text(filter);
                empty.visible = filter.get("visible").and_then(Value::as_bool);
                (path, empty)
            }
            _ => continue,
        };
        insert(
            &mut tree,
            declared,
            &path.split('.').collect::<Vec<_>>(),
            "",
        );
        if let Some(group) = tree.iter_mut().find(|item| item.path == "filter") {
            // The filter group is sent when one of its filters is.
            sent_any |= sent;
            group.required = sent_any;
        }
    }
    // A server search is one optional string.
    if contract.get("search").is_some_and(Value::is_object) {
        tree.push(field("search".into(), "string".into(), false, false));
    }
    tree.sort();
    tree.dedup();
    tree
}

/// One field descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FieldIr {
    /// Dotted path within its carrier, e.g. `value.purchase_order_id`.
    pub path: String,
    /// Contract type name, e.g. `uuid`, `text`, `timestamptz`, `int64`.
    pub type_name: String,
    /// Whether the contract admits null.
    pub nullable: bool,
    /// Whether the property must be present, independently of its null value.
    pub required: bool,
    /// Whether this signed integer is a revision serialized as a decimal JSON
    /// string, and the input path whose record it guards when the contract
    /// names one.
    #[serde(default, skip_serializing_if = "crate::manifest::Revision::is_absent")]
    pub revision: crate::manifest::Revision,
    /// Object members or repeated item members, ordered by path.
    pub children: Vec<FieldIr>,
    /// Declared repeated-item bounds.
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
    /// Closed value domain, when the contract declares one. Empty means open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// Authored text, or the exact field path when no label is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Authored selection for generated table and detail fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Authored text for an author, which reaches a comment and no screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The record this input names, when it names one.
    ///
    /// An authored operation declares it. A generated action derives it from
    /// the column's own foreign key, so both arrive here the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<ReferenceIr>,
    /// The model column this input writes, when a generated write states one
    /// (`writable_fields[].field`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}

/// The model whose record one input names.
///
/// Read straight from the contract file, so its members keep the spelling
/// `generate/operations.rs` writes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceIr {
    /// Model whose record the value names.
    pub model: String,
    /// Input path of the same operation that narrows the list, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrowed_by: Option<String>,
}

/// Leaf descriptors for flat display controls and typed schema hints.
pub fn leaf_fields(fields: &[FieldIr]) -> Vec<&FieldIr> {
    fields
        .iter()
        .flat_map(|field| {
            if field.children.is_empty() {
                vec![field]
            } else {
                leaf_fields(&field.children)
            }
        })
        .collect()
}

/// One typed error case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ErrorCaseIr {
    /// The wire literal a client branches on.
    pub literal: String,
    /// Declared origins of this outcome, including ambiguous infrastructure failures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Detail members always present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_required: Vec<String>,
    /// Detail members that may be present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_optional: Vec<String>,
    /// Authored text a screen shows in place of the code's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

pub fn errors_of(errors: Option<&Value>) -> Vec<ErrorCaseIr> {
    let mut cases: Vec<ErrorCaseIr> = errors
        .and_then(|errors| errors.get("cases"))
        .and_then(Value::as_array)
        .map_or_default(|cases| {
            cases
                .iter()
                .filter_map(|case| {
                    let detail = case.get("detail");
                    Some(ErrorCaseIr {
                        literal: case.get("literal")?.as_str()?.to_owned(),
                        sources: case.get("from").and_then(Value::as_str).map_or_else(
                            || string_list(case.get("from")),
                            |source| vec![source.to_owned()],
                        ),
                        detail_required: string_list(detail.and_then(|d| d.get("required"))),
                        detail_optional: string_list(detail.and_then(|d| d.get("optional"))),
                        text: case.get("text").and_then(Value::as_str).map(str::to_owned),
                    })
                })
                .collect()
        });
    cases.sort_by(|left, right| left.literal.cmp(&right.literal));
    // One literal can name several constraints, and only some of them name a
    // field. A key every case requires stays required, and a key that some
    // case states is optional, so the order of the cases changes nothing.
    cases.dedup_by(|case, kept| {
        if case.literal != kept.literal {
            return false;
        }
        let stated = [
            &kept.detail_required,
            &kept.detail_optional,
            &case.detail_required,
            &case.detail_optional,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
        kept.detail_required
            .retain(|key| case.detail_required.contains(key));
        kept.detail_optional = stated
            .into_iter()
            .filter(|key| !kept.detail_required.contains(key))
            .collect();
        true
    });
    cases
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    let mut list: Vec<String> = value.and_then(Value::as_array).map_or_default(|values| {
        values
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_owned))
            .collect()
    });
    list.sort();
    list.dedup();
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omission_does_not_permit_a_null_writable_value() {
        let fields = input_fields_of(&json!({"writable_fields":[{
            "field":"maker_id","type":"uuid","omitted":"unchanged","explicit_null":"invalid_input"
        }]}));
        assert!(!fields[0].required);
        assert!(!fields[0].nullable);
    }
}
