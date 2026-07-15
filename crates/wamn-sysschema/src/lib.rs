//! The per-project **system schema v1** — the auth/RBAC/config tables that live
//! in a project database (wamn-as5, `docs/platform-plan.md` §2.4).
//!
//! This crate is the pure MODEL: the schema name, the table/column manifest, and
//! the CHECK literals, kept as a single source and tied to the hand-written DDL
//! [`deploy/app-schema.sql`](../../../deploy/app-schema.sql) by a drift guard
//! (`tests/schema.rs`) — the `wamn-registry` → `deploy/system-schema.sql`
//! precedent. It emits no DDL of its own and holds no connection (the pure /
//! effect-shell house rule); the DDL is the authoritative artifact, this model
//! is what downstream (4.2 AuthN, 4.3 AuthZ, 2.5 migrations) references so they
//! never hard-code the schema name or the status literals.
//!
//! # What it is — and is not
//!
//! The tables are the AUTH/RBAC half of plan item 2.4: [`USERS`], [`ROLES`] (+
//! the user↔role linkage [`USER_ROLES`]), [`PERMISSIONS`], [`CONFIGURATIONS`],
//! [`AUDIT_LOG`], [`API_KEYS`]. The "platform metadata" half of 2.4 (entities /
//! fields / relations / flows) is ALREADY shipped — the catalog model in
//! `deploy/catalog-schema.sql` (3.1) and the flow registry in `deploy/flows.sql`
//! (POC-F1) — and is referenced, not redefined here.
//!
//! It is DISTINCT from the T1 control-plane registry (`wamn-registry` /
//! `deploy/system-schema.sql`): that is the platform-global system DB (orgs /
//! projects / envs), owned by `wamn_system`, not tenant-scoped. This schema is
//! PER-PROJECT TENANT DATA under the same RLS floor as the catalog.
//!
//! # Claim integration (3.5 / 4.2)
//!
//! [`USER_ID_CLAIM`] (`app.user_id`) resolves to a `users.id` (`uuid`) — the
//! ownership target the 3.5 RLS builder reads. [`ROLE_CLAIM`] (`app.role`)
//! resolves to a `roles.name` (text) — the role-gate target. [`TENANT_CLAIM`]
//! (`app.tenant`) is the RLS floor every table keys on. The claims are injected
//! by the plugin from a resolved session (4.2); this schema is the substrate.

/// The Postgres schema the tables live in. The single source both the DDL and
/// downstream consumers (`SET search_path` / qualified queries) reference.
pub const SCHEMA_NAME: &str = "app_system";

/// Storage-format version, additive-within-major per the `0.1.x` freeze. A
/// schema model, not a published JSON-Schema contract — no generated file.
pub const SCHEMA_VERSION: &str = "0.1";

/// The tenant RLS-floor claim every table keys on (`app.tenant`).
pub const TENANT_CLAIM: &str = "app.tenant";

/// The per-user claim the 3.5 RLS builder reads as
/// `NULLIF(current_setting('app.user_id', true), '')::uuid` — so the ownership
/// target ([`USERS`]`.id`) is a `uuid`.
pub const USER_ID_CLAIM: &str = "app.user_id";

/// The per-role claim the 3.5 RLS builder reads as
/// `COALESCE(current_setting('app.role', true), '') IN (...)` — so the gate
/// compares against [`ROLES`]`.name` (text).
pub const ROLE_CLAIM: &str = "app.role";

/// A user's account status — the `users.status` CHECK domain. `4.2` decides
/// whether a status may authenticate; this schema only constrains the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserStatus {
    /// A usable account.
    Active,
    /// Suspended — retained, but may not authenticate.
    Disabled,
    /// Provisioned but not yet accepted.
    Invited,
}

impl UserStatus {
    /// Every status. Order is presentational.
    pub const ALL: [UserStatus; 3] = [
        UserStatus::Active,
        UserStatus::Disabled,
        UserStatus::Invited,
    ];

    /// The wire / CHECK-literal form (`active` / `disabled` / `invited`), tied to
    /// the `users_status_check` literals in `deploy/app-schema.sql` by a drift
    /// guard.
    pub fn as_str(self) -> &'static str {
        match self {
            UserStatus::Active => "active",
            UserStatus::Disabled => "disabled",
            UserStatus::Invited => "invited",
        }
    }
}

impl std::fmt::Display for UserStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A table in the system schema and the load-bearing columns the DDL drift guard
/// pins. `columns` is a curated set (PK / FK / claim-target / enum columns), not
/// the exhaustive DDL — pinning every column would make the guard brittle
/// (the `wamn-registry` distinctive-column precedent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table {
    /// The bare table name (unqualified; prepend [`SCHEMA_NAME`] to qualify).
    pub name: &'static str,
    /// The load-bearing columns pinned by the drift guard.
    pub columns: &'static [&'static str],
}

impl Table {
    /// The schema-qualified name, e.g. `app_system.users`.
    pub fn qualified(&self) -> String {
        format!("{SCHEMA_NAME}.{}", self.name)
    }
}

/// Application accounts. `id` (`uuid`) is the [`USER_ID_CLAIM`] ownership target.
pub const USERS: Table = Table {
    name: "users",
    columns: &["tenant_id", "id", "email", "display_name", "status"],
};

/// Named roles. `name` is the [`ROLE_CLAIM`] gate target.
pub const ROLES: Table = Table {
    name: "roles",
    columns: &["tenant_id", "name", "is_system"],
};

/// The user↔role linkage (many-to-many).
pub const USER_ROLES: Table = Table {
    name: "user_roles",
    columns: &["tenant_id", "user_id", "role_name"],
};

/// Role → permission grants (read by 4.3 AuthZ).
pub const PERMISSIONS: Table = Table {
    name: "permissions",
    columns: &["tenant_id", "role_name", "permission"],
};

/// Per-project application settings (opaque jsonb value).
pub const CONFIGURATIONS: Table = Table {
    name: "configurations",
    columns: &["tenant_id", "config_key", "config_value"],
};

/// Append-only audit trail. `actor_id` is a bare uuid (not FK'd — immutable
/// history survives user deletion).
pub const AUDIT_LOG: Table = Table {
    name: "audit_log",
    columns: &["tenant_id", "actor_id", "action", "occurred_at"],
};

/// The api-key substrate. `key_hash` is a one-way digest, never the raw key.
pub const API_KEYS: Table = Table {
    name: "api_keys",
    columns: &["tenant_id", "user_id", "key_hash", "prefix"],
};

/// Every table in the system schema, in dependency order (a superset FK order:
/// users and roles before the linkage / permissions / api_keys that reference
/// them).
pub const TABLES: &[Table] = &[
    USERS,
    ROLES,
    USER_ROLES,
    PERMISSIONS,
    CONFIGURATIONS,
    AUDIT_LOG,
    API_KEYS,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_status_as_str_is_stable() {
        assert_eq!(UserStatus::Active.as_str(), "active");
        assert_eq!(UserStatus::Disabled.as_str(), "disabled");
        assert_eq!(UserStatus::Invited.as_str(), "invited");
        // Every variant is covered by ALL, and the display form matches.
        for s in UserStatus::ALL {
            assert_eq!(s.to_string(), s.as_str());
        }
    }

    #[test]
    fn table_manifest_is_complete_and_unique() {
        // The plan's six auth/RBAC concepts, with the user↔role linkage split
        // out as its own table = seven tables.
        assert_eq!(TABLES.len(), 7);
        let mut names: Vec<&str> = TABLES.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TABLES.len(), "table names must be unique");
        // Every table carries the tenant floor column.
        for t in TABLES {
            assert!(
                t.columns.contains(&"tenant_id"),
                "{} is tenant-scoped and must pin tenant_id",
                t.name
            );
        }
    }

    #[test]
    fn qualified_prepends_the_schema() {
        assert_eq!(USERS.qualified(), "app_system.users");
    }
}
