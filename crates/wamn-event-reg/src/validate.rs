//! Structural validation of an [`EventRegistration`] against the catalog it
//! targets.
//!
//! Checks that a registration is well-formed and *consumable*: the schema
//! version is compatible, it targets the catalog it was validated against, its
//! entity resolves **by id** (the rename-proof key — so a registration can only
//! bind to a real catalog entity), its op set is non-empty and duplicate-free,
//! and each expression ([`condition`](EventRegistration::condition),
//! [`partition_key`](EventRegistration::partition_key)) is **syntactically valid
//! JMESPath**. Expressions are compiled, not evaluated — the materializer
//! (l5i9.17) owns evaluation against the runtime event context; this surface
//! only guarantees a stored expression will parse.
//!
//! Reuses the catalog's [`Issue`] / [`Severity`] shape (3.1), like
//! [`wamn_rls::validate`].

use wamn_catalog::{Catalog, Issue, Severity};

use crate::model::{EventRegistration, SCHEMA_VERSION};

fn error(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue {
        severity: Severity::Error,
        code,
        path: path.into(),
        message: message.into(),
    }
}

/// Validate `reg` against `catalog`. Returns every [`Severity::Error`] found
/// (empty `Ok(())` when the registration is well-formed and consumable).
pub fn validate(reg: &EventRegistration, catalog: &Catalog) -> Result<(), Vec<Issue>> {
    let mut issues = Vec::new();

    if !schema_version_compatible(&reg.schema_version) {
        issues.push(error(
            "unsupported-schema-version",
            "schema-version",
            format!(
                "registration schema-version {:?} is not compatible with {SCHEMA_VERSION}.x",
                reg.schema_version
            ),
        ));
    }
    if reg.catalog_id != catalog.catalog_id {
        issues.push(error(
            "catalog-id-mismatch",
            "catalog-id",
            format!(
                "registration targets catalog {:?} but was validated against {:?}",
                reg.catalog_id, catalog.catalog_id
            ),
        ));
    }
    if reg.registration_id.trim().is_empty() {
        issues.push(error(
            "empty-registration-id",
            "registration-id",
            "registration id is empty",
        ));
    }
    if reg.flow_id.trim().is_empty() {
        issues.push(error(
            "empty-flow-id",
            "flow-id",
            "registration has no subscribing flow",
        ));
    }

    // The entity must resolve BY ID — the rename-proof key. A registration that
    // names no catalog entity can never be materialized (nothing on the stream
    // carries that entity segment).
    if !catalog.entities.iter().any(|e| e.id == reg.entity) {
        issues.push(error(
            "unknown-entity",
            "entity",
            format!("no entity {:?} in the catalog", reg.entity),
        ));
    }

    // A registration matching no op is inert. The op set is tiny (≤3), so a
    // linear duplicate scan is cheaper than hashing (Op is not Hash).
    if reg.ops.is_empty() {
        issues.push(error("empty-ops", "ops", "registration fires on no op"));
    } else {
        for (i, op) in reg.ops.iter().enumerate() {
            if reg.ops[..i].contains(op) {
                issues.push(error(
                    "duplicate-op",
                    format!("ops[{i}]"),
                    format!("op {:?} is listed more than once", op.as_str()),
                ));
            }
        }
    }

    // Expressions must PARSE as JMESPath (syntax only — the materializer owns
    // evaluation). An empty/whitespace expression is a authoring mistake, not a
    // valid "match everything" (that is `None`), so reject it distinctly.
    check_expr(&mut issues, "condition", reg.condition.as_deref());
    check_expr(&mut issues, "partition-key", reg.partition_key.as_deref());

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

/// Reject an empty expression and one that does not compile as JMESPath.
fn check_expr(issues: &mut Vec<Issue>, path: &'static str, expr: Option<&str>) {
    let Some(expr) = expr else { return };
    if expr.trim().is_empty() {
        issues.push(error(
            "empty-expression",
            path,
            format!("{path} is present but empty (omit it to match unconditionally)"),
        ));
        return;
    }
    if let Err(e) = jmespath::compile(expr) {
        issues.push(error(
            "invalid-jmespath",
            path,
            format!("{path} is not valid JMESPath: {e}"),
        ));
    }
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
