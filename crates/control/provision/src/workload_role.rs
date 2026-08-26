//! Closed provisioning vocabulary for scoped workload login generations.

use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_run_state::{
    AuthorityClass, CredentialGeneration, EFFECT_WRITER_ROLE, effect_writer_generation_role,
    effect_writer_scope_hash,
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
}

impl WorkloadRoleFamily {
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
        }
    }

    /// Frozen prefix of this family's derived A/B generation identities.
    ///
    /// Equal to [`Self::acl_role`] for every family whose stable role name still
    /// fits the PostgreSQL identifier cap once the scope digest and generation
    /// suffix are appended. `ManagementAdmitter` (`wamn-0h0g.13.62`),
    /// `ExecutorPlatform` and `EventMaterializer` (`wamn-0fqa`) do not, so each
    /// carries its own shorter frozen prefix.
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
            Self::ControlAuthor => WorkloadRoleScopeKind::Control,
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
    if family == WorkloadRoleFamily::EffectWriter {
        let WorkloadRoleScope::Tenant { tenant, database } = scope else {
            unreachable!("scope kind was checked above")
        };
        return Ok(effect_writer_scope_hash(tenant, database));
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

    /// The exact vocabulary, in declaration order (`wamn-0fqa`: seven to ten).
    const FAMILIES: [WorkloadRoleFamily; 10] = [
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
    ];

    #[test]
    fn family_set_and_scope_classes_are_closed() {
        // An eleventh variant fails to compile here as well as in the
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
