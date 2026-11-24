//! NATS operator bootstrap and account-scoped signing in the identity authority.

mod claims;

use std::{error, fmt, time::SystemTime};

use nkeys::KeyPair;
use serde::Deserialize;
use wamn_registry::{EnvName, OrgId, name};
use zeroize::Zeroizing;

pub use claims::{
    ClientLimits, Permissions, ResponsePermission, SubjectPermission, account_limits_match,
    credentials, is_user_public_key,
};

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

    /// Record a capacity refusal returned by the key store.
    pub fn project_limit(max_projects: i64) -> Self {
        Self::new(
            "reserve NATS project capacity",
            ProjectLimit { max_projects },
        )
    }

    /// Whether the requested account would exceed the platform project limit.
    pub fn is_project_limit(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| source.is::<ProjectLimit>())
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
    max_projects: i64,
    limits_per_project: LimitsPerProject,
}

/// Install-time environment account limits for one project.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsPerProject {
    connections: i64,
    subscriptions: i64,
    payload: i64,
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
/// Resolver endpoint for operator-signed account removal.
pub const DELETE_SUBJECT: &str = "$SYS.REQ.CLAIMS.DELETE";
/// Inbox prefix for the platform runtime operator.
pub const RUNTIME_OPERATOR_INBOX: &str = "_INBOX_wasmcloud-operator";

#[derive(Debug)]
struct ProjectLimit {
    max_projects: i64,
}

impl std::fmt::Display for ProjectLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "platform limit is {} NATS projects", self.max_projects)
    }
}

impl std::error::Error for ProjectLimit {}

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

impl LimitsPerProject {
    /// Refuse limits that cannot admit any workload.
    pub fn validate(&self) -> Result<(), Error> {
        let LimitsPerProject {
            connections,
            subscriptions,
            payload,
        } = self;
        if *connections < 1 || *subscriptions < 1 || *payload < 1 {
            return Err(Error::new(
                "validate NATS limits per project",
                std::io::Error::other("limits must be positive"),
            ));
        }
        Ok(())
    }
}

/// Sign the platform credentials with the keys loaded by the caller.
pub fn bootstrap(
    keys: BootstrapKeys,
    max_projects: i64,
    limits_per_project: LimitsPerProject,
) -> Result<Bootstrap, Error> {
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
        0,
        &claims::UNLIMITED_CLIENT_LIMITS,
        &[system_signing.pair.public_key()],
    )?;
    let platform_jwt = claims::account(
        &signing.pair,
        &platform_id,
        "PLATFORM",
        platform.issued_at,
        0,
        &claims::UNLIMITED_CLIENT_LIMITS,
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
            [UPDATE_SUBJECT, DELETE_SUBJECT, LOOKUP_SUBJECT],
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
            max_projects,
            limits_per_project,
        },
    })
}

impl Authority {
    /// The allowed total number of projects across environment accounts.
    pub fn max_projects(&self) -> i64 {
        self.max_projects
    }

    /// Calculate claim limits for the supplied project count.
    pub fn environment_limits(&self, project_count: i32) -> Result<ClientLimits, Error> {
        let scale = |limit: i64| {
            limit.checked_mul(i64::from(project_count)).ok_or_else(|| {
                Error::new(
                    "scale NATS environment limits",
                    std::io::Error::other("limit exceeds the NATS claim range"),
                )
            })
        };
        Ok(ClientLimits {
            conn: scale(self.limits_per_project.connections)?,
            subs: scale(self.limits_per_project.subscriptions)?,
            payload: scale(self.limits_per_project.payload)?,
        })
    }

    /// Sign the current revision of an environment account.
    pub fn environment_account(
        &self,
        org: &OrgId,
        env: &EnvName,
        project_count: i32,
        key: Account,
        limits: &ClientLimits,
    ) -> Result<(String, String), Error> {
        let jwt = claims::account(
            &self.operator_signing,
            &key.subject,
            &name::environment_nats_account(org, env),
            key.issued_at,
            project_count,
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
