//! NATS operator bootstrap and account-scoped signing in the identity authority.

mod claims;
mod monitor;

use std::{error, fmt, time::SystemTime};

use nkeys::KeyPair;
use serde::{Deserialize, Serialize};
use wamn_registry::{EnvName, OrgId, name};
use zeroize::Zeroizing;

pub(crate) use claims::{AccountRevision, account_revision};
pub use claims::{
    Permissions, ResponsePermission, SubjectPermission, credentials, is_user_public_key,
};
pub(crate) use monitor::check_shrink;

/// Context about a failed NATS identity operation.
#[derive(Debug)]
pub struct Error {
    context: &'static str,
    source: Option<Box<dyn error::Error + Send + Sync>>,
}

impl Error {
    pub fn new(context: &'static str, source: impl error::Error + Send + Sync + 'static) -> Self {
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

    pub(crate) fn limit_refused(reason: &'static str) -> Self {
        Self::new("apply NATS environment limits", LimitRefusal(reason))
    }

    /// Whether the environment limits conflict with current use or platform capacity.
    pub fn is_limit_refused(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| source.is::<LimitRefusal>())
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
    file_store_bytes: i64,
}

/// Desired per-env limits read from the registry.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct EnvLimits {
    pub connections: i64,
    pub consumers: i64,
    pub subscriptions: i64,
    pub payload_bytes: i64,
    pub streams_per_project: i64,
    pub disk_bytes_per_project: i64,
}

/// The adjustable limits in a signed NATS account revision.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Limits {
    pub conn: i64,
    pub consumer: i64,
    pub subs: i64,
    pub payload: i64,
    pub streams: i64,
    pub disk_storage: i64,
}

impl Limits {
    pub(crate) fn exceeds(self, other: Self) -> bool {
        self.conn > other.conn
            || self.consumer > other.consumer
            || self.subs > other.subs
            || self.payload > other.payload
            || self.streams > other.streams
            || self.disk_storage > other.disk_storage
    }
}

pub(crate) fn env_limits(limits: &EnvLimits, project_count: i32) -> Result<Limits, Error> {
    let scale = |limit: i64| {
        limit
            .checked_mul(i64::from(project_count))
            .ok_or_else(|| Error::limit_refused("project allocations exceed the NATS claim range"))
    };
    Ok(Limits {
        conn: limits.connections,
        consumer: limits.consumers,
        subs: limits.subscriptions,
        payload: limits.payload_bytes,
        streams: scale(limits.streams_per_project)?,
        disk_storage: scale(limits.disk_bytes_per_project)?,
    })
}

impl fmt::Debug for Authority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Authority").finish_non_exhaustive()
    }
}

/// The subject used to push account claims to the NATS full resolver.
pub const UPDATE_SUBJECT: &str = "$SYS.REQ.CLAIMS.UPDATE";
/// Subject pattern for reading account JWTs from the full resolver.
pub const LOOKUP_SUBJECT: &str = "$SYS.REQ.ACCOUNT.*.CLAIMS.LOOKUP";
/// Read an environment's current connection and subscription counts.
pub const STATZ_SUBJECT: &str = "$SYS.REQ.ACCOUNT.*.STATZ";
/// Read an environment's current streams and their usage.
pub const JSZ_SUBJECT: &str = "$SYS.REQ.ACCOUNT.*.JSZ";
/// Resolver endpoint for operator-signed account removal.
pub const DELETE_SUBJECT: &str = "$SYS.REQ.CLAIMS.DELETE";
/// Inbox prefix for the platform runtime operator.
pub const RUNTIME_OPERATOR_INBOX: &str = "_INBOX_wasmcloud-operator";

#[derive(Debug)]
struct LimitRefusal(&'static str);

impl fmt::Display for LimitRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl error::Error for LimitRefusal {}

pub struct Key {
    pub pair: KeyPair,
    pub issued_at: i64,
}

pub struct Account {
    pub subject: String,
    pub signer: KeyPair,
    pub issued_at: i64,
}

impl fmt::Debug for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Key").finish_non_exhaustive()
    }
}

impl fmt::Debug for Account {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Account").finish_non_exhaustive()
    }
}

/// Platform keys loaded together before producing credentials.
#[derive(Debug)]
pub struct BootstrapKeys {
    pub root: Key,
    pub signing: Key,
    pub system: Key,
    pub system_signing: Key,
    pub platform: Key,
    pub platform_signing: Key,
    pub runtime_operator: Key,
}

/// Sign the platform credentials with the keys loaded by the caller.
pub fn bootstrap(keys: BootstrapKeys, file_store_bytes: i64) -> Result<Bootstrap, Error> {
    if file_store_bytes <= 0 {
        return Err(Error::limit_refused(
            "the NATS file store size must be positive",
        ));
    }
    let BootstrapKeys {
        root,
        signing,
        system,
        system_signing,
        platform,
        platform_signing,
        runtime_operator,
    } = keys;
    let system_id = system.pair.public_key();
    let platform_id = platform.pair.public_key();
    let operator_jwt = claims::operator(&root.pair, &signing.pair, &system_id, root.issued_at)?;
    let system_jwt = claims::account(
        &signing.pair,
        &system_id,
        "SYS",
        system.issued_at,
        claims::PLATFORM_LIMITS,
        &[system_signing.pair.public_key()],
    )?;
    let platform_jwt = claims::account(
        &signing.pair,
        &platform_id,
        "PLATFORM",
        platform.issued_at,
        claims::PLATFORM_LIMITS,
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
        &permissions(
            [
                UPDATE_SUBJECT,
                DELETE_SUBJECT,
                LOOKUP_SUBJECT,
                STATZ_SUBJECT,
                JSZ_SUBJECT,
            ],
            [format!("{system_inbox}.>")],
        ),
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
            file_store_bytes,
        },
    })
}

impl Authority {
    pub(crate) fn file_store_bytes(&self) -> i64 {
        self.file_store_bytes
    }

    /// Sign the current revision of an environment account.
    pub(crate) fn env_account(
        &self,
        org: &OrgId,
        env: &EnvName,
        key: Account,
        limits: Limits,
    ) -> Result<(String, String), Error> {
        let jwt = claims::account(
            &self.operator_signing,
            &key.subject,
            &name::env_nats_account(org, env),
            key.issued_at,
            limits,
            &[key.signer.public_key()],
        )?;
        Ok((key.subject, jwt))
    }

    /// Sign removal of the retired account's current revision.
    pub fn retired_account(&self, account: &str, issued_at: i64) -> Result<String, Error> {
        claims::delete_account(&self.operator_signing, account, issued_at)
    }

    /// Issue a scoped JWT for a host connection to PLATFORM.
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

pub fn sign_user(signer: &KeyPair, account: &str, user: &User<'_>) -> Result<String, Error> {
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
