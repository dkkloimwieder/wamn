//! Structural validation of a [`Catalog`].
//!
//! Checks catalog *well-formedness* — unique ids, referential integrity of
//! references/relations, type well-formedness (exact-decimal numerics, non-empty
//! enums), index/constraint field resolution, and the system-entity extension
//! rule. It does NOT emit DDL, plan migrations, or evaluate check expressions —
//! those belong to the DDL compiler (3.2).

use std::collections::{HashMap, HashSet};

use crate::types::{Cardinality, Catalog, Constraint, FieldType, SCHEMA_VERSION};

/// Severity of a validation [`Issue`]. Only [`Severity::Error`] makes a catalog
/// invalid; warnings surface designer-fixable smells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A single validation finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    /// Stable machine code, e.g. `duplicate-entity-id`.
    pub code: &'static str,
    /// JSON-ish path to the offending element, e.g. `entities[2].fields[0].type`.
    pub path: String,
    pub message: String,
}

impl Issue {
    fn error(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    fn warning(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{sev} [{}] {}: {}", self.code, self.path, self.message)
    }
}

/// The reserved identifier prefix. The entire `wamn_` family is platform-owned —
/// migration asides (`wamn_mig_drop_*`), the CDC entity map (`wamn_entities`),
/// and run-schema objects all live under it — so a
/// user-authored name must never collide. Rejecting it at *design* time makes the
/// rule clear up front and demotes wamn-ddl's `TempNameCollision` (which only
/// fires at migration-compile time, and only on a colliding aside-rename) to
/// defense-in-depth. (SR6 house rule; external review R9a.)
const RESERVED_PREFIX: &str = "wamn_";

/// Whether `name` begins with the reserved `wamn_` prefix, case-insensitively —
/// the whole family is reserved, and future machinery could vary the case.
/// Panic-safe on short or multibyte names: `get(..len)` yields `None` (not
/// reserved) when byte `len` is past the end or not a char boundary.
fn is_reserved(name: &str) -> bool {
    name.get(..RESERVED_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(RESERVED_PREFIX))
}

/// Push a `reserved-name-prefix` error if `name` (a SQL-emitted identifier —
/// entity/field/index/constraint `.name`) uses the reserved prefix. Safe to call
/// unconditionally: an empty name is not reserved, so it never double-reports
/// alongside the empty-name check.
fn check_reserved(issues: &mut Vec<Issue>, path: String, kind: &str, name: &str) {
    if is_reserved(name) {
        issues.push(Issue::error(
            "reserved-name-prefix",
            path,
            format!(
                "{kind} name {name:?} begins with the reserved {RESERVED_PREFIX:?} prefix \
                 (reserved for platform-generated identifiers)"
            ),
        ));
    }
}

/// Why an author-supplied SQL **expression fragment** is unsafe to splice into
/// DDL, or `None` if it is safe.
///
/// A `Check` constraint expression (3.2 `emit.rs`) and an RLS `RolePredicate`
/// expression (3.5 `wamn-rls`) are spliced **verbatim** into
/// `ADD CONSTRAINT … CHECK (<expr>)` / `… OR (<expr>)` and applied through
/// `batch_execute` — the simple protocol, which honours multiple `;`-separated
/// statements. So an author-controlled fragment like
/// `1=1); DROP TABLE app_system.users; --` closes the wrapping paren early and
/// chains arbitrary statements at migration-role privilege (review C1-1, AR1).
///
/// A single boolean expression never legitimately contains a top-level statement
/// terminator, unbalanced parentheses, or a comment-open — those are exactly the
/// chaining vectors — so we reject them at *validate* time, before emission. The
/// scan is literal-aware: `'…'` string and `"…"` quoted-identifier literals
/// (with `''`/`""` doubling) are skipped, so a `;` inside a literal
/// (`note <> 'a;b'`) stays legal. Dollar-quoting and backslashes outside a
/// literal are rejected outright (both could smuggle a chaining payload past the
/// scanner; neither appears in a legitimate boolean expression). The author still
/// owns the expression's *logic* — this only forbids statement **chaining**.
pub fn unsafe_expression_reason(expr: &str) -> Option<&'static str> {
    let bytes = expr.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"') => {
                // Skip a string / quoted-identifier literal; the quote char is
                // doubled ('' or "") to embed itself.
                i += 1;
                loop {
                    match bytes.get(i) {
                        None => return Some("unterminated string or quoted-identifier literal"),
                        Some(&c) if c == quote => {
                            if bytes.get(i + 1) == Some(&quote) {
                                i += 2; // an embedded doubled quote
                                continue;
                            }
                            break; // closing quote
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            b';' => return Some("contains a statement terminator ';'"),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return Some("has unbalanced parentheses (a ')' with no matching '(')");
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                return Some("contains a line-comment marker '--'");
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                return Some("contains a block-comment marker '/*'");
            }
            b'$' => return Some("contains '$' (dollar-quoting is not permitted)"),
            b'\\' => return Some("contains a backslash outside a literal"),
            _ => {}
        }
        i += 1;
    }
    if depth != 0 {
        return Some("has unbalanced parentheses (an unclosed '(')");
    }
    None
}

/// Push an `unsafe-<kind>-expression` error if the author-supplied SQL
/// `expression` could chain statements when spliced into DDL. Only checks
/// non-empty expressions — an empty expression is reported separately.
fn check_expression(issues: &mut Vec<Issue>, code: &'static str, path: String, expression: &str) {
    if let Some(reason) = unsafe_expression_reason(expression) {
        issues.push(Issue::error(
            code,
            path,
            format!("expression is not a safe boolean expression: it {reason}"),
        ));
    }
}

/// Every issue (errors and warnings) for a catalog, in a stable order.
pub fn validate(catalog: &Catalog) -> Vec<Issue> {
    let mut issues = Vec::new();

    // --- schema-format version ----------------------------------------------
    match compatible(&catalog.schema_version) {
        Compat::Ok => {}
        Compat::Unparsable => issues.push(Issue::error(
            "bad-schema-version",
            "schema_version",
            format!("{:?} is not a MAJOR.MINOR version", catalog.schema_version),
        )),
        Compat::Unsupported => issues.push(Issue::error(
            "unsupported-schema-version",
            "schema_version",
            format!(
                "{:?} is newer than this implementation ({SCHEMA_VERSION})",
                catalog.schema_version
            ),
        )),
    }

    // --- identity -----------------------------------------------------------
    if catalog.catalog_id.trim().is_empty() {
        issues.push(Issue::error(
            "empty-catalog-id",
            "catalog_id",
            "catalog_id is required",
        ));
    }
    if catalog.version < 1 {
        issues.push(Issue::error(
            "bad-version",
            "version",
            "version must be >= 1",
        ));
    }

    // --- entities: unique ids + names ---------------------------------------
    let mut entity_ids: HashSet<&str> = HashSet::new();
    let mut entity_names: HashSet<&str> = HashSet::new();
    for (i, e) in catalog.entities.iter().enumerate() {
        if e.id.trim().is_empty() {
            issues.push(Issue::error(
                "empty-entity-id",
                format!("entities[{i}].id"),
                "entity id is required",
            ));
        } else if !entity_ids.insert(e.id.as_str()) {
            issues.push(Issue::error(
                "duplicate-entity-id",
                format!("entities[{i}].id"),
                format!("entity id {:?} is not unique", e.id),
            ));
        }
        if e.name.trim().is_empty() {
            issues.push(Issue::error(
                "empty-entity-name",
                format!("entities[{i}].name"),
                "entity name is required",
            ));
        } else if !entity_names.insert(e.name.as_str()) {
            issues.push(Issue::error(
                "duplicate-entity-name",
                format!("entities[{i}].name"),
                format!("entity name {:?} is not unique", e.name),
            ));
        }
        check_reserved(
            &mut issues,
            format!("entities[{i}].name"),
            "entity",
            &e.name,
        );
    }

    // Per-entity structure (fields, types, references, indexes, constraints).
    for (i, e) in catalog.entities.iter().enumerate() {
        let ep = format!("entities[{i}]");
        let mut field_ids: HashSet<&str> = HashSet::new();
        let mut field_names: HashSet<&str> = HashSet::new();

        for (j, f) in e.fields.iter().enumerate() {
            let fp = format!("{ep}.fields[{j}]");
            if f.id.trim().is_empty() {
                issues.push(Issue::error(
                    "empty-field-id",
                    format!("{fp}.id"),
                    "field id is required",
                ));
            } else if !field_ids.insert(f.id.as_str()) {
                issues.push(Issue::error(
                    "duplicate-field-id",
                    format!("{fp}.id"),
                    format!("field id {:?} is not unique in entity {:?}", f.id, e.id),
                ));
            }
            if f.name.trim().is_empty() {
                issues.push(Issue::error(
                    "empty-field-name",
                    format!("{fp}.name"),
                    "field name is required",
                ));
            } else if !field_names.insert(f.name.as_str()) {
                issues.push(Issue::error(
                    "duplicate-field-name",
                    format!("{fp}.name"),
                    format!("field name {:?} is not unique in entity {:?}", f.name, e.id),
                ));
            }
            check_reserved(&mut issues, format!("{fp}.name"), "field", &f.name);

            // System-entity extension rule: a system field requires a system
            // entity. (Custom fields on a system entity are allowed — that is
            // the whole point of extensibility.)
            if f.is_system && !e.is_system {
                issues.push(Issue::error(
                    "system-field-on-user-entity",
                    format!("{fp}.is_system"),
                    format!(
                        "field {:?} is marked system but entity {:?} is not a system entity",
                        f.id, e.id
                    ),
                ));
            }

            validate_field_type(
                &mut issues,
                &format!("{fp}.type"),
                &f.field_type,
                &f.id,
                &entity_ids,
            );
        }
        if e.fields.is_empty() {
            issues.push(Issue::warning(
                "entity-has-no-fields",
                format!("{ep}.fields"),
                format!("entity {:?} has no fields", e.id),
            ));
        }

        // Indexes: unique names, non-empty, fields resolve.
        let mut index_names: HashSet<&str> = HashSet::new();
        for (j, idx) in e.indexes.iter().enumerate() {
            let ip = format!("{ep}.indexes[{j}]");
            if !index_names.insert(idx.name.as_str()) {
                issues.push(Issue::error(
                    "duplicate-index-name",
                    format!("{ip}.name"),
                    format!(
                        "index name {:?} is not unique in entity {:?}",
                        idx.name, e.id
                    ),
                ));
            }
            check_reserved(&mut issues, format!("{ip}.name"), "index", &idx.name);
            if idx.fields.is_empty() {
                issues.push(Issue::error(
                    "empty-index",
                    format!("{ip}.fields"),
                    format!("index {:?} covers no fields", idx.name),
                ));
            }
            for (k, fid) in idx.fields.iter().enumerate() {
                if !field_ids.contains(fid.as_str()) {
                    issues.push(Issue::error(
                        "unknown-index-field",
                        format!("{ip}.fields[{k}]"),
                        format!("index {:?} references unknown field {:?}", idx.name, fid),
                    ));
                }
            }
        }

        // Constraints: unique names, fields resolve, non-empty.
        let mut constraint_names: HashSet<&str> = HashSet::new();
        for (j, c) in e.constraints.iter().enumerate() {
            let cp = format!("{ep}.constraints[{j}]");
            if !constraint_names.insert(c.name()) {
                issues.push(Issue::error(
                    "duplicate-constraint-name",
                    format!("{cp}.name"),
                    format!(
                        "constraint name {:?} is not unique in entity {:?}",
                        c.name(),
                        e.id
                    ),
                ));
            }
            check_reserved(&mut issues, format!("{cp}.name"), "constraint", c.name());
            match c {
                Constraint::Unique { fields, .. } => {
                    if fields.is_empty() {
                        issues.push(Issue::error(
                            "empty-unique-constraint",
                            format!("{cp}.fields"),
                            format!("unique constraint {:?} covers no fields", c.name()),
                        ));
                    }
                    for (k, fid) in fields.iter().enumerate() {
                        if !field_ids.contains(fid.as_str()) {
                            issues.push(Issue::error(
                                "unknown-constraint-field",
                                format!("{cp}.fields[{k}]"),
                                format!(
                                    "constraint {:?} references unknown field {:?}",
                                    c.name(),
                                    fid
                                ),
                            ));
                        }
                    }
                }
                Constraint::Check { expression, .. } => {
                    if expression.trim().is_empty() {
                        issues.push(Issue::error(
                            "empty-check-expression",
                            format!("{cp}.expression"),
                            format!("check constraint {:?} has an empty expression", c.name()),
                        ));
                    } else {
                        check_expression(
                            &mut issues,
                            "unsafe-check-expression",
                            format!("{cp}.expression"),
                            expression,
                        );
                    }
                }
            }
        }
    }

    // --- relations: unique ids, endpoints resolve, shape rules --------------
    let field_index = entity_field_ids(catalog);
    let mut relation_ids: HashSet<&str> = HashSet::new();
    for (i, r) in catalog.relations.iter().enumerate() {
        let rp = format!("relations[{i}]");
        if r.id.trim().is_empty() {
            issues.push(Issue::error(
                "empty-relation-id",
                format!("{rp}.id"),
                "relation id is required",
            ));
        } else if !relation_ids.insert(r.id.as_str()) {
            issues.push(Issue::error(
                "duplicate-relation-id",
                format!("{rp}.id"),
                format!("relation id {:?} is not unique", r.id),
            ));
        }
        if !entity_ids.contains(r.from.as_str()) {
            issues.push(Issue::error(
                "unknown-relation-from",
                format!("{rp}.from"),
                format!("relation source {:?} is not an entity id", r.from),
            ));
        }
        if !entity_ids.contains(r.to.as_str()) {
            issues.push(Issue::error(
                "unknown-relation-to",
                format!("{rp}.to"),
                format!("relation target {:?} is not an entity id", r.to),
            ));
        }
        if let Some(ff) = &r.from_field
            && let Some(fields) = field_index.get(r.from.as_str())
            && !fields.contains(ff.as_str())
        {
            issues.push(Issue::error(
                "unknown-relation-field",
                format!("{rp}.from_field"),
                format!("from_field {:?} is not a field of entity {:?}", ff, r.from),
            ));
        }
        match r.cardinality {
            Cardinality::Hierarchical if r.from != r.to => issues.push(Issue::error(
                "hierarchical-not-self-referential",
                rp.clone(),
                format!(
                    "hierarchical relation {:?} must be self-referential (from == to)",
                    r.id
                ),
            )),
            Cardinality::ManyToMany => match &r.through {
                Some(t) if !entity_ids.contains(t.as_str()) => issues.push(Issue::error(
                    "unknown-relation-through",
                    format!("{rp}.through"),
                    format!("join entity {:?} is not an entity id", t),
                )),
                None => issues.push(Issue::warning(
                    "many-to-many-without-through",
                    format!("{rp}.through"),
                    format!("many-to-many relation {:?} declares no join entity", r.id),
                )),
                _ => {}
            },
            _ => {}
        }
    }

    issues
}

/// Type-specific well-formedness: exact-decimal numerics, non-empty/unique
/// enums, resolvable reference targets, sane text caps.
fn validate_field_type(
    issues: &mut Vec<Issue>,
    path: &str,
    ty: &FieldType,
    field_id: &str,
    entity_ids: &HashSet<&str>,
) {
    match ty {
        FieldType::Numeric {
            precision, scale, ..
        } => {
            if *precision < 1 {
                issues.push(Issue::error(
                    "bad-numeric-precision",
                    path,
                    format!("field {field_id:?} numeric precision must be >= 1"),
                ));
            }
            if scale > precision {
                issues.push(Issue::error(
                    "bad-numeric-scale",
                    path,
                    format!(
                        "field {field_id:?} numeric scale ({scale}) exceeds precision ({precision})"
                    ),
                ));
            }
        }
        FieldType::Enum { variants } => {
            if variants.is_empty() {
                issues.push(Issue::error(
                    "empty-enum",
                    path,
                    format!("field {field_id:?} enum has no variants"),
                ));
            }
            let mut seen: HashSet<&str> = HashSet::new();
            for v in variants {
                if v.trim().is_empty() {
                    issues.push(Issue::error(
                        "empty-enum-variant",
                        path,
                        format!("field {field_id:?} enum has an empty variant"),
                    ));
                } else if !seen.insert(v.as_str()) {
                    issues.push(Issue::error(
                        "duplicate-enum-variant",
                        path,
                        format!("field {field_id:?} enum variant {v:?} is duplicated"),
                    ));
                }
            }
        }
        FieldType::Text { max_len: Some(0) } => issues.push(Issue::error(
            "bad-text-length",
            path,
            format!("field {field_id:?} text max_len must be >= 1"),
        )),
        FieldType::Reference { entity } if !entity_ids.contains(entity.as_str()) => {
            issues.push(Issue::error(
                "unknown-reference-target",
                path,
                format!("field {field_id:?} references unknown entity {entity:?}"),
            ));
        }
        _ => {}
    }
}

/// Map of entity id -> set of its field ids, for relation `from_field` checks.
fn entity_field_ids(catalog: &Catalog) -> HashMap<&str, HashSet<&str>> {
    catalog
        .entities
        .iter()
        .map(|e| {
            (
                e.id.as_str(),
                e.fields.iter().map(|f| f.id.as_str()).collect(),
            )
        })
        .collect()
}

enum Compat {
    Ok,
    Unparsable,
    Unsupported,
}

/// A catalog's `schema_version` is compatible if its MAJOR matches and its MINOR
/// is not newer than what this crate implements (additive-within-major, per the
/// `0.1.x` freeze rule — same policy as the WIT and flow-schema).
fn compatible(v: &str) -> Compat {
    let parse = |s: &str| -> Option<(u32, u32)> {
        let (maj, min) = s.split_once('.')?;
        Some((maj.parse().ok()?, min.parse().ok()?))
    };
    let (Some((maj, min)), Some((smaj, smin))) = (parse(v), parse(SCHEMA_VERSION)) else {
        return Compat::Unparsable;
    };
    if maj != smaj || min > smin {
        Compat::Unsupported
    } else {
        Compat::Ok
    }
}

impl Catalog {
    /// All validation issues (errors and warnings).
    pub fn issues(&self) -> Vec<Issue> {
        validate(self)
    }

    /// `true` if the catalog has no error-severity issues (warnings are allowed).
    pub fn is_valid(&self) -> bool {
        !validate(self).iter().any(|i| i.severity == Severity::Error)
    }

    /// `Ok` if valid, else the error-severity issues.
    pub fn validate(&self) -> Result<(), Vec<Issue>> {
        let errors: Vec<Issue> = validate(self)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{
        Cardinality, Catalog, Constraint, Entity, Field, FieldType, Index, Relation,
    };

    fn field(id: &str, ty: FieldType) -> Field {
        Field {
            id: id.into(),
            name: id.into(),
            field_type: ty,
            nullable: false,
            default: None,
            sensitive: false,
            is_system: false,
            label: None,
            description: None,
        }
    }

    fn entity(id: &str, fields: Vec<Field>) -> Entity {
        Entity {
            id: id.into(),
            name: id.into(),
            is_system: false,
            label: None,
            description: None,
            fields,
            indexes: vec![],
            constraints: vec![],
        }
    }

    /// A minimal valid catalog: one entity with one field.
    fn minimal() -> Catalog {
        Catalog {
            schema_version: "0.1".into(),
            catalog_id: "c".into(),
            version: 1,
            name: None,
            entities: vec![entity(
                "thing",
                vec![field("label", FieldType::Text { max_len: None })],
            )],
            relations: vec![],
        }
    }

    fn codes(c: &Catalog) -> Vec<&'static str> {
        c.issues().into_iter().map(|i| i.code).collect()
    }

    #[test]
    fn minimal_catalog_is_valid() {
        let c = minimal();
        assert!(c.is_valid(), "issues: {:?}", c.issues());
        assert!(c.validate().is_ok());
        assert!(c.issues().is_empty());
    }

    #[test]
    fn duplicate_entity_id_is_error() {
        let mut c = minimal();
        c.entities
            .push(entity("thing", vec![field("x", FieldType::Int)]));
        // give the dup a distinct name so only the id collision fires
        c.entities[1].name = "thing2".into();
        assert!(codes(&c).contains(&"duplicate-entity-id"));
        assert!(!c.is_valid());
    }

    #[test]
    fn duplicate_field_id_is_error() {
        let mut c = minimal();
        c.entities[0].fields.push(field("label", FieldType::Int));
        assert!(codes(&c).contains(&"duplicate-field-id"));
    }

    #[test]
    fn numeric_scale_over_precision_is_error() {
        let mut c = minimal();
        c.entities[0].fields.push(field(
            "qty",
            FieldType::Numeric {
                precision: 4,
                scale: 6,
                unit: Some("kg".into()),
            },
        ));
        assert!(codes(&c).contains(&"bad-numeric-scale"));
    }

    #[test]
    fn empty_enum_is_error() {
        let mut c = minimal();
        c.entities[0]
            .fields
            .push(field("status", FieldType::Enum { variants: vec![] }));
        assert!(codes(&c).contains(&"empty-enum"));
    }

    #[test]
    fn unknown_reference_target_is_error() {
        let mut c = minimal();
        c.entities[0].fields.push(field(
            "owner",
            FieldType::Reference {
                entity: "ghost".into(),
            },
        ));
        assert!(codes(&c).contains(&"unknown-reference-target"));
    }

    #[test]
    fn system_field_on_user_entity_is_error() {
        let mut c = minimal();
        c.entities[0].fields[0].is_system = true; // entity is not is_system
        assert!(codes(&c).contains(&"system-field-on-user-entity"));
    }

    #[test]
    fn custom_field_on_system_entity_is_allowed() {
        // The POC's users.cert_level hard path: extend a system entity with a
        // non-system field. Must NOT error.
        let mut c = minimal();
        c.entities[0].is_system = true;
        c.entities[0].fields[0].is_system = true; // the system field
        c.entities[0].fields.push(field(
            "cert_level",
            FieldType::Enum {
                variants: vec!["L1".into(), "L2".into()],
            },
        )); // a custom (non-system) field
        assert!(c.is_valid(), "issues: {:?}", c.issues());
    }

    #[test]
    fn unknown_index_field_is_error() {
        let mut c = minimal();
        c.entities[0].indexes.push(Index {
            name: "by_ghost".into(),
            fields: vec!["ghost".into()],
            unique: false,
        });
        assert!(codes(&c).contains(&"unknown-index-field"));
    }

    #[test]
    fn composite_unique_over_unknown_field_is_error() {
        let mut c = minimal();
        c.entities[0].constraints.push(Constraint::Unique {
            name: "u".into(),
            fields: vec!["label".into(), "ghost".into()],
        });
        assert!(codes(&c).contains(&"unknown-constraint-field"));
    }

    #[test]
    fn hierarchical_relation_must_be_self_referential() {
        let mut c = minimal();
        c.entities
            .push(entity("other", vec![field("x", FieldType::Int)]));
        c.relations.push(Relation {
            id: "tree".into(),
            name: "tree".into(),
            cardinality: Cardinality::Hierarchical,
            from: "thing".into(),
            to: "other".into(),
            from_field: None,
            through: None,
            description: None,
        });
        assert!(codes(&c).contains(&"hierarchical-not-self-referential"));
    }

    #[test]
    fn unknown_relation_endpoint_is_error() {
        let mut c = minimal();
        c.relations.push(Relation {
            id: "r".into(),
            name: "r".into(),
            cardinality: Cardinality::OneToMany,
            from: "ghost".into(),
            to: "thing".into(),
            from_field: None,
            through: None,
            description: None,
        });
        assert!(codes(&c).contains(&"unknown-relation-from"));
    }

    #[test]
    fn entity_with_no_fields_is_warning_not_error() {
        let mut c = minimal();
        c.entities.push(entity("empty", vec![]));
        assert!(c.is_valid(), "no-fields should warn, not error");
        assert!(codes(&c).contains(&"entity-has-no-fields"));
    }

    #[test]
    fn future_major_schema_version_is_unsupported() {
        let mut c = minimal();
        c.schema_version = "1.0".into();
        assert!(codes(&c).contains(&"unsupported-schema-version"));
    }

    #[test]
    fn reserved_wamn_prefix_is_rejected_on_every_named_object() {
        // entity name
        let mut c = minimal();
        c.entities[0].name = "wamn_orders".into();
        let reserved: Vec<_> = c
            .issues()
            .into_iter()
            .filter(|i| i.code == "reserved-name-prefix")
            .collect();
        assert_eq!(reserved.len(), 1, "issues: {:?}", c.issues());
        assert_eq!(reserved[0].path, "entities[0].name");
        assert!(reserved[0].message.contains("entity name"));
        assert!(!c.is_valid());

        // field name (id is NOT checked — only the SQL-emitted .name)
        let mut c = minimal();
        c.entities[0].fields.push(field("wamn_col", FieldType::Int));
        let issue = c
            .issues()
            .into_iter()
            .find(|i| i.code == "reserved-name-prefix")
            .expect("reserved field name should be rejected");
        assert_eq!(issue.path, "entities[0].fields[1].name");
        assert!(issue.message.contains("field name"));

        // index name
        let mut c = minimal();
        c.entities[0].indexes.push(Index {
            name: "wamn_idx".into(),
            fields: vec!["label".into()],
            unique: false,
        });
        let issue = c
            .issues()
            .into_iter()
            .find(|i| i.code == "reserved-name-prefix")
            .expect("reserved index name should be rejected");
        assert_eq!(issue.path, "entities[0].indexes[0].name");
        assert!(issue.message.contains("index name"));

        // constraint name
        let mut c = minimal();
        c.entities[0].constraints.push(Constraint::Unique {
            name: "wamn_uq".into(),
            fields: vec!["label".into()],
        });
        let issue = c
            .issues()
            .into_iter()
            .find(|i| i.code == "reserved-name-prefix")
            .expect("reserved constraint name should be rejected");
        assert_eq!(issue.path, "entities[0].constraints[0].name");
        assert!(issue.message.contains("constraint name"));
    }

    #[test]
    fn reserved_prefix_is_case_insensitive() {
        let mut c = minimal();
        c.entities[0].name = "WAMN_Orders".into();
        assert!(codes(&c).contains(&"reserved-name-prefix"));
        assert!(!c.is_valid());

        let mut c = minimal();
        c.entities[0].name = "Wamn_x".into();
        assert!(codes(&c).contains(&"reserved-name-prefix"));
    }

    #[test]
    fn non_reserved_names_are_fine() {
        // merely CONTAINS "wamn" mid-string — not a leading prefix.
        let mut c = minimal();
        c.entities[0].name = "my_wamn_table".into();
        assert!(c.is_valid(), "issues: {:?}", c.issues());
        assert!(!codes(&c).contains(&"reserved-name-prefix"));

        // "wamn" with no trailing underscore — not the reserved prefix.
        let mut c = minimal();
        c.entities[0].name = "wamn".into();
        assert!(c.is_valid(), "issues: {:?}", c.issues());
        assert!(!codes(&c).contains(&"reserved-name-prefix"));

        // "wamning" — 5th byte is 'i', not '_'.
        let mut c = minimal();
        c.entities[0].name = "wamning".into();
        assert!(c.is_valid());
        assert!(!codes(&c).contains(&"reserved-name-prefix"));
    }

    // --- expression-chaining guard (cjv.5 / review C1-1) -------------------

    fn with_check(name: &str, expression: &str) -> Catalog {
        let mut c = minimal();
        c.entities[0].constraints.push(Constraint::Check {
            name: name.into(),
            expression: expression.into(),
        });
        c
    }

    #[test]
    fn safe_boolean_expressions_pass_the_chaining_guard() {
        for expr in [
            "moisture_max_pct <= 100",
            "status IN ('a','b')",
            "site_id = NULLIF(current_setting('app.site',true),'')::uuid",
            "((a OR b) AND c)",
            "note <> 'a;b'",       // a ';' inside a string literal stays legal
            "\"weird col\" > 0",   // a quoted identifier
            "qty - reserved >= 0", // a lone '-' is subtraction, not a comment
            "total / 2 > 1",       // a lone '/' is division, not a block comment
            "label <> 'it''s ok'", // a doubled-quote escape inside a literal
        ] {
            assert_eq!(
                super::unsafe_expression_reason(expr),
                None,
                "expected safe: {expr:?}"
            );
        }
    }

    #[test]
    fn statement_chaining_expressions_are_rejected() {
        for expr in [
            "1=1); DROP TABLE app_system.users; --", // the C1-1 exploit
            "x)); DELETE FROM y; SELECT (1",
            "a; b",              // a bare statement terminator
            "a = 1 -- trailing", // a line comment
            "a = 1 /* block */", // a block comment
            "a) OR (1=1",        // transiently negative but net-balanced (paren escape)
            "a)",                // unbalanced: a ')' with no '('
            "(a",                // unbalanced: an unclosed '('
            "a = $$x$$",         // dollar-quoting
            "a = 'x' \\g",       // a backslash outside a literal
        ] {
            assert!(
                super::unsafe_expression_reason(expr).is_some(),
                "expected unsafe: {expr:?}"
            );
        }
    }

    #[test]
    fn check_constraint_rejects_a_chaining_expression() {
        let c = with_check("ck", "1=1); DROP TABLE app_system.users; --");
        assert!(codes(&c).contains(&"unsafe-check-expression"));
        assert!(!c.is_valid());
    }

    #[test]
    fn check_constraint_accepts_a_safe_expression() {
        let c = with_check("ck", "label <> 'a;b'");
        assert!(c.is_valid(), "issues: {:?}", c.issues());
        assert!(!codes(&c).contains(&"unsafe-check-expression"));
    }

    #[test]
    fn empty_check_expression_reports_empty_not_unsafe() {
        let c = with_check("ck", "   ");
        let found = codes(&c);
        assert!(found.contains(&"empty-check-expression"));
        assert!(!found.contains(&"unsafe-check-expression"));
    }
}
