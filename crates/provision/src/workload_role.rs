//! Closed provisioning vocabulary for identity-bound workload logins.

use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_identity_protocol::{AuthorityClass, app_identity_hash};

use crate::name::GUEST_SECRET_PREFIX;

/// Stable NOLOGIN role used by service-reader logins.
pub const SERVICE_READER_ROLE: &str = "wamn_service_reader";
/// Stable NOLOGIN role used by run-retention logins.
pub const RETENTION_ROLE: &str = "wamn_run_retention";
/// Stable NOLOGIN role used by executor-platform logins.
pub use wamn_identity_protocol::authority_class::EXECUTOR_PLATFORM_ROLE;
/// Compact prefix that leaves room for the full identity digest.
const EXECUTOR_PLATFORM_LOGIN_PREFIX: &str = "wamn_exec_platform";
/// Stable NOLOGIN role used by callable-HTTP admission logins.
pub use wamn_identity_protocol::authority_class::HTTP_ADMITTER_ROLE;
/// Stable NOLOGIN role used by control-identity reader logins.
pub const IDENTITY_READER_ROLE: &str = "wamn_identity_reader";
/// Host-only reader of connection generations and credentials.
pub const CONNECTION_READER_ROLE: &str = "wamn_conn_reader";
/// Stable NOLOGIN role for the identity service's environment role reads.
pub const SESSION_ROLE_READER_ROLE: &str = "wamn_session_role_reader";
/// Keep the 40-hex identity digest within PostgreSQL's 63 bytes.
const SESSION_ROLE_READER_LOGIN_PREFIX: &str = "wamn_session_roles";
/// Stable NOLOGIN role used by administration logins. Its 19 bytes
/// form a 60-byte login, so the role name is its own prefix.
pub use wamn_identity_protocol::authority_class::ADMINISTRATION_ROLE;
/// Stable NOLOGIN role used by record history retention logins.
pub use wamn_record_history::AUDIT_RETENTION_ROLE;
/// Stable NOLOGIN role used by control logins, the credential of an org's
/// control host (docs/architecture/execution.md#host-run-routes).
pub const CONTROL_ROLE: &str = "wamn_control";

/// The shared NOLOGIN group role every non-guest project-base-grants arm targets
/// (`wamn-0h0g.22.17`).
///
/// ONE role, not one per family, and no `BYPASSRLS` anywhere. The project base grants
/// are the GUEST base grants: narrowing it `TO wamn_app` does not EXEMPT a platform
/// principal, because PostgreSQL DEFAULT-DENIES when RLS is enabled and no
/// policy matches the connected role. Each governed relation therefore carries
/// exactly one permissive arm `TO wamn_platform`, and the family's table grants
/// stay the thing that limits what it can reach.
pub const PLATFORM_GROUP_ROLE: &str = "wamn_platform";

pub(crate) const IDENTITY_HASH_HEX_LEN: usize = 40;

/// Exhaustive provisioning families. Adding authority requires a code change.
///
/// `wamn-0h0g.13.59` froze this vocabulary at six families.
/// `wamn-0h0g.13.61` deliberately expands that frozen set once, from six to
/// seven, by admitting the project-environment-identity-bound management-admitter
/// family.
/// `wamn-0fqa` is the third deliberate expansion, from seven to ten, admitting
/// the three role families `wamn-0h0g.22.14`'s ruled AuthorityClass mapping
/// names and this vocabulary did not yet carry: executor-platform,
/// callable-HTTP admitter and event-materializer. The fourth ruled row,
/// guest-sql, already mapped to [`WorkloadRoleFamily::App`]. Only the families
/// land here; keying credential selection by authority class is
/// `wamn-0h0g.22.8`.
/// `wamn-ctc8.15.2` adds a dedicated project-environment session role reader;
/// it carries no runtime authority class and no operation-permission read.
/// `wamn-emtx.13` adds the project-identity-bound audit retention family, which removes
/// expired record history entries.
/// `wamn-0h0g.10.15` removes the effect-writer family, which had no runtime
/// consumer.
/// `wamn-a40n.2` adds the project-environment administration family, the
/// credential of the host-run administration routes, and the org-identity-bound
/// control family, the credential of an org's control host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleFamily {
    ServiceReader,
    App,
    Query,
    Migration,
    Retention,
    ExecutorPlatform,
    HttpAdmitter,
    IdentityReader,
    SessionRoleReader,
    AuditRetention,
    Administration,
    Control,
    ConnectionReader,
}

impl WorkloadRoleFamily {
    /// Every family, in declaration order.
    ///
    /// `wamn-0h0g.22.16` makes this THE DRIVER rather than a convenience:
    /// provisioning's flag set, action dispatch and Secret naming are all
    /// derived by walking it, so an admitted family reaches every one of them
    /// without a list anywhere being appended to by hand.
    pub const ALL: [Self; 13] = [
        Self::ServiceReader,
        Self::App,
        Self::Query,
        Self::Migration,
        Self::Retention,
        Self::ExecutorPlatform,
        Self::HttpAdmitter,
        Self::IdentityReader,
        Self::SessionRoleReader,
        Self::AuditRetention,
        Self::Administration,
        Self::Control,
        Self::ConnectionReader,
    ];

    /// Stable NOLOGIN ACL role inherited by this family's logins.
    pub const fn acl_role(self) -> &'static str {
        match self {
            Self::ServiceReader => SERVICE_READER_ROLE,
            Self::App => AuthorityClass::GuestSql.acl_role(),
            Self::Query => AuthorityClass::DynamicSql.acl_role(),
            Self::Migration => crate::migration::OWNER_ROLE,
            Self::Retention => RETENTION_ROLE,
            Self::ExecutorPlatform => AuthorityClass::ExecutorPlatform.acl_role(),
            Self::HttpAdmitter => AuthorityClass::CallableHttp.acl_role(),
            Self::IdentityReader => IDENTITY_READER_ROLE,
            Self::ConnectionReader => CONNECTION_READER_ROLE,
            Self::SessionRoleReader => SESSION_ROLE_READER_ROLE,
            Self::AuditRetention => AUDIT_RETENTION_ROLE,
            Self::Administration => AuthorityClass::Administration.acl_role(),
            Self::Control => CONTROL_ROLE,
        }
    }

    /// Prefix of the role identity digest, within the PostgreSQL name limit.
    pub const fn login_prefix(self) -> &'static str {
        match self {
            Self::Migration => crate::migration::LOGIN_PREFIX,
            Self::ExecutorPlatform => EXECUTOR_PLATFORM_LOGIN_PREFIX,
            Self::SessionRoleReader => SESSION_ROLE_READER_LOGIN_PREFIX,
            _ => self.acl_role(),
        }
    }

    /// Exact identity class used to derive login identities.
    pub const fn identity_type(self) -> WorkloadRoleIdentityType {
        match self {
            Self::App | Self::Query | Self::Migration | Self::Retention | Self::AuditRetention => {
                WorkloadRoleIdentityType::ProjectEnvKey
            }
            Self::ServiceReader
            | Self::ExecutorPlatform
            | Self::HttpAdmitter
            | Self::SessionRoleReader
            | Self::Administration => WorkloadRoleIdentityType::ProjectEnvironment,
            // Identity follows the RESOURCE PLANE, not the consumer's home: these
            // credentials reach the CONTROL database (`wamn-0h0g.13.63`).
            Self::IdentityReader | Self::ConnectionReader => WorkloadRoleIdentityType::Control,
            // One credential per org on the control database; `provision-org`
            // mints it (owner ruling of 2026-09-30 on `wamn-a40n.2`).
            Self::Control => WorkloadRoleIdentityType::Org,
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

    /// Whether this family's logins are membered into
    /// [`PLATFORM_GROUP_ROLE`] and so reach the permissive project-base-grants arm.
    ///
    /// DERIVED from the two facts that already decide it, never tabulated:
    ///
    /// * [`Self::App`] is the GUEST. The minimum exists to admit it, and a
    ///   platform arm would hand it every project's rows — the exact
    ///   cross-project read `wamn-0h0g.22.6` closed.
    /// * A [`WorkloadRoleIdentityType::Control`] or [`WorkloadRoleIdentityType::Org`]
    ///   family's credentials reach the CONTROL database, whose store carries its own restrictive
    ///   (`deploy/sql/control-portable-store.sql`). None of them holds a grant
    ///   on any relation carrying the project-plane minimum, so membership would
    ///   be authority without a reader.
    /// * [`Self::AuditRetention`] is not a member either (`wamn-emtx` ruling
    ///   61), and for the same reason: no reader needs the edge. Its grants
    ///   reach only package history tables, which carry no row security.
    ///
    /// Everything else — the eight families whose credentials reach a
    /// project-environment or project database and are not the guest — is a
    /// through its two catalog reads. `ServiceReader` remains the empty-surface
    /// case: an RLS arm with no table grant behind it admits nothing, while the
    /// edge prevents a future first grant from silently reading zero rows.
    ///
    /// `Retention` is `ProjectEnvKey`-identity-bound and is a member anyway. Its login names
    /// carry a project DIGEST, but `current_project_key` recovers a key only from
    /// the guest login pattern, and widening that regex is refused
    /// (`wamn-0h0g.22.17` owner ruling). The shared arm therefore makes it
    /// cross-project on the relations it holds grants on; the project predicate it
    /// keeps in its own statements is what re-narrows it.
    pub const fn is_platform_grain(self) -> bool {
        !matches!(
            self,
            Self::App | Self::Query | Self::Migration | Self::AuditRetention
        ) && !matches!(
            self.identity_type(),
            WorkloadRoleIdentityType::Control | WorkloadRoleIdentityType::Org
        )
    }

    /// The family's operator-facing name, in the hyphenated convention.
    ///
    /// DERIVED from the stable ACL role, not tabulated beside it: the two are
    /// one vocabulary, and a table would be a second place to append a family
    /// to. The wildcard arm is what makes an admitted family reach every
    /// derived flag, Secret prefix and message without an edit here.
    pub fn label(self) -> String {
        match self {
            // `wamn-0h0g.13.59` froze this label before the role name carried
            // its `run_` qualifier; deriving it would rename the family in
            // operator-facing text for no gain.
            Self::Retention => "retention".to_string(),
            _ => self
                .acl_role()
                .trim_start_matches("wamn_")
                .replace('_', "-"),
        }
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

    /// Which body this family's credential Secret carries.
    ///
    /// A closed vocabulary of SHAPES, not a per-family list: an
    /// admitted family lands on the plain single-`url` Secret every consumer
    /// already mounts through `secretKeyRef … key: url`, with no edit here.
    /// Prefix of this family's credential Secret name.
    ///
    /// `wamn-<cli-stem>-` for every family whose Secret name was not frozen
    /// first. The three overrides are the frozen ones and carry their reasons
    /// on the constants themselves.
    pub fn secret_prefix(self) -> String {
        match self {
            Self::App => GUEST_SECRET_PREFIX.to_string(),
            _ => wamn_registry::name::workload_secret_prefix(&self.cli_stem()),
        }
    }

    pub(crate) const fn identity_domain(self) -> &'static [u8] {
        match self {
            Self::ServiceReader => b"wamn.service-reader.identity.v0.1",
            Self::App | Self::Query => b"wamn.app.identity.v0.1",
            Self::Migration => b"wamn.migration.identity.v0.1",
            Self::Retention => b"wamn.run-retention.identity.v0.1",
            Self::ExecutorPlatform => b"wamn.executor-platform.identity.v0.1",
            Self::HttpAdmitter => b"wamn.http-admitter.identity.v0.1",
            Self::IdentityReader => b"wamn.identity-reader.identity.v0.1",
            Self::ConnectionReader => b"wamn.connection-reader.identity.v0.1",
            Self::SessionRoleReader => b"wamn.session-role-reader.identity.v0.1",
            Self::AuditRetention => b"wamn.audit-retention.identity.v0.1",
            Self::Administration => b"wamn.administration.identity.v0.1",
            Self::Control => b"wamn.control.identity.v0.1",
        }
    }
}

/// The ruled projection from authority class onto provisioning family.
///
/// `wamn-0h0g.22.14` fixed these four rows and the shape they must keep: ONE
/// family per class, closed and total over the exact enum, every variant
/// matched explicitly, and NO wildcard or default arm — so an added or unmapped
/// class is a compile error here rather than a runtime fallback. If any row
/// ever needs two families, that row returns as its own owner question rather
/// than growing an arm.
///
/// The direction is deliberate and one-way. Several families (`ServiceReader` and `Retention`) carry no authority class at all, so the inverse is not
/// a function and is not offered.
impl From<AuthorityClass> for WorkloadRoleFamily {
    fn from(class: AuthorityClass) -> Self {
        match class {
            AuthorityClass::GuestSql => Self::App,
            AuthorityClass::DynamicSql => Self::Query,
            AuthorityClass::ExecutorPlatform => Self::ExecutorPlatform,
            AuthorityClass::CallableHttp => Self::HttpAdmitter,
            AuthorityClass::Administration => Self::Administration,
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
        project_env_key: &'a str,
        database: &'a str,
    },
    ProjectEnvironment {
        org: &'a str,
        project: &'a str,
        environment: &'a str,
        database: &'a str,
    },
    Control {
        org: &'a str,
        project: &'a str,
        environment: &'a str,
        database: &'a str,
    },
    Org {
        org: &'a str,
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

/// Derive the deterministic 160-bit identity suffix for one family.
pub fn workload_role_identity_hash(
    family: WorkloadRoleFamily,
    identity: WorkloadRoleIdentity<'_>,
) -> Result<String, WorkloadRoleIdentityError> {
    let actual = identity.identity_type();
    let expected = family.identity_type();
    if actual != expected {
        return Err(WorkloadRoleIdentityError {
            family,
            expected,
            actual,
        });
    }
    // The App family delegates to `wamn-identity-protocol`, which the
    // RUNTIME can reach, because `wamn-0h0g.22.6.7` makes the runtime make sure that
    // a resolved guest credential belongs to the project it is about to serve.
    // Delegation, not duplication — there is still ONE definition.
    if matches!(family, WorkloadRoleFamily::App | WorkloadRoleFamily::Query) {
        let WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database,
        } = identity
        else {
            unreachable!("identity type was validated above")
        };
        return Ok(app_identity_hash(project_env_key, database));
    }

    let mut preimage = Vec::new();
    frame(&mut preimage, family.identity_domain());
    match identity {
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database,
        } => {
            push_field(&mut preimage, "project_env_key", project_env_key);
            push_field(&mut preimage, "database", database);
        }
        WorkloadRoleIdentity::ProjectEnvironment {
            org,
            project,
            environment,
            database,
        }
        | WorkloadRoleIdentity::Control {
            org,
            project,
            environment,
            database,
        } => {
            push_field(&mut preimage, "org", org);
            push_field(&mut preimage, "project", project);
            push_field(&mut preimage, "environment", environment);
            push_field(&mut preimage, "database", database);
        }
        WorkloadRoleIdentity::Org { org, database } => {
            push_field(&mut preimage, "org", org);
            push_field(&mut preimage, "database", database);
        }
    }
    let digest = hex::encode(Sha256::digest(preimage));
    Ok(digest[..IDENTITY_HASH_HEX_LEN].to_string())
}

/// Derive one bounded LOGIN role for a closed workload family.
pub fn workload_login(
    family: WorkloadRoleFamily,
    identity: WorkloadRoleIdentity<'_>,
) -> Result<String, WorkloadRoleIdentityError> {
    Ok(wamn_registry::name::credential_login(
        family.login_prefix(),
        &workload_role_identity_hash(family, identity)?,
    ))
}

fn push_field(preimage: &mut Vec<u8>, tag: &str, value: &str) {
    frame(preimage, tag.as_bytes());
    frame(preimage, value.as_bytes());
}

fn frame(preimage: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("a framed identity field fits u64");
    preimage.extend_from_slice(&length.to_be_bytes());
    preimage.extend_from_slice(value);
}
