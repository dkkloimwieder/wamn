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
    /// Materializer-owned event consumption and admission SQL.
    EventMaterializer,
    /// Host-run administration routes and their writes of the application
    /// authorization relations (docs/architecture/data-access.md#actors).
    Administration,
}

impl AuthorityClass {
    /// Every class, in declaration order, for table-driven tests.
    pub const ALL: [Self; 6] = [
        Self::GuestSql,
        Self::DynamicSql,
        Self::ExecutorPlatform,
        Self::CallableHttp,
        Self::EventMaterializer,
        Self::Administration,
    ];

    /// The stable NOLOGIN ACL role this authority's generations inherit.
    ///
    /// Duplicated here rather than reached through `WorkloadRoleFamily` because
    /// `wamn-control-provision` is a DEV-ONLY dependency of the runtime — the
    /// shipped runtime links no provisioner — and the runtime must be able to
    /// name the role it expects a connection to actually hold. The provisioning
    /// projection carries a drift guard asserting the two agree, so the
    /// duplication cannot diverge silently.
    pub const fn acl_role(self) -> &'static str {
        match self {
            Self::GuestSql => "wamn_app",
            Self::DynamicSql => "wamn_query",
            Self::ExecutorPlatform => "wamn_executor_platform",
            Self::CallableHttp => "wamn_http_admitter",
            Self::EventMaterializer => "wamn_event_materializer",
            Self::Administration => "wamn_administration",
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
            Self::EventMaterializer => "event-materializer",
            Self::Administration => "administration",
        }
    }
}

impl fmt::Display for AuthorityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::AuthorityClass;

    #[test]
    fn display_matches_the_label() {
        for class in AuthorityClass::ALL {
            assert_eq!(class.to_string(), class.as_str());
        }
    }
}
