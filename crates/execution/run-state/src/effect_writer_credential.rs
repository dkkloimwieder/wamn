//! Strict shared credential document for the private effect-ledger writer.

use std::fmt;
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use url::Url;

/// Frozen document identity for the private effect-writer credential.
pub const EFFECT_WRITER_CREDENTIAL_SCHEMA_VERSION: &str = "0.1";
/// Only key carried by the fixed-mount effect-writer Secret.
pub const EFFECT_WRITER_CREDENTIAL_KEY: &str = "credential.json";
/// Stable absolute path read by the executor's private loader.
pub const EFFECT_WRITER_CREDENTIAL_PATH: &str = "/etc/wamn/effect-writer/credential.json";
/// Stable NOLOGIN role carrying ledger authority and narrow fenced-run reads.
pub const EFFECT_WRITER_ROLE: &str = "wamn_effect_writer";
/// Legacy role name accepted only while preserving existing generation memberships.
pub const RUN_PROJECTION_WRITER_ROLE: &str = "wamn_run_projection_writer";

const EFFECT_WRITER_SCOPE_DOMAIN: &[u8] = b"wamn.effect-writer.scope.v0.1";
const EFFECT_WRITER_SCOPE_HASH_HEX_LEN: usize = 40;

/// One of the two reusable credential-generation slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialGeneration {
    A,
    B,
}

impl CredentialGeneration {
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

impl std::str::FromStr for CredentialGeneration {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "a" => Ok(Self::A),
            "b" => Ok(Self::B),
            _ => Err("credential generation must be a or b"),
        }
    }
}

/// Exact project-environment scope bound into an effect-writer credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectWriterCredentialScope {
    pub org: String,
    pub project: String,
    pub environment: String,
    pub database: String,
}

/// Validity window rendered into an effect-writer credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectWriterCredentialValidity {
    pub issued_at: String,
    pub not_before: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
}

/// Strict private effect-writer credential document.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EffectWriterCredential {
    schema_version: String,
    credential_id: String,
    generation: CredentialGeneration,
    role: String,
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

impl fmt::Debug for EffectWriterCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectWriterCredential")
            .field("schema_version", &self.schema_version)
            .field("credential_id", &self.credential_id)
            .field("generation", &self.generation)
            .field("role", &self.role)
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

impl EffectWriterCredential {
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }

    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    #[cfg(feature = "native")]
    pub(crate) fn url(&self) -> &str {
        &self.url
    }
}

/// Category of strict effect-writer credential rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectWriterCredentialErrorKind {
    Document,
    Identity,
    Scope,
    Validity,
    Url,
}

/// Contextual strict credential error; messages never include the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectWriterCredentialError {
    kind: EffectWriterCredentialErrorKind,
    message: String,
}

impl EffectWriterCredentialError {
    fn new(kind: EffectWriterCredentialErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub const fn kind(&self) -> EffectWriterCredentialErrorKind {
        self.kind
    }
}

impl fmt::Display for EffectWriterCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EffectWriterCredentialError {}

/// Construct the exact credential document rendered by ctl.
pub fn effect_writer_credential(
    scope: &EffectWriterCredentialScope,
    credential_id: &str,
    generation: CredentialGeneration,
    validity: &EffectWriterCredentialValidity,
    url: &str,
) -> EffectWriterCredential {
    EffectWriterCredential {
        schema_version: EFFECT_WRITER_CREDENTIAL_SCHEMA_VERSION.to_string(),
        credential_id: credential_id.to_string(),
        generation,
        role: effect_writer_generation_role(
            &scope.org,
            &scope.project,
            &scope.environment,
            &scope.database,
            generation,
        ),
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
pub fn parse_effect_writer_credential(
    bytes: &[u8],
) -> Result<EffectWriterCredential, EffectWriterCredentialError> {
    serde_json::from_slice(bytes).map_err(|error| {
        EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Document,
            format!("invalid effect-writer credential document: {error}"),
        )
    })
}

/// Validate identity, exact scope, validity, role, and URL authority.
pub fn validate_effect_writer_credential(
    credential: &EffectWriterCredential,
    expected: &EffectWriterCredentialScope,
    now: SystemTime,
) -> Result<(), EffectWriterCredentialError> {
    if credential.schema_version != EFFECT_WRITER_CREDENTIAL_SCHEMA_VERSION {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Identity,
            "effect-writer credential schema-version must be 0.1",
        ));
    }
    if credential.credential_id.len() != 32 || !is_lower_hex(&credential.credential_id) {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Identity,
            "effect-writer credential-id must be 32 lowercase hex digits",
        ));
    }
    let expected_role = effect_writer_generation_role(
        &expected.org,
        &expected.project,
        &expected.environment,
        &expected.database,
        credential.generation,
    );
    if credential.role != expected_role {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Identity,
            "effect-writer credential role does not match its scoped generation",
        ));
    }
    if credential.org != expected.org
        || credential.project != expected.project
        || credential.environment != expected.environment
        || credential.database != expected.database
    {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Scope,
            "effect-writer credential scope does not match the host identity",
        ));
    }

    let issued_at = parse_utc(&credential.issued_at, "issued-at")?;
    let not_before = parse_utc(&credential.not_before, "not-before")?;
    let expires_at = parse_utc(&credential.expires_at, "expires-at")?;
    if let Some(revoked_at) = &credential.revoked_at {
        parse_utc(revoked_at, "revoked-at")?;
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Validity,
            "effect-writer credential is revoked",
        ));
    }
    if issued_at > not_before || not_before >= expires_at {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Validity,
            "effect-writer credential validity window is inconsistent",
        ));
    }
    let now = DateTime::<Utc>::from(now);
    if now < not_before {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Validity,
            "effect-writer credential is not yet valid",
        ));
    }
    if now >= expires_at {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Validity,
            "effect-writer credential is expired",
        ));
    }

    let url = Url::parse(&credential.url).map_err(|_| {
        EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Url,
            "effect-writer credential URL is invalid",
        )
    })?;
    if !matches!(url.scheme(), "postgres" | "postgresql")
        || url.username() != expected_role
        || url
            .password()
            .is_none_or(|password| password.len() != 64 || !is_lower_hex(password))
        || url.host_str().is_none()
        || url.path() != format!("/{}", expected.database)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Url,
            "effect-writer credential URL authority does not match its role and database",
        ));
    }
    Ok(())
}

/// Deterministic 160-bit suffix for one project environment.
pub fn effect_writer_scope_hash(
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
) -> String {
    let mut preimage = Vec::new();
    frame(&mut preimage, EFFECT_WRITER_SCOPE_DOMAIN);
    for (tag, value) in [
        ("org", org),
        ("project", project),
        ("environment", environment),
        ("database", database),
    ] {
        frame(&mut preimage, tag.as_bytes());
        frame(&mut preimage, value.as_bytes());
    }
    let digest = hex::encode(Sha256::digest(preimage));
    digest[..EFFECT_WRITER_SCOPE_HASH_HEX_LEN].to_string()
}

/// Scoped PostgreSQL LOGIN role for one credential-generation slot.
pub fn effect_writer_generation_role(
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
) -> String {
    format!(
        "{EFFECT_WRITER_ROLE}_{}_{}",
        effect_writer_scope_hash(org, project, environment, database),
        generation.as_str()
    )
}

fn parse_utc(
    value: &str,
    field: &'static str,
) -> Result<DateTime<Utc>, EffectWriterCredentialError> {
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| {
        EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Validity,
            format!("effect-writer credential {field} is not RFC3339 UTC"),
        )
    })?;
    let utc = parsed.with_timezone(&Utc);
    if parsed.offset().local_minus_utc() != 0
        || utc.to_rfc3339_opts(SecondsFormat::Secs, true) != value
    {
        return Err(EffectWriterCredentialError::new(
            EffectWriterCredentialErrorKind::Validity,
            format!("effect-writer credential {field} is not canonical RFC3339 UTC"),
        ));
    }
    Ok(utc)
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn frame(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("identity field length fits u64");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        CredentialGeneration, EffectWriterCredentialErrorKind, EffectWriterCredentialScope,
        EffectWriterCredentialValidity, effect_writer_credential, effect_writer_generation_role,
        parse_effect_writer_credential, validate_effect_writer_credential,
    };

    fn scope() -> EffectWriterCredentialScope {
        EffectWriterCredentialScope {
            org: "org".to_string(),
            project: "project".to_string(),
            environment: "production".to_string(),
            database: "wamn-db-org--project--production".to_string(),
        }
    }

    fn document() -> Vec<u8> {
        let scope = scope();
        let role = effect_writer_generation_role(
            &scope.org,
            &scope.project,
            &scope.environment,
            &scope.database,
            CredentialGeneration::A,
        );
        serde_json::to_vec(&effect_writer_credential(
            &scope,
            "0123456789abcdef0123456789abcdef",
            CredentialGeneration::A,
            &EffectWriterCredentialValidity {
                issued_at: "2020-01-01T00:00:00Z".to_string(),
                not_before: "2020-01-01T00:00:00Z".to_string(),
                expires_at: "2099-01-01T00:00:00Z".to_string(),
                revoked_at: None,
            },
            &format!(
                "postgres://{role}:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef@db:5432/{}",
                scope.database
            ),
        ))
        .expect("serialize credential")
    }

    #[test]
    fn strict_document_validates_exact_scope_and_authority() {
        let bytes = document();
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("credential JSON");
        assert!(value.get("schema").is_none());
        let credential = parse_effect_writer_credential(&bytes).expect("strict document");
        let now = UNIX_EPOCH + Duration::from_secs(1_893_456_000);
        validate_effect_writer_credential(&credential, &scope(), now).expect("valid credential");
        assert!(credential.role().starts_with("wamn_effect_writer_"));
        assert_eq!(credential.role().len(), 61);
        assert!(credential.role().ends_with("_a"));
    }

    #[test]
    fn unknown_field_and_noncanonical_time_refuse() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&document()).expect("credential JSON");
        value
            .as_object_mut()
            .expect("credential object")
            .insert("extra".to_string(), serde_json::Value::Bool(true));
        let error = parse_effect_writer_credential(
            &serde_json::to_vec(&value).expect("serialize drifted document"),
        )
        .expect_err("unknown field must refuse");
        assert_eq!(error.kind(), EffectWriterCredentialErrorKind::Document);

        value
            .as_object_mut()
            .expect("credential object")
            .remove("extra");
        value["issued-at"] = serde_json::Value::String("2020-01-01T00:00:00+00:00".to_string());
        let credential = parse_effect_writer_credential(
            &serde_json::to_vec(&value).expect("serialize time drift"),
        )
        .expect("time shape parses as a document");
        let now = UNIX_EPOCH + Duration::from_secs(1_893_456_000);
        let error = validate_effect_writer_credential(&credential, &scope(), now)
            .expect_err("noncanonical UTC time must refuse");
        assert_eq!(error.kind(), EffectWriterCredentialErrorKind::Validity);
    }

    #[test]
    fn mismatched_scope_and_expired_window_refuse() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&document()).expect("credential JSON");
        value["org"] = serde_json::Value::String("other-org".to_string());
        let mismatched = parse_effect_writer_credential(
            &serde_json::to_vec(&value).expect("serialize scope mismatch"),
        )
        .expect("scope mismatch remains a strict document");
        let now = UNIX_EPOCH + Duration::from_secs(1_893_456_000);
        let error = validate_effect_writer_credential(&mismatched, &scope(), now)
            .expect_err("mismatched host scope must refuse");
        assert_eq!(error.kind(), EffectWriterCredentialErrorKind::Scope);

        let credential = parse_effect_writer_credential(&document()).expect("strict document");
        let expired = UNIX_EPOCH + Duration::from_secs(4_200_000_000);
        let error = validate_effect_writer_credential(&credential, &scope(), expired)
            .expect_err("expired credential must refuse");
        assert_eq!(error.kind(), EffectWriterCredentialErrorKind::Validity);
        assert!(error.to_string().contains("expired"));
    }
}
