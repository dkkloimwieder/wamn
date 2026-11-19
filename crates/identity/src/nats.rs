//! NATS operator bootstrap and account-scoped signing in the identity authority.

mod claims;
mod store;

use std::{error, fmt, time::SystemTime};

use nkeys::{KeyPair, KeyPairType};
use sqlx::{Connection as _, PgConnection};
use wamn_envelope::encryption::Envelope;
use wamn_registry::{EnvName, OrgId, name};
use zeroize::Zeroizing;

pub use claims::{
    Permissions, ResponsePermission, SubjectPermission, credentials, is_user_public_key,
};

/// Context about a failed NATS identity operation.
#[derive(Debug)]
pub struct Error {
    context: &'static str,
    source: Option<Box<dyn error::Error + Send + Sync>>,
}

impl Error {
    fn new(context: &'static str, source: impl error::Error + Send + Sync + 'static) -> Self {
        Self {
            context,
            source: Some(Box::new(source)),
        }
    }

    fn invalid_user_key() -> Self {
        Self {
            context: "invalid NATS user public key",
            source: None,
        }
    }

    /// Whether the client supplied an invalid NATS user public key.
    pub fn is_invalid_user_key(&self) -> bool {
        self.source.is_none()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &dyn error::Error)
    }
}

/// Output needed to start the resolver and its platform clients.
pub struct Bootstrap {
    pub operator_include: String,
    pub runtime_operator_creds: Zeroizing<String>,
    pub system_creds: Zeroizing<String>,
    pub system_inbox: String,
    pub authority: Authority,
}

impl fmt::Debug for Bootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Bootstrap").finish_non_exhaustive()
    }
}

/// Daily signing authority. The operator root and system account seeds are
/// only opened during bootstrap.
pub struct Authority {
    operator_signing: KeyPair,
    platform: String,
    platform_signing: KeyPair,
}

impl fmt::Debug for Authority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Authority").finish_non_exhaustive()
    }
}

/// The subject used to push account claims to the NATS full resolver.
pub const UPDATE_SUBJECT: &str = "$SYS.REQ.CLAIMS.UPDATE";
/// Inbox prefix for the platform runtime operator.
pub const RUNTIME_OPERATOR_INBOX: &str = "_INBOX_wasmcloud-operator";

/// Create or load the platform's keys in one transaction under the issuer lock.
pub async fn bootstrap(
    client: &mut PgConnection,
    issuer: &str,
    envelope: &Envelope,
) -> Result<Bootstrap, Error> {
    let mut tx = client.begin().await.map_err(store::database_error)?;
    store::lock(&mut tx, issuer).await?;
    let root = store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "operator-root",
        KeyPairType::Operator,
    )
    .await?;
    let signing = store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "operator-signing",
        KeyPairType::Operator,
    )
    .await?;
    let system = store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "system-account",
        KeyPairType::Account,
    )
    .await?;
    let system_signing = store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "system-signing",
        KeyPairType::Account,
    )
    .await?;
    let platform = store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "platform-account",
        KeyPairType::Account,
    )
    .await?;
    let platform_signing = store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "platform-signing",
        KeyPairType::Account,
    )
    .await?;
    let runtime_operator = store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "runtime-operator",
        KeyPairType::User,
    )
    .await?;
    tx.commit().await.map_err(store::database_error)?;

    let system_id = system.pair.public_key();
    let platform_id = platform.pair.public_key();
    let operator_jwt = claims::operator(&root.pair, &signing.pair, &system_id, root.issued_at)?;
    let system_jwt = claims::account(
        &signing.pair,
        &system_id,
        "SYS",
        system.issued_at,
        0,
        &[system_signing.pair.public_key()],
    )?;
    let platform_jwt = claims::account(
        &signing.pair,
        &platform_id,
        "PLATFORM",
        platform.issued_at,
        0,
        &[platform_signing.pair.public_key()],
    )?;
    let operator_include = format!(
        "operator: \"{operator_jwt}\"\nsystem_account: \"{system_id}\"\nresolver_preload: {{\n  \"{system_id}\": \"{system_jwt}\"\n  \"{platform_id}\": \"{platform_jwt}\"\n}}\n"
    );

    let resolver_user = KeyPair::new_user();
    let resolver_user_id = resolver_user.public_key();
    let system_inbox = format!("_INBOX_wamn-identity-resolver.{resolver_user_id}");
    let jwt = claims::user(
        &system_signing.pair,
        Some(&system_id),
        &resolver_user_id,
        "wamn-identity-resolver",
        unix_seconds()?,
        &permissions([UPDATE_SUBJECT], [format!("{system_inbox}.>")]),
    )?;
    let system_creds = claims::credentials(&jwt, &resolver_user)?;
    let jwt = claims::user(
        &platform_signing.pair,
        Some(&platform_id),
        &runtime_operator.pair.public_key(),
        "wasmcloud-operator",
        runtime_operator.issued_at,
        &permissions(
            ["runtime.host.>"],
            [
                "runtime.operator.heartbeat.>".to_owned(),
                format!("{RUNTIME_OPERATOR_INBOX}.>"),
            ],
        ),
    )?;
    let runtime_operator_creds = claims::credentials(&jwt, &runtime_operator.pair)?;

    Ok(Bootstrap {
        operator_include,
        runtime_operator_creds,
        system_creds,
        system_inbox,
        authority: Authority {
            operator_signing: signing.pair,
            platform: platform_id,
            platform_signing: platform_signing.pair,
        },
    })
}

impl Authority {
    /// Persist the environment key and issue its current account JWT.
    pub async fn ensure_environment(
        &self,
        client: &mut PgConnection,
        envelope: &Envelope,
        issuer: &str,
        org: &OrgId,
        env: &EnvName,
        project_count: i32,
    ) -> Result<(String, String), Error> {
        let key =
            store::ensure_environment(client, envelope, issuer, org, env, project_count).await?;
        let jwt = claims::account(
            &self.operator_signing,
            &key.subject,
            &name::environment_nats_account(org, env),
            key.issued_at,
            project_count,
            &[key.signer.public_key()],
        )?;
        Ok((key.subject, jwt))
    }

    /// Issue a scoped JWT in an existing environment account.
    pub async fn environment_user(
        &self,
        client: &mut PgConnection,
        envelope: &Envelope,
        issuer: &str,
        org: &OrgId,
        env: &EnvName,
        user: &User<'_>,
    ) -> Result<Option<String>, Error> {
        store::environment_signer(client, envelope, issuer, org, env)
            .await?
            .map(|key| sign_user(&key.signer, &key.subject, user))
            .transpose()
    }

    /// Issue a scoped JWT for a host group's connection to PLATFORM.
    pub fn platform_user(&self, user: &User<'_>) -> Result<String, Error> {
        sign_user(&self.platform_signing, &self.platform, user)
    }
}

/// A user public key and the permissions derived by identity.
#[derive(Debug)]
pub struct User<'a> {
    pub public_key: &'a str,
    pub name: &'a str,
    pub permissions: &'a Permissions,
}

fn sign_user(signer: &KeyPair, account: &str, user: &User<'_>) -> Result<String, Error> {
    claims::user(
        signer,
        Some(account),
        user.public_key,
        user.name,
        unix_seconds()?,
        user.permissions,
    )
}

fn permissions<const P: usize, const S: usize>(
    publish: [&str; P],
    subscribe: [String; S],
) -> Permissions {
    Permissions {
        pub_: SubjectPermission {
            allow: publish.map(str::to_owned).into(),
            deny: Vec::new(),
        },
        sub: SubjectPermission {
            allow: subscribe.into(),
            deny: Vec::new(),
        },
        resp: None,
    }
}

fn unix_seconds() -> Result<i64, Error> {
    let elapsed = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(|source| Error::new("read NATS signing time", source))?;
    i64::try_from(elapsed.as_secs())
        .map_err(|source| Error::new("convert NATS signing time", source))
}
