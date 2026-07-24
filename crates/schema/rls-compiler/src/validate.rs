//! Structural validation of an [`AccessPolicy`] against the catalog it targets.
//!
//! Checks that rules resolve — the entity exists, an ownership field exists and
//! is uuid-typed, roles and expressions are non-empty — reusing the catalog's
//! [`Issue`] / [`Severity`] shape (3.1). It does **not** parse or evaluate a
//! custom predicate's SQL: a [`Rule::RolePredicate`] expression's *logic* is the
//! author's responsibility (the escape-hatch intent). It does, however, reject an
//! expression that could **chain statements** when spliced into DDL
//! (`wamn_catalog::unsafe_expression_reason` — a top-level `;`, unbalanced
//! parens, a comment-open; review C1-1) — the author owns the predicate's logic,
//! not the right to append statements.

use std::collections::HashSet;

use wamn_catalog::{Catalog, FieldType, Issue, Severity, unsafe_expression_reason};

use crate::model::{AccessPolicy, CommandGrant, Rule, SCHEMA_VERSION};

fn error(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue {
        severity: Severity::Error,
        code,
        path: path.into(),
        message: message.into(),
    }
}

/// Validate `policy` against `catalog`. Returns every [`Severity::Error`] found
/// (empty `Ok(())` when the policy is well-formed).
pub fn validate(policy: &AccessPolicy, catalog: &Catalog) -> Result<(), Vec<Issue>> {
    let mut issues = Vec::new();

    if !schema_version_compatible(&policy.schema_version) {
        issues.push(error(
            "unsupported-schema-version",
            "schema-version",
            format!(
                "policy schema-version {:?} is not compatible with {SCHEMA_VERSION}.x",
                policy.schema_version
            ),
        ));
    }
    if policy.catalog_id != catalog.catalog_id {
        issues.push(error(
            "catalog-id-mismatch",
            "catalog-id",
            format!(
                "policy targets catalog {:?} but was validated against {:?}",
                policy.catalog_id, catalog.catalog_id
            ),
        ));
    }

    let mut explicit_names = HashSet::new();
    for (i, rule) in policy.rules.iter().enumerate() {
        let path = format!("rules[{i}]");

        if let Some(name) = rule.explicit_name()
            && !explicit_names.insert(name)
        {
            issues.push(error(
                "duplicate-policy-name",
                format!("{path}.name"),
                format!("policy name {name:?} is used by more than one rule"),
            ));
        }

        let Some(entity) = catalog.entities.iter().find(|e| e.id == rule.entity()) else {
            issues.push(error(
                "unknown-entity",
                format!("{path}.entity"),
                format!("no entity {:?} in the catalog", rule.entity()),
            ));
            continue;
        };

        match rule {
            Rule::RowOwnership {
                owner_field,
                exempt_roles,
                ..
            } => {
                match entity.fields.iter().find(|f| f.id == *owner_field) {
                    None => issues.push(error(
                        "unknown-owner-field",
                        format!("{path}.owner-field"),
                        format!("entity {:?} has no field {owner_field:?}", entity.id),
                    )),
                    Some(f) if !is_uuid_typed(&f.field_type) => issues.push(error(
                        "owner-field-not-uuid",
                        format!("{path}.owner-field"),
                        format!(
                            "ownership field {owner_field:?} must be uuid or a reference (found {:?})",
                            f.field_type
                        ),
                    )),
                    Some(_) => {}
                }
                check_roles(&mut issues, &path, "exempt-roles", exempt_roles);
            }
            Rule::RoleCommands { grants, .. } => {
                if grants.is_empty() {
                    issues.push(error(
                        "empty-grants",
                        format!("{path}.grants"),
                        "role-command rule has no grants",
                    ));
                }
                let mut seen = HashSet::new();
                for (j, CommandGrant { command, roles }) in grants.iter().enumerate() {
                    if !seen.insert(*command) {
                        issues.push(error(
                            "duplicate-command",
                            format!("{path}.grants[{j}].command"),
                            format!("command {:?} is granted more than once", command.as_sql()),
                        ));
                    }
                    if roles.is_empty() {
                        issues.push(error(
                            "empty-grant-roles",
                            format!("{path}.grants[{j}].roles"),
                            format!("grant for {:?} lists no roles", command.as_sql()),
                        ));
                    }
                    check_roles(&mut issues, &format!("{path}.grants[{j}]"), "roles", roles);
                }
            }
            Rule::RolePredicate {
                role, expression, ..
            } => {
                if role.trim().is_empty() {
                    issues.push(error(
                        "empty-role",
                        format!("{path}.role"),
                        "role predicate has an empty role",
                    ));
                }
                if expression.trim().is_empty() {
                    issues.push(error(
                        "empty-expression",
                        format!("{path}.expression"),
                        "role predicate has an empty expression",
                    ));
                } else if let Some(reason) = unsafe_expression_reason(expression) {
                    issues.push(error(
                        "unsafe-expression",
                        format!("{path}.expression"),
                        format!("role predicate expression is not a safe boolean expression: it {reason}"),
                    ));
                }
            }
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn check_roles(issues: &mut Vec<Issue>, path: &str, field: &str, roles: &[String]) {
    for (k, r) in roles.iter().enumerate() {
        if r.trim().is_empty() {
            issues.push(error(
                "empty-role",
                format!("{path}.{field}[{k}]"),
                "role name is empty",
            ));
        }
    }
}

fn is_uuid_typed(ty: &FieldType) -> bool {
    matches!(ty, FieldType::Uuid | FieldType::Reference { .. })
}

/// `0.1.x` is additive/clarifying only; reject a newer major/minor.
fn schema_version_compatible(v: &str) -> bool {
    fn major_minor(s: &str) -> Option<(u32, u32)> {
        let mut it = s.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().unwrap_or("0").parse().ok()?;
        Some((major, minor))
    }
    match (major_minor(v), major_minor(SCHEMA_VERSION)) {
        (Some((vmaj, vmin)), Some((cmaj, cmin))) => vmaj == cmaj && vmin <= cmin,
        _ => false,
    }
}
