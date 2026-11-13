//! Closed provisioning vocabulary for identity-bound workload login generations.

use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_run_state::{AuthorityClass, CredentialGeneration, app_identity_hash};

use crate::APP_ROLE;
use crate::name::GUEST_SECRET_PREFIX;

/// Stable NOLOGIN role used by service-reader generations.
pub const SERVICE_READER_ROLE: &str = "wamn_service_reader";
/// Stable NOLOGIN role used by run-retention generations.
pub const RETENTION_ROLE: &str = "wamn_run_retention";
/// Stable NOLOGIN role used by executor-platform generations.
pub const EXECUTOR_PLATFORM_ROLE: &str = "wamn_executor_platform";
/// Frozen generation prefix for the executor-platform family (`wamn-0fqa`).
///
/// The stable ACL role name is 22 bytes, so reusing it as the generation prefix
/// would mint a 65-byte identifier and PostgreSQL caps identifiers at 63. Worse
/// than long: PostgreSQL truncates with a NOTICE instead of refusing, and the
/// 63-byte prefix of the `_a` and `_b` names is identical, so both generations
/// would collide on one role. This shorter frozen prefix keeps the derived login
/// at 61 bytes with the 160-bit identity digest and `_a`/`_b` suffix intact.
const EXECUTOR_PLATFORM_GENERATION_PREFIX: &str = "wamn_exec_platform";
/// Stable NOLOGIN role used by callable-HTTP admission generations.
pub const HTTP_ADMITTER_ROLE: &str = "wamn_http_admitter";
/// Stable NOLOGIN role used by event-materializer generations.
pub const EVENT_MATERIALIZER_ROLE: &str = "wamn_event_materializer";
/// Frozen generation prefix for the event-materializer family (`wamn-0fqa`).
///
/// The stable ACL role name is 23 bytes, so reusing it as the generation prefix
/// would mint a 66-byte identifier that PostgreSQL truncates into the identity
/// digest itself, collapsing both generations and three digest characters. This
/// shorter frozen prefix keeps the derived login at 60 bytes.
const EVENT_MATERIALIZER_GENERATION_PREFIX: &str = "wamn_materializer";
/// Stable NOLOGIN role used by control-registry reader generations.
pub const REGISTRY_READER_ROLE: &str = "wamn_registry_reader";
/// Stable NOLOGIN role used by control-identity reader generations.
pub const IDENTITY_READER_ROLE: &str = "wamn_identity_reader";
/// Host-only reader of connection generations and credentials.
pub const CONNECTION_READER_ROLE: &str = "wamn_conn_reader";
/// Stable NOLOGIN role for the identity service's environment role reads.
pub const SESSION_ROLE_READER_ROLE: &str = "wamn_session_role_reader";
/// Keep the 40-hex identity digest and A/B suffix within PostgreSQL's 63 bytes.
const SESSION_ROLE_READER_GENERATION_PREFIX: &str = "wamn_session_roles";
/// Stable NOLOGIN role used by record history retention generations.
pub use wamn_record_history::AUDIT_RETENTION_ROLE;
/// Stable NOLOGIN role used by administration generations. Its 19 bytes
/// mint a 62-byte login, so the role name is its own generation prefix.
pub const ADMINISTRATION_ROLE: &str = "wamn_administration";
/// Stable NOLOGIN role used by control generations, the credential of an org's
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
/// `wamn-0h0g.13.63` is the fourth deliberate expansion, from ten to twelve,
/// admitting the two CONTROL-identity-bound reader families the T1 system database's
/// two purported read-only consumers need. Identity follows the RESOURCE PLANE,
/// not the consumer's home, so both are [`WorkloadRoleIdentityType::Control`] and
/// neither may reuse the project-environment-identity-bound
/// [`WorkloadRoleFamily::ServiceReader`]. TWO families, not one, because the
/// two grant sets are disjoint and must stay so: `wamn-0h0g.12.116`'s consumer
/// reads `registry.event_readers` and `wamn-0h0g.12.67`'s reads
/// `identity.pats`, `identity.principals`, `identity.project_env_memberships`
/// and `identity.project_roles` — one
/// family for both is how one role gets widened to the union. Only the
/// families land here; the grants, Secrets and consumers are those two beads.
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
    Retention,
    ExecutorPlatform,
    HttpAdmitter,
    EventMaterializer,
    RegistryReader,
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
        Self::Retention,
        Self::ExecutorPlatform,
        Self::HttpAdmitter,
        Self::EventMaterializer,
        Self::RegistryReader,
        Self::IdentityReader,
        Self::SessionRoleReader,
        Self::AuditRetention,
        Self::Administration,
        Self::Control,
        Self::ConnectionReader,
    ];

    /// Stable NOLOGIN ACL role inherited by this family's generations.
    pub const fn acl_role(self) -> &'static str {
        match self {
            Self::ServiceReader => SERVICE_READER_ROLE,
            Self::App => APP_ROLE,
            Self::Retention => RETENTION_ROLE,
            Self::ExecutorPlatform => EXECUTOR_PLATFORM_ROLE,
            Self::HttpAdmitter => HTTP_ADMITTER_ROLE,
            Self::EventMaterializer => EVENT_MATERIALIZER_ROLE,
            Self::RegistryReader => REGISTRY_READER_ROLE,
            Self::IdentityReader => IDENTITY_READER_ROLE,
            Self::ConnectionReader => CONNECTION_READER_ROLE,
            Self::SessionRoleReader => SESSION_ROLE_READER_ROLE,
            Self::AuditRetention => AUDIT_RETENTION_ROLE,
            Self::Administration => ADMINISTRATION_ROLE,
            Self::Control => CONTROL_ROLE,
        }
    }

    /// Frozen prefix of this family's derived A/B generation identities.
    ///
    /// Equal to [`Self::acl_role`] for every family whose stable role name still
    /// fits the PostgreSQL identifier cap once the identity digest and generation
    /// suffix are appended. `ExecutorPlatform` and `EventMaterializer` (`wamn-0fqa`) do not, so each
    /// carries its own shorter frozen prefix. `SessionRoleReader` also needs
    /// a shorter prefix. `RegistryReader` and
    /// `IdentityReader` (`wamn-0h0g.13.63`) are the first families to fit
    /// EXACTLY: their 20-byte role names mint 63-byte logins, the largest
    /// PostgreSQL stores untruncated, so they take the wildcard arm and the
    /// exactness is pinned by test rather than left to be rediscovered.
    pub const fn generation_prefix(self) -> &'static str {
        match self {
            Self::ExecutorPlatform => EXECUTOR_PLATFORM_GENERATION_PREFIX,
            Self::EventMaterializer => EVENT_MATERIALIZER_GENERATION_PREFIX,
            Self::SessionRoleReader => SESSION_ROLE_READER_GENERATION_PREFIX,
            _ => self.acl_role(),
        }
    }

    /// Exact identity class used to derive generation identities.
    pub const fn identity_type(self) -> WorkloadRoleIdentityType {
        match self {
            Self::App | Self::Retention | Self::AuditRetention => {
                WorkloadRoleIdentityType::ProjectEnvKey
            }
            Self::ServiceReader
            | Self::ExecutorPlatform
            | Self::HttpAdmitter
            | Self::SessionRoleReader
            | Self::EventMaterializer
            | Self::Administration => WorkloadRoleIdentityType::ProjectEnvironment,
            // Identity follows the RESOURCE PLANE, not the consumer's home: these
            // credentials reach the CONTROL database (`wamn-0h0g.13.63`).
            Self::RegistryReader | Self::IdentityReader | Self::ConnectionReader => {
                WorkloadRoleIdentityType::Control
            }
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

    /// Whether this family's generations are membered into
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
    /// member. `EventMaterializer` exercises that edge
    /// through its two catalog reads. `ServiceReader` remains the empty-surface
    /// case: an RLS arm with no table grant behind it admits nothing, while the
    /// edge prevents a future first grant from silently reading zero rows.
    ///
    /// `Retention` is `ProjectEnvKey`-identity-bound and is a member anyway. Its login names
    /// carry a project DIGEST, but `current_project_key` recovers a key only from
    /// the guest generation pattern, and widening that regex is refused
    /// (`wamn-0h0g.22.17` owner ruling). The shared arm therefore makes it
    /// cross-project on the relations it holds grants on; the project predicate it
    /// keeps in its own statements is what re-narrows it.
    pub const fn is_platform_grain(self) -> bool {
        !matches!(self, Self::App | Self::AuditRetention)
            && !matches!(
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
    pub fn secret_body_type(self) -> WorkloadSecretBodyType {
        match self {
            // The guest credential IS the project authority, so its Secret
            // carries the project key that keys every governed predicate.
            Self::App => WorkloadSecretBodyType::ProjectEnvKeyUrl,
            // The credential and trusted target coordinates publish atomically.
            Self::SessionRoleReader => WorkloadSecretBodyType::SessionTarget,
            _ => WorkloadSecretBodyType::Url,
        }
    }

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
            Self::App => b"wamn.app.identity.v0.1",
            Self::Retention => b"wamn.run-retention.identity.v0.1",
            Self::ExecutorPlatform => b"wamn.executor-platform.identity.v0.1",
            Self::HttpAdmitter => b"wamn.http-admitter.identity.v0.1",
            Self::EventMaterializer => b"wamn.event-materializer.identity.v0.1",
            Self::RegistryReader => b"wamn.registry-reader.identity.v0.1",
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
            AuthorityClass::ExecutorPlatform => Self::ExecutorPlatform,
            AuthorityClass::CallableHttp => Self::HttpAdmitter,
            AuthorityClass::EventMaterializer => Self::EventMaterializer,
            AuthorityClass::Administration => Self::Administration,
        }
    }
}

/// The closed set of credential-Secret body shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadSecretBodyType {
    /// One `url` key.
    Url,
    /// One `url` key, plus the project key label and project_env_key annotation.
    ProjectEnvKeyUrl,
    /// One validated environment target and credential in `target.json`.
    SessionTarget,
}

/// The four admitted provisioning identity shapes.
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

/// Exact identity inputs for one workload generation pair.
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
    /// Database receiving this generation's sole direct CONNECT grant.
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
    // The App family delegates to `wamn-run-state`, which is the leaf the
    // RUNTIME can reach, because `wamn-0h0g.22.6.7` makes the runtime make sure that
    // a resolved guest credential belongs to the project it is about to serve.
    // Delegation, not duplication — there is still ONE definition.
    if family == WorkloadRoleFamily::App {
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

/// Derive one bounded A/B LOGIN role for a closed workload family.
pub fn workload_generation_role(
    family: WorkloadRoleFamily,
    identity: WorkloadRoleIdentity<'_>,
    generation: CredentialGeneration,
) -> Result<String, WorkloadRoleIdentityError> {
    Ok(wamn_registry::name::credential_login(
        family.generation_prefix(),
        &workload_role_identity_hash(family, identity)?,
        generation.as_str(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// THE DRIFT GUARD for a deliberate duplication (`wamn-0h0g.22.8.4`).
    ///
    /// `AuthorityClass::acl_role` repeats these role names inside
    /// `wamn-run-state`, because the shipped runtime links no provisioner and
    /// still has to name the role it expects a connection to hold. Two copies
    /// of a security-relevant string is a drift risk, so this is the assertion
    /// that they are one vocabulary. If it ever fails, the runtime is probing
    /// for a role the provisioner no longer grants.
    #[test]
    fn the_runtime_role_vocabulary_matches_the_provisioning_families() {
        for class in AuthorityClass::ALL {
            assert_eq!(
                class.acl_role(),
                WorkloadRoleFamily::from(class).acl_role(),
                "{class}: the runtime expects a different role than the provisioner grants"
            );
        }
    }

    /// The four rows `wamn-0h0g.22.14` ruled, pinned end to end: class ->
    /// family -> the stable ACL role name the ruling actually named. Going all
    /// the way to the role string is the point — asserting only the family
    /// would pass even if a family were later repointed at another role.
    #[test]
    fn the_ruled_authority_class_rows_hold() {
        let ruled = [
            (AuthorityClass::GuestSql, "wamn_app"),
            (AuthorityClass::ExecutorPlatform, "wamn_executor_platform"),
            (AuthorityClass::CallableHttp, "wamn_http_admitter"),
            (AuthorityClass::EventMaterializer, "wamn_event_materializer"),
            (AuthorityClass::Administration, "wamn_administration"),
        ];
        for (class, role) in ruled {
            assert_eq!(
                WorkloadRoleFamily::from(class).acl_role(),
                role,
                "wamn-0h0g.22.14 ruled {class} -> {role}"
            );
        }
    }

    /// One family per class. A collapse mapping two classes onto one family is
    /// the mutant this kills; the ruling forbids it, and it would silently give
    /// one class the other's authority.
    #[test]
    fn no_two_authority_classes_share_a_family() {
        let mut seen: Vec<(AuthorityClass, WorkloadRoleFamily)> = Vec::new();
        for class in AuthorityClass::ALL {
            let family = WorkloadRoleFamily::from(class);
            if let Some((other, _)) = seen.iter().find(|(_, f)| *f == family) {
                panic!(
                    "{class} and {other} both map to {family:?}; the ruling is one family per class"
                );
            }
            seen.push((class, family));
        }
        assert_eq!(seen.len(), 5);
    }

    /// Every class is covered. `ALL` plus the exhaustive match in `From` means a
    /// new variant cannot reach production unmapped: the match is a compile
    /// error first, and this catches an `ALL` that was not extended with it.
    #[test]
    fn every_authority_class_projects() {
        assert_eq!(
            AuthorityClass::ALL.len(),
            5,
            "a new authority class must be added to the ruled table above and to ALL"
        );
        for class in AuthorityClass::ALL {
            let role = WorkloadRoleFamily::from(class).acl_role();
            assert!(
                role.starts_with("wamn_"),
                "{class} projects onto {role}, which is not a wamn role"
            );
        }
    }

    const FAMILIES: [WorkloadRoleFamily; 13] = WorkloadRoleFamily::ALL;

    #[test]
    fn wrong_identity_grain_refuses() {
        let error = workload_generation_role(
            WorkloadRoleFamily::App,
            WorkloadRoleIdentity::Control {
                org: "o",
                project: "p",
                environment: "dev",
                database: "control",
            },
            CredentialGeneration::A,
        )
        .expect_err("a project family accepted a control identity");
        assert_eq!(error.expected, WorkloadRoleIdentityType::ProjectEnvKey);
        assert_eq!(error.actual, WorkloadRoleIdentityType::Control);
    }

    #[test]
    fn every_role_fits_postgres_and_generations_differ() {
        let identities = [
            (
                WorkloadRoleFamily::ServiceReader,
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::App,
                WorkloadRoleIdentity::ProjectEnvKey {
                    project_env_key: "t",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::Retention,
                WorkloadRoleIdentity::ProjectEnvKey {
                    project_env_key: "t",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::ExecutorPlatform,
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::HttpAdmitter,
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::EventMaterializer,
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::RegistryReader,
                WorkloadRoleIdentity::Control {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "control",
                },
            ),
            (
                WorkloadRoleFamily::IdentityReader,
                WorkloadRoleIdentity::Control {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "control",
                },
            ),
            (
                WorkloadRoleFamily::ConnectionReader,
                WorkloadRoleIdentity::Control {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "control",
                },
            ),
            (
                WorkloadRoleFamily::SessionRoleReader,
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::AuditRetention,
                WorkloadRoleIdentity::ProjectEnvKey {
                    project_env_key: "t",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::Administration,
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::Control,
                WorkloadRoleIdentity::Org {
                    org: "o",
                    database: "control",
                },
            ),
        ];
        assert_eq!(identities.len(), FAMILIES.len());
        for (family, identity) in identities {
            let a = workload_generation_role(family, identity, CredentialGeneration::A).unwrap();
            let b = workload_generation_role(family, identity, CredentialGeneration::B).unwrap();
            assert_ne!(a, b);
            assert!(a.len() <= 63, "{a}");
            assert!(b.len() <= 63, "{b}");
        }
    }

    /// THE TRUNCATION HAZARD, CLOSED BY CONSTRUCTION rather than by a runtime
    /// refusal (`wamn-0h0g.22.6.4`).
    ///
    /// `valid_project_env_key` admits 64 bytes and PostgreSQL caps identifiers at 63,
    /// truncating with a NOTICE instead of refusing — so a name carrying the
    /// project verbatim would collapse two long projects onto ONE role, which is
    /// a cross-project breach wearing a naming bug. The identity digest is
    /// fixed-width, so the project contributes NOTHING to the length: asserted
    /// here against the longest project the validator admits, so the property is
    /// measured rather than assumed.
    #[test]
    fn the_longest_admitted_project_env_key_still_mints_a_bounded_role() {
        let longest = "t".repeat(64);
        let short = WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: "t",
            database: "wamn-db-acme--dev--billing",
        };
        let long = WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &longest,
            database: "wamn-db-acme--dev--billing",
        };
        for family in [
            WorkloadRoleFamily::App,
            WorkloadRoleFamily::Retention,
            WorkloadRoleFamily::AuditRetention,
        ] {
            for generation in [CredentialGeneration::A, CredentialGeneration::B] {
                let from_short = workload_generation_role(family, short, generation).unwrap();
                let from_long = workload_generation_role(family, long, generation).unwrap();
                assert_eq!(
                    from_short.len(),
                    from_long.len(),
                    "{family:?}: the project must not reach the role NAME's length"
                );
                assert!(from_long.len() <= 63, "{from_long}");
                assert!(
                    !from_long.contains(&longest),
                    "{family:?}: the project id must never appear verbatim"
                );
                assert_ne!(from_short, from_long, "{family:?}: two projects, two roles");
            }
        }
    }

    #[test]
    fn the_two_overlong_new_families_carry_short_frozen_generation_prefixes() {
        // Reusing these 22- and 23-byte ACL role names as generation prefixes
        // mints 65- and 66-byte logins. PostgreSQL truncates rather than
        // refusing, and both truncations drop the `_a`/`_b` suffix, so the two
        // generations of one identity would collide on a single role.
        // `_` + 40-hex digest + `_` + one generation character.
        let suffix = IDENTITY_HASH_HEX_LEN + 3;
        for (family, prefix) in [
            (WorkloadRoleFamily::ExecutorPlatform, "wamn_exec_platform"),
            (WorkloadRoleFamily::EventMaterializer, "wamn_materializer"),
        ] {
            assert_eq!(family.generation_prefix(), prefix, "{family:?}");
            assert_ne!(family.generation_prefix(), family.acl_role(), "{family:?}");
            assert!(family.acl_role().len() + suffix > 63, "{family:?}");
            assert!(prefix.len() + suffix <= 63, "{family:?}");
        }
        // The callable-HTTP admitter's 18-byte name fits, so it keeps the
        // wildcard arm's default of reusing the ACL role name.
        assert_eq!(
            WorkloadRoleFamily::HttpAdmitter.generation_prefix(),
            WorkloadRoleFamily::HttpAdmitter.acl_role()
        );
    }

    /// The identity tuple the module's existing frozen rows already use. `database`
    /// is an opaque framed field in the derivation, so reusing this tuple puts
    /// the new identities in the same coordinate system as the old ones rather
    /// than inventing a control-database name here.
    const FROZEN_IDENTITY: WorkloadRoleIdentity<'static> = WorkloadRoleIdentity::Control {
        org: "acme",
        project: "billing",
        environment: "dev",
        database: "wamn-db-acme--billing--dev--k3m9x2p7",
    };

    /// THE IDENTITY LIE, CLOSED BY THE DERIVATION ITSELF (`wamn-0h0g.13.63`).
    ///
    /// Both control readers are `Control`-identity-bound because identity follows the
    /// RESOURCE PLANE: their credentials reach the CONTROL database. The
    /// convenient reuse — re-pointing either consumer's credential at the
    /// already-existing `ServiceReader` — would encode an identity lie in a variant
    /// that means `ProjectEnvironment`. It cannot be reintroduced quietly: the
    /// grain test refuses before any role name exists, so the reuse fails here
    /// rather than minting a plausible-looking login under the wrong plane.
    #[test]
    fn the_control_readers_cannot_be_reissued_as_the_project_environment_service_reader() {
        for family in [
            WorkloadRoleFamily::RegistryReader,
            WorkloadRoleFamily::IdentityReader,
        ] {
            assert_eq!(
                family.identity_type(),
                WorkloadRoleIdentityType::Control,
                "{family:?}: identity follows the resource plane, and that plane is control"
            );
            workload_generation_role(family, FROZEN_IDENTITY, CredentialGeneration::A)
                .expect("a control reader derives under control identity");
        }
        assert_eq!(
            WorkloadRoleFamily::ServiceReader.identity_type(),
            WorkloadRoleIdentityType::ProjectEnvironment,
            "ServiceReader is the project-environment reader; that is why it is not reusable"
        );
        let error = workload_generation_role(
            WorkloadRoleFamily::ServiceReader,
            FROZEN_IDENTITY,
            CredentialGeneration::A,
        )
        .expect_err("ServiceReader accepted a control identity");
        assert_eq!(error.family, WorkloadRoleFamily::ServiceReader);
        assert_eq!(error.expected, WorkloadRoleIdentityType::ProjectEnvironment);
        assert_eq!(error.actual, WorkloadRoleIdentityType::Control);
    }
}
