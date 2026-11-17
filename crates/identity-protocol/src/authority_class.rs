//! The closed authority a trusted caller selects its credential under.
//!
//! `wamn-0h0g.22.14` ruled this vocabulary closed and total: one role family per
//! class, no wildcard arm, no default arm, and an added or unmapped class is a
//! COMPILE ERROR rather than a runtime fallback. The projection onto the
//! provisioning families lives beside `WorkloadRoleFamily` in
//! `wamn-provision`, because that is where the target type is defined.
//!
//! # Why there is deliberately no parser
//!
//! A class originates ONLY in trusted host composition or operator-owned
//! workload binding (`wamn-0h0g.22.8`), so a new class is a code change and
//! never a configuration string. Adding `FromStr`, `From<&str>` or a
//! `Deserialize` impl here would reopen precisely the guest-selected-class path
//! the ruling closes. [`AuthorityClass::as_str`] is one-way on purpose.

use std::fmt;

/// Read the current operation user's membership in the bound ACL role.
///
/// A missing role returns false. This query grants no authority.
pub const CURRENT_USER_ROLE_MEMBERSHIP_SQL: &str = "SELECT EXISTS ( \
    SELECT 1 FROM pg_catalog.pg_roles AS authority \
     WHERE authority.rolname = $1 \
       AND pg_catalog.pg_has_role(CURRENT_USER, authority.oid, 'MEMBER'))";

/// Which authority a trusted caller acts under when it selects a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorityClass {
    /// Project component `wamn:postgres` imports.
    GuestSql,
    /// Dynamic SQL reads use a separate SELECT-only login.
    DynamicSql,
    /// All host platform work, including retention.
    Platform,
}

impl AuthorityClass {
    /// Every class, in declaration order, for table-driven tests.
    pub const ALL: [Self; 3] = [Self::GuestSql, Self::DynamicSql, Self::Platform];

    /// The stable NOLOGIN ACL role this authority's generations inherit.
    pub const fn acl_role(self) -> &'static str {
        match self {
            Self::GuestSql => wamn_role::APP_ROLE,
            Self::DynamicSql => wamn_role::QUERY_ROLE,
            Self::Platform => wamn_role::PLATFORM_ROLE,
        }
    }

    /// Stable label for cache keys, telemetry and error text.
    ///
    /// One-way by design: no inverse exists, so a label can never be turned
    /// back into a class. See the module doc.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GuestSql => "guest-sql",
            Self::DynamicSql => "dynamic-sql",
            Self::Platform => "platform",
        }
    }
}

impl fmt::Display for AuthorityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
