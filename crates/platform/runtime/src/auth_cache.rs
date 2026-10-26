//! The host's memory of the auth facts of a request
//! (docs/plan/request-round-trips.md, issue 2).
//!
//! Postgres stays the owner of every fact here. The memory is the local
//! replica of the NATS KV bucket [`BUCKET`] on the scheduler NATS. The key
//! `<tenant>.<principal>` holds the grants of a principal, and the key
//! `<tenant>.environment` holds the environment status. A host that reads a
//! fact from Postgres puts it with the revision of the key from before the
//! read, so a delete in between refuses the put. Every host watches the keys
//! of its tenant and keeps each value it sees. Every writer of a fact deletes
//! its key after the commit ([`wamn_platform_identity::auth_changes`]). The
//! bucket keeps a value for at most [`AUTH_TTL`], and so does the replica.
//!
//! An admitted PAT and an active session stay in the host that read them,
//! because the token answer never leaves the host. A delete of the key
//! `=identity.<principal>` drops them. A host keeps such an answer only when
//! no identity key changed during its read. Only a positive identity answer
//! is kept, so a new PAT or a new login works on its first request.
//!
//! A host keeps facts only while it watches the bucket, because only the
//! watch brings the drops. A host without the bucket, such as a test host,
//! keeps no fact and reads every request from Postgres. So does a host
//! whose watch broke, until the watch runs again.
//!
//! [`BUCKET`]: wamn_platform_identity::auth_changes::BUCKET

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_nats::jetstream::kv::{Operation, Store};
use futures_util::StreamExt as _;
use sha2::{Digest as _, Sha256};
use wamn_platform_identity::AuthenticatedPrincipal;
pub use wamn_platform_identity::auth_changes::AUTH_TTL;
use wamn_platform_identity::auth_changes::{
    AuthChanges, IDENTITY_TOKEN, auth_bucket, environment_key, principal_key,
};

/// A map that grows past this many entries drops its expired ones.
const SWEEP_AT: usize = 10_000;

/// How long a put to the bucket may take before the host answers without
/// keeping the fact.
const PUT_TIMEOUT: Duration = Duration::from_secs(1);

/// The roles a principal holds in one tenant, with the permissions of each.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrincipalGrants {
    /// Whether the tenant has an `platform."user"` row for the principal.
    pub present: bool,
    /// Whether that row is `active`. An inactive principal holds no role.
    pub active: bool,
    /// Only a user can select an active role for a session.
    #[serde(default)]
    pub user: bool,
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

/// The revision of a key that a host saw before it read the fact of the key.
/// A put with it fails when a writer changed the key during the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

/// The identity revision that a host saw before it read a PAT or a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityTicket(u64);

struct Entry {
    revision: u64,
    seen: Instant,
    /// `None` after a delete.
    answer: Option<Answer>,
}

struct Token {
    seen: Instant,
    principal: String,
    /// The principal a PAT admitted, or `None` for an active session.
    pat: Option<AuthenticatedPrincipal>,
}

#[derive(Default)]
struct Replica {
    entries: HashMap<String, Entry>,
    tokens: HashMap<[u8; 32], Token>,
    /// The highest revision of an identity key this host saw.
    identity_revision: u64,
}

/// The auth facts one host keeps. One instance lives in
/// [`WamnPostgres`](crate::plugins::wamn_postgres::WamnPostgres), so the
/// route authenticator and the claim bind share it.
pub struct AuthCache {
    ttl: Duration,
    replica: Mutex<Replica>,
    /// The bucket, while the host watches it. Without it, the host keeps no
    /// fact.
    bucket: RwLock<Option<Store>>,
}

impl Default for AuthCache {
    fn default() -> Self {
        Self::with_ttl(AUTH_TTL)
    }
}

impl std::fmt::Debug for AuthCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthCache")
            .field("ttl", &self.ttl)
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
            ttl,
            replica: Mutex::new(Replica::default()),
            bucket: RwLock::new(None),
        }
    }

    fn replica(&self) -> std::sync::MutexGuard<'_, Replica> {
        self.replica
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The replica, while the host watches the bucket. A caller holds the
    /// replica lock during the check, so a fact kept here falls to the clear
    /// that follows a broken watch.
    fn watched_replica(&self) -> Option<std::sync::MutexGuard<'_, Replica>> {
        let replica = self.replica();
        self.bucket().is_some().then_some(replica)
    }

    fn bucket(&self) -> Option<Store> {
        self.bucket
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_bucket(&self, bucket: Option<Store>) {
        *self
            .bucket
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = bucket;
    }

    /// Drop every fact.
    pub fn clear(&self) {
        let mut replica = self.replica();
        replica.entries.clear();
        replica.tokens.clear();
    }

    /// The live answer of `key`, or the ticket of a read of it. The bucket
    /// expires a value at the same age as the replica.
    fn answer(&self, key: &str) -> Result<Answer, Ticket> {
        let replica = self.replica();
        match replica.entries.get(key) {
            Some(entry) if entry.seen.elapsed() < self.ttl => {
                entry.answer.clone().ok_or(Ticket(entry.revision))
            }
            _ => Err(Ticket(0)),
        }
    }

    /// The ticket of a read of `key`.
    fn ticket(&self, key: &str) -> Ticket {
        let replica = self.replica();
        replica
            .entries
            .get(key)
            .filter(|entry| entry.seen.elapsed() < self.ttl)
            .map_or(Ticket(0), |entry| Ticket(entry.revision))
    }

    /// The answer that a put with `ticket` replaces, when the replica holds
    /// the answer of that revision.
    fn replaced(&self, key: &str, ticket: Ticket) -> Option<Answer> {
        let replica = self.replica();
        replica
            .entries
            .get(key)
            .filter(|entry| entry.revision == ticket.0 && entry.seen.elapsed() < self.ttl)
            .and_then(|entry| entry.answer.clone())
    }

    fn keep_entry(&self, key: String, revision: u64, answer: Option<Answer>) {
        let Some(mut replica) = self.watched_replica() else {
            return;
        };
        if replica.entries.len() >= SWEEP_AT {
            let ttl = self.ttl;
            replica
                .entries
                .retain(|_, entry| entry.seen.elapsed() < ttl);
        }
        let entry = replica.entries.entry(key).or_insert(Entry {
            revision: 0,
            seen: Instant::now(),
            answer: None,
        });
        // The watch also delivers the revision this host put.
        if revision >= entry.revision {
            entry.revision = revision;
            entry.seen = Instant::now();
            entry.answer = answer;
        }
    }

    /// Put `answer` at `key` if the key still has the revision of `ticket`,
    /// and keep it. Without the bucket, keep nothing.
    async fn put(&self, key: String, ticket: Ticket, answer: Answer) {
        let Some(bucket) = self.bucket() else {
            return;
        };
        let bytes = match serde_json::to_vec(&answer) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, "the auth answer does not encode");
                return;
            }
        };
        match tokio::time::timeout(PUT_TIMEOUT, bucket.update(&key, bytes.into(), ticket.0)).await {
            Ok(Ok(revision)) => self.keep_entry(key, revision, Some(answer)),
            // A writer changed the key during the read, or another host put
            // it first. The watch brings the current value.
            Ok(Err(error))
                if error.kind()
                    == async_nats::jetstream::kv::UpdateErrorKind::WrongLastRevision => {}
            Ok(Err(error)) => tracing::warn!(%error, key, "the auth answer was not put"),
            Err(_) => tracing::warn!(key, "the auth answer put timed out"),
        }
    }

    /// The principal of an admitted PAT.
    pub fn pat(&self, token: &str) -> Option<AuthenticatedPrincipal> {
        let replica = self.replica();
        replica
            .tokens
            .get(&digest(token))
            .filter(|token| token.seen.elapsed() < self.ttl)
            .and_then(|token| token.pat.clone())
    }

    /// Whether the login or PAT behind a session token is active.
    pub fn session_active(&self, token: &str) -> bool {
        let replica = self.replica();
        replica
            .tokens
            .get(&digest(token))
            .is_some_and(|token| token.seen.elapsed() < self.ttl && token.pat.is_none())
    }

    /// The ticket of a PAT or session read, taken before the read.
    pub fn identity_ticket(&self) -> IdentityTicket {
        IdentityTicket(self.replica().identity_revision)
    }

    fn keep_token(
        &self,
        ticket: IdentityTicket,
        token: &str,
        principal: &str,
        pat: Option<AuthenticatedPrincipal>,
    ) {
        let Some(mut replica) = self.watched_replica() else {
            return;
        };
        // An identity key changed during the read, so the answer can be stale.
        if replica.identity_revision != ticket.0 {
            return;
        }
        if replica.tokens.len() >= SWEEP_AT {
            let ttl = self.ttl;
            replica.tokens.retain(|_, token| token.seen.elapsed() < ttl);
        }
        replica.tokens.insert(
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
    pub fn grants(&self, tenant: &str, principal: &str) -> Result<PrincipalGrants, Ticket> {
        let key = principal_key(tenant, principal);
        match self.answer(&key) {
            Ok(Answer::Principal {
                grants: Some(grants),
                ..
            }) => Ok(grants),
            Ok(_) => Err(self.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    /// Keep the grants of a principal. A present user row also answers the
    /// provisioned-principal check of the claim bind.
    pub async fn keep_grants(
        &self,
        tenant: &str,
        principal: &str,
        ticket: Ticket,
        grants: PrincipalGrants,
    ) {
        let key = principal_key(tenant, principal);
        let provisioned = grants.present
            || matches!(
                self.replaced(&key, ticket),
                Some(Answer::Principal {
                    provisioned: true,
                    ..
                })
            );
        let answer = Answer::Principal {
            grants: Some(grants),
            provisioned,
        };
        self.put(key, ticket, answer).await;
    }

    /// Whether the tenant has the user row of a principal, or the ticket of
    /// a read of it.
    pub fn provisioned(&self, tenant: &str, principal: &str) -> Result<(), Ticket> {
        let key = principal_key(tenant, principal);
        match self.answer(&key) {
            Ok(Answer::Principal {
                provisioned: true, ..
            }) => Ok(()),
            Ok(_) => Err(self.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_provisioned(&self, tenant: &str, principal: &str, ticket: Ticket) {
        let key = principal_key(tenant, principal);
        let grants = match self.replaced(&key, ticket) {
            Some(Answer::Principal { grants, .. }) => grants,
            _ => None,
        };
        let answer = Answer::Principal {
            grants,
            provisioned: true,
        };
        self.put(key, ticket, answer).await;
    }

    /// Whether the environment is inactive, or the ticket of a read of it.
    pub fn environment_inactive(&self, tenant: &str) -> Result<bool, Ticket> {
        let key = environment_key(tenant);
        match self.answer(&key) {
            Ok(Answer::Environment { inactive }) => Ok(inactive),
            Ok(_) => Err(self.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_environment_inactive(&self, tenant: &str, ticket: Ticket, inactive: bool) {
        self.put(
            environment_key(tenant),
            ticket,
            Answer::Environment { inactive },
        )
        .await;
    }

    /// Keep what the watch of the bucket delivered.
    fn apply(&self, entry: async_nats::jetstream::kv::Entry) {
        if let Some(principal) = entry
            .key
            .strip_prefix(IDENTITY_TOKEN)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            let mut replica = self.replica();
            replica.identity_revision = replica.identity_revision.max(entry.revision);
            replica
                .tokens
                .retain(|_, token| token.principal != principal);
            return;
        }
        let answer = match entry.operation {
            Operation::Put => match serde_json::from_slice(&entry.value) {
                Ok(answer) => Some(answer),
                Err(error) => {
                    tracing::warn!(%error, key = entry.key, "the auth answer does not decode");
                    None
                }
            },
            Operation::Delete | Operation::Purge => None,
        };
        self.keep_entry(entry.key, entry.revision, answer);
    }
}

impl AsRef<AuthCache> for AuthCache {
    fn as_ref(&self) -> &AuthCache {
        self
    }
}

/// Watch the bucket on the scheduler NATS into the memory that `holder`
/// holds, and return the writer of the host's own changes. A host with
/// `tenant` watches the keys of that tenant and the identity keys. The
/// control host serves every tenant, so it watches every key. The watch runs
/// until the client closes, and opens the bucket again after it breaks.
pub fn replicate<H: AsRef<AuthCache> + Send + Sync + 'static>(
    client: &async_nats::Client,
    tenant: Option<&str>,
    holder: Arc<H>,
) -> AuthChanges {
    let filters = match tenant {
        Some(tenant) => vec![format!("{tenant}.*"), format!("{IDENTITY_TOKEN}.*")],
        None => vec![">".to_owned()],
    };
    let client = client.clone();
    let watched = client.clone();
    tokio::spawn(async move {
        loop {
            let cache = (*holder).as_ref();
            if let Err(error) = watch(&watched, &filters, cache).await {
                tracing::warn!(error = format!("{error:#}"), "the auth memory watch broke");
            }
            // Without the watch, a delete can pass the host unseen.
            cache.set_bucket(None);
            cache.clear();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    AuthChanges::new(client)
}

async fn watch(
    client: &async_nats::Client,
    filters: &[String],
    cache: &AuthCache,
) -> anyhow::Result<()> {
    let bucket = auth_bucket(client).await?;
    let mut entries = bucket.watch_many_with_history(filters).await?;
    // Facts kept before the watch ran can miss a delete.
    cache.clear();
    cache.set_bucket(Some(bucket));
    while let Some(entry) = entries.next().await {
        cache.apply(entry?);
    }
    anyhow::bail!("the watch ended")
}

/// Live auth-cache fixtures shared by writer tests.
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) use super::test_util::{nats, until, watched};

    #[tokio::test]
    async fn a_host_without_the_bucket_keeps_no_fact() {
        let cache = AuthCache::default();
        let ticket = cache.environment_inactive("t").unwrap_err();
        cache.keep_environment_inactive("t", ticket, true).await;
        let ticket = cache.grants("t", "p").unwrap_err();
        let present = PrincipalGrants {
            present: true,
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
            until("the watch runs", || cache.bucket().is_some()).await;
        }
        let present = PrincipalGrants {
            present: true,
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
            a.ticket(&principal_key("t", "p")) != ticket
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
        drops.tenant("t");
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
            ..PrincipalGrants::default()
        };
        cache.keep_grants("t", "b", ticket, present).await;
        assert_eq!(cache.provisioned("t", "b"), Ok(()));
        assert!(cache.provisioned("u", "b").is_err());
    }
}
