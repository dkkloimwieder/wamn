//! Registry transport configuration shared by component, release and package readers.

use std::time::Duration;

use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::{Client, errors::OciDistributionError};

/// Explicit protocol, trust and request deadline for one registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RegistrySource {
    base: wamn_catalog::ComponentArtifactBase,
    insecure: bool,
    timeout: Duration,
    ca_bundles: Vec<Vec<u8>>,
}

impl RegistrySource {
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
            ca_bundles: Vec::new(),
        }
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
}
