//! The control-plane deployment-attestation write (wamn-0h0g.8.21).
//!
//! `catalog.register_deployment_attestation` records that one release coordinate
//! really reached one `(org, project, environment)` placement. It is CONTROL-plane
//! only (`deploy/sql/control-portable-store.sql`); the `catalog.releases` its
//! foreign key targets is the control copy, not the project one.
//!
//! The routine's own DDL already names the single refusal this write can raise,
//! so nothing here mints a second dialect for it: [`CONTENT_CONFLICT`] IS the
//! server's message.
//!
//! Pure, like the rest of the crate (SR3): [`register_attestation`] binds the
//! statement, the driver executes it, and [`translate_failure`] is the ONE place
//! the resulting failure becomes a typed error. What the pure tests here cannot
//! observe is whether PostgreSQL accepts the binding at all — that is
//! `deployment_attestation_rust_binding_holds_on_postgres` in
//! `crates/control/provision/tests/control_portable_store.rs` (SR12b).

use std::fmt;

use crate::model::{SqlStatement, Value};
use crate::sql;

/// The refusal `catalog.register_deployment_attestation` raises when a coordinate
/// is re-attested with a different `deployed_manifest_hash`.
///
/// One condition, one literal: this is the DDL's own `MESSAGE`, not a Rust
/// synonym for it.
pub const CONTENT_CONFLICT: &str = "deployment-attestation-content-conflict";

/// The `ERRCODE` the routine raises [`CONTENT_CONFLICT`] under (`unique_violation`).
const UNIQUE_VIOLATION: &str = "23505";

/// One deployment attestation: the six-part coordinate it is keyed by, and the
/// content it attests.
///
/// The coordinate is exactly `(tenant_id, catalog_id, catalog_version, org_id,
/// project_id, environment)` — the relation's `deployment_attestations_coordinate`
/// UNIQUE constraint. `deployed_manifest_hash` and `attested_at` are the attested
/// content, not part of the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attestation<'a> {
    pub tenant_id: &'a str,
    pub catalog_id: &'a str,
    pub catalog_version: i32,
    pub org_id: &'a str,
    pub project_id: &'a str,
    pub environment: &'a str,
    /// `sha256:<64 hex>` — the relation's `CHECK` is the only place that shape
    /// is enforced.
    pub deployed_manifest_hash: &'a str,
    /// The attestation instant, as a literal PostgreSQL parses to `timestamptz`
    /// (RFC 3339). Bound as text and cast by the statement, because the engine's
    /// [`Value`] carries no timestamp variant; an unparseable instant is refused
    /// by the server, not here.
    pub attested_at: &'a str,
}

/// Bind one attestation write for the driver to execute.
///
/// The parameter order is the routine's argument order, which is also the
/// coordinate's own order — a part bound at the wrong position would key the
/// attestation under a placement nothing deployed to.
pub fn register_attestation(attestation: &Attestation<'_>) -> SqlStatement {
    SqlStatement {
        summary: format!(
            "register deployment attestation {}",
            coordinate(attestation)
        ),
        sql: sql::register_deployment_attestation_sql().to_owned(),
        params: vec![
            Value::Text(attestation.tenant_id.to_owned()),
            Value::Text(attestation.catalog_id.to_owned()),
            Value::Int(attestation.catalog_version),
            Value::Text(attestation.org_id.to_owned()),
            Value::Text(attestation.project_id.to_owned()),
            Value::Text(attestation.environment.to_owned()),
            Value::Text(attestation.deployed_manifest_hash.to_owned()),
            Value::Text(attestation.attested_at.to_owned()),
        ],
    }
}

/// Translate the driver's failure into [`AttestationError`], exactly once, here.
///
/// `sqlstate` is the five-character SQLSTATE the driver reported (`None` when the
/// failure never reached the server) and `reported` is the driver's own rendering
/// of it, kept verbatim as the translated error's cause.
pub fn translate_failure(
    attestation: &Attestation<'_>,
    sqlstate: Option<&str>,
    reported: &str,
) -> AttestationError {
    // Both halves are required. A bare `unique_violation` can come from anywhere
    // else in the caller's transaction, and the message alone does not establish
    // that the routine's own RAISE is what produced it.
    let kind = if sqlstate == Some(UNIQUE_VIOLATION) && reported.contains(CONTENT_CONFLICT) {
        AttestationErrorKind::ContentConflict
    } else {
        AttestationErrorKind::Storage
    };
    AttestationError {
        kind,
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
    /// Any other failure the driver reported.
    Storage,
}

impl AttestationErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContentConflict => CONTENT_CONFLICT,
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

    /// The driver's own rendering of the failure this was translated from.
    pub fn driver(&self) -> &str {
        &self.driver
    }
}

impl fmt::Display for AttestationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}: {}",
            self.kind.as_str(),
            self.coordinate,
            self.driver
        )
    }
}

impl std::error::Error for AttestationError {}

/// The six-part coordinate, rendered as a refusal's context.
fn coordinate(attestation: &Attestation<'_>) -> String {
    format!(
        "{}/{}@{} -> {}/{}/{}",
        attestation.tenant_id,
        attestation.catalog_id,
        attestation.catalog_version,
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
            catalog_id: "orders",
            catalog_version: 7,
            org_id: "acme",
            project_id: "billing",
            environment: "prod",
            deployed_manifest_hash: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
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
                Value::Text("orders".to_owned()),
                Value::Int(7),
                Value::Text("acme".to_owned()),
                Value::Text("billing".to_owned()),
                Value::Text("prod".to_owned()),
                Value::Text(
                    "sha256:2222222222222222222222222222222222222222222222222222222222222222"
                        .to_owned()
                ),
                Value::Text("2026-08-15T12:00:00Z".to_owned()),
            ]
        );
    }

    #[test]
    fn a_conflicting_re_attestation_translates_to_the_routines_own_refusal() {
        let error = translate_failure(
            &attestation(),
            Some("23505"),
            "db error: ERROR: deployment-attestation-content-conflict",
        );
        assert_eq!(error.kind(), AttestationErrorKind::ContentConflict);
        assert_eq!(error.coordinate(), "tenant-a/orders@7 -> acme/billing/prod");
        // The DDL's literal, surfaced verbatim and first — no Rust synonym.
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
            Some("23503"),
            "db error: ERROR: insert or update violates foreign key constraint",
        );
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
        assert!(!error.to_string().contains(CONTENT_CONFLICT));
        assert!(error.driver().contains("foreign key"));
    }

    #[test]
    fn a_unique_violation_from_elsewhere_is_not_the_content_conflict() {
        // The coordinate is `ON CONFLICT DO NOTHING`, so a 23505 that does not
        // carry the routine's own message came from somewhere else entirely.
        let error = translate_failure(
            &attestation(),
            Some("23505"),
            "db error: ERROR: duplicate key value violates unique constraint \"catalogs_pkey\"",
        );
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
    }

    #[test]
    fn a_failure_that_never_reached_the_server_is_storage() {
        let error = translate_failure(&attestation(), None, "connection closed");
        assert_eq!(error.kind(), AttestationErrorKind::Storage);
    }
}
