//! Project authorization contracts and held grants.

use std::{collections::BTreeSet, time::SystemTime};
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_identity_protocol::PrincipalId;

/// The application authority a caller holds now: whether it holds `admin`, and
/// the stable operation references its other roles grant under the loaded
/// release (docs/architecture/execution.md#routing-and-availability).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldOperationGrants {
    pub admin: bool,
    /// A service holds permissions but never selects a user's active role.
    pub user: bool,
    pub references: BTreeSet<String>,
    /// The names of the held roles.
    pub roles: BTreeSet<String>,
}

impl HeldOperationGrants {
    /// The one role a caller with these grants acts with
    /// (docs/architecture/execution.md#environment-workflows): `admin` for a
    /// holder of `admin`, the single role of a caller that holds exactly one,
    /// and none for a caller that holds no role or more than one.
    pub fn active_role(&self) -> Option<Box<str>> {
        if !self.user {
            return None;
        }
        self.pat_role()
    }

    /// A PAT caller selects only a role held by its own active principal.
    /// Service callers never inherit the role of a user session.
    pub fn pat_role(&self) -> Option<Box<str>> {
        if self.admin {
            return Some(wamn_identity_protocol::ADMIN_ROLE.into());
        }
        let mut roles = self.roles.iter();
        match (roles.next(), roles.next()) {
            (Some(role), None) => Some(role.as_str().into()),
            _ => None,
        }
    }
}

/// Collect permissions and roles from an active project principal.
pub fn held_grants(grants: &PrincipalGrants) -> HeldOperationGrants {
    let mut held = HeldOperationGrants::default();
    if !grants.active || !grants.present {
        return held;
    }
    held.user = grants.user;
    for (role, permissions) in &grants.roles {
        held.admin |= role == wamn_identity_protocol::ADMIN_ROLE;
        held.references.extend(permissions.iter().cloned());
        held.roles.insert(role.clone());
    }
    held
}

/// The active local service admitted by a PAT.
#[derive(Debug)]
pub struct ProjectPat {
    pub principal: PrincipalId,
    pub subject: Box<str>,
    pub deadline: SystemTime,
}

#[derive(Debug)]
pub struct SessionIdentityError {
    pub type_: SessionIdentityErrorType,
    pub context: &'static str,
    pub source: anyhow::Error,
}

#[derive(Debug)]
pub enum SessionIdentityErrorType {
    Authorization,
    Infrastructure,
}

impl std::fmt::Display for SessionIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for SessionIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

mod project_reads;
mod store;

pub use store::Authorization;

#[cfg(test)]
mod tests;
