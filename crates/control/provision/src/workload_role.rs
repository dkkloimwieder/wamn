//! Closed provisioning vocabulary for scoped workload login generations.

use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_run_state::{
    CredentialGeneration, EFFECT_WRITER_ROLE, effect_writer_generation_role,
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

const SCOPE_HASH_HEX_LEN: usize = 40;

/// Exhaustive provisioning families. Adding authority requires a code change.
///
/// `wamn-0h0g.13.59` froze this vocabulary at six families.
/// `wamn-0h0g.13.61` deliberately expands that frozen set once, from six to
/// seven, by admitting the project-environment-scoped management-admitter
/// family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadRoleFamily {
    EffectWriter,
    ControlAuthor,
    ManagementAdmitter,
    DispatchReader,
    ServiceReader,
    App,
    Retention,
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
        }
    }

    /// Frozen prefix of this family's derived A/B generation identities.
    ///
    /// Equal to [`Self::acl_role`] for every family but `ManagementAdmitter`,
    /// whose stable role name does not fit the PostgreSQL identifier cap once
    /// the scope digest and generation suffix are appended (`wamn-0h0g.13.62`).
    pub const fn generation_prefix(self) -> &'static str {
        match self {
            Self::ManagementAdmitter => MANAGEMENT_ADMITTER_GENERATION_PREFIX,
            _ => self.acl_role(),
        }
    }

    /// Exact scope class used to derive generation identities.
    pub const fn scope_kind(self) -> WorkloadRoleScopeKind {
        match self {
            Self::EffectWriter | Self::App | Self::Retention => WorkloadRoleScopeKind::Tenant,
            Self::ManagementAdmitter | Self::DispatchReader | Self::ServiceReader => {
                WorkloadRoleScopeKind::ProjectEnvironment
            }
            Self::ControlAuthor => WorkloadRoleScopeKind::Control,
        }
    }

    const fn scope_domain(self) -> &'static [u8] {
        match self {
            Self::EffectWriter => b"wamn.effect-writer.scope.v0.1",
            Self::ControlAuthor => b"wamn.control-author.scope.v0.1",
            Self::ManagementAdmitter => b"wamn.management-admitter.scope.v0.1",
            Self::DispatchReader => b"wamn.dispatch-reader.scope.v0.1",
            Self::ServiceReader => b"wamn.service-reader.scope.v0.1",
            Self::App => b"wamn.app.scope.v0.1",
            Self::Retention => b"wamn.run-retention.scope.v0.1",
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

    #[test]
    fn family_set_and_scope_classes_are_closed() {
        assert_eq!(
            [
                WorkloadRoleFamily::EffectWriter,
                WorkloadRoleFamily::ControlAuthor,
                WorkloadRoleFamily::ManagementAdmitter,
                WorkloadRoleFamily::DispatchReader,
                WorkloadRoleFamily::ServiceReader,
                WorkloadRoleFamily::App,
                WorkloadRoleFamily::Retention,
            ]
            .map(WorkloadRoleFamily::scope_kind),
            [
                WorkloadRoleScopeKind::Tenant,
                WorkloadRoleScopeKind::Control,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::ProjectEnvironment,
                WorkloadRoleScopeKind::Tenant,
                WorkloadRoleScopeKind::Tenant,
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
        ];
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
