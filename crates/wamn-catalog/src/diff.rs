//! Structured version diff between two catalogs.
//!
//! Compares by entity id, field id, relation id, and the table-level index /
//! constraint sets, so a reviewer sees *what changed* structurally. This is the
//! input to the DDL compiler's migration planning (3.2 — added/removed/retyped
//! columns become `ALTER`s) and to schema-impact analysis (11.8 — a staged
//! field rename or retype flags the flow suites and generated types that depend
//! on it before any DDL applies). Field identity is the stable [`FieldId`], so a
//! *rename* surfaces as a change, not a drop + add.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{Catalog, Constraint, Entity, EntityId, Field, FieldId, FieldType, Index};

/// What changed about a single field kept across both versions.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldChange {
    pub id: FieldId,
    /// `Some((old, new))` if the field's logical name changed (a rename).
    pub name_changed: Option<(String, String)>,
    /// `Some((old, new))` if the field's type changed (a retype).
    pub type_changed: Option<(FieldType, FieldType)>,
    /// `true` if nullability changed.
    pub nullable_changed: bool,
    /// `true` if the default changed.
    pub default_changed: bool,
    /// `true` if the sensitivity flag changed.
    pub sensitive_changed: bool,
}

impl FieldChange {
    fn any(&self) -> bool {
        self.name_changed.is_some()
            || self.type_changed.is_some()
            || self.nullable_changed
            || self.default_changed
            || self.sensitive_changed
    }
}

/// What changed about a single entity kept across both versions.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityChange {
    pub id: EntityId,
    pub fields_added: Vec<FieldId>,
    pub fields_removed: Vec<FieldId>,
    pub fields_changed: Vec<FieldChange>,
    /// `true` if the entity's index set changed (by identity).
    pub indexes_changed: bool,
    /// `true` if the entity's constraint set changed (by identity).
    pub constraints_changed: bool,
    /// `true` if entity attributes (name, is_system, label, description) changed.
    pub attrs_changed: bool,
}

impl EntityChange {
    fn any(&self) -> bool {
        !self.fields_added.is_empty()
            || !self.fields_removed.is_empty()
            || !self.fields_changed.is_empty()
            || self.indexes_changed
            || self.constraints_changed
            || self.attrs_changed
    }
}

/// A structured diff from `old` to `new`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogDiff {
    pub entities_added: Vec<EntityId>,
    pub entities_removed: Vec<EntityId>,
    pub entities_changed: Vec<EntityChange>,
    pub relations_added: Vec<String>,
    pub relations_removed: Vec<String>,
    pub relations_changed: Vec<String>,
}

impl CatalogDiff {
    /// `true` if the two catalog versions are structurally identical.
    pub fn is_empty(&self) -> bool {
        self.entities_added.is_empty()
            && self.entities_removed.is_empty()
            && self.entities_changed.is_empty()
            && self.relations_added.is_empty()
            && self.relations_removed.is_empty()
            && self.relations_changed.is_empty()
    }
}

/// Diff `old` → `new`. Entity identity is `id`; field identity is `id`; relation
/// identity is `id`.
pub fn diff(old: &Catalog, new: &Catalog) -> CatalogDiff {
    let old_entities: BTreeMap<&str, &Entity> =
        old.entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let new_entities: BTreeMap<&str, &Entity> =
        new.entities.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut d = CatalogDiff::default();

    for (id, n) in &new_entities {
        match old_entities.get(id) {
            None => d.entities_added.push((*id).to_string()),
            Some(o) => {
                let change = entity_change(id, o, n);
                if change.any() {
                    d.entities_changed.push(change);
                }
            }
        }
    }
    for id in old_entities.keys() {
        if !new_entities.contains_key(id) {
            d.entities_removed.push((*id).to_string());
        }
    }

    let old_rel: BTreeMap<&str, &crate::types::Relation> =
        old.relations.iter().map(|r| (r.id.as_str(), r)).collect();
    let new_rel: BTreeMap<&str, &crate::types::Relation> =
        new.relations.iter().map(|r| (r.id.as_str(), r)).collect();
    for (id, n) in &new_rel {
        match old_rel.get(id) {
            None => d.relations_added.push((*id).to_string()),
            Some(o) if **o != **n => d.relations_changed.push((*id).to_string()),
            _ => {}
        }
    }
    for id in old_rel.keys() {
        if !new_rel.contains_key(id) {
            d.relations_removed.push((*id).to_string());
        }
    }

    d
}

fn entity_change(id: &str, old: &Entity, new: &Entity) -> EntityChange {
    let old_fields: BTreeMap<&str, &Field> =
        old.fields.iter().map(|f| (f.id.as_str(), f)).collect();
    let new_fields: BTreeMap<&str, &Field> =
        new.fields.iter().map(|f| (f.id.as_str(), f)).collect();

    let mut fields_added = Vec::new();
    let mut fields_changed = Vec::new();
    for (fid, n) in &new_fields {
        match old_fields.get(fid) {
            None => fields_added.push((*fid).to_string()),
            Some(o) => {
                let fc = FieldChange {
                    id: (*fid).to_string(),
                    name_changed: (o.name != n.name).then(|| (o.name.clone(), n.name.clone())),
                    type_changed: (o.field_type != n.field_type)
                        .then(|| (o.field_type.clone(), n.field_type.clone())),
                    nullable_changed: o.nullable != n.nullable,
                    default_changed: o.default != n.default,
                    sensitive_changed: o.sensitive != n.sensitive,
                };
                if fc.any() {
                    fields_changed.push(fc);
                }
            }
        }
    }
    let fields_removed: Vec<FieldId> = old_fields
        .keys()
        .filter(|k| !new_fields.contains_key(*k))
        .map(|k| k.to_string())
        .collect();

    EntityChange {
        id: id.to_string(),
        fields_added,
        fields_removed,
        fields_changed,
        indexes_changed: index_set(old) != index_set(new),
        constraints_changed: constraint_set(old) != constraint_set(new),
        attrs_changed: old.name != new.name
            || old.is_system != new.is_system
            || old.label != new.label
            || old.description != new.description,
    }
}

/// Index identity = the full `(name, fields, unique)` shape.
fn index_set(e: &Entity) -> BTreeSet<(&str, &[FieldId], bool)> {
    e.indexes
        .iter()
        .map(|i: &Index| (i.name.as_str(), i.fields.as_slice(), i.unique))
        .collect()
}

/// Constraint identity = the constraint's canonical JSON (covers both variants).
fn constraint_set(e: &Entity) -> BTreeSet<String> {
    e.constraints
        .iter()
        .map(|c: &Constraint| serde_json::to_string(c).expect("constraint serializes"))
        .collect()
}
