//! Compile an [`AccessPolicy`] into Postgres RLS policies.
//!
//! Every rule becomes one `CREATE POLICY … AS RESTRICTIVE`. Because Postgres
//! combines permissive policies with `OR` (which would *widen* access) and
//! restrictive policies with `AND`, restrictive is the only correct choice for
//! layering on the 3.2 tenant floor: the floor keeps tenant isolation, and each
//! rule here narrows *within* the tenant. Output is a [`MigrationPlan`] (reused
//! from 3.2) so callers get the same review / gate surface — all policy creation
//! is additive (it loses no data), though a note flags that a new restriction
//! can deny access until the `app.role` / `app.user_id` claims are injected (4.2).

use wamn_catalog::{Catalog, Issue};
use wamn_ddl::sql::{quote_ident, quote_literal};
use wamn_ddl::{MigrationPlan, Operation, Safety};

use crate::model::{AccessPolicy, Command, Rule};
use crate::validate;

/// Why an access policy could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The policy failed structural validation against the catalog.
    InvalidPolicy(Vec<Issue>),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::InvalidPolicy(issues) => {
                write!(f, "access policy is invalid ({} error(s)): ", issues.len())?;
                for (i, issue) in issues.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{issue}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// The caller's `app.role`, coalesced to `''` so an absent claim compares as a
/// non-match (deny) rather than propagating NULL through the predicate.
fn role_text() -> &'static str {
    "COALESCE(current_setting('app.role', true), '')"
}

/// The caller's `app.user_id` as a uuid; `NULLIF(…, '')` turns an unset/empty
/// claim into NULL (so ownership comparisons yield NULL → deny) and avoids an
/// `''::uuid` cast error.
fn user_uuid() -> &'static str {
    "NULLIF(current_setting('app.user_id', true), '')::uuid"
}

/// `COALESCE(current_setting('app.role', true), '') IN ('r1', 'r2', …)`.
fn role_in(roles: &[String]) -> String {
    let list = roles
        .iter()
        .map(|r| quote_literal(r))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} IN ({list})", role_text())
}

const CLAIM_NOTE: &str = "requires the app.role / app.user_id session claims \
    (injected by the plugin per 4.2); until they are set this restrictive policy \
    denies the gated access";

/// Assemble one `CREATE POLICY … AS RESTRICTIVE` statement (no trailing `;`).
fn policy_stmt(
    name: &str,
    table: &str,
    command: Command,
    using: Option<&str>,
    check: Option<&str>,
) -> String {
    let mut s = format!(
        "CREATE POLICY {} ON {} AS RESTRICTIVE\n    FOR {}",
        quote_ident(name),
        quote_ident(table),
        command.as_sql()
    );
    if let Some(u) = using {
        s.push_str(&format!("\n    USING ({u})"));
    }
    if let Some(c) = check {
        s.push_str(&format!("\n    WITH CHECK ({c})"));
    }
    s
}

/// The policy name for a rule: the author's explicit name (optionally suffixed),
/// else `<table>_<kind>_<index>` — the index keeps derived names unique.
fn policy_name(rule: &Rule, table: &str, index: usize, suffix: Option<&str>) -> String {
    let base = rule
        .explicit_name()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{table}_{}_{index}", rule.kind_slug()));
    match suffix {
        Some(s) => format!("{base}_{s}"),
        None => base,
    }
}

/// Compile the access policy against its catalog. Validates first (an invalid
/// policy is rejected, not compiled to unsafe SQL).
pub fn compile(policy: &AccessPolicy, catalog: &Catalog) -> Result<MigrationPlan, CompileError> {
    validate::validate(policy, catalog).map_err(CompileError::InvalidPolicy)?;

    let mut operations = Vec::new();
    for (i, rule) in policy.rules.iter().enumerate() {
        // Validation guarantees the entity resolves.
        let entity = catalog
            .entities
            .iter()
            .find(|e| e.id == rule.entity())
            .expect("validated: entity resolves");
        let table = entity.name.clone();

        match rule {
            Rule::RowOwnership {
                owner_field,
                exempt_roles,
                ..
            } => {
                let owner_col = entity
                    .fields
                    .iter()
                    .find(|f| f.id == *owner_field)
                    .map(|f| f.name.clone())
                    .expect("validated: owner field resolves");
                let owned = format!("{} = {}", quote_ident(&owner_col), user_uuid());
                let pred = if exempt_roles.is_empty() {
                    owned
                } else {
                    format!("{} OR {owned}", role_in(exempt_roles))
                };
                let name = policy_name(rule, &table, i, None);
                operations.push(Operation {
                    summary: format!("row-ownership RLS on {table}.{owner_col}"),
                    sql: policy_stmt(&name, &table, Command::All, Some(&pred), Some(&pred)),
                    safety: Safety::Additive,
                    entity: entity.id.to_string(),
                    field: Some(owner_field.to_string()),
                    note: Some(CLAIM_NOTE.into()),
                });
            }
            Rule::RoleCommands { grants, .. } => {
                for grant in grants {
                    let pred = role_in(&grant.roles);
                    let suffix = grant.command.as_sql().to_ascii_lowercase();
                    let name = policy_name(rule, &table, i, Some(&suffix));
                    operations.push(Operation {
                        summary: format!(
                            "role gate {} on {table} for {}",
                            grant.command.as_sql(),
                            grant.roles.join(", ")
                        ),
                        sql: policy_stmt(
                            &name,
                            &table,
                            grant.command,
                            grant.command.has_using().then_some(pred.as_str()),
                            grant.command.has_check().then_some(pred.as_str()),
                        ),
                        safety: Safety::Additive,
                        entity: entity.id.to_string(),
                        field: None,
                        note: Some(CLAIM_NOTE.into()),
                    });
                }
            }
            Rule::RolePredicate {
                role,
                command,
                expression,
                ..
            } => {
                // "if you are this role, the row must satisfy the predicate;
                // other roles are unaffected by this rule."
                let pred = format!(
                    "{} <> {} OR ({expression})",
                    role_text(),
                    quote_literal(role)
                );
                let name = policy_name(rule, &table, i, None);
                operations.push(Operation {
                    summary: format!(
                        "role predicate ({role}) on {table} for {}",
                        command.as_sql()
                    ),
                    sql: policy_stmt(
                        &name,
                        &table,
                        *command,
                        command.has_using().then_some(pred.as_str()),
                        command.has_check().then_some(pred.as_str()),
                    ),
                    safety: Safety::Additive,
                    entity: entity.id.to_string(),
                    field: None,
                    note: Some(CLAIM_NOTE.into()),
                });
            }
        }
    }

    Ok(MigrationPlan { operations })
}
