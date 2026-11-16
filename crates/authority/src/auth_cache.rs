//! The host's memory of the auth facts of a request
//! (docs/architecture/execution.md#auth-memory).
//!

use std::collections::{BTreeMap, BTreeSet};

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
