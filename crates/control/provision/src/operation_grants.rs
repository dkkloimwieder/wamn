//! Exact package-operation grants for the first-party route caller.
//!
//! This is the pure half of reconciliation: it parses the package's strict
//! manifest and emits the floor check plus one server-side convergence query.
//! The effect owner runs both against the registry-verified project database
//! and reads the query's final count row. The existing `app_system` floor is a
//! precondition; this module never installs or redesigns that authority schema.

use std::collections::BTreeSet;
use std::error::Error as StdError;

use wamn_pg_core::quote_literal;
use wamn_schema_generator::{
    OperationVisibility, PackageManifest, canonical_operation_identity, canonical_operation_prefix,
    validate_operation_vocabulary,
};

/// The project-role slug minted for first-party route callers.
///
/// This is deliberately the project-role vocabulary itself, not a mapping to a
/// second application-role name.
pub const OPERATION_CALLER_ROLE: &str = "route-caller";

/// Serialize the shared route-caller carrier within one tenant.
///
/// Package lineage remains independently locked per family. Grant
/// reconciliation additionally shares this tenant-grain lock because the first
/// package creates the one role row every package coordinate contributes to.
pub const OPERATION_GRANT_LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.operation-grants:' || $1, 0))";

/// Stable refusal when the project database has not installed its auth floor.
pub const APP_SYSTEM_FLOOR_MISSING: &str = "wamn-operation-grants-app-system-floor-missing";

/// Required first statement inside the administrator-owned transaction.
///
/// A principal that cannot bypass forced RLS errors on the subsequent read or
/// write instead of observing a silently filtered tenant+role grant set.
pub const OPERATION_GRANT_TRANSACTION_PRELUDE_SQL: &str = "SET LOCAL row_security = off";

/// Refuse unless the existing application-authorization floor is installed.
///
/// The effect owner executes this after acquiring [`OPERATION_GRANT_LOCK_SQL`]
/// and applying [`OPERATION_GRANT_TRANSACTION_PRELUDE_SQL`], and before the
/// single-row query from [`reconcile_operation_grants_sql`], all in one
/// transaction.
pub fn operation_grant_floor_check_sql() -> String {
    format!(
        "DO $operation_grant_floor$ BEGIN \
           IF pg_catalog.to_regclass('app_system.roles') IS NULL \
              OR pg_catalog.to_regclass('app_system.permissions') IS NULL THEN \
             RAISE EXCEPTION USING ERRCODE = '55000', \
               MESSAGE = '{APP_SYSTEM_FLOOR_MISSING}'; \
           END IF; \
         END $operation_grant_floor$;"
    )
}

/// Stable class for a package-operation reconciliation refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationGrantErrorKind {
    /// The supplied tenant key is empty.
    InvalidTenant,
    /// The manifest is not the strict public package-manifest shape.
    InvalidManifest,
}

/// Contextual package-operation reconciliation refusal.
#[derive(Debug)]
pub struct OperationGrantError {
    kind: OperationGrantErrorKind,
    context: Box<str>,
    source: Option<wamn_schema_generator::GenerateError>,
}

impl OperationGrantError {
    fn new(kind: OperationGrantErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            source: None,
        }
    }

    fn with_source(
        kind: OperationGrantErrorKind,
        context: impl Into<Box<str>>,
        source: wamn_schema_generator::GenerateError,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            source: Some(source),
        }
    }

    /// Return the stable refusal class.
    pub const fn kind(&self) -> OperationGrantErrorKind {
        self.kind
    }

    /// Human-readable context naming the refused grant input.
    pub fn context(&self) -> &str {
        &self.context
    }
}

impl std::fmt::Display for OperationGrantError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.context)
    }
}

impl StdError for OperationGrantError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// Counts returned by the final row of [`reconcile_operation_grants_sql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationGrantReconcileResult {
    role_rows_changed: i64,
    grants_added: i64,
    grants_removed: i64,
}

struct ManifestOperationGrants {
    coordinate_prefix: String,
    coordinate_suffix: String,
    tokens: BTreeSet<String>,
}

impl OperationGrantReconcileResult {
    /// Construct from the three `bigint` columns PostgreSQL returned.
    pub const fn new(role_rows_changed: i64, grants_added: i64, grants_removed: i64) -> Self {
        Self {
            role_rows_changed,
            grants_added,
            grants_removed,
        }
    }

    /// Whether the server changed no role or grant row.
    pub const fn is_noop(self) -> bool {
        self.role_rows_changed == 0 && self.grants_added == 0 && self.grants_removed == 0
    }

    /// Number of role rows inserted or hardened as system-owned.
    pub const fn role_rows_changed(self) -> i64 {
        self.role_rows_changed
    }

    /// Number of missing operation grants inserted.
    pub const fn grants_added(self) -> i64 {
        self.grants_added
    }

    /// Number of residual grants deleted.
    pub const fn grants_removed(self) -> i64 {
        self.grants_removed
    }
}

/// Derive the package-qualified operation tokens from strict manifest bytes.
///
/// Public component `operations` members are the package-local callable-operation
/// vocabulary. Private custom operations remain callable only from declared
/// internal wirings and never become route-caller grants. Rendering each public
/// member with the package's native extern spelling yields the generated grant
/// identity.
pub fn operation_grant_tokens(
    manifest_bytes: &[u8],
) -> Result<BTreeSet<String>, OperationGrantError> {
    Ok(manifest_operation_grants(manifest_bytes)?.tokens)
}

fn manifest_operation_grants(
    manifest_bytes: &[u8],
) -> Result<ManifestOperationGrants, OperationGrantError> {
    let manifest = PackageManifest::from_slice(manifest_bytes).map_err(|source| {
        OperationGrantError::with_source(
            OperationGrantErrorKind::InvalidManifest,
            "operation-grant manifest does not match the strict package shape",
            source,
        )
    })?;
    let mut local_tokens = validate_operation_vocabulary(&manifest).map_err(|source| {
        OperationGrantError::with_source(
            OperationGrantErrorKind::InvalidManifest,
            "operation-grant manifest has an invalid operation vocabulary",
            source,
        )
    })?;
    local_tokens.retain(|token| {
        manifest
            .custom_operations
            .get(token)
            .is_none_or(|operation| operation.visibility() == OperationVisibility::Public)
    });
    let coordinate_prefix = canonical_operation_prefix(&manifest.package).map_err(|source| {
        OperationGrantError::with_source(
            OperationGrantErrorKind::InvalidManifest,
            "operation-grant manifest has an invalid package coordinate",
            source,
        )
    })?;
    let coordinate_suffix = format!("@{}", manifest.package.version);
    let tokens = local_tokens
        .into_iter()
        .map(|token| canonical_operation_identity(&manifest.package, &token))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|source| {
            OperationGrantError::with_source(
                OperationGrantErrorKind::InvalidManifest,
                "operation-grant manifest has an invalid canonical operation identity",
                source,
            )
        })?;
    Ok(ManifestOperationGrants {
        coordinate_prefix,
        coordinate_suffix,
        tokens,
    })
}

/// Emit one exact, transactional route-caller grant reconciliation.
///
/// After [`operation_grant_floor_check_sql`] proves the existing `app_system`
/// grant floor is present, this single data-modifying CTE:
///
/// 1. creates the fixed `route-caller` role, or hardens it as system-owned;
/// 2. deletes this tenant+role's same-coordinate permissions absent from the
///    manifest;
/// 3. inserts this manifest's missing package-qualified operation grants; and
/// 4. returns `role_rows_changed`, `grants_added`, `grants_removed` as `bigint`.
///
/// The canonical `<package-id-kebab>:` prefix and `@<package-version>` suffix
/// select one exact package coordinate, so reconciliation preserves every
/// other package and version without a token mapping.
///
/// The caller owns the surrounding transaction: after target identity is
/// verified, it begins, acquires the tenant-grain operation-grant lock, executes
/// the prelude and floor check, queries this output, consumes the final row, and
/// commits. Keeping transaction control out of the pure builders lets the
/// effect owner use the driver's transaction object; requiring
/// `row_security = off` turns a non-bypass administrator into an error instead
/// of a silently filtered delete. A converged replay returns `0, 0, 0`, which
/// maps to [`OperationGrantReconcileResult::is_noop`].
pub fn reconcile_operation_grants_sql(
    manifest_bytes: &[u8],
    tenant: &str,
) -> Result<String, OperationGrantError> {
    if tenant.is_empty() {
        return Err(OperationGrantError::new(
            OperationGrantErrorKind::InvalidTenant,
            "operation-grant tenant must not be empty",
        ));
    }
    let grants = manifest_operation_grants(manifest_bytes)?;
    let desired = grants
        .tokens
        .iter()
        .map(|grant| format!("({})", quote_literal(grant)))
        .collect::<Vec<_>>()
        .join(", ");
    let desired = format!("VALUES {desired}");
    let coordinate_prefix = quote_literal(&grants.coordinate_prefix);
    let coordinate_suffix = quote_literal(&grants.coordinate_suffix);
    let tenant = quote_literal(tenant);
    let role = quote_literal(OPERATION_CALLER_ROLE);

    Ok(format!(
        "WITH desired(permission) AS ({desired}), \
         role_changed AS ( \
           INSERT INTO app_system.roles AS stored_role (tenant_id, name, is_system) \
           VALUES ({tenant}, {role}, true) \
           ON CONFLICT (tenant_id, name) DO UPDATE SET is_system = true \
             WHERE stored_role.is_system IS DISTINCT FROM true \
           RETURNING tenant_id, name \
         ), \
         role_target AS MATERIALIZED ( \
           SELECT tenant_id, name FROM role_changed \
           UNION ALL \
           SELECT tenant_id, name FROM app_system.roles \
            WHERE tenant_id = {tenant} AND name = {role} \
              AND NOT EXISTS (SELECT FROM role_changed) \
         ), \
         removed AS ( \
           DELETE FROM app_system.permissions AS stored USING role_target \
           WHERE stored.tenant_id = role_target.tenant_id \
              AND stored.role_name = role_target.name \
              AND pg_catalog.starts_with(stored.permission, {coordinate_prefix}) \
              AND pg_catalog.right(stored.permission, pg_catalog.length({coordinate_suffix})) \
                    = {coordinate_suffix} \
              AND NOT EXISTS (SELECT FROM desired \
                               WHERE desired.permission = stored.permission) \
           RETURNING stored.permission \
         ), \
         added AS ( \
           INSERT INTO app_system.permissions (tenant_id, role_name, permission) \
           SELECT role_target.tenant_id, role_target.name, desired.permission \
             FROM role_target CROSS JOIN desired \
           ON CONFLICT (tenant_id, role_name, permission) DO NOTHING \
           RETURNING permission \
         ) \
         SELECT (SELECT count(*) FROM role_changed)::bigint AS role_rows_changed, \
                (SELECT count(*) FROM added)::bigint AS grants_added, \
                (SELECT count(*) FROM removed)::bigint AS grants_removed;"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIVING_MANIFEST: &[u8] = include_bytes!("../../../../packages/receiving/wamn.json");

    #[test]
    fn receiving_manifest_yields_the_eight_canonical_operation_grants() {
        assert_eq!(
            operation_grant_tokens(RECEIVING_MANIFEST).expect("parse strict Receiving manifest"),
            [
                "wamn-receiving:location/list@1.0.0",
                "wamn-receiving:purchase-order/get@1.0.0",
                "wamn-receiving:purchase-order/query@1.0.0",
                "wamn-receiving:purchase-order/update@1.0.0",
                "wamn-receiving:receipt/get@1.0.0",
                "wamn-receiving:receipt/query@1.0.0",
                "wamn-receiving:receiving/load-receipt-screen@1.0.0",
                "wamn-receiving:receiving/record-receipt@1.0.0",
            ]
            .map(str::to_owned)
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn private_custom_operations_never_become_route_caller_grants() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(RECEIVING_MANIFEST).expect("fixture is JSON");
        let operation = &mut manifest["custom_operations"]["receiving.record_receipt"];
        operation["visibility"] = serde_json::json!("private");
        operation
            .as_object_mut()
            .expect("operation is an object")
            .remove("permission");
        operation["errors"]
            .as_array_mut()
            .expect("operation errors are an array")
            .retain(|error| error != "permission_denied");
        operation["error_details"]
            .as_object_mut()
            .expect("operation error details are an object")
            .remove("permission_denied");
        let bytes = serde_json::to_vec(&manifest).expect("serialize private operation fixture");

        let grants = operation_grant_tokens(&bytes).expect("private operation remains valid");
        assert!(
            !grants
                .iter()
                .any(|grant| grant == "wamn-receiving:receiving/record-receipt@1.0.0")
        );
        assert_eq!(grants.len(), 7);
    }

    #[test]
    fn manifest_unknown_fields_are_refused_by_the_public_strict_parser() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(RECEIVING_MANIFEST).expect("fixture is JSON");
        manifest["invented_grant_grammar"] = serde_json::Value::Bool(true);
        let bytes = serde_json::to_vec(&manifest).expect("serialize mutated fixture");
        let error = operation_grant_tokens(&bytes).expect_err("unknown field was accepted");
        assert_eq!(error.kind(), OperationGrantErrorKind::InvalidManifest);
        assert!(
            error.source().is_some(),
            "strict parser refusal lost source"
        );
    }

    #[test]
    fn semantic_operation_vocabulary_refusal_reaches_the_grant_boundary() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(RECEIVING_MANIFEST).expect("fixture is JSON");
        manifest["models"]["receipt"]["operations"]["get"]["component"] =
            serde_json::json!("missing");
        let bytes = serde_json::to_vec(&manifest).expect("serialize mutated fixture");
        let error = operation_grant_tokens(&bytes).expect_err("unknown operation was granted");
        assert_eq!(error.kind(), OperationGrantErrorKind::InvalidManifest);
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<wamn_schema_generator::GenerateError>())
                .map(wamn_schema_generator::GenerateError::kind),
            Some(wamn_schema_generator::GenerateErrorKind::InvalidComponent)
        );
    }

    #[test]
    fn noncanonical_package_identity_reaches_the_grant_boundary() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(RECEIVING_MANIFEST).expect("fixture is JSON");
        manifest["package"]["id"] = serde_json::json!("wamn-Receiving");
        let bytes = serde_json::to_vec(&manifest).expect("serialize mutated fixture");
        let error = operation_grant_tokens(&bytes).expect_err("invalid package id was granted");
        assert_eq!(error.kind(), OperationGrantErrorKind::InvalidManifest);
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<wamn_schema_generator::GenerateError>())
                .map(wamn_schema_generator::GenerateError::kind),
            Some(wamn_schema_generator::GenerateErrorKind::InvalidIdentity)
        );
    }

    #[test]
    fn server_counts_define_the_closing_predicate() {
        assert!(!OperationGrantReconcileResult::new(1, 5, 1).is_noop());
        assert!(OperationGrantReconcileResult::new(0, 0, 0).is_noop());
    }
}
