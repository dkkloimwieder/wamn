//! The auth facts a committed write changed, and their publication to the
//! memory of every host (docs/architecture/execution.md#auth-memory).
//!
//! Every function of this crate that writes a fact a host keeps takes an
//! [`AuthDrops`] and records the fact in it. Its caller publishes the list
//! with [`AuthChanges::publish`] after the commit. The facts are the users
//! rows, roles, permissions and user roles of a tenant, its environment
//! status, and the PATs, logins, memberships and status of a principal.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

/// One auth fact that a committed write changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthChange {
    /// The users row or the user roles of `principal` in `tenant`.
    Principal { tenant: String, principal: String },
    /// A role or a permission of `tenant`, which every principal holding the
    /// role reads, or the environment status of `tenant`.
    Tenant(String),
    /// A PAT, a login, a membership or the status of `principal`. These are
    /// facts of the organization, so they change the principal in every
    /// tenant.
    Identity(String),
}

/// The changes of one transaction, published after its commit.
#[must_use = "publish the drops with AuthChanges::publish after the commit"]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthDrops(BTreeSet<AuthChange>);

impl AuthDrops {
    pub fn principal(&mut self, tenant: &str, principal: &str) {
        self.0.insert(AuthChange::Principal {
            tenant: tenant.to_owned(),
            principal: principal.to_owned(),
        });
    }

    pub fn tenant(&mut self, tenant: &str) {
        self.0.insert(AuthChange::Tenant(tenant.to_owned()));
    }

    pub fn identity(&mut self, principal: &str) {
        self.0.insert(AuthChange::Identity(principal.to_owned()));
    }

    /// Add the changes of `other`, for a caller that runs several writers in
    /// one transaction.
    pub fn extend(&mut self, other: AuthDrops) {
        self.0.extend(other.0);
    }

    pub fn changes(&self) -> impl Iterator<Item = &AuthChange> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The scheduler NATS subject a change of the facts of `tenant` goes to.
pub fn tenant_subject(tenant: &str) -> String {
    format!("wamn.auth.{tenant}")
}

/// The scheduler NATS subject a change of an identity fact goes to. Every
/// host listens on it, because an identity fact holds in every tenant.
pub const IDENTITY_SUBJECT: &str = "wamn.identity";

/// Publishes the changes of a commit on the scheduler NATS. A host without
/// that client publishes nothing, and the TTL of its memory applies.
#[derive(Debug, Clone, Default)]
pub struct AuthChanges(Option<async_nats::Client>);

impl AuthChanges {
    pub fn new(client: async_nats::Client) -> Self {
        Self(Some(client))
    }

    /// Connect to the scheduler NATS that `connection` names.
    pub async fn connect(connection: &AuthNats) -> anyhow::Result<Self> {
        Ok(Self::new(connection.connect().await?))
    }

    /// Publish after the commit that made `drops`. A failed publish is
    /// logged, because the commit stands and the TTL of the memory bounds it.
    pub async fn publish(&self, drops: AuthDrops) {
        let Some(client) = &self.0 else {
            return;
        };
        let subjects: BTreeSet<String> = drops
            .0
            .into_iter()
            .map(|change| match change {
                AuthChange::Principal { tenant, .. } | AuthChange::Tenant(tenant) => {
                    tenant_subject(&tenant)
                }
                AuthChange::Identity(_) => IDENTITY_SUBJECT.to_owned(),
            })
            .collect();
        for subject in subjects {
            if let Err(error) = client.publish(subject.clone(), bytes::Bytes::new()).await {
                tracing::warn!(%error, subject, "the auth change was not published; the TTL bounds it");
            }
        }
        // A short-lived writer, such as a wamn-ctl verb, exits right after this.
        if let Err(error) = client.flush().await {
            tracing::warn!(%error, "the auth changes were not flushed; the TTL bounds them");
        }
    }
}

/// The scheduler NATS connection of a writer that is not a host: the identity
/// service and wamn-ctl. The server takes only a client certificate of the
/// wasmCloud CA.
#[derive(Debug, Clone)]
pub struct AuthNats {
    /// The scheduler NATS URL.
    pub nats_url: String,
    /// The CA certificate of the server.
    pub nats_tls_ca: PathBuf,
    /// The client certificate.
    pub nats_tls_cert: PathBuf,
    /// The key of the client certificate.
    pub nats_tls_key: PathBuf,
    /// The name the server certificate must carry, when the URL reaches the
    /// server by another name, for example through a port-forward.
    pub nats_tls_server_name: Option<String>,
}

impl AuthNats {
    /// Read the connection from `WAMN_AUTH_NATS_URL`, `WAMN_AUTH_NATS_TLS_CA`,
    /// `WAMN_AUTH_NATS_TLS_CERT`, `WAMN_AUTH_NATS_TLS_KEY` and the optional
    /// `WAMN_AUTH_NATS_TLS_SERVER_NAME`.
    pub fn from_env() -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let variable = |name: &str| {
            std::env::var(name).with_context(|| format!("set {name} for the auth changes"))
        };
        Ok(Self {
            nats_url: variable("WAMN_AUTH_NATS_URL")?,
            nats_tls_ca: variable("WAMN_AUTH_NATS_TLS_CA")?.into(),
            nats_tls_cert: variable("WAMN_AUTH_NATS_TLS_CERT")?.into(),
            nats_tls_key: variable("WAMN_AUTH_NATS_TLS_KEY")?.into(),
            nats_tls_server_name: std::env::var("WAMN_AUTH_NATS_TLS_SERVER_NAME").ok(),
        })
    }

    async fn connect(&self) -> anyhow::Result<async_nats::Client> {
        use anyhow::Context as _;
        use rustls::pki_types::pem::PemObject as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_file_iter(&self.nats_tls_ca)
            .with_context(|| format!("read {}", self.nats_tls_ca.display()))?
        {
            roots.add(certificate?)?;
        }
        let chain = CertificateDer::pem_file_iter(&self.nats_tls_cert)
            .with_context(|| format!("read {}", self.nats_tls_cert.display()))?
            .collect::<Result<Vec<_>, _>>()?;
        let key = PrivateKeyDer::from_pem_file(&self.nats_tls_key)
            .with_context(|| format!("read {}", self.nats_tls_key.display()))?;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let roots = Arc::new(roots);
        let verifier =
            rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider.clone())
                .build()?;
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?;
        let config = match &self.nats_tls_server_name {
            Some(name) => builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PinnedName {
                    inner: verifier,
                    name: ServerName::try_from(name.clone())?,
                })),
            None => builder.with_webpki_verifier(verifier),
        }
        .with_client_auth_cert(chain, key)?;
        async_nats::ConnectOptions::new()
            .require_tls(true)
            .tls_client_config(config)
            .connect(&self.nats_url)
            .await
            .with_context(|| format!("connect to the scheduler NATS at {}", self.nats_url))
    }
}

/// Verifies the server certificate against one fixed name instead of the host
/// of the URL. The chain and the signatures get the full WebPKI check.
#[derive(Debug)]
struct PinnedName {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    name: rustls::pki_types::ServerName<'static>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedName {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.inner
            .verify_server_cert(end_entity, intermediates, &self.name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner
            .verify_tls12_signature(message, certificate, signature)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner
            .verify_tls13_signature(message, certificate, signature)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
