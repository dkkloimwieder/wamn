//! The host's memory of the auth facts of a request
//! (docs/architecture/execution.md#auth-memory).
//!
//! Postgres stays the owner of every fact here. The memory is the local
//! replica of the NATS KV bucket [`BUCKET`] on the scheduler NATS. The key
//! `<env_key>.<principal>` holds the grants of a principal, and the key
//! `<env_key>.environment` holds the environment status. A host that reads a
//! fact from Postgres puts it with the revision of the key from before the
//! read, so a delete in between refuses the put. Every host watches the keys
//! of its tenant and keeps each value it sees. Every writer of a fact deletes
//! its key after the commit ([`wamn_platform_identity::auth_changes`]). The
//! bucket keeps a value for at most [`MEMORY_TTL`], and so does the replica.
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
//! [`BUCKET`]: wamn_platform_identity::auth_changes::BUCKET

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_nats::jetstream::kv::Store;
use sha2::{Digest as _, Sha256};
use wamn_platform_identity::AuthenticatedPrincipal;
use wamn_platform_identity::auth_changes::{
    AuthChanges, BUCKET, IDENTITY_TOKEN, env_key as environment_status_key, principal_key,
};
pub use wamn_platform_identity::kv_memory::MEMORY_TTL;

pub use crate::kv_memory::Ticket;
use crate::kv_memory::{KvMemory, SWEEP_AT, Watched};

/// The roles a principal holds in one tenant, with the permissions of each.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrincipalGrants {
    /// Whether the tenant has an `platform."user"` row for the principal.
    pub present: bool,
    /// Whether that row is `active`. An inactive principal holds no role.
    pub active: bool,
    /// Only a user can select an active role for a session.
    pub user: Option<bool>,
    /// Each held role, with each permission of that role.
    pub roles: BTreeMap<String, BTreeSet<String>>,
}

/// The value of a key of the bucket.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Answer {
    /// The grants of a principal, when the host read them, and whether the
    /// tenant has its user row.
    Principal {
        grants: Option<PrincipalGrants>,
        provisioned: bool,
    },
    /// Whether the environment is inactive.
    Environment { inactive: bool },
}

/// The identity revision that a host saw before it read a PAT or a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityTicket(u64);

struct Token {
    seen: Instant,
    principal: String,
    /// The principal a PAT admitted, or `None` for an active session.
    pat: Option<AuthenticatedPrincipal>,
}

#[derive(Default)]
struct Tokens {
    tokens: HashMap<[u8; 32], Token>,
    /// The highest revision of an identity key this host saw.
    identity_revision: u64,
}

/// The auth facts one host keeps. One instance lives in
/// [`WamnPostgres`](crate::plugins::wamn_postgres::WamnPostgres), so the
/// route authenticator and the claim bind share it.
pub struct AuthCache {
    memory: KvMemory<Answer>,
    tokens: Mutex<Tokens>,
}

impl Default for AuthCache {
    fn default() -> Self {
        Self::with_ttl(MEMORY_TTL)
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
            tokens: Mutex::new(Tokens::default()),
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
        self.tokens().tokens.clear();
    }

    /// The principal of an admitted PAT.
    pub fn pat(&self, token: &str) -> Option<AuthenticatedPrincipal> {
        let ttl = self.memory.ttl();
        self.tokens()
            .tokens
            .get(&digest(token))
            .filter(|token| token.seen.elapsed() < ttl)
            .and_then(|token| token.pat.clone())
    }

    /// Whether the login or PAT behind a session token is active.
    pub fn session_active(&self, token: &str) -> bool {
        let ttl = self.memory.ttl();
        self.tokens()
            .tokens
            .get(&digest(token))
            .is_some_and(|token| token.seen.elapsed() < ttl && token.pat.is_none())
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
        pat: Option<AuthenticatedPrincipal>,
    ) {
        // The lock is held during the check, so a token kept here falls to
        // the clear that follows a broken watch.
        let mut tokens = self.tokens();
        if self.memory.bucket().is_none() {
            return;
        }
        // An identity key changed during the read, so the answer can be stale.
        if tokens.identity_revision != ticket.0 {
            return;
        }
        if tokens.tokens.len() >= SWEEP_AT {
            let ttl = self.memory.ttl();
            tokens.tokens.retain(|_, token| token.seen.elapsed() < ttl);
        }
        tokens.tokens.insert(
            digest(token),
            Token {
                seen: Instant::now(),
                principal: principal.to_owned(),
                pat,
            },
        );
    }

    pub fn keep_pat(&self, ticket: IdentityTicket, token: &str, principal: AuthenticatedPrincipal) {
        let id = principal.principal().id().as_str().to_owned();
        self.keep_token(ticket, token, &id, Some(principal));
    }

    pub fn keep_session_active(&self, ticket: IdentityTicket, token: &str, principal: &str) {
        self.keep_token(ticket, token, principal, None);
    }

    /// The grants of a principal, or the ticket of a read of them.
    pub fn grants(&self, env_key: &str, principal: &str) -> Result<PrincipalGrants, Ticket> {
        let key = principal_key(env_key, principal);
        match self.memory.answer(&key) {
            Ok(Answer::Principal {
                grants: Some(grants),
                ..
            }) if grants.user.is_some() => Ok(grants),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    /// Keep the grants of a principal. A present user row also answers the
    /// provisioned-principal check of the claim bind.
    pub async fn keep_grants(
        &self,
        env_key: &str,
        principal: &str,
        ticket: Ticket,
        grants: PrincipalGrants,
    ) {
        let key = principal_key(env_key, principal);
        let provisioned = grants.present
            || matches!(
                self.memory.replaced(&key, ticket),
                Some(Answer::Principal {
                    provisioned: true,
                    ..
                })
            );
        let answer = Answer::Principal {
            grants: Some(grants),
            provisioned,
        };
        self.memory.put(key, ticket, answer).await;
    }

    /// Whether the tenant has the user row of a principal, or the ticket of
    /// a read of it.
    pub fn provisioned(&self, env_key: &str, principal: &str) -> Result<(), Ticket> {
        let key = principal_key(env_key, principal);
        match self.memory.answer(&key) {
            Ok(Answer::Principal {
                provisioned: true, ..
            }) => Ok(()),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_provisioned(&self, env_key: &str, principal: &str, ticket: Ticket) {
        let key = principal_key(env_key, principal);
        let grants = match self.memory.replaced(&key, ticket) {
            Some(Answer::Principal { grants, .. }) => grants,
            _ => None,
        };
        let answer = Answer::Principal {
            grants,
            provisioned: true,
        };
        self.memory.put(key, ticket, answer).await;
    }

    /// Whether the environment is inactive, or the ticket of a read of it.
    pub fn environment_inactive(&self, env_key: &str) -> Result<bool, Ticket> {
        let key = environment_status_key(env_key);
        match self.memory.answer(&key) {
            Ok(Answer::Environment { inactive }) => Ok(inactive),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_environment_inactive(&self, env_key: &str, ticket: Ticket, inactive: bool) {
        self.memory
            .put(
                environment_status_key(env_key),
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
            tokens.identity_revision = tokens.identity_revision.max(entry.revision);
            tokens
                .tokens
                .retain(|_, token| token.principal != principal);
            return;
        }
        self.memory.apply(entry);
    }
}

impl AsRef<AuthCache> for AuthCache {
    fn as_ref(&self) -> &AuthCache {
        self
    }
}

/// Watch the bucket on the scheduler NATS into the memory that `holder`
/// holds, and return the writer of the host's own changes. A host with
/// `env_key` watches the keys of that `env_key` and the identity keys. The
/// control host serves every tenant, so it watches every key.
pub fn replicate<H: AsRef<AuthCache> + Send + Sync + 'static>(
    client: &async_nats::Client,
    env_key: Option<&str>,
    holder: Arc<H>,
) -> AuthChanges {
    let filters = match env_key {
        Some(env_key) => vec![format!("{env_key}.*"), format!("{IDENTITY_TOKEN}.*")],
        None => vec![">".to_owned()],
    };
    crate::kv_memory::spawn_watch(client, BUCKET, filters, holder, |holder| {
        (*holder).as_ref() as &dyn Watched
    });
    AuthChanges::new(client.clone())
}

/// Live auth-cache fixtures shared by writer tests.
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) use super::test_util::{nats, until, watched};

    #[test]
    fn grants_without_principal_type_are_a_cache_miss() {
        let cache = AuthCache::default();
        let missing: PrincipalGrants = serde_json::from_value(
            serde_json::json!({"present":true,"active":true,"roles":{"admin":[]}}),
        )
        .unwrap();
        assert_eq!(missing.user, None);
        let key = principal_key("t", "p");
        cache.memory.insert(
            &key,
            7,
            Answer::Principal {
                grants: Some(missing.clone()),
                provisioned: true,
            },
        );
        assert_eq!(cache.grants("t", "p"), Err(Ticket(7)));
        for user in [true, false] {
            let grants = PrincipalGrants {
                user: Some(user),
                ..missing.clone()
            };
            cache.memory.insert(
                &key,
                7,
                Answer::Principal {
                    grants: Some(grants.clone()),
                    provisioned: true,
                },
            );
            assert_eq!(cache.grants("t", "p"), Ok(grants));
        }
    }

    #[tokio::test]
    async fn a_host_without_the_bucket_keeps_no_fact() {
        let cache = AuthCache::default();
        let ticket = cache.environment_inactive("t").unwrap_err();
        cache.keep_environment_inactive("t", ticket, true).await;
        let ticket = cache.grants("t", "p").unwrap_err();
        let present = PrincipalGrants {
            present: true,
            user: Some(false),
            ..PrincipalGrants::default()
        };
        cache.keep_grants("t", "p", ticket, present).await;
        cache.keep_session_active(cache.identity_ticket(), "token", "p");
        assert!(cache.environment_inactive("t").is_err());
        assert!(cache.grants("t", "p").is_err());
        assert!(cache.provisioned("t", "p").is_err());
        assert!(!cache.session_active("token"));
    }

    #[tokio::test]
    #[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
    async fn a_fact_expires_after_the_ttl_and_a_clear_drops_it() {
        let (_server, client) = nats().await;
        let cache = Arc::new(AuthCache::with_ttl(Duration::from_millis(50)));
        watched(&client, None, Arc::clone(&cache)).await;
        let ticket = cache.environment_inactive("t").unwrap_err();
        cache.keep_environment_inactive("t", ticket, true).await;
        cache.keep_session_active(cache.identity_ticket(), "token", "p");
        assert_eq!(cache.environment_inactive("t"), Ok(true));
        assert!(cache.session_active("token"));
        assert!(!cache.session_active("other"));

        cache.clear();
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
    #[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
    async fn the_bucket_shares_kept_facts_and_every_drop_reaches_every_host() {
        use wamn_platform_identity::auth_changes::AuthDrops;

        let (_server, client) = nats().await;
        let a = Arc::new(AuthCache::default());
        let b = Arc::new(AuthCache::default());
        let other = Arc::new(AuthCache::default());
        let writer = replicate(&client, Some("t"), Arc::clone(&a));
        replicate(&client, Some("t"), Arc::clone(&b));
        replicate(&client, Some("u"), Arc::clone(&other));
        for cache in [&a, &b, &other] {
            until("the watch runs", || cache.memory.bucket().is_some()).await;
        }
        let present = PrincipalGrants {
            present: true,
            user: Some(false),
            ..PrincipalGrants::default()
        };

        // A fact one host keeps reaches the other host of the tenant only.
        let ticket = a.grants("t", "p").unwrap_err();
        a.keep_grants("t", "p", ticket, present.clone()).await;
        assert_eq!(a.grants("t", "p"), Ok(present.clone()));
        until("host b sees the grants", || b.grants("t", "p").is_ok()).await;
        assert_eq!(b.provisioned("t", "p"), Ok(()));
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

        // A tenant change deletes every key of the tenant.
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
        drops.env_key("t");
        writer.publish(drops).await;
        until("host b drops the tenant", || {
            b.environment_inactive("t").is_err() && b.grants("t", "p").is_err()
        })
        .await;

        assert_eq!(other.grants("u", "p"), Ok(present.clone()));
        assert_eq!(other.environment_inactive("u"), Ok(true));

        // An identity change drops the session of its principal on every
        // host, and a session read during any identity change is not kept.
        a.keep_session_active(a.identity_ticket(), "token-p", "p");
        a.keep_session_active(a.identity_ticket(), "token-q", "q");
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
        b.keep_session_active(read, "token-p", "p");
        assert!(!b.session_active("token-p"));
    }

    #[tokio::test]
    #[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
    async fn present_grants_answer_the_provisioned_check() {
        let (_server, client) = nats().await;
        let cache = Arc::new(AuthCache::default());
        watched(&client, None, Arc::clone(&cache)).await;
        let ticket = cache.grants("t", "a").unwrap_err();
        cache
            .keep_grants("t", "a", ticket, PrincipalGrants::default())
            .await;
        assert!(cache.provisioned("t", "a").is_err());
        let ticket = cache.grants("t", "b").unwrap_err();
        let present = PrincipalGrants {
            present: true,
            user: Some(false),
            ..PrincipalGrants::default()
        };
        cache.keep_grants("t", "b", ticket, present).await;
        assert_eq!(cache.provisioned("t", "b"), Ok(()));
        assert!(cache.provisioned("u", "b").is_err());
    }
}
