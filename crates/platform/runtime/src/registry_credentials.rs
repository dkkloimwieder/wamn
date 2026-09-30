//! Exact registry credentials: a Kubernetes pull-secret projection, or a token
//! from the GKE metadata server.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Stable classification of a refused registry credential file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryCredentialsErrorKind {
    /// The projected credential file or the metadata server could not be read.
    Unreadable,
    /// The file did not carry one complete credential for the expected registry.
    Rejected,
}

#[derive(Debug)]
enum RegistryCredentialsErrorSource {
    Io(std::io::Error),
    Json(serde_json::Error),
    Http(reqwest::Error),
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
    kind: RegistryCredentialsErrorKind,
    origin: CredentialOrigin,
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
            origin: CredentialOrigin::File(path.to_owned()),
            registry: registry.into(),
            refusal: "registry-credentials-unreadable",
            source: Some(RegistryCredentialsErrorSource::Io(source)),
        }
    }

    fn malformed(path: &Path, registry: &str, source: serde_json::Error) -> Self {
        Self {
            kind: RegistryCredentialsErrorKind::Rejected,
            origin: CredentialOrigin::File(path.to_owned()),
            registry: registry.into(),
            refusal: "registry-credentials-malformed",
            source: Some(RegistryCredentialsErrorSource::Json(source)),
        }
    }

    fn rejected(path: &Path, registry: &str, refusal: &'static str) -> Self {
        Self {
            kind: RegistryCredentialsErrorKind::Rejected,
            origin: CredentialOrigin::File(path.to_owned()),
            registry: registry.into(),
            refusal,
            source: None,
        }
    }

    fn metadata(
        registry: &str,
        kind: RegistryCredentialsErrorKind,
        refusal: &'static str,
        source: Option<RegistryCredentialsErrorSource>,
    ) -> Self {
        Self {
            kind,
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

/// The variable Google's client libraries read for the metadata server's host.
pub const METADATA_HOST_ENV: &str = "GCE_METADATA_HOST";

/// The metadata server's host when [`METADATA_HOST_ENV`] is unset or empty.
const METADATA_DEFAULT_HOST: &str = "metadata.google.internal";

/// The token path for the pod's Workload Identity.
const METADATA_TOKEN_PATH: &str = "/computeMetadata/v1/instance/service-accounts/default/token";

/// The username Artifact Registry takes with an OAuth access token.
const METADATA_TOKEN_USERNAME: &str = "oauth2accesstoken";

/// Bound the one request to the metadata server, which is on the node.
const METADATA_TOKEN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct MetadataToken {
    access_token: String,
}

/// Ask the GKE metadata server for a token, as the credential of `registry`.
///
/// This is the one token source of the host's readers and of
/// `docker-credential-wamn`. The server's host is [`METADATA_HOST_ENV`] when it
/// is set, as Google's client libraries read it, and `http://` and the token
/// path are fixed. Each call makes one request and keeps nothing: the metadata
/// server caches the token itself. `registry` only names the refusal.
pub async fn read_metadata_registry_credentials(
    registry: &str,
) -> Result<RegistryCredentials, RegistryCredentialsError> {
    let host = std::env::var(METADATA_HOST_ENV)
        .ok()
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| METADATA_DEFAULT_HOST.to_owned());
    let token_url = format!("http://{host}{METADATA_TOKEN_PATH}");
    let unavailable = |source| {
        RegistryCredentialsError::metadata(
            registry,
            RegistryCredentialsErrorKind::Unreadable,
            "registry-token-metadata-unavailable",
            Some(RegistryCredentialsErrorSource::Http(source)),
        )
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(METADATA_TOKEN_TIMEOUT)
        .build()
        .map_err(unavailable)?;
    let body = client
        .get(&token_url)
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(unavailable)?
        .bytes()
        .await
        .map_err(unavailable)?;
    let token: MetadataToken = serde_json::from_slice(&body).map_err(|source| {
        RegistryCredentialsError::metadata(
            registry,
            RegistryCredentialsErrorKind::Rejected,
            "registry-token-metadata-malformed",
            Some(RegistryCredentialsErrorSource::Json(source)),
        )
    })?;
    if token.access_token.is_empty() {
        return Err(RegistryCredentialsError::metadata(
            registry,
            RegistryCredentialsErrorKind::Rejected,
            "registry-token-metadata-incomplete",
            None,
        ));
    }
    Ok(RegistryCredentials {
        username: METADATA_TOKEN_USERNAME.into(),
        password: token.access_token.into(),
    })
}

/// Where a registry source gets the credential of each pull.
#[derive(Clone, PartialEq, Eq)]
pub enum RegistryCredentialSource {
    /// One credential, read once from a projected Docker config file.
    Fixed(RegistryCredentials),
    /// A token from the metadata server, asked for at each pull.
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
