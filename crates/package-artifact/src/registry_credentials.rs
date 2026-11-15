//! Exact registry credentials: a Kubernetes pull-secret projection, or a token
//! from the GKE metadata server.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Stable classification of a refused registry credential file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryCredentialsErrorType {
    /// The projected credential file or the metadata server could not be read.
    Unreadable,
    /// The file did not carry one complete credential for the expected registry.
    Rejected,
}

#[derive(Debug)]
enum RegistryCredentialsErrorSource {
    Io(std::io::Error),
    Json(serde_json::Error),
    Http(anyhow::Error),
}

impl fmt::Display for RegistryCredentialsErrorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(formatter),
            Self::Json(source) => source.fmt(formatter),
            Self::Http(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for RegistryCredentialsErrorSource {}

/// Where a refused credential came from.
#[derive(Debug)]
enum CredentialOrigin {
    File(PathBuf),
    MetadataServer,
}

/// Contextual refusal from the registry credential boundary.
#[derive(Debug)]
pub struct RegistryCredentialsError {
    type_: RegistryCredentialsErrorType,
    origin: CredentialOrigin,
    registry: Box<str>,
    refusal: &'static str,
    source: Option<RegistryCredentialsErrorSource>,
}

impl RegistryCredentialsError {
    /// Stable refusal class for startup and command boundaries.
    pub fn error_type(&self) -> RegistryCredentialsErrorType {
        self.type_
    }

    /// Stable literal naming the rejected invariant.
    pub fn refusal(&self) -> &'static str {
        self.refusal
    }

    fn unreadable(path: &Path, registry: &str, source: std::io::Error) -> Self {
        Self {
            type_: RegistryCredentialsErrorType::Unreadable,
            origin: CredentialOrigin::File(path.to_owned()),
            registry: registry.into(),
            refusal: "registry-credentials-unreadable",
            source: Some(RegistryCredentialsErrorSource::Io(source)),
        }
    }

    fn malformed(path: &Path, registry: &str, source: serde_json::Error) -> Self {
        Self {
            type_: RegistryCredentialsErrorType::Rejected,
            origin: CredentialOrigin::File(path.to_owned()),
            registry: registry.into(),
            refusal: "registry-credentials-malformed",
            source: Some(RegistryCredentialsErrorSource::Json(source)),
        }
    }

    fn rejected(path: &Path, registry: &str, refusal: &'static str) -> Self {
        Self {
            type_: RegistryCredentialsErrorType::Rejected,
            origin: CredentialOrigin::File(path.to_owned()),
            registry: registry.into(),
            refusal,
            source: None,
        }
    }

    fn metadata(
        registry: &str,
        type_: RegistryCredentialsErrorType,
        refusal: &'static str,
        source: Option<RegistryCredentialsErrorSource>,
    ) -> Self {
        Self {
            type_,
            origin: CredentialOrigin::MetadataServer,
            registry: registry.into(),
            refusal,
            source,
        }
    }
}

impl fmt::Display for RegistryCredentialsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            CredentialOrigin::File(path) => write!(
                formatter,
                "registry credential file {} for {}: {}",
                path.display(),
                self.registry,
                self.refusal
            ),
            CredentialOrigin::MetadataServer => write!(
                formatter,
                "registry token from the metadata server for {}: {}",
                self.registry, self.refusal
            ),
        }
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
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl DockerCredential {
    /// `{}`: the entry names the registry and carries no field at all.
    fn is_empty(&self) -> bool {
        self.username.is_none() && self.password.is_none() && self.other.is_empty()
    }
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
    let credential = read_registry_entry(path, registry)?;
    complete_credentials(&credential, path, registry)
}

/// Read the push credential for one exact registry, or `None` for an anonymous push.
///
/// Only an explicit empty entry, `{"auths":{"<registry>":{}}}`, states an
/// anonymous push. A missing entry or a partial one refuses as in
/// [`read_registry_credentials`]. Pulls never read an empty entry as anonymous.
pub fn read_registry_push_credentials(
    path: &Path,
    registry: &str,
) -> Result<Option<RegistryCredentials>, RegistryCredentialsError> {
    let credential = read_registry_entry(path, registry)?;
    if credential.is_empty() {
        return Ok(None);
    }
    complete_credentials(&credential, path, registry).map(Some)
}

fn read_registry_entry(
    path: &Path,
    registry: &str,
) -> Result<DockerCredential, RegistryCredentialsError> {
    let bytes = std::fs::read(path)
        .map_err(|source| RegistryCredentialsError::unreadable(path, registry, source))?;
    let mut config: DockerConfig = serde_json::from_slice(&bytes)
        .map_err(|source| RegistryCredentialsError::malformed(path, registry, source))?;
    config.auths.remove(registry).ok_or_else(|| {
        RegistryCredentialsError::rejected(path, registry, "registry-credentials-not-found")
    })
}

fn complete_credentials(
    credential: &DockerCredential,
    path: &Path,
    registry: &str,
) -> Result<RegistryCredentials, RegistryCredentialsError> {
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

/// The variable Google's client libraries read for the metadata server's host.
pub const METADATA_HOST_ENV: &str = "GCE_METADATA_HOST";

/// Ask the shared metadata service for this registry's access token.
pub async fn read_metadata_registry_credentials(
    registry: &str,
) -> Result<RegistryCredentials, RegistryCredentialsError> {
    let unavailable = |source: anyhow::Error| {
        let token = source.downcast_ref::<crate::metadata::InvalidToken>();
        let malformed = token.is_some_and(|error| !error.incomplete);
        let incomplete = token.is_some_and(|error| error.incomplete);
        RegistryCredentialsError::metadata(
            registry,
            if malformed || incomplete {
                RegistryCredentialsErrorType::Rejected
            } else {
                RegistryCredentialsErrorType::Unreadable
            },
            if malformed {
                "registry-token-metadata-malformed"
            } else if incomplete {
                "registry-token-metadata-incomplete"
            } else {
                "registry-token-metadata-unavailable"
            },
            Some(RegistryCredentialsErrorSource::Http(source)),
        )
    };
    let metadata = crate::metadata::Metadata::shared().map_err(unavailable)?;
    let token = metadata.token().await.map_err(unavailable)?;
    Ok(RegistryCredentials {
        username: "oauth2accesstoken".into(),
        password: token.as_ref().into(),
    })
}

/// Where a registry source gets the credential of each pull.
#[derive(Clone, PartialEq, Eq)]
pub enum RegistryCredentialSource {
    /// One credential, read once from a projected Docker config file.
    Fixed(RegistryCredentials),
    /// An access token from the shared metadata service.
    MetadataServer,
}

impl RegistryCredentialSource {
    /// The credential for one pull from `registry`.
    pub async fn credentials(
        &self,
        registry: &str,
    ) -> Result<RegistryCredentials, RegistryCredentialsError> {
        match self {
            Self::Fixed(credentials) => Ok(credentials.clone()),
            Self::MetadataServer => read_metadata_registry_credentials(registry).await,
        }
    }
}

impl fmt::Debug for RegistryCredentialSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(credentials) => formatter.debug_tuple("Fixed").field(credentials).finish(),
            Self::MetadataServer => formatter.write_str("MetadataServer"),
        }
    }
}
