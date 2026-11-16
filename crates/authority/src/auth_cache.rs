//! The host's memory of the auth facts of a request
//! (docs/architecture/execution.md#auth-memory).
//!

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use wamn_broker::Publisher;

use wamn_platform_identity::AuthenticatedPrincipal;

pub use crate::kv_memory::Ticket;

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

struct Token {
    deadline: SystemTime,
    /// The principal a PAT admitted, or `None` for an active session.
    pat: Option<AuthenticatedPrincipal>,
}

struct CredentialDeadline;
impl moka::Expiry<wamn_digest::Digest, Arc<Token>> for CredentialDeadline {
    fn expire_after_create(
        &self,
        _: &wamn_digest::Digest,
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
    tokens: moka::sync::Cache<wamn_digest::Digest, Arc<Token>>,
}

/// The auth facts one host keeps. One instance lives in
/// [`WamnPostgres`](wamn_sql::WamnPostgres), so the
/// route authenticator and the identity binding share it.
pub struct AuthCache {
    tokens: Mutex<Tokens>,
}

impl Default for AuthCache {
    fn default() -> Self {
        Self::with_ttl(Duration::ZERO)
    }
}

impl std::fmt::Debug for AuthCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("AuthCache").finish_non_exhaustive()
    }
}

fn digest(token: &str) -> wamn_digest::Digest {
    wamn_digest::bytes_sha256(token.as_bytes())
}

impl AuthCache {
    /// A cache whose facts live for `ttl`.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            tokens: Mutex::new(Tokens {
                tokens: moka::sync::Cache::builder()
                    .time_to_live(ttl)
                    .expire_after(CredentialDeadline)
                    .build(),
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
        let tokens = self.tokens();
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

    /// The grants of a principal, or the ticket of a read of them.
    pub fn grants(
        &self,
        _project_env_key: &wamn_registry::ProjectEnvKey,
        _principal: &wamn_identity_protocol::PrincipalId,
    ) -> Result<PrincipalGrants, Ticket> {
        Err(Ticket(0))
    }

    /// Keep the grants of a principal. A present user row also answers the
    /// provisioned-principal test of the identity binding.
    pub async fn keep_grants(
        &self,
        _project_env_key: &wamn_registry::ProjectEnvKey,
        _principal: &wamn_identity_protocol::PrincipalId,
        _ticket: Ticket,
        _grants: PrincipalGrants,
    ) {
    }
}

pub fn replicate(
    _client: &async_nats::Client,
    _project_env_key: Option<&wamn_registry::ProjectEnvKey>,
    _holder: Arc<AuthCache>,
) -> Publisher {
    Publisher::default()
}

/// Live auth-cache fixtures shared by writer tests.
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

/// The caller of a run that executes as `principal_id` with `role` active,
/// from the roles the principal `held` with their permissions: the
/// permissions of `role` only, and `admin` only when `role` is `admin`. None
/// when the principal does not hold `role`.
pub fn role_caller(
    principal_id: &wamn_identity_protocol::PrincipalId,
    role: &str,
    held: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> Option<wamn_engine::flow_http_routing::AuthenticatedCaller> {
    let permissions = held.get(role)?;
    Some(wamn_engine::flow_http_routing::AuthenticatedCaller::new(
        "workflow".parse().unwrap(),
        *principal_id,
        wamn_engine::flow_http_routing::CredentialType::QueuedService,
        role == wamn_identity_protocol::ADMIN_ROLE,
        permissions.iter().cloned().collect(),
        Some(role.into()),
    ))
}

#[cfg(test)]
mod role_tests {
    use super::*;
    use wamn_identity_protocol::ADMIN_ROLE;

    /// A queued run executes with its one role active: a principal holding
    /// roles `a` and `b`, queued with `a`, holds `a`'s operations only; it is
    /// `admin` only when the role is `admin`; a role it does not hold refuses.
    #[test]
    fn a_queued_run_holds_the_operations_of_its_role_only() {
        let held = BTreeMap::from([
            ("a".to_owned(), BTreeSet::from(["app:order/get".to_owned()])),
            ("b".to_owned(), BTreeSet::from(["app:order/put".to_owned()])),
            (ADMIN_ROLE.to_owned(), BTreeSet::new()),
        ]);
        let principal = "00000000-0000-4000-8000-000000000001".parse().unwrap();
        let caller = role_caller(&principal, "a", &held).expect("the principal holds a");
        assert!(caller.permits("app:order/get@1.0.0"));
        assert!(!caller.permits("app:order/put@1.0.0"));
        assert!(!caller.admin());
        assert_eq!(caller.active_role(), Some("a"));

        let admin = role_caller(&principal, ADMIN_ROLE, &held).expect("the principal holds admin");
        assert!(admin.admin());
        assert!(admin.permits("app:order/put@1.0.0"));

        assert!(role_caller(&principal, "c", &held).is_none());
        let without_admin =
            BTreeMap::from([("a".to_owned(), BTreeSet::from(["app:order/get".to_owned()]))]);
        assert!(role_caller(&principal, ADMIN_ROLE, &without_admin).is_none());
    }
}
