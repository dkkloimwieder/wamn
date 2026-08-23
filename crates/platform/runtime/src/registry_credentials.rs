//! Exact registry credentials loaded from a Kubernetes pull-secret projection.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Stable classification of a refused registry credential file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryCredentialsErrorKind {
    /// The projected credential file could not be read.
    Unreadable,
    /// The file did not carry one complete credential for the expected registry.
    Rejected,
}

#[derive(Debug)]
enum RegistryCredentialsErrorSource {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for RegistryCredentialsErrorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(formatter),
            Self::Json(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for RegistryCredentialsErrorSource {}

/// Contextual refusal from the registry credential boundary.
#[derive(Debug)]
pub struct RegistryCredentialsError {
    kind: RegistryCredentialsErrorKind,
    path: PathBuf,
    registry: Box<str>,
    refusal: &'static str,
    source: Option<RegistryCredentialsErrorSource>,
}

impl RegistryCredentialsError {
    /// Stable refusal class for startup and command boundaries.
    pub fn kind(&self) -> RegistryCredentialsErrorKind {
        self.kind
    }

    /// Stable literal naming the rejected invariant.
    pub fn refusal(&self) -> &'static str {
        self.refusal
    }

    fn unreadable(path: &Path, registry: &str, source: std::io::Error) -> Self {
        Self {
            kind: RegistryCredentialsErrorKind::Unreadable,
            path: path.to_owned(),
            registry: registry.into(),
            refusal: "registry-credentials-unreadable",
            source: Some(RegistryCredentialsErrorSource::Io(source)),
        }
    }

    fn malformed(path: &Path, registry: &str, source: serde_json::Error) -> Self {
        Self {
            kind: RegistryCredentialsErrorKind::Rejected,
            path: path.to_owned(),
            registry: registry.into(),
            refusal: "registry-credentials-malformed",
            source: Some(RegistryCredentialsErrorSource::Json(source)),
        }
    }

    fn rejected(path: &Path, registry: &str, refusal: &'static str) -> Self {
        Self {
            kind: RegistryCredentialsErrorKind::Rejected,
            path: path.to_owned(),
            registry: registry.into(),
            refusal,
            source: None,
        }
    }
}

impl fmt::Display for RegistryCredentialsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "registry credential file {} for {}: {}",
            self.path.display(),
            self.registry,
            self.refusal
        )
    }
}

impl std::error::Error for RegistryCredentialsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// One complete HTTP Basic credential for an exact OCI registry authority.
#[derive(Clone, PartialEq, Eq)]
pub struct RegistryCredentials {
    username: Box<str>,
    password: Box<str>,
}

impl RegistryCredentials {
    /// Registry username supplied to the OCI transport.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Registry password supplied to the OCI transport.
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for RegistryCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistryCredentials")
            .field("username", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
struct DockerConfig {
    auths: BTreeMap<String, DockerCredential>,
}

#[derive(Deserialize)]
struct DockerCredential {
    username: Option<String>,
    password: Option<String>,
}

/// Read one exact registry entry from a projected `.dockerconfigjson` file.
///
/// The registry key must equal the authority used by artifact references. No
/// Docker Hub fallback, URL normalization, credential helper, or wildcard is
/// admitted at this production boundary.
pub fn read_registry_credentials(
    path: &Path,
    registry: &str,
) -> Result<RegistryCredentials, RegistryCredentialsError> {
    let bytes = std::fs::read(path)
        .map_err(|source| RegistryCredentialsError::unreadable(path, registry, source))?;
    parse_registry_credentials(&bytes, path, registry)
}

fn parse_registry_credentials(
    bytes: &[u8],
    path: &Path,
    registry: &str,
) -> Result<RegistryCredentials, RegistryCredentialsError> {
    let config: DockerConfig = serde_json::from_slice(bytes)
        .map_err(|source| RegistryCredentialsError::malformed(path, registry, source))?;
    let credential = config.auths.get(registry).ok_or_else(|| {
        RegistryCredentialsError::rejected(path, registry, "registry-credentials-not-found")
    })?;
    let username = credential
        .username
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RegistryCredentialsError::rejected(path, registry, "registry-credentials-incomplete")
        })?;
    let password = credential
        .password
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RegistryCredentialsError::rejected(path, registry, "registry-credentials-incomplete")
        })?;
    Ok(RegistryCredentials {
        username: username.into(),
        password: password.into(),
    })
}
