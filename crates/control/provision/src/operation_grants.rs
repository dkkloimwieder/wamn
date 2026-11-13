//! The built-in role `admin` and the package-operation vocabulary.
//!
//! `admin` has no permission rows and holds every operation the current serving
//! release serves (docs/architecture/execution.md#host-run-routes).
//! `apply-package` only creates the `admin` row when it is absent. Every other
//! role is authored, and its permission rows are reconciled against the
//! candidate serving release before activation, not by `apply-package`. The
//! existing `platform` base grants are a precondition; this module never installs or
//! redesigns that authority schema.

use std::collections::BTreeSet;
use std::error::Error as StdError;

use wamn_catalog::manifest::{
    OperationVisibility, PackageManifest, canonical_operation_identity,
    validate_operation_vocabulary,
};

/// Serialize the writes of the environment's roles and permission rows.
///
/// Package lineage remains independently locked per family. Role and
/// permission writes additionally share this environment lock.
pub const OPERATION_GRANT_LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.operation-grants:' || $1, 0))";

/// Stable refusal when the project database has not installed its base authorization grants.
pub const PLATFORM_BASE_GRANTS_MISSING: &str = "wamn-operation-grants-platform-base-grants-missing";

/// Required statement inside the administrator-owned transaction.
///
/// A principal that cannot bypass forced RLS errors on the subsequent read or
/// write instead of observing a silently filtered role grant set.
pub const OPERATION_GRANT_TRANSACTION_PRELUDE_SQL: &str = "SET LOCAL row_security = off";

/// Refuse unless the existing application-authorization base grants are installed.
///
/// The effect owner executes this after acquiring [`OPERATION_GRANT_LOCK_SQL`]
/// and applying [`OPERATION_GRANT_TRANSACTION_PRELUDE_SQL`], and before
/// [`ENSURE_ADMIN_ROLE_SQL`], all in one transaction.
pub fn operation_base_grants_requirements_sql() -> String {
    format!(
        "DO $operation_base_grants$ BEGIN \
           IF pg_catalog.to_regclass('platform.role') IS NULL \
              OR pg_catalog.to_regclass('platform.permission') IS NULL THEN \
             RAISE EXCEPTION USING ERRCODE = '55000', \
               MESSAGE = '{PLATFORM_BASE_GRANTS_MISSING}'; \
           END IF; \
         END $operation_base_grants$;"
    )
}

/// Stable class for a package-operation reconciliation refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationGrantErrorType {
    /// The manifest is not the strict public package-manifest shape.
    InvalidManifest,
}

/// Contextual package-operation reconciliation refusal.
#[derive(Debug)]
pub struct OperationGrantError {
    type_: OperationGrantErrorType,
    context: Box<str>,
    source: Option<wamn_catalog::ManifestError>,
}

impl OperationGrantError {
    fn with_source(
        type_: OperationGrantErrorType,
        context: impl Into<Box<str>>,
        source: wamn_catalog::ManifestError,
    ) -> Self {
        Self {
            type_,
            context: context.into(),
            source: Some(source),
        }
    }

    /// Return the stable refusal class.
    pub const fn error_type(&self) -> OperationGrantErrorType {
        self.type_
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

/// Derive the package-qualified operation tokens from strict manifest bytes.
///
/// Public component `operations` members are the package-local callable-operation
/// vocabulary. Private custom operations remain callable only from declared
/// internal wirings and never become role grants. Rendering each public
/// member with the package's native extern spelling yields the generated grant
/// identity.
pub fn operation_grant_tokens(
    manifest_bytes: &[u8],
) -> Result<BTreeSet<String>, OperationGrantError> {
    let manifest = PackageManifest::from_slice(manifest_bytes).map_err(|source| {
        OperationGrantError::with_source(
            OperationGrantErrorType::InvalidManifest,
            "operation-grant manifest does not match the strict package shape",
            source,
        )
    })?;
    let mut local_tokens = validate_operation_vocabulary(&manifest).map_err(|source| {
        OperationGrantError::with_source(
            OperationGrantErrorType::InvalidManifest,
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
    local_tokens
        .into_iter()
        .map(|token| canonical_operation_identity(&manifest.package, &token))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|source| {
            OperationGrantError::with_source(
                OperationGrantErrorType::InvalidManifest,
                "operation-grant manifest has an invalid canonical operation identity",
                source,
            )
        })
}

/// Create the environment's `admin` role when it is absent.
///
/// The caller owns the surrounding transaction: it acquires
/// [`OPERATION_GRANT_LOCK_SQL`], applies
/// [`OPERATION_GRANT_TRANSACTION_PRELUDE_SQL`] and
/// [`operation_base_grants_requirements_sql`], then executes this statement. It
/// changes one row or none.
pub const ENSURE_ADMIN_ROLE_SQL: &str = "INSERT INTO platform.role (name) \
     VALUES ('admin') ON CONFLICT (name) DO NOTHING";

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_manifest() -> Vec<u8> {
        wamn_schema_generator::build_record::require_current(&wamn_fixture_package::package_root())
            .unwrap_or_else(|error| panic!("{error:#}"));
        wamn_fixture_package::manifest_bytes()
    }

    fn owned_manifest() -> serde_json::Value {
        let mut operations = serde_json::Map::new();
        for (name, visibility, errors) in [
            (
                "widget.publish",
                "public",
                vec!["permission_denied", "internal_error"],
            ),
            (
                "widget.archive",
                "public",
                vec!["permission_denied", "internal_error"],
            ),
            ("widget.prepare", "private", vec!["internal_error"]),
        ] {
            let mut operation = serde_json::json!({
                "type": "command",
                "visibility": visibility,
                "idempotent_by": "stateless",
                "input": {"fields": [{"path": "value.note", "type": "text", "nullable": false}]},
                "result": {"class": "one", "fields": [{"path": "note", "type": "text", "nullable": false}]},
                "errors": errors,
            });
            if visibility == "public" {
                operation["permission"] = serde_json::json!(name);
            }
            operations.insert(name.to_owned(), operation);
        }
        serde_json::json!({
            "package": {"id": "grant_fixture", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "grant_access", "state": "unsatisfied"},
            "models": {},
            "custom_operations": operations,
            "connections": {},
            "components": {"data": {"connections": []}},
        })
    }

    #[test]
    fn public_operations_receive_only_their_canonical_grants() {
        let bytes = serde_json::to_vec(&owned_manifest()).expect("serialize owned manifest");
        assert_eq!(
            operation_grant_tokens(&bytes).expect("parse owned manifest"),
            [
                "grant-fixture:widget/archive@1.0.0",
                "grant-fixture:widget/publish@1.0.0",
            ]
            .map(str::to_owned)
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn private_custom_operations_never_become_role_grants() {
        let mut manifest = owned_manifest();
        let operation = &mut manifest["custom_operations"]["widget.publish"];
        operation["visibility"] = serde_json::json!("private");
        operation
            .as_object_mut()
            .expect("operation is an object")
            .remove("permission");
        operation["errors"] = serde_json::json!(["internal_error"]);
        let bytes = serde_json::to_vec(&manifest).expect("serialize private operation fixture");
        assert_eq!(
            operation_grant_tokens(&bytes).expect("private operation remains valid"),
            ["grant-fixture:widget/archive@1.0.0"]
                .map(str::to_owned)
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn manifest_unknown_fields_are_refused_by_the_public_strict_parser() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fixture_manifest()).expect("fixture is JSON");
        manifest["invented_grant_grammar"] = serde_json::Value::Bool(true);
        let bytes = serde_json::to_vec(&manifest).expect("serialize mutated fixture");
        let error = operation_grant_tokens(&bytes).expect_err("unknown field was accepted");
        assert_eq!(error.error_type(), OperationGrantErrorType::InvalidManifest);
        assert!(
            error.source().is_some(),
            "strict parser refusal lost source"
        );
    }

    #[test]
    fn semantic_operation_vocabulary_refusal_reaches_the_grant_boundary() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fixture_manifest()).expect("fixture is JSON");
        manifest["models"]["widget"]["operations"]["get"]["component"] =
            serde_json::json!("missing");
        let bytes = serde_json::to_vec(&manifest).expect("serialize mutated fixture");
        let error = operation_grant_tokens(&bytes).expect_err("unknown operation was granted");
        assert_eq!(error.error_type(), OperationGrantErrorType::InvalidManifest);
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<wamn_catalog::ManifestError>())
                .map(wamn_catalog::ManifestError::error_type),
            Some(wamn_catalog::ManifestErrorType::InvalidComponent)
        );
    }

    #[test]
    fn noncanonical_package_identity_reaches_the_grant_boundary() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fixture_manifest()).expect("fixture is JSON");
        manifest["package"]["id"] = serde_json::json!("platform-Fixture");
        let bytes = serde_json::to_vec(&manifest).expect("serialize mutated fixture");
        let error = operation_grant_tokens(&bytes).expect_err("invalid package id was granted");
        assert_eq!(error.error_type(), OperationGrantErrorType::InvalidManifest);
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<wamn_catalog::ManifestError>())
                .map(wamn_catalog::ManifestError::error_type),
            Some(wamn_catalog::ManifestErrorType::InvalidIdentity)
        );
    }
}
