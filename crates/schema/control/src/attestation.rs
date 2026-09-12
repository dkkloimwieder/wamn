//! Deployment content and release identity decisions for the control store.
//!
//! PostgreSQL keeps immutable rows, unique coordinates, foreign keys, and tenant isolation.
//! Rust compares stored content after each insert attempt in the caller's transaction.

use std::fmt;

use crate::model::{SqlStatement, Value};
use crate::sql;

/// A deployment coordinate already records different content or source provenance.
pub const CONTENT_CONFLICT: &str = "deployment-attestation-content-conflict";

/// A release identity already records a different environment.
pub const PROJECTION_CONTENT_CONFLICT: &str =
    "effective-release-identity-projection-content-conflict";

/// Insert one release identity without replacing an existing row.
pub fn project_effective_release_identity_sql() -> &'static str {
    "INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment) \
     VALUES ($1, $2, $3) ON CONFLICT (tenant_id, effective_release_id) DO NOTHING"
}

/// Read the winning identity after the insert finishes.
pub fn read_effective_release_identity_sql() -> &'static str {
    "SELECT environment FROM catalog.effective_releases \
     WHERE tenant_id = $1 AND effective_release_id = $2"
}

/// One effective release identity as the CONTROL plane records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveReleaseIdentity<'a> {
    pub tenant_id: &'a str,
    pub effective_release_id: i32,
    pub environment: &'a str,
}

/// Bind one release-identity projection for the driver to execute.
///
/// Each parameter keeps its declared coordinate position.
pub fn project_effective_release_identity(identity: &EffectiveReleaseIdentity<'_>) -> SqlStatement {
    SqlStatement {
        summary: format!(
            "project effective release identity {}/{}",
            identity.tenant_id, identity.effective_release_id
        ),
        sql: project_effective_release_identity_sql().to_owned(),
        params: vec![
            Value::Text(identity.tenant_id.to_owned()),
            Value::Int(identity.effective_release_id),
            Value::Text(identity.environment.to_owned()),
        ],
    }
}

/// Accept an exact identity retry and refuse different stored content.
pub fn check_projected_identity(
    identity: &EffectiveReleaseIdentity<'_>,
    recorded_environment: &str,
) -> Result<(), AttestationError> {
    if identity.environment == recorded_environment {
        Ok(())
    } else {
        Err(AttestationError {
            kind: AttestationErrorKind::IdentityProjectionConflict,
            coordinate: identity_coordinate(identity),
            driver: String::new(),
        })
    }
}

/// Preserve the actual database failure at the identity boundary.
pub fn translate_projection_failure(
    identity: &EffectiveReleaseIdentity<'_>,
    reported: &str,
) -> AttestationError {
    AttestationError {
        kind: AttestationErrorKind::Storage,
        coordinate: identity_coordinate(identity),
        driver: reported.to_owned(),
    }
}

fn identity_coordinate(identity: &EffectiveReleaseIdentity<'_>) -> String {
    format!(
        "{}/{} in {:?}",
        identity.tenant_id, identity.effective_release_id, identity.environment
    )
}

/// A deployment's six-part coordinate, content, source, and proposed timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attestation<'a> {
    pub tenant_id: &'a str,
    pub environment_instance: &'a str,
    pub effective_release_id: i32,
    pub org_id: &'a str,
    pub project_id: &'a str,
    pub environment: &'a str,
    /// `sha256:<64 hex>` — the relation's `CHECK` is the only place that shape
    /// is enforced.
    pub deployed_manifest_hash: &'a str,
    /// Optional clean source commit attributed by a checkout-aware publisher.
    pub source_commit: Option<&'a str>,
    /// A proposed attestation instant, as a literal PostgreSQL parses to
    /// `timestamptz` (RFC 3339). The server keeps this only when this invocation
    /// wins the insert; exact retries retain the recorded winner. Bound as text
    /// because the engine's [`Value`] carries no timestamp variant.
    pub attested_at: &'a str,
}

/// Bind one attestation write for the driver to execute.
///
/// Each parameter keeps its declared coordinate position.
pub fn register_attestation(attestation: &Attestation<'_>) -> SqlStatement {
    SqlStatement {
        summary: format!(
            "register deployment attestation {}",
            coordinate(attestation)
        ),
        sql: sql::register_deployment_attestation_sql().to_owned(),
        params: vec![
            Value::Text(attestation.tenant_id.to_owned()),
            Value::Text(attestation.environment_instance.to_owned()),
            Value::Int(attestation.effective_release_id),
            Value::Text(attestation.org_id.to_owned()),
            Value::Text(attestation.project_id.to_owned()),
            Value::Text(attestation.environment.to_owned()),
            Value::Text(attestation.deployed_manifest_hash.to_owned()),
            Value::NullableText(attestation.source_commit.map(str::to_owned)),
            Value::Text(attestation.attested_at.to_owned()),
        ],
    }
}

/// Read the tenant's current environment instance in the caller's transaction.
pub fn read_environment_instance_sql() -> &'static str {
    "SELECT environment_instance FROM catalog.tenant_environments WHERE tenant_id = $1"
}

/// Read the winning row using all six parts of the resolved coordinate.
pub fn read_attestation_sql() -> &'static str {
    "SELECT deployed_manifest_hash, source_commit, attested_at FROM catalog.deployment_attestations \
     WHERE tenant_id = $1 AND environment_instance = $2 AND effective_release_id = $3 \
     AND org_id = $4 AND project_id = $5 AND environment = $6"
}

/// Compare content and optional source provenance without changing the winning row.
pub fn check_attestation(
    attestation: &Attestation<'_>,
    recorded_hash: &str,
    recorded_source: Option<&str>,
) -> Result<(), AttestationError> {
    if attestation.deployed_manifest_hash == recorded_hash
        && attestation.source_commit == recorded_source
    {
        Ok(())
    } else {
        Err(AttestationError {
            kind: AttestationErrorKind::ContentConflict,
            coordinate: coordinate(attestation),
            driver: String::new(),
        })
    }
}

/// Preserve the actual database failure at the deployment boundary.
pub fn translate_failure(attestation: &Attestation<'_>, reported: &str) -> AttestationError {
    AttestationError {
        kind: AttestationErrorKind::Storage,
        coordinate: coordinate(attestation),
        driver: reported.to_owned(),
    }
}

/// Stable predicate that refused a deployment-attestation write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestationErrorKind {
    /// The coordinate is already attested with a DIFFERENT
    /// `deployed_manifest_hash`. Not a retry: the remedy is to find out which
    /// bytes actually deployed, never to re-publish over the recorded fact.
    ContentConflict,
    /// The release coordinate is already projected onto the control plane with
    /// DIFFERENT release facts. The remedy is to find out which environment
    /// actually minted it, never to overwrite the record.
    IdentityProjectionConflict,
    /// Any other failure the driver reported.
    Storage,
}

impl AttestationErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContentConflict => CONTENT_CONFLICT,
            Self::IdentityProjectionConflict => PROJECTION_CONTENT_CONFLICT,
            Self::Storage => "storage",
        }
    }
}

/// A refused deployment-attestation write: what refused it, which coordinate,
/// and the driver failure it was translated from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationError {
    kind: AttestationErrorKind,
    coordinate: String,
    driver: String,
}

impl AttestationError {
    pub const fn kind(&self) -> AttestationErrorKind {
        self.kind
    }

    /// The six-part coordinate the write was refused at.
    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Database error text, or an empty string when Rust refused conflicting content.
    pub fn driver(&self) -> &str {
        &self.driver
    }
}

impl fmt::Display for AttestationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.coordinate)?;
        if !self.driver.is_empty() {
            write!(f, ": {}", self.driver)?;
        }
        Ok(())
    }
}

impl std::error::Error for AttestationError {}

/// The six-part coordinate, rendered as a refusal's context.
fn coordinate(attestation: &Attestation<'_>) -> String {
    format!(
        "{}/{}/{} -> {}/{}/{}",
        attestation.tenant_id,
        attestation.environment_instance,
        attestation.effective_release_id,
        attestation.org_id,
        attestation.project_id,
        attestation.environment,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every part distinct, so a swapped pair cannot hide behind an equal value.
    fn attestation() -> Attestation<'static> {
        Attestation {
            tenant_id: "tenant-a",
            environment_instance: "instance-a",
            effective_release_id: 7,
            org_id: "acme",
            project_id: "billing",
            environment: "prod",
            deployed_manifest_hash: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
            source_commit: Some("0123456789abcdef"),
            attested_at: "2026-08-15T12:00:00Z",
        }
    }

    #[test]
    fn the_binding_places_every_part_at_its_own_position() {
        let statement = register_attestation(&attestation());
        assert_eq!(statement.sql, sql::register_deployment_attestation_sql());
        assert_eq!(
            statement.params,
            vec![
                Value::Text("tenant-a".to_owned()),
                Value::Text("instance-a".to_owned()),
                Value::Int(7),
                Value::Text("acme".to_owned()),
                Value::Text("billing".to_owned()),
                Value::Text("prod".to_owned()),
                Value::Text(
                    "sha256:2222222222222222222222222222222222222222222222222222222222222222"
                        .to_owned()
                ),
                Value::NullableText(Some("0123456789abcdef".to_owned())),
                Value::Text("2026-08-15T12:00:00Z".to_owned()),
            ]
        );
    }

    #[test]
    fn a_conflicting_re_attestation_translates_to_the_routines_own_refusal() {
        let error = check_attestation(&attestation(), "different hash", Some("0123456789abcdef"))
            .unwrap_err();
        assert_eq!(error.kind(), AttestationErrorKind::ContentConflict);
        assert_eq!(
            error.coordinate(),
            "tenant-a/instance-a/7 -> acme/billing/prod"
        );
        // The existing refusal literal remains first.
        assert!(
            error
                .to_string()
                .starts_with("deployment-attestation-content-conflict: ")
        );
    }

    #[test]
    fn an_unrelated_database_failure_stays_storage() {
        // A foreign-key violation: the release coordinate was never published.
        let error = translate_failure(
            &attestation(),
            "db error: ERROR: insert or update violates foreign key constraint",
        );
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
        assert!(!error.to_string().contains(CONTENT_CONFLICT));
        assert!(error.driver().contains("foreign key"));
    }

    #[test]
    fn a_unique_violation_from_elsewhere_is_not_the_content_conflict() {
        // Database errors remain storage failures.
        let error = translate_failure(
            &attestation(),
            "db error: ERROR: duplicate key value violates unique constraint \"packages_pkey\"",
        );
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
    }

    #[test]
    fn a_failure_that_never_reached_the_server_is_storage() {
        let error = translate_failure(&attestation(), "connection closed");
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
    }

    /// Every part distinct, so a swapped pair cannot hide behind an equal value.
    fn identity() -> EffectiveReleaseIdentity<'static> {
        EffectiveReleaseIdentity {
            tenant_id: "tenant-a",
            effective_release_id: 7,
            environment: "prod",
        }
    }

    /// A moved `$n` would anchor the attestation's foreign key to a coordinate
    /// nothing minted, which no live gate keyed on the same wrong values could
    /// see. Pinned here, where the string is built.
    #[test]
    fn the_projection_binding_places_every_part_at_its_own_position() {
        let statement = project_effective_release_identity(&identity());
        assert_eq!(statement.sql, project_effective_release_identity_sql());
        assert_eq!(
            statement.params,
            vec![
                Value::Text("tenant-a".to_owned()),
                Value::Int(7),
                Value::Text("prod".to_owned()),
            ]
        );
    }

    #[test]
    fn a_conflicting_re_projection_translates_to_the_routines_own_refusal() {
        let error = check_projected_identity(&identity(), "dev").unwrap_err();
        assert_eq!(
            error.kind(),
            AttestationErrorKind::IdentityProjectionConflict
        );
        assert_eq!(error.coordinate(), "tenant-a/7 in \"prod\"");
        assert!(
            error
                .to_string()
                .starts_with("effective-release-identity-projection-content-conflict: ")
        );
    }

    #[test]
    fn the_attestations_own_conflict_is_not_a_projection_conflict() {
        let error = translate_projection_failure(
            &identity(),
            "db error: ERROR: deployment-attestation-content-conflict",
        );
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
    }

    #[test]
    fn a_projection_failure_that_never_reached_the_server_is_storage() {
        let error = translate_projection_failure(&identity(), "connection closed");
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
    }
    #[test]
    fn exact_retries_preserve_optional_source_provenance() {
        let first = attestation();
        assert!(
            check_attestation(&first, first.deployed_manifest_hash, first.source_commit).is_ok()
        );
        assert!(check_attestation(&first, first.deployed_manifest_hash, None).is_err());
        let without_source = Attestation {
            source_commit: None,
            ..first
        };
        assert!(check_attestation(&without_source, first.deployed_manifest_hash, None).is_ok());
        assert!(
            check_attestation(&without_source, first.deployed_manifest_hash, Some("")).is_err()
        );
        assert!(check_projected_identity(&identity(), "prod").is_ok());
    }
}
