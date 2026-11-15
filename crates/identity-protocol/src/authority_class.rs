//! The closed authority a trusted caller selects its credential under.
//!
//! `wamn-0h0g.22.14` ruled this vocabulary closed and total: one role family per
//! class, no wildcard arm, no default arm, and an added or unmapped class is a
//! COMPILE ERROR rather than a runtime fallback. The projection onto the
//! provisioning families lives beside `WorkloadRoleFamily` in
//! `wamn-control-provision`, because that is where the target type is defined.
//!
//! # Why there is deliberately no parser
//!
//! A class originates ONLY in trusted host composition or operator-owned
//! workload binding (`wamn-0h0g.22.8`), so a new class is a code change and
//! never a configuration string. Adding `FromStr`, `From<&str>` or a
//! `Deserialize` impl here would reopen precisely the guest-selected-class path
//! the ruling closes. [`AuthorityClass::as_str`] is one-way on purpose.

use std::fmt;

/// Stable database role for wamn_app.
pub const APP_ROLE: &str = "wamn_app";
/// Stable database role for wamn_query.
pub const QUERY_ROLE: &str = "wamn_query";
/// Stable database role for wamn_executor_platform.
pub const EXECUTOR_PLATFORM_ROLE: &str = "wamn_executor_platform";
/// Stable database role for wamn_http_admitter.
pub const HTTP_ADMITTER_ROLE: &str = "wamn_http_admitter";
/// Stable database role for wamn_administration.
pub const ADMINISTRATION_ROLE: &str = "wamn_administration";

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
    /// Executor run-state and catalog work.
    ExecutorPlatform,
    /// Connection-HTTP authorization and effect-snapshot operations.
    CallableHttp,
    /// Host-run administration routes and their writes of the application
    /// authorization relations (docs/architecture/data-access.md#actors).
    Administration,
}

impl AuthorityClass {
    /// Every class, in declaration order, for table-driven tests.
    pub const ALL: [Self; 5] = [
        Self::GuestSql,
        Self::DynamicSql,
        Self::ExecutorPlatform,
        Self::CallableHttp,
        Self::Administration,
    ];

    /// The stable NOLOGIN ACL role this authority's generations inherit.
    pub const fn acl_role(self) -> &'static str {
        match self {
            Self::GuestSql => APP_ROLE,
            Self::DynamicSql => QUERY_ROLE,
            Self::ExecutorPlatform => EXECUTOR_PLATFORM_ROLE,
            Self::CallableHttp => HTTP_ADMITTER_ROLE,
            Self::Administration => ADMINISTRATION_ROLE,
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
            Self::ExecutorPlatform => "executor-platform",
            Self::CallableHttp => "callable-http",
            Self::Administration => "administration",
        }
    }
}

impl fmt::Display for AuthorityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
