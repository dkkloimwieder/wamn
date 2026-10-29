//! The built-in role `admin` and the package-operation vocabulary.
//!
//! `admin` has no permission rows and holds every operation the current serving
//! release serves (docs/architecture/execution.md#host-run-routes).
//! `apply-package` only creates the `admin` row when it is absent. Every other
//! role is authored, and its permission rows are reconciled against the
//! candidate serving release before activation, not by `apply-package`. The
//! existing `platform` floor is a precondition; this module never installs or
//! redesigns that authority schema.

use std::collections::BTreeSet;
use std::error::Error as StdError;

use wamn_schema_generator::{
    OperationVisibility, PackageManifest, canonical_operation_identity,
    validate_operation_vocabulary,
};

/// Serialize the writes of the environment's roles and permission rows.
///
/// Package lineage remains independently locked per family. Role and
/// permission writes additionally share this environment lock.
pub const OPERATION_GRANT_LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.operation-grants:' || $1, 0))";

/// Stable refusal when the project database has not installed its auth floor.
pub const PLATFORM_FLOOR_MISSING: &str = "wamn-operation-grants-platform-floor-missing";

/// Required statement inside the administrator-owned transaction.
///
/// A principal that cannot bypass forced RLS errors on the subsequent read or
/// write instead of observing a silently filtered role grant set.
pub const OPERATION_GRANT_TRANSACTION_PRELUDE_SQL: &str = "SET LOCAL row_security = off";

/// Refuse unless the existing application-authorization floor is installed.
///
/// The effect owner executes this after acquiring [`OPERATION_GRANT_LOCK_SQL`]
/// and applying [`OPERATION_GRANT_TRANSACTION_PRELUDE_SQL`], and before
/// [`ENSURE_ADMIN_ROLE_SQL`], all in one transaction.
pub fn operation_grant_floor_check_sql() -> String {
    format!(
        "DO $operation_grant_floor$ BEGIN \
           IF pg_catalog.to_regclass('platform.role') IS NULL \
              OR pg_catalog.to_regclass('platform.permission') IS NULL THEN \
             RAISE EXCEPTION USING ERRCODE = '55000', \
               MESSAGE = '{PLATFORM_FLOOR_MISSING}'; \
           END IF; \
         END $operation_grant_floor$;"
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
    source: Option<wamn_schema_generator::GenerateError>,
}

impl OperationGrantError {
    fn with_source(
        type_: OperationGrantErrorType,
        context: impl Into<Box<str>>,
        source: wamn_schema_generator::GenerateError,
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
/// [`operation_grant_floor_check_sql`], then executes this statement. It
/// changes one row or none.
pub const ENSURE_ADMIN_ROLE_SQL: &str = "INSERT INTO platform.role (name) \
     VALUES ('admin') ON CONFLICT (name) DO NOTHING";

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_manifest() -> Vec<u8> {
        wamn_schema_generator::receipt::check_current(&wamn_fixture_package::package_root())
            .unwrap_or_else(|error| panic!("{error:#}"));
        wamn_fixture_package::manifest_bytes()
    }

    #[test]
    #[ignore = "wamn-e1zd9"]
    fn fixture_manifest_yields_the_nineteen_canonical_operation_grants() {
        assert_eq!(
            operation_grant_tokens(&fixture_manifest()).expect("parse strict fixture manifest"),
            [
                "platform-fixture:counter/create@2.1.0",
                "platform-fixture:counter/delete@2.1.0",
                "platform-fixture:counter/get@2.1.0",
                "platform-fixture:counter/query@2.1.0",
                "platform-fixture:counter/update@2.1.0",
                "platform-fixture:number/issue@2.1.0",
                "platform-fixture:numbered-label/create@2.1.0",
                "platform-fixture:widget-maker/get@2.1.0",
                "platform-fixture:widget-maker/list@2.1.0",
                "platform-fixture:widget-maker/query@2.1.0",
                "platform-fixture:widget-tag/update@2.1.0",
                "platform-fixture:widget/archive@2.1.0",
                "platform-fixture:widget/create@2.1.0",
                "platform-fixture:widget/delete@2.1.0",
                "platform-fixture:widget/get@2.1.0",
                "platform-fixture:widget/list@2.1.0",
                "platform-fixture:widget/query@2.1.0",
                "platform-fixture:widget/record-batch@2.1.0",
                "platform-fixture:widget/update@2.1.0",
            ]
            .map(str::to_owned)
            .into_iter()
            .collect()
        );
    }

    #[test]
    #[ignore = "wamn-slf04"]
    fn private_custom_operations_never_become_role_grants() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fixture_manifest()).expect("fixture is JSON");
        let operation = &mut manifest["custom_operations"]["widget.record_batch"];
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
                .any(|grant| grant == "platform-fixture:widget/record-batch@2.1.0")
        );
        assert_eq!(grants.len(), 18);
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
                .and_then(|source| source.downcast_ref::<wamn_schema_generator::GenerateError>())
                .map(wamn_schema_generator::GenerateError::error_type),
            Some(wamn_schema_generator::GenerateErrorType::InvalidComponent)
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
                .and_then(|source| source.downcast_ref::<wamn_schema_generator::GenerateError>())
                .map(wamn_schema_generator::GenerateError::error_type),
            Some(wamn_schema_generator::GenerateErrorType::InvalidIdentity)
        );
    }
}
