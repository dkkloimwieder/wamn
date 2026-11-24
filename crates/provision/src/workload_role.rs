//! Closed provisioning vocabulary for identity-bound workload logins.

use std::fmt;

use wamn_identity_protocol::AuthorityClass;

/// Project database login families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleFamily {
    App,
    Query,
    Migration,
    Platform,
}

impl WorkloadRoleFamily {
    /// Every family, in declaration order.
    ///
    /// `wamn-0h0g.22.16` makes this THE DRIVER rather than a convenience:
    /// provisioning's flag set, action dispatch and Secret naming are all
    /// derived by walking it, so an admitted family reaches every one of them
    /// without a list anywhere being appended to by hand.
    pub const ALL: [Self; 4] = [Self::App, Self::Query, Self::Migration, Self::Platform];

    /// Stable NOLOGIN group role inherited by this family.
    pub const fn acl_role(self) -> &'static str {
        match self {
            Self::App => AuthorityClass::GuestSql.acl_role(),
            Self::Query => AuthorityClass::DynamicSql.acl_role(),
            Self::Migration | Self::Platform => wamn_role::PLATFORM_ROLE,
        }
    }

    /// Fixed login in each project's Cluster.
    pub const fn login_name(self) -> &'static str {
        match self {
            Self::App => wamn_role::APP_LOGIN,
            Self::Query => wamn_role::QUERY_LOGIN,
            Self::Migration => wamn_role::MIGRATION_ROLE,
            Self::Platform => wamn_role::PLATFORM_LOGIN,
        }
    }

    /// Stable role name used in labels.
    pub const fn login_prefix(self) -> &'static str {
        match self {
            Self::Migration => wamn_role::MIGRATION_ROLE,
            _ => self.acl_role(),
        }
    }

    /// Exact identity class used to derive login identities.
    pub const fn identity_type(self) -> WorkloadRoleIdentityType {
        match self {
            Self::App | Self::Query | Self::Migration | Self::Platform => {
                WorkloadRoleIdentityType::ProjectEnvKey
            } // Identity follows the RESOURCE PLANE, not the consumer's home: these
              // credentials reach the CONTROL database (`wamn-0h0g.13.63`).
              // One credential per org on the control database; `provision-org`
              // mints it (owner ruling of 2026-09-30 on `wamn-a40n.2`).
        }
    }

    /// Whether `provision-project-env` mints this family. An org-identity-bound
    /// family belongs to `provision-org` instead.
    pub const fn is_project_env_provisioned(self) -> bool {
        !matches!(self.identity_type(), WorkloadRoleIdentityType::Org)
    }

    /// The families `provision-project-env` mints, in declaration order.
    pub fn project_env_families() -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(|family| family.is_project_env_provisioned())
    }

    /// Whether this family inherits the platform group.
    pub const fn is_platform_grain(self) -> bool {
        matches!(self, Self::Platform | Self::Migration)
    }

    /// The family's operator-facing name, in the hyphenated convention.
    ///
    /// DERIVED from the stable ACL role, not tabulated beside it: the two are
    /// one vocabulary, and a table would be a second place to append a family
    /// to. The wildcard arm is what makes an admitted family reach every
    /// derived flag, Secret prefix and message without an edit here.
    pub fn label(self) -> String {
        self.login_prefix()
            .trim_start_matches("wamn_")
            .replace('_', "-")
    }

    /// The stem this family's provisioning flags and Secret name are built from.
    ///
    /// Equal to [`Self::label`] except where an operator-facing name was frozen
    /// before the role vocabulary settled. `App` is the guest-SQL family, and
    /// its flags and Secret have always said `guest`.
    pub fn cli_stem(self) -> String {
        match self {
            Self::App => "guest".to_string(),
            _ => self.label(),
        }
    }

    /// This family's `app.kubernetes.io/component` label value, less the
    /// `-credentials` suffix every workload Secret shares.
    pub fn component_stem(self) -> String {
        match self {
            // Frozen before the family vocabulary settled on `app`.
            Self::App => "guest-sql".to_string(),
            _ => self.cli_stem(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleIdentityType {
    ProjectEnvKey,
    ProjectEnvironment,
    Control,
    /// One org on the control database.
    Org,
}

impl WorkloadRoleIdentityType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectEnvKey => "project_env_key",
            Self::ProjectEnvironment => "project-environment",
            Self::Control => "control",
            Self::Org => "org",
        }
    }
}

/// Exact identity inputs for one workload login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleIdentity<'a> {
    ProjectEnvKey {
        project_env_key: &'a wamn_registry::ProjectEnvKey,
        database: &'a str,
    },
    ProjectEnvironment {
        org: &'a wamn_registry::OrgId,
        project: &'a wamn_registry::ProjectId,
        environment: &'a wamn_registry::EnvName,
        database: &'a str,
    },
    Control {
        org: &'a wamn_registry::OrgId,
        project: &'a wamn_registry::ProjectId,
        environment: &'a wamn_registry::EnvName,
        database: &'a str,
    },
    Org {
        org: &'a wamn_registry::OrgId,
        database: &'a str,
    },
}

impl<'a> WorkloadRoleIdentity<'a> {
    /// Database receiving this login's sole direct CONNECT grant.
    pub const fn database(self) -> &'a str {
        match self {
            Self::ProjectEnvKey { database, .. }
            | Self::ProjectEnvironment { database, .. }
            | Self::Control { database, .. }
            | Self::Org { database, .. } => database,
        }
    }

    const fn identity_type(self) -> WorkloadRoleIdentityType {
        match self {
            Self::ProjectEnvKey { .. } => WorkloadRoleIdentityType::ProjectEnvKey,
            Self::ProjectEnvironment { .. } => WorkloadRoleIdentityType::ProjectEnvironment,
            Self::Control { .. } => WorkloadRoleIdentityType::Control,
            Self::Org { .. } => WorkloadRoleIdentityType::Org,
        }
    }
}

/// A family was paired with the wrong identity grain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadRoleIdentityError {
    family: WorkloadRoleFamily,
    expected: WorkloadRoleIdentityType,
    actual: WorkloadRoleIdentityType,
}

impl fmt::Display for WorkloadRoleIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "workload role family {:?} requires {} identity, not {} identity",
            self.family,
            self.expected.as_str(),
            self.actual.as_str(),
        )
    }
}

impl std::error::Error for WorkloadRoleIdentityError {}

/// Select the fixed login for a project fixture.
pub fn workload_login(
    family: WorkloadRoleFamily,
    identity: WorkloadRoleIdentity<'_>,
) -> Result<wamn_role::LoginName, WorkloadRoleIdentityError> {
    let actual = identity.identity_type();
    let expected = family.identity_type();
    if actual != expected {
        return Err(WorkloadRoleIdentityError {
            family,
            expected,
            actual,
        });
    }
    Ok(family.login_name().parse().expect("fixed project login"))
}

/// Select the platform credential from public project connection facts.
pub fn platform_configuration(input: &str) -> anyhow::Result<String> {
    let input = wamn_postgres::configuration(input)?;
    if !input.trim_start().starts_with('{') {
        return Ok(input.into_owned());
    }
    platform_connection(serde_json::from_str(&input)?)?.to_json()
}

/// Select the platform login on the same project endpoint.
pub fn platform_connection(
    mut config: wamn_postgres::ConnectionConfig,
) -> anyhow::Result<wamn_postgres::ConnectionConfig> {
    config.username = wamn_role::PLATFORM_LOGIN.parse()?;
    config.credentials.name =
        wamn_registry::name::project_login_certificate(&config.database, config.username.as_str());
    Ok(config)
}
