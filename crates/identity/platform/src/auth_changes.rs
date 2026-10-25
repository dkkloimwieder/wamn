//! The auth facts a committed write changed, and their publication to the
//! memory of every host (docs/architecture/execution.md#auth-memory).
//!
//! Every function of this crate that writes a fact a host keeps takes an
//! [`AuthDrops`] and records the fact in it. Its caller publishes the list
//! with [`AuthChanges::publish`] after the commit, which deletes the keys of
//! the facts from the NATS KV bucket [`BUCKET`]. Every host watches the
//! bucket and keeps the values as its memory. The facts are the users
//! rows, roles, permissions and user roles of an environment, its
//! status, and the PATs, logins, memberships and status of a principal.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// One auth fact that a committed write changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthChange {
    /// The user row or the user roles of `principal` in `tenant`.
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

/// The NATS KV bucket of the auth memory, on the scheduler NATS.
pub const BUCKET: &str = "wamn-auth";

/// How long the bucket keeps a value, and a host its copy: the backstop for
/// a lost delete.
pub const AUTH_TTL: Duration = Duration::from_secs(60);

/// The key of the grants of `principal` in the environment named `tenant`.
/// This shared cache key stays scoped after the table tenant columns leave.
pub fn principal_key(tenant: &str, principal: &str) -> String {
    format!("{tenant}.{principal}")
}

/// The key of the environment status of `tenant`. A principal id is a UUID,
/// so it is never `environment`.
pub fn environment_key(tenant: &str) -> String {
    format!("{tenant}.environment")
}

/// The first token of the key of an identity fact. A tenant name holds only
/// `[A-Za-z0-9_-]`, so this token never meets a tenant key.
pub const IDENTITY_TOKEN: &str = "=identity";

/// The key a change of an identity fact of `principal` deletes.
pub fn identity_key(principal: &str) -> String {
    format!("{IDENTITY_TOKEN}.{principal}")
}

/// Open the bucket, and make it when it is absent: the scheduler NATS keeps
/// JetStream in memory of its pod, so a restart loses the bucket.
pub async fn auth_bucket(
    client: &async_nats::Client,
) -> anyhow::Result<async_nats::jetstream::kv::Store> {
    use anyhow::Context as _;
    async_nats::jetstream::new(client.clone())
        .create_or_update_key_value(async_nats::jetstream::kv::Config {
            bucket: BUCKET.to_owned(),
            history: 1,
            max_age: AUTH_TTL,
            storage: async_nats::jetstream::stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .with_context(|| format!("open the NATS KV bucket {BUCKET}"))
}

/// Deletes the keys of the changes of a commit from the bucket. A writer
/// without a scheduler NATS client deletes nothing, and the TTL of the memory
/// applies.
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

    /// Delete the keys of `drops` after the commit that made them. A failed
    /// delete is logged, because the commit stands and the TTL bounds it.
    /// Each publish opens the bucket, so a writer outlives a restart of the
    /// scheduler NATS.
    pub async fn publish(&self, drops: AuthDrops) {
        let Some(client) = &self.0 else {
            return;
        };
        if drops.is_empty() {
            return;
        }
        let store = match auth_bucket(client).await {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the auth changes were not deleted; the TTL bounds them"
                );
                return;
            }
        };
        for change in drops.0 {
            if let Err(error) = delete(&store, &change).await {
                tracing::warn!(
                    error = format!("{error:#}"),
                    ?change,
                    "the auth change was not deleted; the TTL bounds it"
                );
            }
        }
    }
}

async fn delete(
    store: &async_nats::jetstream::kv::Store,
    change: &AuthChange,
) -> anyhow::Result<()> {
    use futures_util::TryStreamExt as _;
    let keys = match change {
        AuthChange::Principal { tenant, principal } => vec![principal_key(tenant, principal)],
        AuthChange::Identity(principal) => vec![identity_key(principal)],
        // Every key of the tenant: the grants of each principal, which hold
        // the role, and the environment status.
        AuthChange::Tenant(tenant) => {
            let mut subjects = store
                .stream
                .info_with_subjects(format!("{}{tenant}.*", store.prefix))
                .await?;
            let mut keys = Vec::new();
            while let Some((subject, _)) = subjects.try_next().await? {
                if let Some(key) = subject.strip_prefix(&store.prefix) {
                    keys.push(key.to_owned());
                }
            }
            keys
        }
    };
    for key in keys {
        store.delete(&key).await?;
    }
    Ok(())
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
