//! Registry transport configuration shared by component, release and package readers.

use std::time::Duration;

use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, errors::OciDistributionError};

use crate::registry_credentials::{RegistryCredentialSource, RegistryCredentialsError};

/// Explicit protocol, trust, credentials and request deadline for one registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrySource {
    base: wamn_catalog::ComponentArtifactBase,
    insecure: bool,
    timeout: Duration,
    credentials: Option<RegistryCredentialSource>,
    ca_bundles: Vec<Vec<u8>>,
}

impl RegistrySource {
    /// Delete one manifest digest with this source's trust and authorization.
    pub async fn delete_manifest(
        &self,
        reference: &oci_client::Reference,
        auth: &RegistryAuth,
        digest: &str,
    ) -> anyhow::Result<()> {
        crate::ensure_digest(digest)?;
        let http = crate::http_client::HttpClient::with_extra_roots(&self.ca_bundles)?;
        crate::registry_transport::delete_manifest(
            &http,
            self.insecure,
            reference,
            auth,
            digest,
            self.timeout,
        )
        .await
    }

    /// Keep explicit transport inputs. Call `client` to validate its trust roots.
    pub fn new(
        base: wamn_catalog::ComponentArtifactBase,
        insecure: bool,
        timeout: Duration,
    ) -> Self {
        Self {
            base,
            insecure,
            timeout,
            credentials: None,
            ca_bundles: Vec::new(),
        }
    }

    /// The validated artifact repository.
    pub fn base(&self) -> &wamn_catalog::ComponentArtifactBase {
        &self.base
    }

    /// The complete pull deadline and per-request transport timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Set the credentials of this source.
    pub fn with_credentials(mut self, credentials: RegistryCredentialSource) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Set the additional trust bundles of this source.
    pub fn with_ca_bundles(mut self, ca_bundles: Vec<Vec<u8>>) -> Self {
        self.ca_bundles = ca_bundles;
        self
    }

    /// Each pull gets a fresh upstream client because it remembers its first credential.
    pub fn client(&self) -> Result<Client, OciDistributionError> {
        // Client::new silently falls back to default configuration on failure.
        Client::try_from(ClientConfig {
            protocol: if self.insecure {
                ClientProtocol::HttpsExcept(vec![self.base.registry().to_owned()])
            } else {
                ClientProtocol::Https
            },
            read_timeout: Some(self.timeout),
            connect_timeout: Some(self.timeout),
            extra_root_certificates: self
                .ca_bundles
                .iter()
                .map(|data| Certificate {
                    encoding: CertificateEncoding::Pem,
                    data: data.clone(),
                })
                .collect(),
            ..ClientConfig::default()
        })
    }

    /// Resolve the current credential, including the shared metadata token.
    pub async fn auth(&self) -> Result<RegistryAuth, RegistryCredentialsError> {
        let Some(source) = &self.credentials else {
            return Ok(RegistryAuth::Anonymous);
        };
        let credentials = source.credentials(self.base.registry()).await?;
        Ok(RegistryAuth::Basic(
            credentials.username().to_owned(),
            credentials.password().to_owned(),
        ))
    }
}
