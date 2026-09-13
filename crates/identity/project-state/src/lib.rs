//! The per-project **system schema v1** — the auth/RBAC/config tables that live
//! in a project database (wamn-as5, `docs/archive/platform-plan.md` §2.4).
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! This crate is the pure MODEL: the schema name, the table/column manifest, and
//! the CHECK literals, kept as a single source and tied to the hand-written DDL
//! [`deploy/sql/app-schema.sql`](../../../../deploy/sql/app-schema.sql) by a drift guard
//! (`tests/schema.rs`) — the `wamn-control-registry` → `deploy/sql/system-schema.sql`
//! precedent. It emits no DDL of its own and holds no connection (the pure /
//! effect-shell house rule); the DDL is the authoritative artifact, this model
//! is what downstream (4.2 AuthN, 4.3 AuthZ, 2.5 migrations) references so they
//! never hard-code the schema name or the status literals.
//!
//! # What it is — and is not
//!
//! The tables are the AUTH/RBAC half of plan item 2.4: [`USERS`], [`ROLES`] (+
//! the user↔role linkage [`USER_ROLES`]), [`PERMISSIONS`], [`CONFIGURATIONS`],
//! [`API_KEYS`]. The "platform metadata" half of 2.4 (entities /
//! fields / relations / flows) is ALREADY shipped — the catalog model in
//! `deploy/sql/catalog-schema.sql` (3.1) — and is referenced, not redefined here.
//!
//! It is DISTINCT from the T1 control-plane registry (`wamn-control-registry` /
//! `deploy/sql/system-schema.sql`): that is the platform-global system DB (orgs /
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
//!
//! # Platform principals
//!
//! A platform component writes under a `users` row named `wamn:<component>`.
//! [`PlatformComponent`] is the closed component list, and
//! [`PlatformComponent::principal_id`] derives the row id. Provisioning and the
//! host compute the same id, so no configuration carries it.

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
    /// the `users_status_check` literals in `deploy/sql/app-schema.sql` by a drift
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

/// The kind of principal a user row names, the `users.type` CHECK domain.
///
/// The column has no default, so every insert names its type. A type does not
/// change how a principal authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserType {
    /// A person.
    Person,
    /// A station or an integration.
    Service,
    /// A platform component named `wamn:<component>`.
    Platform,
}

impl UserType {
    /// Every type. Order is presentational.
    pub const ALL: [UserType; 3] = [UserType::Person, UserType::Service, UserType::Platform];

    /// The CHECK-literal form (`person` / `service` / `platform`).
    pub fn as_str(self) -> &'static str {
        match self {
            UserType::Person => "person",
            UserType::Service => "service",
            UserType::Platform => "platform",
        }
    }
}

impl std::fmt::Display for UserType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The fixed WAMN namespace for platform principal ids. Every tenant and every
/// deployment derives the same id from the same name, so no configuration
/// carries the id. The value is frozen: a change changes every platform row id.
/// Anyone can recompute it from the domain.
///
/// `WAMN_NAMESPACE = uuid5(NAMESPACE_DNS, "wamn.dev")`
pub const WAMN_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x0df2_99cf_7085_537d_919b_1145_b5ef_00f8);

/// The reserved principal namespace. A platform principal name is
/// `wamn:<component>`, where the component is kebab-case and carries no action
/// and no version. A tenant or application cannot create a name with this prefix.
pub const PLATFORM_PRINCIPAL_PREFIX: &str = "wamn:";

/// A platform component that writes under its own `app_system.users` row.
///
/// The row names the component, never the invocation. The list is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformComponent {
    /// Tenant provisioning. Its row is the first row in a tenant database.
    Provisioning,
    /// Package application, including operation grants.
    ApplyPackage,
    /// Post-commit registration delivery.
    Materializer,
    /// Executor queue delivery and management candidate cases.
    Executor,
}

impl PlatformComponent {
    /// Every component. Order is presentational.
    pub const ALL: [PlatformComponent; 4] = [
        PlatformComponent::Provisioning,
        PlatformComponent::ApplyPackage,
        PlatformComponent::Materializer,
        PlatformComponent::Executor,
    ];

    /// The kebab-case component name, for example `apply-package`.
    pub fn as_str(self) -> &'static str {
        match self {
            PlatformComponent::Provisioning => "provisioning",
            PlatformComponent::ApplyPackage => "apply-package",
            PlatformComponent::Materializer => "materializer",
            PlatformComponent::Executor => "executor",
        }
    }

    /// The principal name `wamn:<component>`, which `display_name` carries.
    pub fn principal_name(self) -> &'static str {
        match self {
            PlatformComponent::Provisioning => "wamn:provisioning",
            PlatformComponent::ApplyPackage => "wamn:apply-package",
            PlatformComponent::Materializer => "wamn:materializer",
            PlatformComponent::Executor => "wamn:executor",
        }
    }

    /// The `app_system.users` id: the UUIDv5 of [`Self::principal_name`] under
    /// [`WAMN_NAMESPACE`].
    pub fn principal_id(self) -> uuid::Uuid {
        uuid::Uuid::new_v5(&WAMN_NAMESPACE, self.principal_name().as_bytes())
    }
}

/// A table in the system schema and the load-bearing columns the DDL drift guard
/// pins. `columns` is a curated set (PK / FK / claim-target / enum columns), not
/// the exhaustive DDL — pinning every column would make the guard brittle
/// (the `wamn-control-registry` distinctive-column precedent).
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
    columns: &["tenant_id", "id", "type", "email", "display_name", "status"],
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
    fn user_type_as_str_is_stable() {
        assert_eq!(UserType::Person.as_str(), "person");
        assert_eq!(UserType::Service.as_str(), "service");
        assert_eq!(UserType::Platform.as_str(), "platform");
        for t in UserType::ALL {
            assert_eq!(t.to_string(), t.as_str());
        }
    }

    #[test]
    fn table_manifest_is_complete_and_unique() {
        // Five auth/RBAC concepts, with the user↔role linkage split out as its
        // own table, make six tables.
        assert_eq!(TABLES.len(), 6);
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
    fn wamn_namespace_derives_from_the_domain() {
        assert_eq!(
            WAMN_NAMESPACE,
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"wamn.dev")
        );
    }

    #[test]
    fn platform_principal_ids_are_pinned() {
        // Computed independently with Python `uuid.uuid5`.
        let pinned = [
            (
                PlatformComponent::Provisioning,
                "770df186-ac15-579e-b46b-c297cae2011b",
            ),
            (
                PlatformComponent::ApplyPackage,
                "7695180f-4b9a-581f-84ef-d7e9cdbd2b77",
            ),
            (
                PlatformComponent::Materializer,
                "968bd0cc-e612-5d29-9d6c-af1993b8df0a",
            ),
            (
                PlatformComponent::Executor,
                "d318d033-29ea-5cb0-ab56-24340413fbcc",
            ),
        ];
        assert_eq!(
            pinned.map(|(component, _)| component),
            PlatformComponent::ALL
        );
        for (component, id) in pinned {
            assert_eq!(component.principal_id().to_string(), id, "{component:?}");
        }
    }

    #[test]
    fn platform_principal_names_follow_the_grammar() {
        for component in PlatformComponent::ALL {
            let name = component.as_str();
            assert_eq!(
                component.principal_name(),
                format!("{PLATFORM_PRINCIPAL_PREFIX}{name}")
            );
            let mut bytes = name.bytes();
            assert!(
                bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                    && bytes.all(|byte| byte.is_ascii_lowercase() || byte == b'-')
                    && !name.ends_with('-')
                    && !name.contains("--"),
                "{name} is not kebab-case"
            );
        }
    }

    #[test]
    fn qualified_prepends_the_schema() {
        assert_eq!(USERS.qualified(), "app_system.users");
    }
}
