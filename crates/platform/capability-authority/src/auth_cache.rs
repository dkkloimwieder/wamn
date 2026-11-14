//! The host's memory of the auth facts of a request
//! (docs/architecture/execution.md#auth-memory).
//!
//! Postgres stays the owner of every fact here. The memory is the local
//! replica of the NATS KV bucket [`BUCKET`] on the scheduler NATS. The key
//! `<project_env_key>.<principal>` holds the grants of a principal, and the key
//! `<project_env_key>.environment` holds the environment status. A host that reads a
//! fact from Postgres puts it with the revision of the key from before the
//! read, so a delete in between refuses the put. Every host watches the keys
//! of its project and keeps each value it sees. Every writer of a fact deletes
//! its key after the commit ([`wamn_broker::auth_changes`]). The
//! replica expires facts at the configured project and application revocation bound.
//!
//! An admitted PAT and an active session stay in the host that read them,
//! because the token answer never leaves the host. A delete of the key
//! `=identity.<principal>` drops them. A host keeps such an answer only when
//! no identity key changed during its read. Only a positive identity answer
//! is kept, so a new PAT or a new login works on its first request.
//!
//! The replica follows the rules of [`crate::kv_memory`]: a host without the
//! bucket, such as a test host, or with a broken watch, keeps no fact and
//! reads every request from Postgres.
//!
//! [`BUCKET`]: wamn_broker::auth_changes::BUCKET

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use wamn_broker::Publisher;

use async_nats::jetstream::kv::Store;
use sha2::{Digest as _, Sha256};
use wamn_broker::auth_changes::{
    BUCKET, IDENTITY_TOKEN, principal_key, project_env_key as environment_status_key,
};
use wamn_platform_identity::AuthenticatedPrincipal;

pub use crate::kv_memory::Ticket;
use crate::kv_memory::{KvMemory, Watched};

/// The roles a principal holds in one project, with the permissions of each.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrincipalGrants {
    /// Whether the project has an `platform."user"` row for the principal.
    pub present: bool,
    /// Whether that row is `active`. An inactive principal holds no role.
    pub active: bool,
    /// Only a user can select an active role for a session.
    pub user: bool,
    /// Each held role, with each permission of that role.
    pub roles: BTreeMap<String, BTreeSet<String>>,
}

/// The value of a key of the bucket.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Answer {
    /// The grants of a principal, when the host read them, and whether the
    /// project has its user row.
    Principal { grants: PrincipalGrants },
    /// Whether the environment is inactive.
    Environment { inactive: bool },
}

/// The identity revision that a host saw before it read a PAT or a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityTicket(u64);

struct Token {
    deadline: SystemTime,
    principal: String,
    /// The principal a PAT admitted, or `None` for an active session.
    pat: Option<AuthenticatedPrincipal>,
}

struct CredentialDeadline;
impl moka::Expiry<[u8; 32], Arc<Token>> for CredentialDeadline {
    fn expire_after_create(
        &self,
        _: &[u8; 32],
        token: &Arc<Token>,
        _: std::time::Instant,
    ) -> Option<Duration> {
        Some(
            token
                .deadline
                .duration_since(SystemTime::now())
                .unwrap_or_default(),
        )
    }
}

struct Tokens {
    tokens: moka::sync::Cache<[u8; 32], Arc<Token>>,
    /// The local generation, advanced by identity invalidations and watch resets.
    identity_revision: u64,
}

/// The auth facts one host keeps. One instance lives in
/// [`WamnPostgres`](wamn_postgres_capability::WamnPostgres), so the
/// route authenticator and the identity binding share it.
pub struct AuthCache {
    memory: KvMemory<Answer>,
    tokens: Mutex<Tokens>,
}

impl Default for AuthCache {
    fn default() -> Self {
        Self::with_ttl(Duration::ZERO)
    }
}

impl std::fmt::Debug for AuthCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthCache")
            .field("ttl", &self.memory.ttl())
            .finish_non_exhaustive()
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl AuthCache {
    /// A cache whose facts live for `ttl`.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            memory: KvMemory::new(BUCKET, ttl),
            tokens: Mutex::new(Tokens {
                tokens: moka::sync::Cache::builder()
                    .time_to_live(ttl)
                    .expire_after(CredentialDeadline)
                    .support_invalidation_closures()
                    .build(),
                identity_revision: 0,
            }),
        }
    }

    fn tokens(&self) -> std::sync::MutexGuard<'_, Tokens> {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Drop every fact.
    pub fn clear(&self) {
        self.memory.clear();
        let mut tokens = self.tokens();
        tokens.identity_revision = tokens.identity_revision.wrapping_add(1);
        tokens.tokens.invalidate_all();
    }

    /// The principal of an admitted PAT.
    pub fn pat(&self, token: &str) -> Option<AuthenticatedPrincipal> {
        self.tokens()
            .tokens
            .get(&digest(token))
            .filter(|token| SystemTime::now() < token.deadline)
            .and_then(|token| token.pat.clone())
    }

    /// Whether the login or PAT behind a session token is active.
    pub fn session_active(&self, token: &str) -> bool {
        self.tokens()
            .tokens
            .get(&digest(token))
            .is_some_and(|token| SystemTime::now() < token.deadline && token.pat.is_none())
    }

    /// The ticket of a PAT or session read, taken before the read.
    pub fn identity_ticket(&self) -> IdentityTicket {
        IdentityTicket(self.tokens().identity_revision)
    }

    fn keep_token(
        &self,
        ticket: IdentityTicket,
        token: &str,
        principal: &str,
        pat: Option<&AuthenticatedPrincipal>,
        deadline: SystemTime,
    ) {
        // The lock is held during the test, so a token kept here falls to
        // the clear that follows a broken watch.
        let tokens = self.tokens();
        if self.memory.bucket().is_none()
            || self.memory.ttl().is_zero()
            || SystemTime::now() >= deadline
        {
            return;
        }
        // An identity key changed during the read, so the answer can be stale.
        if tokens.identity_revision != ticket.0 {
            return;
        }
        tokens.tokens.insert(
            digest(token),
            Arc::new(Token {
                deadline,
                principal: principal.to_owned(),
                pat: pat.cloned(),
            }),
        );
    }

    pub fn keep_pat(
        &self,
        ticket: IdentityTicket,
        token: &str,
        principal: &AuthenticatedPrincipal,
    ) {
        let id = principal.principal().id().to_string();
        let Some(deadline) = principal.pat_deadline() else {
            return;
        };
        self.keep_token(ticket, token, &id, Some(principal), deadline);
    }

    pub fn keep_session_active(
        &self,
        ticket: IdentityTicket,
        token: &str,
        principal: &str,
        deadline: SystemTime,
    ) {
        self.keep_token(ticket, token, principal, None, deadline);
    }

    /// The grants of a principal, or the ticket of a read of them.
    pub fn grants(
        &self,
        project_env_key: &str,
        principal: &str,
    ) -> Result<PrincipalGrants, Ticket> {
        let key = principal_key(project_env_key, principal);
        match self.memory.answer(&key) {
            Ok(Answer::Principal { grants }) => Ok(grants),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    /// Keep the grants of a principal. A present user row also answers the
    /// provisioned-principal test of the identity binding.
    pub async fn keep_grants(
        &self,
        project_env_key: &str,
        principal: &str,
        ticket: Ticket,
        grants: PrincipalGrants,
    ) {
        let key = principal_key(project_env_key, principal);
        self.memory
            .put(key, ticket, Answer::Principal { grants })
            .await;
    }

    /// Whether the environment is inactive, or the ticket of a read of it.
    pub fn environment_inactive(&self, project_env_key: &str) -> Result<bool, Ticket> {
        let key = environment_status_key(project_env_key);
        match self.memory.answer(&key) {
            Ok(Answer::Environment { inactive }) => Ok(inactive),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_environment_inactive(
        &self,
        project_env_key: &str,
        ticket: Ticket,
        inactive: bool,
    ) {
        self.memory
            .put(
                environment_status_key(project_env_key),
                ticket,
                Answer::Environment { inactive },
            )
            .await;
    }
}

impl Watched for AuthCache {
    fn set_bucket(&self, bucket: Option<Store>) {
        self.memory.set_bucket(bucket);
    }

    fn clear(&self) {
        AuthCache::clear(self);
    }

    fn apply(&self, entry: async_nats::jetstream::kv::Entry) {
        if let Some(principal) = entry
            .key
            .strip_prefix(IDENTITY_TOKEN)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            let mut tokens = self.tokens();
            tokens.identity_revision = tokens.identity_revision.wrapping_add(1);
            let principal = principal.to_owned();
            tokens
                .tokens
                .invalidate_entries_if(move |_, token| token.principal == principal)
                .expect("token cache supports invalidation closures");
            return;
        }
        self.memory.apply(entry);
    }
}

/// Watch the bucket on the scheduler NATS into the memory that `holder`
/// holds, and return the writer of the host's own changes. A host with
/// `project_env_key` watches the keys of that `project_env_key` and the identity keys. The
/// control host serves every project, so it watches every key.
pub fn replicate(
    client: &async_nats::Client,
    project_env_key: Option<&str>,
    holder: Arc<AuthCache>,
) -> Publisher {
    let filters = match project_env_key {
        Some(project_env_key) => vec![
            wamn_registry::name::nats_subject(&[project_env_key, ">"]),
            wamn_registry::name::nats_subject(&[IDENTITY_TOKEN, ">"]),
        ],
        None => vec![">".to_owned()],
    };
    crate::kv_memory::spawn_watch(client, BUCKET, filters, holder);
    Publisher::new(client.clone())
}

/// Live auth-cache fixtures shared by writer tests.
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

#[cfg(test)]
pub mod tests {
    use super::*;

    pub use super::test_util::{nats, until, watched};

    #[tokio::test]
    async fn a_host_without_the_bucket_keeps_no_fact() {
        let cache = AuthCache::with_ttl(Duration::from_secs(60));
        let ticket = cache.environment_inactive("t").unwrap_err();
        cache.keep_environment_inactive("t", ticket, true).await;
        let ticket = cache.grants("t", "p").unwrap_err();
        let present = PrincipalGrants {
            present: true,
            user: false,
            ..PrincipalGrants::default()
        };
        cache.keep_grants("t", "p", ticket, present).await;
        cache.keep_session_active(
            cache.identity_ticket(),
            "token",
            "p",
            SystemTime::now() + Duration::from_secs(3600),
        );
        assert!(cache.environment_inactive("t").is_err());
        assert!(cache.grants("t", "p").is_err());
        assert!(!cache.session_active("token"));
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn a_fact_expires_after_the_ttl_and_a_clear_drops_it() {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        let (_server, client) = nats().await;
        let cache = Arc::new(AuthCache::with_ttl(Duration::from_millis(50)));
        watched(&client, None, Arc::clone(&cache)).await;
        let ticket = cache.environment_inactive("t").unwrap_err();
        cache.keep_environment_inactive("t", ticket, true).await;
        cache.keep_session_active(
            cache.identity_ticket(),
            "token",
            "p",
            SystemTime::now() + Duration::from_secs(3600),
        );
        assert_eq!(cache.environment_inactive("t"), Ok(true));
        assert!(cache.session_active("token"));
        assert!(!cache.session_active("other"));

        cache.keep_session_active(
            cache.identity_ticket(),
            "expired",
            "p",
            SystemTime::UNIX_EPOCH,
        );
        assert!(!cache.session_active("expired"));
        let in_flight = cache.identity_ticket();
        cache.clear();
        cache.keep_session_active(
            in_flight,
            "stale",
            "p",
            SystemTime::now() + Duration::from_secs(3600),
        );
        assert!(!cache.session_active("stale"));
        assert!(cache.environment_inactive("t").is_err());
        assert!(!cache.session_active("token"));

        // The bucket still holds the key of `t`, so a put with no revision
        // fails there. A key the bucket does not hold shows the expiry.
        let ticket = cache.environment_inactive("u").unwrap_err();
        cache.keep_environment_inactive("u", ticket, false).await;
        assert_eq!(cache.environment_inactive("u"), Ok(false));
        // The watch also delivers the put, which can renew the fact once.
        until("the fact expires", || {
            cache.environment_inactive("u").is_err()
        })
        .await;
        assert!(cache.environment_inactive("u").is_err());
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn the_bucket_shares_kept_facts_and_every_drop_reaches_every_host() {
        use wamn_broker::auth_changes::AuthDrops;
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");

        let (_server, client) = nats().await;
        let a = Arc::new(AuthCache::with_ttl(Duration::from_secs(60)));
        let b = Arc::new(AuthCache::with_ttl(Duration::from_secs(60)));
        let other = Arc::new(AuthCache::with_ttl(Duration::from_secs(60)));
        let writer = replicate(&client, Some("t"), Arc::clone(&a));
        replicate(&client, Some("t"), Arc::clone(&b));
        replicate(&client, Some("u"), Arc::clone(&other));
        for cache in [&a, &b, &other] {
            until("the watch runs", || cache.memory.bucket().is_some()).await;
        }
        let present = PrincipalGrants {
            present: true,
            user: false,
            ..PrincipalGrants::default()
        };

        // A fact one host keeps reaches the other host of the project only.
        let ticket = a.grants("t", "p").unwrap_err();
        a.keep_grants("t", "p", ticket, present.clone()).await;
        assert_eq!(a.grants("t", "p"), Ok(present.clone()));
        until("host b sees the grants", || b.grants("t", "p").is_ok()).await;
        assert!(b.grants("t", "p").unwrap().present);
        assert!(other.grants("t", "p").is_err());

        let ticket = other.grants("u", "p").unwrap_err();
        other.keep_grants("u", "p", ticket, present.clone()).await;
        let ticket = other.environment_inactive("u").unwrap_err();
        other.keep_environment_inactive("u", ticket, true).await;
        assert_eq!(other.grants("u", "p"), Ok(present.clone()));
        assert_eq!(other.environment_inactive("u"), Ok(true));

        // A writer's delete reaches both hosts.
        let mut drops = AuthDrops::default();
        drops.principal("t", "p");
        writer.publish(drops).await;
        until("host a drops the grants", || a.grants("t", "p").is_err()).await;
        until("host b drops the grants", || b.grants("t", "p").is_err()).await;

        // A delete during a read refuses the put of the read.
        let ticket = a.grants("t", "p").unwrap_err();
        let mut drops = AuthDrops::default();
        drops.principal("t", "p");
        writer.publish(drops).await;
        a.keep_grants("t", "p", ticket, present.clone()).await;
        assert!(a.grants("t", "p").is_err());

        // A project change deletes every key of the project.
        until("host a sees the second delete", || {
            a.memory.ticket(&principal_key("t", "p")) != ticket
        })
        .await;
        let ticket = a.grants("t", "p").unwrap_err();
        a.keep_grants("t", "p", ticket, present.clone()).await;
        let ticket = a.environment_inactive("t").unwrap_err();
        a.keep_environment_inactive("t", ticket, false).await;
        until("host b sees the environment", || {
            b.environment_inactive("t") == Ok(false) && b.grants("t", "p").is_ok()
        })
        .await;
        let mut drops = AuthDrops::default();
        drops.project_env_key("t");
        writer.publish(drops).await;
        until("host b drops the project", || {
            b.environment_inactive("t").is_err() && b.grants("t", "p").is_err()
        })
        .await;

        assert_eq!(other.grants("u", "p"), Ok(present.clone()));
        assert_eq!(other.environment_inactive("u"), Ok(true));

        // An identity change drops the session of its principal on every
        // host, and a session read during any identity change is not kept.
        a.keep_session_active(
            a.identity_ticket(),
            "token-p",
            "p",
            SystemTime::now() + Duration::from_secs(3600),
        );
        a.keep_session_active(
            a.identity_ticket(),
            "token-q",
            "q",
            SystemTime::now() + Duration::from_secs(3600),
        );
        let read = b.identity_ticket();
        let mut drops = AuthDrops::default();
        drops.identity("p");
        writer.publish(drops).await;
        until("host a drops the session of p", || {
            !a.session_active("token-p")
        })
        .await;
        assert!(a.session_active("token-q"));
        until("host b sees the identity change", || {
            b.identity_ticket() != read
        })
        .await;
        b.keep_session_active(
            read,
            "token-p",
            "p",
            SystemTime::now() + Duration::from_secs(3600),
        );
        assert!(!b.session_active("token-p"));
    }
}

/// The caller of a run that executes as `principal_id` with `role` active,
/// from the roles the principal `held` with their permissions: the
/// permissions of `role` only, and `admin` only when `role` is `admin`. None
/// when the principal does not hold `role`.
pub fn role_caller(
    principal_id: &str,
    role: &str,
    held: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> Option<wamn_engine::flow_http_routing::AuthenticatedCaller> {
    let permissions = held.get(role)?;
    Some(wamn_engine::flow_http_routing::AuthenticatedCaller::new(
        "workflow",
        principal_id,
        wamn_engine::flow_http_routing::CredentialType::QueuedService,
        role == wamn_identity_protocol::ADMIN_ROLE,
        permissions.iter().cloned().collect(),
        Some(role.into()),
    ))
}
