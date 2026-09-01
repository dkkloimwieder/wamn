//! Closed provisioning vocabulary for scoped workload login generations.

use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_run_state::{
    AuthorityClass, CredentialGeneration, EFFECT_WRITER_ROLE, app_scope_hash,
    effect_writer_generation_role, effect_writer_scope_hash,
};

use crate::name::{
    CONTROL_AUTHOR_SECRET_PREFIX, GUEST_SECRET_PREFIX, MANAGEMENT_ADMITTER_SECRET_PREFIX,
};
use crate::{APP_ROLE, DISPATCH_READER_ROLE};

/// Stable NOLOGIN role used by control-author generations.
pub const CONTROL_AUTHOR_ROLE: &str = "wamn_control_author";
/// Stable NOLOGIN role used by management-admission generations.
pub const MANAGEMENT_ADMITTER_ROLE: &str = "wamn_management_admitter";
/// Frozen generation prefix for the management-admitter family (`wamn-0h0g.13.62`).
///
/// The stable ACL role name is 24 bytes, so reusing it as the generation prefix
/// would mint a 67-byte identifier and PostgreSQL caps identifiers at 63. This
/// shorter frozen prefix keeps the derived login at 61 bytes with the 160-bit
/// scope digest and `_a`/`_b` suffix intact.
const MANAGEMENT_ADMITTER_GENERATION_PREFIX: &str = "wamn_mgmt_admitter";
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
/// at 61 bytes with the 160-bit scope digest and `_a`/`_b` suffix intact.
const EXECUTOR_PLATFORM_GENERATION_PREFIX: &str = "wamn_exec_platform";
/// Stable NOLOGIN role used by callable-HTTP admission generations.
pub const HTTP_ADMITTER_ROLE: &str = "wamn_http_admitter";
/// Stable NOLOGIN role used by event-materializer generations.
pub const EVENT_MATERIALIZER_ROLE: &str = "wamn_event_materializer";
/// Frozen generation prefix for the event-materializer family (`wamn-0fqa`).
///
/// The stable ACL role name is 23 bytes, so reusing it as the generation prefix
/// would mint a 66-byte identifier that PostgreSQL truncates into the scope
/// digest itself, collapsing both generations and three digest characters. This
/// shorter frozen prefix keeps the derived login at 60 bytes.
const EVENT_MATERIALIZER_GENERATION_PREFIX: &str = "wamn_materializer";
/// Stable NOLOGIN role used by control-registry reader generations.
pub const REGISTRY_READER_ROLE: &str = "wamn_registry_reader";
/// Stable NOLOGIN role used by control-identity reader generations.
pub const IDENTITY_READER_ROLE: &str = "wamn_identity_reader";

/// The shared NOLOGIN group role every non-guest tenant-floor arm targets
/// (`wamn-0h0g.22.17`).
///
/// ONE role, not one per family, and no `BYPASSRLS` anywhere. The tenant floor
/// is the GUEST floor: narrowing it `TO wamn_app` does not EXEMPT a platform
/// principal, because PostgreSQL DEFAULT-DENIES when RLS is enabled and no
/// policy matches the connected role. Each governed relation therefore carries
/// exactly one permissive arm `TO wamn_platform`, and the family's table grants
/// stay the thing that limits what it can reach.
pub const PLATFORM_GROUP_ROLE: &str = "wamn_platform";

pub(crate) const SCOPE_HASH_HEX_LEN: usize = 40;

/// Exhaustive provisioning families. Adding authority requires a code change.
///
/// `wamn-0h0g.13.59` froze this vocabulary at six families.
/// `wamn-0h0g.13.61` deliberately expands that frozen set once, from six to
/// seven, by admitting the project-environment-scoped management-admitter
/// family.
/// `wamn-0fqa` is the third deliberate expansion, from seven to ten, admitting
/// the three role families `wamn-0h0g.22.14`'s ruled AuthorityClass mapping
/// names and this vocabulary did not yet carry: executor-platform,
/// callable-HTTP admitter and event-materializer. The fourth ruled row,
/// guest-sql, already mapped to [`WorkloadRoleFamily::App`]. Only the families
/// land here; keying credential selection by authority class is
/// `wamn-0h0g.22.8`.
/// `wamn-0h0g.13.63` is the fourth deliberate expansion, from ten to twelve,
/// admitting the two CONTROL-scoped reader families the T1 system database's
/// two purported read-only consumers need. Scope follows the RESOURCE PLANE,
/// not the consumer's home, so both are [`WorkloadRoleScopeKind::Control`] and
/// neither may reuse the project-environment-scoped
/// [`WorkloadRoleFamily::ServiceReader`]. TWO families, not one, because the
/// two grant sets are disjoint and must stay so: `wamn-0h0g.12.116`'s consumer
/// reads `registry.event_readers` and `wamn-0h0g.12.67`'s reads
/// `identity.pats`, `identity.principals` and `identity.project_roles` — one
/// family for both is how one role gets widened to the union. Only the
/// families land here; the grants, Secrets and consumers are those two beads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleFamily {
    EffectWriter,
    ControlAuthor,
    ManagementAdmitter,
    DispatchReader,
    ServiceReader,
    App,
    Retention,
    ExecutorPlatform,
    HttpAdmitter,
    EventMaterializer,
    RegistryReader,
    IdentityReader,
}

impl WorkloadRoleFamily {
    /// Every family, in declaration order.
    ///
    /// `wamn-0h0g.22.16` makes this THE DRIVER rather than a convenience:
    /// provisioning's flag set, action dispatch and Secret naming are all
    /// derived by walking it, so an admitted family reaches every one of them
    /// without a list anywhere being appended to by hand.
    pub const ALL: [Self; 12] = [
        Self::EffectWriter,
        Self::ControlAuthor,
        Self::ManagementAdmitter,
        Self::DispatchReader,
        Self::ServiceReader,
        Self::App,
        Self::Retention,
        Self::ExecutorPlatform,
        Self::HttpAdmitter,
        Self::EventMaterializer,
        Self::RegistryReader,
        Self::IdentityReader,
    ];

    /// Stable NOLOGIN ACL role inherited by this family's generations.
    pub const fn acl_role(self) -> &'static str {
        match self {
            Self::EffectWriter => EFFECT_WRITER_ROLE,
            Self::ControlAuthor => CONTROL_AUTHOR_ROLE,
            Self::ManagementAdmitter => MANAGEMENT_ADMITTER_ROLE,
            Self::DispatchReader => DISPATCH_READER_ROLE,
            Self::ServiceReader => SERVICE_READER_ROLE,
            Self::App => APP_ROLE,
            Self::Retention => RETENTION_ROLE,
            Self::ExecutorPlatform => EXECUTOR_PLATFORM_ROLE,
            Self::HttpAdmitter => HTTP_ADMITTER_ROLE,
            Self::EventMaterializer => EVENT_MATERIALIZER_ROLE,
            Self::RegistryReader => REGISTRY_READER_ROLE,
            Self::IdentityReader => IDENTITY_READER_ROLE,
        }
    }

    /// Frozen prefix of this family's derived A/B generation identities.
    ///
    /// Equal to [`Self::acl_role`] for every family whose stable role name still
    /// fits the PostgreSQL identifier cap once the scope digest and generation
    /// suffix are appended. `ManagementAdmitter` (`wamn-0h0g.13.62`),
    /// `ExecutorPlatform` and `EventMaterializer` (`wamn-0fqa`) do not, so each
    /// carries its own shorter frozen prefix. `RegistryReader` and
    /// `IdentityReader` (`wamn-0h0g.13.63`) are the first families to fit
    /// EXACTLY: their 20-byte role names mint 63-byte logins, the largest
    /// PostgreSQL stores untruncated, so they take the wildcard arm and the
    /// exactness is pinned by test rather than left to be rediscovered.
    pub const fn generation_prefix(self) -> &'static str {
        match self {
            Self::ManagementAdmitter => MANAGEMENT_ADMITTER_GENERATION_PREFIX,
            Self::ExecutorPlatform => EXECUTOR_PLATFORM_GENERATION_PREFIX,
            Self::EventMaterializer => EVENT_MATERIALIZER_GENERATION_PREFIX,
            _ => self.acl_role(),
        }
    }

    /// Exact scope class used to derive generation identities.
    pub const fn scope_kind(self) -> WorkloadRoleScopeKind {
        match self {
            Self::EffectWriter | Self::App | Self::Retention => WorkloadRoleScopeKind::Tenant,
            Self::ManagementAdmitter
            | Self::DispatchReader
            | Self::ServiceReader
            | Self::ExecutorPlatform
            | Self::HttpAdmitter
            | Self::EventMaterializer => WorkloadRoleScopeKind::ProjectEnvironment,
            // Scope follows the RESOURCE PLANE, not the consumer's home: these
            // credentials reach the CONTROL database (`wamn-0h0g.13.63`).
            Self::ControlAuthor | Self::RegistryReader | Self::IdentityReader => {
                WorkloadRoleScopeKind::Control
            }
        }
    }

    /// Whether this family's generations are membered into
    /// [`PLATFORM_GROUP_ROLE`] and so reach the permissive tenant-floor arm.
    ///
    /// DERIVED from the two facts that already decide it, never tabulated:
    ///
    /// * [`Self::App`] is the GUEST. The floor exists to admit it, and a
    ///   platform arm would hand it every tenant's rows — the exact
    ///   cross-tenant read `wamn-0h0g.22.6` closed.
    /// * A [`WorkloadRoleScopeKind::Control`] family's credentials reach the
    ///   CONTROL database, whose store carries its own restrictive
    ///   `TO wamn_control_author` arm on a different authority derivation
    ///   (`deploy/sql/control-portable-store.sql`). None of them holds a grant
    ///   on any relation carrying the project-plane floor, so membership would
    ///   be authority without a reader.
    ///
    /// * [`Self::EffectWriter`] holds a STABLE ROLE UNDER A SHAPE GUARD that
    ///   forbids it (`wamn-0h0g.22.32`). `RunPlaneActionKind::VerifyEffectWriterRole`
    ///   raises 42501 `effect-writer-role-out-of-bounds` when `wamn_effect_writer`
    ///   holds ANY row in `pg_auth_members` as a member, and a `wamn_platform`
    ///   edge is exactly such a row. The membership and the guard cannot both
    ///   hold, and the guard is the older, narrower contract. The writer reaches
    ///   its four ledgers through PER-RELATION arms naming it directly in
    ///   `deploy/sql/run-state.sql` instead — not through this group.
    ///
    /// Everything else — the seven families whose credentials reach a
    /// project-environment or tenant database, are not the guest, and are not
    /// under that guard — is a member. `EventMaterializer` exercises that edge
    /// through its two catalog reads. `ServiceReader` remains the empty-surface
    /// case: an RLS arm with no table grant behind it admits nothing, while the
    /// edge prevents a future first grant from silently reading zero rows.
    ///
    /// `Retention` is `Tenant`-scoped and is a member anyway. Its login names
    /// carry a tenant DIGEST, but `current_tenant_key` recovers a key only from
    /// the guest generation pattern, and widening that regex is refused
    /// (`wamn-0h0g.22.17` owner ruling). The shared arm therefore makes it
    /// cross-tenant on the relations it holds grants on; the tenant predicate it
    /// keeps in its own statements is what re-narrows it.
    pub const fn is_platform_grain(self) -> bool {
        !matches!(self, Self::App | Self::EffectWriter)
            && !matches!(self.scope_kind(), WorkloadRoleScopeKind::Control)
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
    /// A closed three-value vocabulary of SHAPES, not a per-family list: an
    /// admitted family lands on the plain single-`url` Secret every consumer
    /// already mounts through `secretKeyRef … key: url`, with no edit here.
    pub fn secret_body_kind(self) -> WorkloadSecretBodyKind {
        match self {
            // The frozen `credential.json` document the runtime parses.
            Self::EffectWriter => WorkloadSecretBodyKind::EffectWriterCredential,
            // The guest credential IS the tenant authority, so its Secret
            // carries the tenant key that keys every governed predicate.
            Self::App => WorkloadSecretBodyKind::TenantUrl,
            _ => WorkloadSecretBodyKind::Url,
        }
    }

    /// Prefix of this family's credential Secret name.
    ///
    /// `wamn-<cli-stem>-` for every family whose Secret name was not frozen
    /// first. The three overrides are the frozen ones and carry their reasons
    /// on the constants themselves.
    pub fn secret_prefix(self) -> String {
        match self {
            Self::ControlAuthor => CONTROL_AUTHOR_SECRET_PREFIX.to_string(),
            Self::ManagementAdmitter => MANAGEMENT_ADMITTER_SECRET_PREFIX.to_string(),
            Self::App => GUEST_SECRET_PREFIX.to_string(),
            _ => format!("wamn-{}-", self.cli_stem()),
        }
    }

    pub(crate) const fn scope_domain(self) -> &'static [u8] {
        match self {
            Self::EffectWriter => b"wamn.effect-writer.scope.v0.1",
            Self::ControlAuthor => b"wamn.control-author.scope.v0.1",
            Self::ManagementAdmitter => b"wamn.management-admitter.scope.v0.1",
            Self::DispatchReader => b"wamn.dispatch-reader.scope.v0.1",
            Self::ServiceReader => b"wamn.service-reader.scope.v0.1",
            Self::App => b"wamn.app.scope.v0.1",
            Self::Retention => b"wamn.run-retention.scope.v0.1",
            Self::ExecutorPlatform => b"wamn.executor-platform.scope.v0.1",
            Self::HttpAdmitter => b"wamn.http-admitter.scope.v0.1",
            Self::EventMaterializer => b"wamn.event-materializer.scope.v0.1",
            Self::RegistryReader => b"wamn.registry-reader.scope.v0.1",
            Self::IdentityReader => b"wamn.identity-reader.scope.v0.1",
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
/// The direction is deliberate and one-way. Several families (`EffectWriter`,
/// `ControlAuthor`, `DispatchReader`, `ServiceReader`, `Retention`,
/// `ManagementAdmitter`) carry no authority class at all, so the inverse is not
/// a function and is not offered.
impl From<AuthorityClass> for WorkloadRoleFamily {
    fn from(class: AuthorityClass) -> Self {
        match class {
            AuthorityClass::GuestSql => Self::App,
            AuthorityClass::ExecutorPlatform => Self::ExecutorPlatform,
            AuthorityClass::CallableHttp => Self::HttpAdmitter,
            AuthorityClass::EventMaterializer => Self::EventMaterializer,
        }
    }
}

/// The closed set of credential-Secret body shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadSecretBodyKind {
    /// One `url` key.
    Url,
    /// One `url` key, plus the tenant key label and tenant annotation.
    TenantUrl,
    /// The frozen effect-writer `credential.json` document.
    EffectWriterCredential,
}

/// The three admitted provisioning scope shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleScopeKind {
    Tenant,
    ProjectEnvironment,
    Control,
}

impl WorkloadRoleScopeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::ProjectEnvironment => "project-environment",
            Self::Control => "control",
        }
    }
}

/// Exact identity inputs for one workload generation pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleScope<'a> {
    Tenant {
        tenant: &'a str,
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
}

impl<'a> WorkloadRoleScope<'a> {
    /// Database receiving this generation's sole direct CONNECT grant.
    pub const fn database(self) -> &'a str {
        match self {
            Self::Tenant { database, .. }
            | Self::ProjectEnvironment { database, .. }
            | Self::Control { database, .. } => database,
        }
    }

    const fn kind(self) -> WorkloadRoleScopeKind {
        match self {
            Self::Tenant { .. } => WorkloadRoleScopeKind::Tenant,
            Self::ProjectEnvironment { .. } => WorkloadRoleScopeKind::ProjectEnvironment,
            Self::Control { .. } => WorkloadRoleScopeKind::Control,
        }
    }
}

/// A family was paired with the wrong scope grain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadRoleScopeError {
    family: WorkloadRoleFamily,
    expected: WorkloadRoleScopeKind,
    actual: WorkloadRoleScopeKind,
}

impl fmt::Display for WorkloadRoleScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "workload role family {:?} requires {} scope, not {} scope",
            self.family,
            self.expected.as_str(),
            self.actual.as_str(),
        )
    }
}

impl std::error::Error for WorkloadRoleScopeError {}

/// Derive the deterministic 160-bit scope suffix for one family.
pub fn workload_role_scope_hash(
    family: WorkloadRoleFamily,
    scope: WorkloadRoleScope<'_>,
) -> Result<String, WorkloadRoleScopeError> {
    let actual = scope.kind();
    let expected = family.scope_kind();
    if actual != expected {
        return Err(WorkloadRoleScopeError {
            family,
            expected,
            actual,
        });
    }
    // Two families delegate to `wamn-run-state`, which is the leaf the RUNTIME
    // can reach: the effect writer because its credential lives there, and the
    // App family because `wamn-0h0g.22.6.7` makes the runtime check that a
    // resolved guest credential belongs to the tenant it is about to serve.
    // Delegation, not duplication — there is still ONE definition of each.
    if family == WorkloadRoleFamily::EffectWriter {
        let WorkloadRoleScope::Tenant { tenant, database } = scope else {
            unreachable!("scope kind was checked above")
        };
        return Ok(effect_writer_scope_hash(tenant, database));
    }
    if family == WorkloadRoleFamily::App {
        let WorkloadRoleScope::Tenant { tenant, database } = scope else {
            unreachable!("scope kind was checked above")
        };
        return Ok(app_scope_hash(tenant, database));
    }

    let mut preimage = Vec::new();
    frame(&mut preimage, family.scope_domain());
    match scope {
        WorkloadRoleScope::Tenant { tenant, database } => {
            push_field(&mut preimage, "tenant", tenant);
            push_field(&mut preimage, "database", database);
        }
        WorkloadRoleScope::ProjectEnvironment {
            org,
            project,
            environment,
            database,
        }
        | WorkloadRoleScope::Control {
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
    }
    let digest = hex::encode(Sha256::digest(preimage));
    Ok(digest[..SCOPE_HASH_HEX_LEN].to_string())
}

/// Derive one bounded A/B LOGIN role for a closed workload family.
pub fn workload_generation_role(
    family: WorkloadRoleFamily,
    scope: WorkloadRoleScope<'_>,
    generation: CredentialGeneration,
) -> Result<String, WorkloadRoleScopeError> {
    if family == WorkloadRoleFamily::EffectWriter {
        let actual = scope.kind();
        let expected = family.scope_kind();
        if actual != expected {
            return Err(WorkloadRoleScopeError {
                family,
                expected,
                actual,
            });
        }
        let WorkloadRoleScope::Tenant { tenant, database } = scope else {
            unreachable!("scope kind was checked above")
        };
        return Ok(effect_writer_generation_role(tenant, database, generation));
    }
    Ok(format!(
        "{}_{}_{}",
        family.generation_prefix(),
        workload_role_scope_hash(family, scope)?,
        generation.as_str(),
    ))
}

/// Derive the retired project-environment effect-writer role for migration only.
///
/// Callers may inspect and retire this exact role. Generic prepare never creates
/// it; all newly minted effect-writer roles use tenant scope.
pub fn legacy_effect_writer_generation_role(
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
) -> String {
    let mut preimage = Vec::new();
    frame(
        &mut preimage,
        WorkloadRoleFamily::EffectWriter.scope_domain(),
    );
    for (tag, value) in [
        ("org", org),
        ("project", project),
        ("environment", environment),
        ("database", database),
    ] {
        push_field(&mut preimage, tag, value);
    }
    let digest = hex::encode(Sha256::digest(preimage));
    format!(
        "{EFFECT_WRITER_ROLE}_{}_{}",
        &digest[..SCOPE_HASH_HEX_LEN],
        generation.as_str()
    )
}

fn push_field(preimage: &mut Vec<u8>, tag: &str, value: &str) {
    frame(preimage, tag.as_bytes());
    frame(preimage, value.as_bytes());
}

fn frame(preimage: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("a framed scope field fits u64");
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
        assert_eq!(seen.len(), 4);
    }

    /// Every class is covered. `ALL` plus the exhaustive match in `From` means a
    /// new variant cannot reach production unmapped: the match is a compile
    /// error first, and this catches an `ALL` that was not extended with it.
    #[test]
    fn every_authority_class_projects() {
        assert_eq!(
            AuthorityClass::ALL.len(),
            4,
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

    /// The exact vocabulary, in declaration order (`wamn-0fqa`: seven to ten;
    /// `wamn-0h0g.13.63`: ten to twelve).
    const FAMILIES: [WorkloadRoleFamily; 12] = [
        WorkloadRoleFamily::EffectWriter,
        WorkloadRoleFamily::ControlAuthor,
        WorkloadRoleFamily::ManagementAdmitter,
        WorkloadRoleFamily::DispatchReader,
        WorkloadRoleFamily::ServiceReader,
        WorkloadRoleFamily::App,
        WorkloadRoleFamily::Retention,
        WorkloadRoleFamily::ExecutorPlatform,
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleFamily::EventMaterializer,
        WorkloadRoleFamily::RegistryReader,
        WorkloadRoleFamily::IdentityReader,
    ];

    #[test]
    fn family_set_and_scope_classes_are_closed() {
        // A thirteenth variant fails to compile here as well as in the
        // implementation, so the pinned vocabulary cannot silently grow.
        for (index, family) in FAMILIES.into_iter().enumerate() {
            let pinned = match family {
                WorkloadRoleFamily::EffectWriter => 0,
                WorkloadRoleFamily::ControlAuthor => 1,
                WorkloadRoleFamily::ManagementAdmitter => 2,
                WorkloadRoleFamily::DispatchReader => 3,
                WorkloadRoleFamily::ServiceReader => 4,
                WorkloadRoleFamily::App => 5,
                WorkloadRoleFamily::Retention => 6,
                WorkloadRoleFamily::ExecutorPlatform => 7,
                WorkloadRoleFamily::HttpAdmitter => 8,
                WorkloadRoleFamily::EventMaterializer => 9,
                WorkloadRoleFamily::RegistryReader => 10,
                WorkloadRoleFamily::IdentityReader => 11,
            };
            assert_eq!(index, pinned, "{family:?}");
        }
        assert_eq!(
            FAMILIES.map(WorkloadRoleFamily::scope_kind),
            [
                WorkloadRoleScopeKind::Tenant,
                WorkloadRoleScopeKind::Control,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::Tenant,
                WorkloadRoleScopeKind::Tenant,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::Control,
                WorkloadRoleScopeKind::Control,
            ],
        );
        assert_eq!(
            FAMILIES.map(WorkloadRoleFamily::acl_role),
            [
                "wamn_effect_writer",
                "wamn_control_author",
                "wamn_management_admitter",
                "wamn_dispatch_reader",
                "wamn_service_reader",
                "wamn_app",
                "wamn_run_retention",
                "wamn_executor_platform",
                "wamn_http_admitter",
                "wamn_event_materializer",
                "wamn_registry_reader",
                "wamn_identity_reader",
            ],
        );
    }

    #[test]
    fn wrong_scope_grain_refuses() {
        let error = workload_generation_role(
            WorkloadRoleFamily::EffectWriter,
            WorkloadRoleScope::Control {
                org: "o",
                project: "p",
                environment: "dev",
                database: "control",
            },
            CredentialGeneration::A,
        )
        .expect_err("a tenant family accepted a control scope");
        assert_eq!(error.expected, WorkloadRoleScopeKind::Tenant);
        assert_eq!(error.actual, WorkloadRoleScopeKind::Control);
    }

    #[test]
    fn every_role_fits_postgres_and_generations_differ() {
        let scopes = [
            (
                WorkloadRoleFamily::EffectWriter,
                WorkloadRoleScope::Tenant {
                    tenant: "t",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::ControlAuthor,
                WorkloadRoleScope::Control {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "control",
                },
            ),
            (
                WorkloadRoleFamily::ManagementAdmitter,
                WorkloadRoleScope::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::DispatchReader,
                WorkloadRoleScope::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::ServiceReader,
                WorkloadRoleScope::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::App,
                WorkloadRoleScope::Tenant {
                    tenant: "t",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::Retention,
                WorkloadRoleScope::Tenant {
                    tenant: "t",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::ExecutorPlatform,
                WorkloadRoleScope::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::HttpAdmitter,
                WorkloadRoleScope::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::EventMaterializer,
                WorkloadRoleScope::ProjectEnvironment {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "db",
                },
            ),
            (
                WorkloadRoleFamily::RegistryReader,
                WorkloadRoleScope::Control {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "control",
                },
            ),
            (
                WorkloadRoleFamily::IdentityReader,
                WorkloadRoleScope::Control {
                    org: "o",
                    project: "p",
                    environment: "dev",
                    database: "control",
                },
            ),
        ];
        assert_eq!(scopes.len(), FAMILIES.len());
        for (family, scope) in scopes {
            let a = workload_generation_role(family, scope, CredentialGeneration::A).unwrap();
            let b = workload_generation_role(family, scope, CredentialGeneration::B).unwrap();
            assert_ne!(a, b);
            assert!(a.len() <= 63, "{a}");
            assert!(b.len() <= 63, "{b}");
        }
    }

    /// THE TRUNCATION HAZARD, CLOSED BY CONSTRUCTION rather than by a runtime
    /// refusal (`wamn-0h0g.22.6.4`).
    ///
    /// `valid_tenant` admits 64 bytes and PostgreSQL caps identifiers at 63,
    /// truncating with a NOTICE instead of refusing — so a name carrying the
    /// tenant verbatim would collapse two long tenants onto ONE role, which is
    /// a cross-tenant breach wearing a naming bug. The scope digest is
    /// fixed-width, so the tenant contributes NOTHING to the length: asserted
    /// here against the longest tenant the validator admits, so the property is
    /// measured rather than assumed.
    #[test]
    fn the_longest_admitted_tenant_still_mints_a_bounded_role() {
        let longest = "t".repeat(64);
        let short = WorkloadRoleScope::Tenant {
            tenant: "t",
            database: "wamn-db-acme--billing--dev",
        };
        let long = WorkloadRoleScope::Tenant {
            tenant: &longest,
            database: "wamn-db-acme--billing--dev",
        };
        for family in [
            WorkloadRoleFamily::App,
            WorkloadRoleFamily::EffectWriter,
            WorkloadRoleFamily::Retention,
        ] {
            for generation in [CredentialGeneration::A, CredentialGeneration::B] {
                let from_short = workload_generation_role(family, short, generation).unwrap();
                let from_long = workload_generation_role(family, long, generation).unwrap();
                assert_eq!(
                    from_short.len(),
                    from_long.len(),
                    "{family:?}: the tenant must not reach the role NAME's length"
                );
                assert!(from_long.len() <= 63, "{from_long}");
                assert!(
                    !from_long.contains(&longest),
                    "{family:?}: the tenant id must never appear verbatim"
                );
                assert_ne!(from_short, from_long, "{family:?}: two tenants, two roles");
            }
        }
    }

    #[test]
    fn management_admitter_generation_identity_is_frozen_and_distinct_from_its_acl_role() {
        assert_eq!(
            WorkloadRoleFamily::ManagementAdmitter.acl_role(),
            MANAGEMENT_ADMITTER_ROLE
        );
        // The owner froze this prefix at 19 bytes or fewer so the derived
        // login stays under the PostgreSQL identifier cap.
        assert!(MANAGEMENT_ADMITTER_GENERATION_PREFIX.len() <= 19);
        assert_ne!(
            MANAGEMENT_ADMITTER_GENERATION_PREFIX,
            MANAGEMENT_ADMITTER_ROLE
        );
        let role = workload_generation_role(
            WorkloadRoleFamily::ManagementAdmitter,
            WorkloadRoleScope::ProjectEnvironment {
                org: "acme",
                project: "billing",
                environment: "dev",
                database: "wamn-db-acme--billing--dev--k3m9x2p7",
            },
            CredentialGeneration::A,
        )
        .unwrap();
        assert_eq!(
            role,
            "wamn_mgmt_admitter_c1e0f3849ce98895f9593009bc5f60e870150758_a"
        );
        assert_eq!(role.len(), 61);
    }

    #[test]
    fn the_three_authority_class_families_freeze_their_strings_and_identities() {
        // `wamn-0fqa`. Role names are `wamn-0h0g.22.14`'s ruled mapping targets;
        // the derived logins were computed independently of this module and
        // created on PostgreSQL 18.6, which stored all three untruncated.
        for (family, acl_role, scope_domain, role, length) in [
            (
                WorkloadRoleFamily::ExecutorPlatform,
                "wamn_executor_platform",
                b"wamn.executor-platform.scope.v0.1".as_slice(),
                "wamn_exec_platform_3626eacd20996441de7f3fcb96938db40ea70218_a",
                61,
            ),
            (
                WorkloadRoleFamily::HttpAdmitter,
                "wamn_http_admitter",
                b"wamn.http-admitter.scope.v0.1".as_slice(),
                "wamn_http_admitter_2c32ce283199537fe77884c321e5148093e10895_a",
                61,
            ),
            (
                WorkloadRoleFamily::EventMaterializer,
                "wamn_event_materializer",
                b"wamn.event-materializer.scope.v0.1".as_slice(),
                "wamn_materializer_3506a61f50601c211eec66b05f70f45a57c34879_a",
                60,
            ),
        ] {
            assert_eq!(family.acl_role(), acl_role, "{family:?}");
            assert_eq!(family.scope_domain(), scope_domain, "{family:?}");
            assert_eq!(
                family.scope_kind(),
                WorkloadRoleScopeKind::ProjectEnvironment,
                "{family:?}"
            );
            let derived = workload_generation_role(
                family,
                WorkloadRoleScope::ProjectEnvironment {
                    org: "acme",
                    project: "billing",
                    environment: "dev",
                    database: "wamn-db-acme--billing--dev--k3m9x2p7",
                },
                CredentialGeneration::A,
            )
            .unwrap();
            assert_eq!(derived, role);
            assert_eq!(derived.len(), length, "{family:?}");
        }
    }

    #[test]
    fn the_two_overlong_new_families_carry_short_frozen_generation_prefixes() {
        // Reusing these 22- and 23-byte ACL role names as generation prefixes
        // mints 65- and 66-byte logins. PostgreSQL truncates rather than
        // refusing, and both truncations drop the `_a`/`_b` suffix, so the two
        // generations of one scope would collide on a single role.
        // `_` + 40-hex digest + `_` + one generation character.
        let suffix = SCOPE_HASH_HEX_LEN + 3;
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

    /// The scope tuple the module's existing frozen rows already use. `database`
    /// is an opaque framed field in the derivation, so reusing this tuple puts
    /// the new identities in the same coordinate system as the old ones rather
    /// than inventing a control-database name here.
    const FROZEN_SCOPE: WorkloadRoleScope<'static> = WorkloadRoleScope::Control {
        org: "acme",
        project: "billing",
        environment: "dev",
        database: "wamn-db-acme--billing--dev--k3m9x2p7",
    };

    /// THE SCOPE LIE, CLOSED BY THE DERIVATION ITSELF (`wamn-0h0g.13.63`).
    ///
    /// Both control readers are `Control`-scoped because scope follows the
    /// RESOURCE PLANE: their credentials reach the CONTROL database. The
    /// convenient reuse — re-pointing either consumer's credential at the
    /// already-existing `ServiceReader` — would encode a scope lie in a variant
    /// that means `ProjectEnvironment`. It cannot be reintroduced quietly: the
    /// grain check refuses before any role name exists, so the reuse fails here
    /// rather than minting a plausible-looking login under the wrong plane.
    #[test]
    fn the_control_readers_cannot_be_reissued_as_the_project_environment_service_reader() {
        for family in [
            WorkloadRoleFamily::RegistryReader,
            WorkloadRoleFamily::IdentityReader,
        ] {
            assert_eq!(
                family.scope_kind(),
                WorkloadRoleScopeKind::Control,
                "{family:?}: scope follows the resource plane, and that plane is control"
            );
            workload_generation_role(family, FROZEN_SCOPE, CredentialGeneration::A)
                .expect("a control reader derives under control scope");
        }
        assert_eq!(
            WorkloadRoleFamily::ServiceReader.scope_kind(),
            WorkloadRoleScopeKind::ProjectEnvironment,
            "ServiceReader is the project-environment reader; that is why it is not reusable"
        );
        let error = workload_generation_role(
            WorkloadRoleFamily::ServiceReader,
            FROZEN_SCOPE,
            CredentialGeneration::A,
        )
        .expect_err("ServiceReader accepted a control scope");
        assert_eq!(error.family, WorkloadRoleFamily::ServiceReader);
        assert_eq!(error.expected, WorkloadRoleScopeKind::ProjectEnvironment);
        assert_eq!(error.actual, WorkloadRoleScopeKind::Control);
    }

    /// The two control readers' frozen strings and identities.
    ///
    /// `wamn-0h0g.13.63`. The derived logins were computed independently of this
    /// module. Both land at EXACTLY 63 bytes — the largest PostgreSQL stores
    /// untruncated — so the equality on the length is the guard: a longer role
    /// name or a longer digest would be truncated silently, and truncation
    /// drops the `_a`/`_b` suffix that keeps the two generations apart.
    #[test]
    fn the_two_control_reader_families_freeze_their_strings_and_identities() {
        for (family, acl_role, scope_domain, role_a) in [
            (
                WorkloadRoleFamily::RegistryReader,
                "wamn_registry_reader",
                b"wamn.registry-reader.scope.v0.1".as_slice(),
                "wamn_registry_reader_c8216ab3ed30b424606deacec7d0d7cb7e65649b_a",
            ),
            (
                WorkloadRoleFamily::IdentityReader,
                "wamn_identity_reader",
                b"wamn.identity-reader.scope.v0.1".as_slice(),
                "wamn_identity_reader_4ea81a9f35b250844e8e95f9985cf1e4f7c16dba_a",
            ),
        ] {
            assert_eq!(family.acl_role(), acl_role, "{family:?}");
            assert_eq!(family.scope_domain(), scope_domain, "{family:?}");
            // The 20-byte role name fits, so no shorter frozen prefix is minted.
            assert_eq!(family.generation_prefix(), acl_role, "{family:?}");
            assert_eq!(acl_role.len() + SCOPE_HASH_HEX_LEN + 3, 63, "{family:?}");
            let derived =
                workload_generation_role(family, FROZEN_SCOPE, CredentialGeneration::A).unwrap();
            assert_eq!(derived, role_a);
            assert_eq!(derived.len(), 63, "{family:?}");
        }
        // Two families, two domains, two distinct identities on one scope: the
        // disjoint grant sets `wamn-0h0g.12.116` and `wamn-0h0g.12.67` own can
        // never converge on a single role.
        assert_ne!(
            workload_generation_role(
                WorkloadRoleFamily::RegistryReader,
                FROZEN_SCOPE,
                CredentialGeneration::A
            )
            .unwrap(),
            workload_generation_role(
                WorkloadRoleFamily::IdentityReader,
                FROZEN_SCOPE,
                CredentialGeneration::A
            )
            .unwrap(),
        );
    }

    #[test]
    fn legacy_effect_writer_identity_is_retirement_only_and_frozen() {
        assert_eq!(
            legacy_effect_writer_generation_role(
                "acme",
                "billing",
                "dev",
                "wamn-db-acme--billing--dev--k3m9x2p7",
                CredentialGeneration::A,
            ),
            "wamn_effect_writer_3c92a981fa554e60b309efa67f5b35e8ba687221_a"
        );
    }
}
