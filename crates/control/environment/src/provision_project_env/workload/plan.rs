use super::{
    DateTime, PLATFORM_GROUP_ROLE, SecondsFormat, Utc, WORKLOAD_CREDENTIAL_TTL_DAYS,
    WorkloadActionRequest, WorkloadRoleFamily, WorkloadRoleScope, WorkloadRoleScopeKind,
    validate_project_env,
};

pub(in super::super) fn is_workload_generation_role(
    family: WorkloadRoleFamily,
    role: &str,
) -> bool {
    let prefix = format!("{}_", family.generation_prefix());
    let Some(scoped) = role.strip_prefix(&prefix) else {
        return false;
    };
    let Some((hash, generation)) = scoped.split_once('_') else {
        return false;
    };
    hash.len() == 40
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && matches!(generation, "a" | "b")
}

/// ONE lifecycle constructor, for any family (`wamn-0h0g.22.16`).
///
/// Replaces four copy-pasted constructors that differed only in the scope arm
/// they filled in. The scope GRAIN is the family's own declaration, so pairing a
/// family with the wrong grain is not something a caller can get wrong here.
///
/// The control tenant follows the same derivation: only a control-scoped family
/// records a login-to-tenant mapping row, because that row is the control
/// plane's and a project-environment credential never reaches the control
/// database.
pub(in super::super) fn workload_lifecycle<'a>(
    family: WorkloadRoleFamily,
    identity: WorkloadActionIdentity<'a>,
    env_key: &'a str,
    database: &'a str,
) -> WorkloadLifecycle<'a> {
    let WorkloadActionIdentity {
        org,
        project,
        environment,
    } = identity;
    let scope = match family.scope_kind() {
        // Tenant scope for the guest credential: the digest in the role name IS
        // the tenant key, so the login the mint issues and the key
        // `wamn_authority.tenant_key` computes are the same string
        // (`wamn-0h0g.22.6.4`).
        WorkloadRoleScopeKind::EnvKey => WorkloadRoleScope::EnvKey { env_key, database },
        WorkloadRoleScopeKind::ProjectEnvironment => WorkloadRoleScope::ProjectEnvironment {
            org,
            project,
            environment,
            database,
        },
        WorkloadRoleScopeKind::Control => WorkloadRoleScope::Control {
            org,
            project,
            environment,
            database,
        },
        WorkloadRoleScopeKind::Org => WorkloadRoleScope::Org { org, database },
    };
    WorkloadLifecycle {
        family,
        scope,
        control_env_key: (family.scope_kind() == WorkloadRoleScopeKind::Control).then_some(env_key),
    }
}

pub(super) fn workload_action_identity(
    args: &WorkloadActionRequest,
) -> anyhow::Result<WorkloadActionIdentity<'_>> {
    let org = args.org.as_str();
    let project = args.project.as_str();
    let environment = args.env.as_str();
    validate_project_env(org, project, environment)
        .map_err(|error| anyhow::anyhow!("project-env names: {error}"))?;
    Ok(WorkloadActionIdentity {
        org,
        project,
        environment,
    })
}

pub(super) fn workload_expires_at(now: DateTime<Utc>) -> String {
    (now + chrono::Duration::days(WORKLOAD_CREDENTIAL_TTL_DAYS))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the booleans mirror the `pg_roles` attribute columns and the exactness checks \
              beside them one for one; PostgreSQL reports each as its own boolean, and a \
              faithful copy of what the server says is what makes the drift check auditable"
)]
pub(super) struct WorkloadRoleState {
    pub(super) login: bool,
    pub(super) superuser: bool,
    pub(super) inherit: bool,
    pub(super) create_role: bool,
    pub(super) create_db: bool,
    pub(super) replication: bool,
    pub(super) bypass_rls: bool,
    pub(super) password_set: bool,
    pub(super) valid_until: Option<String>,
    pub(super) valid_until_finite: bool,
    pub(super) memberships: Vec<String>,
    pub(super) membership_options_exact: bool,
    pub(super) membership_options_migratable: bool,
    pub(super) member_roles: Vec<String>,
    pub(super) member_options_exact: bool,
    pub(super) generation_children_exact: bool,
    pub(super) connect_databases: Vec<String>,
    pub(super) sessions: i64,
    pub(super) owned_objects: i64,
}

#[derive(Debug, Clone, Copy)]
pub(in super::super) struct WorkloadLifecycle<'a> {
    pub(in super::super) family: WorkloadRoleFamily,
    pub(in super::super) scope: WorkloadRoleScope<'a>,
    pub(in super::super) control_env_key: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub(in super::super) struct WorkloadActionIdentity<'a> {
    pub(in super::super) org: &'a str,
    pub(in super::super) project: &'a str,
    pub(in super::super) environment: &'a str,
}

impl WorkloadRoleState {
    pub(super) fn is_active_for(&self, family: WorkloadRoleFamily, database: &str) -> bool {
        self.has_active_shape_for(database)
            && self.memberships == [family.acl_role()]
            && self.membership_options_exact
    }

    pub(super) fn is_migratable_active_for(
        &self,
        family: WorkloadRoleFamily,
        database: &str,
    ) -> bool {
        self.has_active_shape_for(database)
            && self.memberships == [family.acl_role()]
            && self.membership_options_migratable
    }

    pub(super) fn has_active_shape_for(&self, database: &str) -> bool {
        self.has_active_attributes() && self.connect_databases == [database]
    }

    pub(super) fn lost_database_access(&self, family: WorkloadRoleFamily) -> bool {
        self.has_active_attributes()
            && self.memberships == [family.acl_role()]
            && self.membership_options_exact
            && self.connect_databases.is_empty()
    }

    fn has_active_attributes(&self) -> bool {
        self.login
            && self.restrictive_attributes()
            && self.inherit
            && self.password_set
            && self.valid_until_finite
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.owned_objects == 0
    }

    pub(super) fn is_inactive(&self) -> bool {
        self.has_inactive_shape() && self.memberships.is_empty() && self.membership_options_exact
    }

    pub(super) fn is_migratable_inactive(&self) -> bool {
        self.has_inactive_shape()
            && self.memberships.is_empty()
            && self.membership_options_migratable
    }

    pub(super) fn has_inactive_shape(&self) -> bool {
        !self.login
            && self.restrictive_attributes()
            && self.inherit
            && self.valid_until.as_deref() == Some("1970-01-01T00:00:00Z")
            && self.valid_until_finite
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.connect_databases.is_empty()
            && self.sessions == 0
            && self.owned_objects == 0
    }

    pub(super) fn is_acl_role(&self, family: WorkloadRoleFamily) -> bool {
        self.has_acl_role_shape(family)
            && self.member_options_exact
            && self.generation_children_exact
    }

    /// THE ONE PARENT EDGE A STABLE ACL ROLE MAY CARRY (`wamn-0h0g.22.17`).
    ///
    /// A platform-grain family's ACL role is a member of
    /// [`PLATFORM_GROUP_ROLE`], and it has to be: the tenant floor is narrowed
    /// `TO wamn_app`, PostgreSQL default-denies when no policy matches the
    /// connected role, and the permissive arm names the group. Nothing else may
    /// appear here — an extra parent is authority this provisioner did not
    /// confer.
    pub(super) fn expected_acl_parents(family: WorkloadRoleFamily) -> &'static [&'static str] {
        if family.is_platform_grain() {
            &[PLATFORM_GROUP_ROLE]
        } else {
            &[]
        }
    }

    pub(super) fn has_acl_role_shape(&self, family: WorkloadRoleFamily) -> bool {
        !self.login
            && self.restrictive_attributes()
            && !self.inherit
            && !self.password_set
            && self.valid_until.is_none()
            && !self.valid_until_finite
            && self.memberships == Self::expected_acl_parents(family)
            && self.membership_options_exact
            && self
                .member_roles
                .iter()
                .all(|role| is_workload_generation_role(family, role))
            && self.connect_databases.is_empty()
            && self.sessions == 0
            && self.owned_objects == 0
    }

    pub(super) fn restrictive_attributes(&self) -> bool {
        !self.superuser
            && !self.create_role
            && !self.create_db
            && !self.replication
            && !self.bypass_rls
    }
}
