//! The RLS rule model (3.5).
//!
//! An [`AccessPolicy`] is a set of [`Rule`]s attached to a catalog (3.1). Each
//! rule compiles to one Postgres `CREATE POLICY … AS RESTRICTIVE`, layered on
//! top of the 3.2 **tenant floor** so it narrows access *within* a tenant (never
//! widens it). The rules key on the session claims the Postgres plugin injects —
//! `app.role` and `app.user_id` (the per-user/role counterparts of the floor's
//! `app.tenant`), set alongside the tenant claim per 4.2.
//!
//! This is **data, not DDL**: the compiler (`compile`) turns it into policy
//! statements. Rules are stored as jsonb in `catalog.rls_policies` (the crate is
//! the source of truth for their semantics).

use serde::{Deserialize, Serialize};

use wamn_catalog::{EntityId, FieldId};

/// The rule-model **format** version. Compatibility rule mirrors the catalog /
/// flow / WIT freezes: `0.1.x` is additive/clarifying only.
pub const SCHEMA_VERSION: &str = "0.1";

/// A set of access rules attached to a catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AccessPolicy {
    /// The rule-model format version (e.g. `"0.1"`). See [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// The catalog these rules apply to (`Catalog::catalog_id`).
    pub catalog_id: String,
    /// The access rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
}

/// A SQL command an access rule can target. `SELECT` reads are otherwise left
/// open within the tenant floor — gate them only with a [`Rule::RolePredicate`]
/// or a [`Rule::RoleCommands`] entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Command {
    All,
    Select,
    Insert,
    Update,
    Delete,
}

impl Command {
    /// The `FOR <command>` keyword.
    pub fn as_sql(self) -> &'static str {
        match self {
            Command::All => "ALL",
            Command::Select => "SELECT",
            Command::Insert => "INSERT",
            Command::Update => "UPDATE",
            Command::Delete => "DELETE",
        }
    }

    /// Whether a policy for this command has a `USING` clause (existing-row read
    /// — applies to SELECT / UPDATE / DELETE / ALL).
    pub fn has_using(self) -> bool {
        matches!(
            self,
            Command::All | Command::Select | Command::Update | Command::Delete
        )
    }

    /// Whether a policy for this command has a `WITH CHECK` clause (new/updated
    /// row — applies to INSERT / UPDATE / ALL).
    pub fn has_check(self) -> bool {
        matches!(self, Command::All | Command::Insert | Command::Update)
    }
}

/// One access rule. Each compiles to a single restrictive policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
// NOTE: no `deny_unknown_fields` — serde forbids it on an internally-tagged enum.
pub enum Rule {
    /// Row ownership: the `owner-field` (a uuid / reference column) must equal
    /// the caller's `app.user_id`, unless the caller's `app.role` is one of
    /// `exempt-roles`. The core "users see only their own rows" rule.
    RowOwnership {
        entity: EntityId,
        owner_field: FieldId,
        /// Roles that bypass the ownership check (e.g. `supervisor`, `admin`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exempt_roles: Vec<String>,
        /// Optional explicit policy name; otherwise one is derived.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Role command gates: which roles may run each (write) command. Reads stay
    /// open within the tenant floor unless a grant lists `SELECT`.
    RoleCommands {
        entity: EntityId,
        /// One grant per command; the listed roles may perform it.
        grants: Vec<CommandGrant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// A custom per-role predicate (escape hatch). For callers whose `app.role`
    /// is `role`, the row must satisfy `expression`; other roles are unaffected
    /// by this rule. The expression is emitted verbatim — the author owns its
    /// SQL correctness (it is validated non-empty only).
    RolePredicate {
        entity: EntityId,
        role: String,
        #[serde(default = "default_command")]
        command: Command,
        expression: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

fn default_command() -> Command {
    Command::All
}

/// The roles allowed to perform a command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CommandGrant {
    pub command: Command,
    pub roles: Vec<String>,
}

impl Rule {
    /// The entity this rule targets.
    pub fn entity(&self) -> &str {
        match self {
            Rule::RowOwnership { entity, .. }
            | Rule::RoleCommands { entity, .. }
            | Rule::RolePredicate { entity, .. } => entity,
        }
    }

    /// The explicit `name`, if the author supplied one.
    pub fn explicit_name(&self) -> Option<&str> {
        match self {
            Rule::RowOwnership { name, .. }
            | Rule::RoleCommands { name, .. }
            | Rule::RolePredicate { name, .. } => name.as_deref(),
        }
    }

    /// A short kind slug used in derived policy names.
    pub fn kind_slug(&self) -> &'static str {
        match self {
            Rule::RowOwnership { .. } => "owner",
            Rule::RoleCommands { .. } => "rolecmd",
            Rule::RolePredicate { .. } => "rolepred",
        }
    }
}

impl AccessPolicy {
    /// Parse from canonical JSON (import; also the jsonb stored per rule).
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serialize to canonical pretty JSON (export). Default-valued fields are
    /// omitted, so an exported policy re-imports to an identical value.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("AccessPolicy serializes")
    }
}
