//! The host's memory of the auth facts of a request
//! (docs/plan/request-round-trips.md, issue 2).
//!
//! Postgres stays the owner of every fact here. The host reads a fact once,
//! keeps it for at most [`AUTH_TTL`], and answers the next request from
//! memory. Every writer of a fact here publishes the change after its commit
//! ([`wamn_platform_identity::auth_changes`]), and every host of the tenant
//! drops its whole memory. The TTL is the backstop for a lost message.
//!
//! Only a positive identity answer is kept, so a new PAT or a new login works
//! on its first request.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};
use wamn_platform_identity::AuthenticatedPrincipal;

/// How long the host keeps a fact that no message dropped.
pub const AUTH_TTL: Duration = Duration::from_secs(60);

/// A map that grows past this many entries drops its expired ones.
const SWEEP_AT: usize = 10_000;

/// The roles a principal holds in one tenant, with the permissions of each.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalGrants {
    /// Whether the tenant has an `app_system.users` row for the principal.
    pub present: bool,
    /// Whether that row is `active`. An inactive principal holds no role.
    pub active: bool,
    /// Each held role, with each permission of that role.
    pub roles: BTreeMap<String, BTreeSet<String>>,
}

struct Expiring<K, V>(Mutex<HashMap<K, (Instant, V)>>);

impl<K: Eq + Hash, V: Clone> Expiring<K, V> {
    fn get(&self, key: &K, ttl: Duration) -> Option<V> {
        let entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries
            .get(key)
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, value)| value.clone())
    }

    fn insert(&self, key: K, value: V, ttl: Duration) {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= SWEEP_AT {
            entries.retain(|_, (at, _)| at.elapsed() < ttl);
        }
        entries.insert(key, (Instant::now(), value));
    }

    fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl<K, V> Default for Expiring<K, V> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

/// The auth facts one host keeps. One instance lives in
/// [`WamnPostgres`](crate::plugins::wamn_postgres::WamnPostgres), so the
/// route authenticator and the claim bind share it.
pub struct AuthCache {
    ttl: Duration,
    /// PAT digest → the principal its route authentication admitted.
    pats: Expiring<[u8; 32], AuthenticatedPrincipal>,
    /// Session token digest → the login or PAT behind it is active.
    sessions: Expiring<[u8; 32], ()>,
    /// (project, tenant, principal) → its roles and permissions.
    grants: Expiring<(String, String, String), PrincipalGrants>,
    /// (project, tenant, principal) → the tenant has its users row.
    provisioned: Expiring<(String, String, String), ()>,
    /// (project, tenant) → the environment is inactive.
    environments: Expiring<(String, String), bool>,
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

fn key(project: &str, tenant: &str, principal: &str) -> (String, String, String) {
    (project.to_owned(), tenant.to_owned(), principal.to_owned())
}

impl AuthCache {
    /// A cache whose facts live for `ttl`.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            pats: Expiring::default(),
            sessions: Expiring::default(),
            grants: Expiring::default(),
            provisioned: Expiring::default(),
            environments: Expiring::default(),
        }
    }

    /// Drop every fact. A published auth change calls this.
    pub fn clear(&self) {
        self.pats.clear();
        self.sessions.clear();
        self.grants.clear();
        self.provisioned.clear();
        self.environments.clear();
    }

    pub fn pat(&self, token: &str) -> Option<AuthenticatedPrincipal> {
        self.pats.get(&digest(token), self.ttl)
    }

    pub fn keep_pat(&self, token: &str, principal: AuthenticatedPrincipal) {
        self.pats.insert(digest(token), principal, self.ttl);
    }

    pub fn session_active(&self, token: &str) -> bool {
        self.sessions.get(&digest(token), self.ttl).is_some()
    }

    pub fn keep_session_active(&self, token: &str) {
        self.sessions.insert(digest(token), (), self.ttl);
    }

    pub fn grants(&self, project: &str, tenant: &str, principal: &str) -> Option<PrincipalGrants> {
        self.grants.get(&key(project, tenant, principal), self.ttl)
    }

    /// Keep the grants of a principal. A present users row also answers the
    /// provisioned-principal check of the claim bind.
    pub fn keep_grants(
        &self,
        project: &str,
        tenant: &str,
        principal: &str,
        grants: PrincipalGrants,
    ) {
        if grants.present {
            self.keep_provisioned(project, tenant, principal);
        }
        self.grants
            .insert(key(project, tenant, principal), grants, self.ttl);
    }

    pub fn provisioned(&self, project: &str, tenant: &str, principal: &str) -> bool {
        self.provisioned
            .get(&key(project, tenant, principal), self.ttl)
            .is_some()
    }

    pub fn keep_provisioned(&self, project: &str, tenant: &str, principal: &str) {
        self.provisioned
            .insert(key(project, tenant, principal), (), self.ttl);
    }

    pub fn environment_inactive(&self, project: &str, tenant: &str) -> Option<bool> {
        self.environments
            .get(&(project.to_owned(), tenant.to_owned()), self.ttl)
    }

    pub fn keep_environment_inactive(&self, project: &str, tenant: &str, inactive: bool) {
        self.environments
            .insert((project.to_owned(), tenant.to_owned()), inactive, self.ttl);
    }
}

/// Clear the memory of `postgres` on each change of the facts of `tenant`
/// and on each change of an identity fact. The subscriptions run until the
/// client closes.
pub async fn clear_on_changes(
    client: &async_nats::Client,
    tenant: &str,
    postgres: std::sync::Arc<crate::plugins::wamn_postgres::WamnPostgres>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use futures_util::StreamExt as _;
    use wamn_platform_identity::auth_changes::{IDENTITY_SUBJECT, tenant_subject};
    let mut changes = futures_util::stream::select(
        client.subscribe(tenant_subject(tenant)).await?,
        client.subscribe(IDENTITY_SUBJECT).await?,
    );
    Ok(tokio::spawn(async move {
        while changes.next().await.is_some() {
            postgres.auth_cache().clear();
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fact_expires_after_the_ttl_and_a_clear_drops_it() {
        let cache = AuthCache::with_ttl(Duration::from_millis(50));
        cache.keep_environment_inactive("p", "t", true);
        cache.keep_session_active("token");
        assert_eq!(cache.environment_inactive("p", "t"), Some(true));
        assert!(cache.session_active("token"));
        assert!(!cache.session_active("other"));

        cache.clear();
        assert_eq!(cache.environment_inactive("p", "t"), None);
        assert!(!cache.session_active("token"));

        cache.keep_environment_inactive("p", "t", false);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(cache.environment_inactive("p", "t"), None);
    }

    #[test]
    fn present_grants_answer_the_provisioned_check() {
        let cache = AuthCache::default();
        cache.keep_grants("p", "t", "a", PrincipalGrants::default());
        assert!(!cache.provisioned("p", "t", "a"));
        cache.keep_grants(
            "p",
            "t",
            "b",
            PrincipalGrants {
                present: true,
                ..PrincipalGrants::default()
            },
        );
        assert!(cache.provisioned("p", "t", "b"));
        assert!(!cache.provisioned("p", "u", "b"));
    }
}
