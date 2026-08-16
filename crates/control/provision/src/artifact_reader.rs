//! Strict credential and role-marker contracts for the control artifact reader.

use std::fmt;
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use wamn_run_state::CredentialGeneration;

use crate::name::{
    artifact_reader_generation_role, artifact_reader_tenant_role, validate_project_env,
};

/// Frozen document identity for the private artifact-reader credential.
pub const ARTIFACT_READER_CREDENTIAL_SCHEMA_VERSION: &str = "0.1";
/// Only key carried by the fixed-mount artifact-reader Secret.
pub const ARTIFACT_READER_CREDENTIAL_KEY: &str = "credential.json";
/// Stable absolute path reserved for the future executor-side loader.
pub const ARTIFACT_READER_CREDENTIAL_PATH: &str = "/etc/wamn/artifact-reader/credential.json";
/// Application name that proves a replacement executor pool is using a generation.
pub const ARTIFACT_READER_APPLICATION_NAME: &str = "wamn-executor-artifact-reader";
/// Distinct application name used by ctl while verifying a prepared generation.
pub const ARTIFACT_READER_VERIFY_APPLICATION_NAME: &str = "wamn-ctl-artifact-reader-verify";
/// Frozen identity for non-secret PostgreSQL role ownership comments.
pub const ARTIFACT_READER_ROLE_MARKER_SCHEMA: &str = "wamn.artifact-reader.role-marker.v0.1";

/// Tenant and control-database identity carried by the stable ACL role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReaderTenantScope {
    pub tenant_id: String,
    pub database: String,
}

/// Exact project-environment deployment scope bound into a reader credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReaderCredentialScope {
    pub tenant_id: String,
    pub org: String,
    pub project: String,
    pub environment: String,
    pub database: String,
}

/// Trusted control endpoint against which a credential URL is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReaderEndpoint {
    pub host: String,
    pub port: u16,
}

impl ArtifactReaderCredentialScope {
    /// Return the stable tenant/database authority scope for this deployment.
    pub fn tenant_scope(&self) -> ArtifactReaderTenantScope {
        ArtifactReaderTenantScope {
            tenant_id: self.tenant_id.clone(),
            database: self.database.clone(),
        }
    }
}

/// Validity window rendered into an artifact-reader credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReaderCredentialValidity {
    pub issued_at: String,
    pub not_before: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
}

/// Strict private artifact-reader credential document.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ArtifactReaderCredential {
    schema_version: String,
    credential_id: String,
    generation: CredentialGeneration,
    role: String,
    tenant_id: String,
    org: String,
    project: String,
    environment: String,
    database: String,
    issued_at: String,
    not_before: String,
    expires_at: String,
    revoked_at: Option<String>,
    url: String,
}

impl fmt::Debug for ArtifactReaderCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactReaderCredential")
            .field("schema_version", &self.schema_version)
            .field("credential_id", &self.credential_id)
            .field("generation", &self.generation)
            .field("role", &self.role)
            .field("tenant_id", &self.tenant_id)
            .field("org", &self.org)
            .field("project", &self.project)
            .field("environment", &self.environment)
            .field("database", &self.database)
            .field("issued_at", &self.issued_at)
            .field("not_before", &self.not_before)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .field("url", &"<redacted>")
            .finish()
    }
}

impl ArtifactReaderCredential {
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }

    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn org(&self) -> &str {
        &self.org
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    pub fn database(&self) -> &str {
        &self.database
    }
}

/// Category of strict artifact-reader credential or marker rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactReaderCredentialErrorKind {
    Document,
    Identity,
    Scope,
    Validity,
    Url,
    Marker,
}

/// Contextual strict credential error; messages never include secret URL material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReaderCredentialError {
    kind: ArtifactReaderCredentialErrorKind,
    message: String,
}

impl ArtifactReaderCredentialError {
    fn new(kind: ArtifactReaderCredentialErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub const fn kind(&self) -> ArtifactReaderCredentialErrorKind {
        self.kind
    }
}

impl fmt::Display for ArtifactReaderCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ArtifactReaderCredentialError {}

/// Construct the exact credential document rendered by ctl.
pub fn artifact_reader_credential(
    scope: &ArtifactReaderCredentialScope,
    credential_id: &str,
    generation: CredentialGeneration,
    validity: &ArtifactReaderCredentialValidity,
    url: &str,
) -> ArtifactReaderCredential {
    ArtifactReaderCredential {
        schema_version: ARTIFACT_READER_CREDENTIAL_SCHEMA_VERSION.to_string(),
        credential_id: credential_id.to_string(),
        generation,
        role: artifact_reader_generation_role(
            &scope.tenant_id,
            &scope.org,
            &scope.project,
            &scope.environment,
            &scope.database,
            generation,
        ),
        tenant_id: scope.tenant_id.clone(),
        org: scope.org.clone(),
        project: scope.project.clone(),
        environment: scope.environment.clone(),
        database: scope.database.clone(),
        issued_at: validity.issued_at.clone(),
        not_before: validity.not_before.clone(),
        expires_at: validity.expires_at.clone(),
        revoked_at: validity.revoked_at.clone(),
        url: url.to_string(),
    }
}

/// Parse a credential document, rejecting unknown fields and malformed JSON.
pub fn parse_artifact_reader_credential(
    bytes: &[u8],
) -> Result<ArtifactReaderCredential, ArtifactReaderCredentialError> {
    serde_json::from_slice(bytes).map_err(|error| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Document,
            format!("invalid artifact-reader credential document: {error}"),
        )
    })
}

/// Extract a non-secret endpoint identity from a trusted control URL.
pub fn artifact_reader_endpoint(
    value: &str,
) -> Result<ArtifactReaderEndpoint, ArtifactReaderCredentialError> {
    let url = Url::parse(value).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Url,
            "artifact-reader control URL is invalid",
        )
    })?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Url,
            "artifact-reader control URL must use postgres or postgresql",
        ));
    }
    let host = url.host_str().ok_or_else(|| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Url,
            "artifact-reader control URL has no host",
        )
    })?;
    let port = url.port().unwrap_or(5432);
    Ok(ArtifactReaderEndpoint {
        host: host.to_string(),
        port,
    })
}

/// Reject empty, padded, or NUL-bearing persisted scope identities.
pub fn validate_artifact_reader_scope(
    scope: &ArtifactReaderCredentialScope,
) -> Result<(), ArtifactReaderCredentialError> {
    for (value, field) in [
        (&scope.tenant_id, "tenant-id"),
        (&scope.org, "org"),
        (&scope.project, "project"),
        (&scope.environment, "environment"),
        (&scope.database, "database"),
    ] {
        if value.is_empty() || value.trim() != value || value.as_bytes().contains(&0) {
            return Err(ArtifactReaderCredentialError::new(
                ArtifactReaderCredentialErrorKind::Scope,
                format!("artifact-reader credential {field} is not canonical"),
            ));
        }
    }
    Ok(())
}

/// Validate identity, exact scope, validity, role, and URL authority.
pub fn validate_artifact_reader_credential(
    credential: &ArtifactReaderCredential,
    expected: &ArtifactReaderCredentialScope,
    expected_endpoint: &ArtifactReaderEndpoint,
    now: SystemTime,
) -> Result<(), ArtifactReaderCredentialError> {
    if credential.schema_version != ARTIFACT_READER_CREDENTIAL_SCHEMA_VERSION {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Identity,
            "artifact-reader credential schema-version must be 0.1",
        ));
    }
    if credential.credential_id.len() != 32 || !is_lower_hex(&credential.credential_id) {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Identity,
            "artifact-reader credential-id must be 32 lowercase hex digits",
        ));
    }
    validate_artifact_reader_scope(expected)?;
    if credential.tenant_id != expected.tenant_id
        || credential.org != expected.org
        || credential.project != expected.project
        || credential.environment != expected.environment
        || credential.database != expected.database
    {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Scope,
            "artifact-reader credential scope does not match the executor deployment",
        ));
    }
    let expected_role = artifact_reader_generation_role(
        &expected.tenant_id,
        &expected.org,
        &expected.project,
        &expected.environment,
        &expected.database,
        credential.generation,
    );
    if credential.role != expected_role {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Identity,
            "artifact-reader credential role does not match its scoped generation",
        ));
    }

    let issued_at = parse_utc(&credential.issued_at, "issued-at")?;
    let not_before = parse_utc(&credential.not_before, "not-before")?;
    let expires_at = parse_utc(&credential.expires_at, "expires-at")?;
    if let Some(revoked_at) = &credential.revoked_at {
        parse_utc(revoked_at, "revoked-at")?;
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Validity,
            "artifact-reader credential is revoked",
        ));
    }
    if issued_at > not_before || not_before >= expires_at {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Validity,
            "artifact-reader credential validity window is inconsistent",
        ));
    }
    let now = DateTime::<Utc>::from(now);
    if now < not_before {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Validity,
            "artifact-reader credential is not yet valid",
        ));
    }
    if now >= expires_at {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Validity,
            "artifact-reader credential is expired",
        ));
    }

    let url = Url::parse(&credential.url).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Url,
            "artifact-reader credential URL is invalid",
        )
    })?;
    if !matches!(url.scheme(), "postgres" | "postgresql")
        || url.username() != expected_role
        || url
            .password()
            .is_none_or(|password| password.len() != 64 || !is_lower_hex(password))
        || url.host_str() != Some(expected_endpoint.host.as_str())
        || url.port().unwrap_or(5432) != expected_endpoint.port
        || url.path() != format!("/{}", expected.database)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Url,
            "artifact-reader credential URL authority does not match its role and database",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct TenantRoleMarker {
    schema_version: String,
    kind: String,
    role: String,
    tenant_id: String,
    database: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct GenerationRoleMarker {
    schema_version: String,
    kind: String,
    role: String,
    generation: CredentialGeneration,
    tenant_id: String,
    org: String,
    project: String,
    environment: String,
    database: String,
}

/// Serialize the non-secret owner marker for one stable tenant ACL role.
pub fn artifact_reader_tenant_role_marker(scope: &ArtifactReaderTenantScope) -> String {
    serde_json::to_string(&TenantRoleMarker {
        schema_version: ARTIFACT_READER_ROLE_MARKER_SCHEMA.to_string(),
        kind: "tenant-authority".to_string(),
        role: artifact_reader_tenant_role(&scope.tenant_id, &scope.database),
        tenant_id: scope.tenant_id.clone(),
        database: scope.database.clone(),
    })
    .expect("artifact-reader tenant role marker serializes")
}

/// Serialize the non-secret owner marker for one scoped generation role.
pub fn artifact_reader_generation_role_marker(
    scope: &ArtifactReaderCredentialScope,
    generation: CredentialGeneration,
) -> String {
    serde_json::to_string(&GenerationRoleMarker {
        schema_version: ARTIFACT_READER_ROLE_MARKER_SCHEMA.to_string(),
        kind: "generation".to_string(),
        role: artifact_reader_generation_role(
            &scope.tenant_id,
            &scope.org,
            &scope.project,
            &scope.environment,
            &scope.database,
            generation,
        ),
        generation,
        tenant_id: scope.tenant_id.clone(),
        org: scope.org.clone(),
        project: scope.project.clone(),
        environment: scope.environment.clone(),
        database: scope.database.clone(),
    })
    .expect("artifact-reader generation role marker serializes")
}

/// Validate the exact stable-role marker for a known tenant scope.
pub fn validate_artifact_reader_tenant_role_marker(
    marker: &str,
    expected: &ArtifactReaderTenantScope,
) -> Result<(), ArtifactReaderCredentialError> {
    let actual: TenantRoleMarker = serde_json::from_str(marker).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader tenant role marker is invalid",
        )
    })?;
    let expected_marker = TenantRoleMarker {
        schema_version: ARTIFACT_READER_ROLE_MARKER_SCHEMA.to_string(),
        kind: "tenant-authority".to_string(),
        role: artifact_reader_tenant_role(&expected.tenant_id, &expected.database),
        tenant_id: expected.tenant_id.clone(),
        database: expected.database.clone(),
    };
    if actual != expected_marker {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader tenant role marker does not match its authority scope",
        ));
    }
    Ok(())
}

/// Validate a generation-role marker against trusted caller-supplied scope.
pub fn validate_artifact_reader_generation_role_marker(
    marker: &str,
    expected: &ArtifactReaderCredentialScope,
    expected_generation: CredentialGeneration,
) -> Result<(), ArtifactReaderCredentialError> {
    let actual: GenerationRoleMarker = serde_json::from_str(marker).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker is invalid",
        )
    })?;
    let expected_marker = GenerationRoleMarker {
        schema_version: ARTIFACT_READER_ROLE_MARKER_SCHEMA.to_string(),
        kind: "generation".to_string(),
        role: artifact_reader_generation_role(
            &expected.tenant_id,
            &expected.org,
            &expected.project,
            &expected.environment,
            &expected.database,
            expected_generation,
        ),
        generation: expected_generation,
        tenant_id: expected.tenant_id.clone(),
        org: expected.org.clone(),
        project: expected.project.clone(),
        environment: expected.environment.clone(),
        database: expected.database.clone(),
    };
    if actual != expected_marker {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker does not match its scoped role",
        ));
    }
    Ok(())
}

/// Validate an active stable-role child without deriving a mutation target.
///
/// The trusted tenant/database scope and observed role name are supplied by the
/// caller. Marker-owned project fields are used only to recompute that exact
/// observed name; they are never returned for cleanup or SQL construction.
pub fn validate_artifact_reader_tenant_child_role_marker(
    marker: &str,
    expected_tenant: &ArtifactReaderTenantScope,
    observed_role: &str,
) -> Result<(), ArtifactReaderCredentialError> {
    let actual: GenerationRoleMarker = serde_json::from_str(marker).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker is invalid",
        )
    })?;
    let marker_scope = ArtifactReaderCredentialScope {
        tenant_id: actual.tenant_id.clone(),
        org: actual.org.clone(),
        project: actual.project.clone(),
        environment: actual.environment.clone(),
        database: actual.database.clone(),
    };
    validate_artifact_reader_scope(&marker_scope).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker carries a noncanonical scope",
        )
    })?;
    validate_project_env(&actual.org, &actual.project, &actual.environment).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker carries an invalid project environment",
        )
    })?;
    let recomputed = artifact_reader_generation_role(
        &actual.tenant_id,
        &actual.org,
        &actual.project,
        &actual.environment,
        &actual.database,
        actual.generation,
    );
    if actual.schema_version != ARTIFACT_READER_ROLE_MARKER_SCHEMA
        || actual.kind != "generation"
        || actual.tenant_id != expected_tenant.tenant_id
        || actual.database != expected_tenant.database
        || actual.role != observed_role
        || recomputed != observed_role
    {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker is outside its tenant authority",
        ));
    }
    Ok(())
}

fn parse_utc(
    value: &str,
    field: &'static str,
) -> Result<DateTime<Utc>, ArtifactReaderCredentialError> {
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| {
        ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Validity,
            format!("artifact-reader credential {field} is not RFC3339 UTC"),
        )
    })?;
    let utc = parsed.with_timezone(&Utc);
    if parsed.offset().local_minus_utc() != 0
        || utc.to_rfc3339_opts(SecondsFormat::Secs, true) != value
    {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Validity,
            format!("artifact-reader credential {field} is not canonical RFC3339 UTC"),
        ));
    }
    Ok(utc)
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn scope() -> ArtifactReaderCredentialScope {
        ArtifactReaderCredentialScope {
            tenant_id: "tenant-a".to_string(),
            org: "acme".to_string(),
            project: "billing".to_string(),
            environment: "dev".to_string(),
            database: "wamn_system".to_string(),
        }
    }

    fn credential() -> ArtifactReaderCredential {
        let scope = scope();
        let role = artifact_reader_generation_role(
            &scope.tenant_id,
            &scope.org,
            &scope.project,
            &scope.environment,
            &scope.database,
            CredentialGeneration::A,
        );
        artifact_reader_credential(
            &scope,
            "0123456789abcdef0123456789abcdef",
            CredentialGeneration::A,
            &ArtifactReaderCredentialValidity {
                issued_at: "2026-01-01T00:00:00Z".to_string(),
                not_before: "2026-01-01T00:00:00Z".to_string(),
                expires_at: "2026-02-01T00:00:00Z".to_string(),
                revoked_at: None,
            },
            &format!(
                "postgres://{role}:{}@wamn-sysdb-rw:5432/wamn_system",
                "a".repeat(64)
            ),
        )
    }

    fn endpoint() -> ArtifactReaderEndpoint {
        artifact_reader_endpoint("postgres://admin@wamn-sysdb-rw:5432/wamn_system").unwrap()
    }

    #[test]
    fn credential_round_trips_and_redacts_secret_material() {
        let credential = credential();
        let bytes = serde_json::to_vec(&credential).unwrap();
        let parsed = parse_artifact_reader_credential(&bytes).unwrap();
        validate_artifact_reader_credential(
            &parsed,
            &scope(),
            &endpoint(),
            UNIX_EPOCH + Duration::from_secs(1_767_225_600),
        )
        .unwrap();
        let debug = format!("{parsed:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&"a".repeat(64)));
        assert!(!debug.contains("wamn-sysdb-rw"));

        let object = serde_json::to_value(&parsed)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            object,
            BTreeSet::from([
                "credential-id".to_string(),
                "database".to_string(),
                "environment".to_string(),
                "expires-at".to_string(),
                "generation".to_string(),
                "issued-at".to_string(),
                "not-before".to_string(),
                "org".to_string(),
                "project".to_string(),
                "revoked-at".to_string(),
                "role".to_string(),
                "schema-version".to_string(),
                "tenant-id".to_string(),
                "url".to_string(),
            ])
        );
    }

    #[test]
    fn credential_rejects_unknown_fields_and_scope_drift() {
        let mut value = serde_json::to_value(credential()).unwrap();
        value["extra"] = serde_json::json!(true);
        assert_eq!(
            parse_artifact_reader_credential(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .kind(),
            ArtifactReaderCredentialErrorKind::Document
        );

        let mut wrong = scope();
        wrong.tenant_id = "tenant-b".to_string();
        assert_eq!(
            validate_artifact_reader_credential(
                &credential(),
                &wrong,
                &endpoint(),
                UNIX_EPOCH + Duration::from_secs(1_767_225_600),
            )
            .unwrap_err()
            .kind(),
            ArtifactReaderCredentialErrorKind::Scope
        );
    }

    #[test]
    fn credential_url_is_bound_to_the_trusted_control_endpoint() {
        let credential = credential();
        for expected in [
            ArtifactReaderEndpoint {
                host: "attacker.example".to_string(),
                port: 5432,
            },
            ArtifactReaderEndpoint {
                host: "wamn-sysdb-rw".to_string(),
                port: 6432,
            },
        ] {
            assert_eq!(
                validate_artifact_reader_credential(
                    &credential,
                    &scope(),
                    &expected,
                    UNIX_EPOCH + Duration::from_secs(1_767_225_600),
                )
                .unwrap_err()
                .kind(),
                ArtifactReaderCredentialErrorKind::Url
            );
        }
        assert_eq!(
            artifact_reader_endpoint("postgres://admin@control.example/wamn_system").unwrap(),
            ArtifactReaderEndpoint {
                host: "control.example".to_string(),
                port: 5432,
            }
        );
    }

    #[test]
    fn credential_validity_is_canonical_and_closed() {
        let now = UNIX_EPOCH + Duration::from_secs(1_767_225_600);
        let base = serde_json::to_value(credential()).unwrap();
        for (field, value) in [
            ("issued-at", "2026-01-01T00:00:00+00:00"),
            ("not-before", "2026-01-01T00:00:00.000Z"),
            ("expires-at", "2026-01-01T00:00:00Z"),
            ("revoked-at", "invalid"),
        ] {
            let mut changed = base.clone();
            changed[field] = serde_json::json!(value);
            let parsed =
                parse_artifact_reader_credential(&serde_json::to_vec(&changed).unwrap()).unwrap();
            assert_eq!(
                validate_artifact_reader_credential(&parsed, &scope(), &endpoint(), now)
                    .unwrap_err()
                    .kind(),
                ArtifactReaderCredentialErrorKind::Validity,
                "accepted {field}={value:?}"
            );
        }

        for (not_before, expires_at, now) in [
            (
                "2026-02-01T00:00:00Z",
                "2026-03-01T00:00:00Z",
                UNIX_EPOCH + Duration::from_secs(1_767_225_600),
            ),
            (
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
                UNIX_EPOCH + Duration::from_secs(1_767_225_600),
            ),
        ] {
            let mut changed = base.clone();
            changed["not-before"] = serde_json::json!(not_before);
            changed["expires-at"] = serde_json::json!(expires_at);
            let parsed =
                parse_artifact_reader_credential(&serde_json::to_vec(&changed).unwrap()).unwrap();
            assert_eq!(
                validate_artifact_reader_credential(&parsed, &scope(), &endpoint(), now)
                    .unwrap_err()
                    .kind(),
                ArtifactReaderCredentialErrorKind::Validity
            );
        }
    }

    #[test]
    fn role_markers_are_closed_and_scope_bound() {
        let scope = scope();
        let tenant = scope.tenant_scope();
        let tenant_marker = artifact_reader_tenant_role_marker(&tenant);
        validate_artifact_reader_tenant_role_marker(&tenant_marker, &tenant).unwrap();
        let generation_marker =
            artifact_reader_generation_role_marker(&scope, CredentialGeneration::B);
        validate_artifact_reader_generation_role_marker(
            &generation_marker,
            &scope,
            CredentialGeneration::B,
        )
        .unwrap();
        assert!(
            validate_artifact_reader_generation_role_marker(
                &generation_marker.replace("tenant-a", "tenant-b"),
                &scope,
                CredentialGeneration::B,
            )
            .is_err()
        );
        assert!(
            validate_artifact_reader_generation_role_marker(
                &generation_marker,
                &scope,
                CredentialGeneration::A,
            )
            .is_err()
        );

        let mut extra: serde_json::Value = serde_json::from_str(&tenant_marker).unwrap();
        extra["extra"] = serde_json::json!(true);
        assert!(validate_artifact_reader_tenant_role_marker(&extra.to_string(), &tenant).is_err());
    }
}
