//! Provisioning validation and database names.

use crate::error::ProvisionError;
use crate::workload_role::WorkloadRoleFamily;

/// The shared role of generated application statements.
pub use wamn_identity_protocol::authority_class::APP_ROLE;
/// The owner of project databases and platform tables.
pub const DB_OWNER_ROLE: &str = "wamn_db_owner";
pub use wamn_registry::identifiers::MAX_PROJECT_ID_LEN;
pub use wamn_registry::name::{
    CDC_OBJECT_PREFIX, CDC_SECRET_PREFIX, DB_PREFIX, GUEST_SECRET_PREFIX, INSTANCE_SUFFIX_LEN,
    MAX_DB_NAME_LEN, MAX_NAMESPACE_LEN, MAX_NAMESPACE_STEM_LEN, NAMESPACE_PREFIX, cdc_object_name,
    control_administration_key, control_administration_secret_name, project_env_cdc_secret_name,
    project_env_database_name, project_env_namespace, project_env_namespace_stem,
    project_env_secret_name,
};
pub use wamn_registry::project_env_key;

fn slug_reason(id: &str) -> Option<&'static str> {
    wamn_registry::identifiers::project_id_reason(id)
}

/// Refuse invalid or reserved project identifiers.
pub fn validate_project_id(id: &str) -> Result<(), ProvisionError> {
    if let Some(reason) = slug_reason(id) {
        return Err(ProvisionError::InvalidProjectId {
            id: id.to_string(),
            reason,
        });
    }
    if wamn_registry::identifiers::project_id_is_reserved(id) {
        return Err(ProvisionError::ReservedProjectId { id: id.to_string() });
    }
    Ok(())
}
fn is_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// Use the registry name for a project workload credential.
pub fn workload_secret_name(
    family: WorkloadRoleFamily,
    org: &str,
    project: &str,
    env: &wamn_registry::EnvName,
) -> String {
    wamn_registry::name::workload_secret_name(&family.secret_prefix(), org, project, env)
}

/// Use the registry name for an org workload credential.
pub fn org_workload_secret_name(family: WorkloadRoleFamily, org: &str) -> String {
    wamn_registry::name::org_workload_secret_name(&family.secret_prefix(), org)
}

/// Refuse a suffix that cannot identify a provisioned database.
pub fn validate_instance_suffix(instance: &str) -> Result<(), ProvisionError> {
    let reason = if instance.len() != INSTANCE_SUFFIX_LEN {
        "must be exactly 8 bytes"
    } else if !instance.bytes().all(is_alnum) {
        "only lowercase letters and digits are allowed"
    } else {
        return Ok(());
    };
    Err(ProvisionError::InvalidComponent {
        component: "instance",
        value: instance.to_string(),
        reason,
    })
}

/// Use the application workload name for guest SQL credentials.
pub fn project_env_guest_secret_name(
    org: &str,
    project: &str,
    env: &wamn_registry::EnvName,
) -> String {
    workload_secret_name(WorkloadRoleFamily::App, org, project, env)
}

/// Refuse coordinates whose database or namespace name exceeds its limit.
pub fn validate_project_env(
    org: &str,
    project: &str,
    env: &wamn_registry::EnvName,
) -> Result<(), ProvisionError> {
    if let Some(reason) = slug_reason(org) {
        return Err(ProvisionError::InvalidComponent {
            component: "org",
            value: org.to_string(),
            reason,
        });
    }
    if wamn_registry::identifiers::project_id_is_reserved(org) {
        return Err(ProvisionError::InvalidComponent {
            component: "org",
            value: org.to_owned(),
            reason: "reserved platform prefix",
        });
    }
    validate_project_id(project)?;
    let probe = "0".repeat(INSTANCE_SUFFIX_LEN);
    let database = project_env_database_name(org, project, env, &probe);
    if database.len() > MAX_DB_NAME_LEN {
        return Err(ProvisionError::NameTooLong {
            name: database,
            max: MAX_DB_NAME_LEN,
        });
    }
    let stem = project_env_namespace_stem(org, project, env);
    if stem.len() > MAX_NAMESPACE_STEM_LEN {
        return Err(ProvisionError::NameTooLong {
            name: stem,
            max: MAX_NAMESPACE_STEM_LEN,
        });
    }
    Ok(())
}

/// Refuse coordinates whose CDC object name exceeds the database limit.
pub fn validate_project_env_cdc(
    org: &str,
    project: &str,
    env: &wamn_registry::EnvName,
) -> Result<(), ProvisionError> {
    validate_project_env(org, project, env)?;
    let probe = "0".repeat(INSTANCE_SUFFIX_LEN);
    let cdc = cdc_object_name(org, project, env, &probe);
    if cdc.len() > MAX_DB_NAME_LEN {
        return Err(ProvisionError::NameTooLong {
            name: cdc,
            max: MAX_DB_NAME_LEN,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANCE: &str = "k3m9x2p7";

    #[test]
    fn project_ids_accept_valid_names_and_refuse_invalid_or_reserved_names() {
        let max = "x".repeat(MAX_PROJECT_ID_LEN);
        for id in [
            "a",
            "acme",
            "acme-corp",
            "proj-1",
            "p9",
            "a-b-c",
            &max,
            "wamning",
            "wamnable",
        ] {
            assert!(validate_project_id(id).is_ok(), "{id:?}");
        }
        let too_long = "x".repeat(MAX_PROJECT_ID_LEN + 1);
        for id in [
            "",
            &too_long,
            "Acme",
            "under_score",
            "has.dot",
            "space bar",
            "sql;--",
            "tab\tx",
            "-lead",
            "trail-",
            "-",
            "9-",
            "a--b",
            "x---y",
            "a--b--c",
            "ab--",
            "--ab",
            "a_b",
        ] {
            assert!(
                matches!(
                    validate_project_id(id),
                    Err(ProvisionError::InvalidProjectId { .. })
                ),
                "{id:?}"
            );
        }
        for id in ["wamn", "wamn-db", "wamn-proj", "wamn-anything"] {
            assert!(
                matches!(
                    validate_project_id(id),
                    Err(ProvisionError::ReservedProjectId { .. })
                ),
                "{id:?}"
            );
        }
    }

    #[test]
    fn validate_project_env_slug_requires_a_bounded_name() {
        assert!(validate_project_env("acme", "billing", &"prod".parse().unwrap()).is_ok());
        for project in ["Bad", "x--p", "a_b"] {
            assert!(
                matches!(
                    validate_project_env("acme", project, &"dev".parse().unwrap()),
                    Err(ProvisionError::InvalidProjectId { .. })
                ),
                "{project}"
            );
        }
        assert!(matches!(
            validate_project_env("acme", "wamn-x", &"dev".parse().unwrap()),
            Err(ProvisionError::ReservedProjectId { .. })
        ));
        for org in ["Bad_Org", "a--x", "a_b"] {
            assert!(
                matches!(
                    validate_project_env(org, "billing", &"dev".parse().unwrap()),
                    Err(ProvisionError::InvalidComponent {
                        component: "org",
                        ..
                    })
                ),
                "{org}"
            );
        }

        // The assembled name must fit the Postgres / DNS-1123 63-byte limit.
        let long_org = "o".repeat(40);
        let long_proj = "p".repeat(40);
        let err =
            validate_project_env(&long_org, &long_proj, &"canary".parse().unwrap()).unwrap_err();
        assert!(matches!(err, ProvisionError::NameTooLong { max: 63, .. }));
        // 41 combined bytes — the longest triple the suffixed database admits.
        assert!(
            validate_project_env(&"o".repeat(20), &"p".repeat(17), &"prod".parse().unwrap())
                .is_ok()
        );
    }

    #[test]
    fn the_environment_namespace_is_a_valid_dns_1123_label() {
        // A Kubernetes namespace name is a DNS-1123 LABEL: at most 63 bytes,
        // `[a-z0-9-]`, starting and ending alphanumeric. The database's suffixed
        // 63-byte bound is tighter; take its longest admitted triple.
        let org = "o".repeat(37);
        assert!(validate_project_env(&org, "pp", &"de".parse().unwrap()).is_ok());
        let namespace = project_env_namespace(&org, "pp", &"de".parse().unwrap(), "0z9a8b7c");
        assert!(namespace.len() <= MAX_NAMESPACE_LEN);
        assert!(
            namespace
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "a DNS-1123 label admits only [a-z0-9-]: {namespace:?}"
        );
        assert!(
            !namespace.starts_with('-') && !namespace.ends_with('-'),
            "a DNS-1123 label starts and ends alphanumeric: {namespace:?}"
        );
        // The suffix decides the label's LAST byte, which is why a hyphen in it
        // is refused before it can name anything.
        assert!(validate_instance_suffix("k3m9x2p-").is_err());
    }

    #[test]
    fn instance_suffix_discipline_is_the_dns_label_tail() {
        let probe = "0".repeat(INSTANCE_SUFFIX_LEN);
        assert!(validate_instance_suffix("k3m9x2p7").is_ok());
        assert!(validate_instance_suffix(&probe).is_ok());
        for bad in [
            "",
            "k3m9x2p",
            "k3m9x2p78",
            "K3M9X2P7",
            "k3m9x2p-",
            "k3m9x2p_",
        ] {
            assert!(
                matches!(
                    validate_instance_suffix(bad),
                    Err(ProvisionError::InvalidComponent {
                        component: "instance",
                        ..
                    })
                ),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn both_global_name_bounds_account_for_the_instance_suffix() {
        // The database sits exactly on 63 bytes with 41 component bytes.
        let org = "o".repeat(37);
        let database = project_env_database_name(&org, "pp", &"de".parse().unwrap(), INSTANCE);
        assert_eq!(database.len(), MAX_DB_NAME_LEN);
        assert!(validate_project_env(&org, "pp", &"de".parse().unwrap()).is_ok());

        // One more component byte is refused only because the separator and
        // eight-byte instance suffix consume ten bytes of the identifier budget.
        let over_database = "o".repeat(38);
        assert!(
            wamn_registry::name::project_env_database_stem(
                &over_database,
                "pp",
                &"de".parse().unwrap()
            )
            .len()
                <= MAX_DB_NAME_LEN
        );
        assert_eq!(
            project_env_database_name(&over_database, "pp", &"de".parse().unwrap(), INSTANCE).len(),
            MAX_DB_NAME_LEN + 1
        );
        match validate_project_env(&over_database, "pp", &"de".parse().unwrap()) {
            Err(ProvisionError::NameTooLong { max, .. }) => {
                assert_eq!(max, MAX_DB_NAME_LEN);
            }
            other => panic!("suffixed database candidate was not refused: {other:?}"),
        }

        // The namespace formula independently budgets the same ten bytes.
        let org = "o".repeat(40);
        let stem = project_env_namespace_stem(&org, "pp", &"de".parse().unwrap());
        assert_eq!(stem.len(), MAX_NAMESPACE_STEM_LEN);
        assert_eq!(
            project_env_namespace(&org, "pp", &"de".parse().unwrap(), INSTANCE).len(),
            MAX_NAMESPACE_LEN
        );
    }

    #[test]
    fn derived_names_are_injective_over_valid_triples() {
        // Enumerate short slugs across the three components; every VALID triple
        // must map to a UNIQUE database name AND a unique CDC object name. The
        // alphabet deliberately includes `--` slugs — `("a","b--c")` and
        // `("a--b","c")` both derive `wamn-db-a--b--c--<env>` (and
        // `wamn_cdc_a__b__c__<env>`). With the `--` ban they are FILTERED out (so
        // injectivity holds over the survivors); remove the ban and both survive
        // and collide here — this test is also a mutation catcher for the ban.
        let slugs = ["a", "b", "a-b", "c", "a--b", "b--c"];
        let envs = ["dev", "prod", "c-d"];
        let mut by_db: std::collections::HashMap<String, (&str, &str, &str)> =
            std::collections::HashMap::new();
        let mut by_cdc: std::collections::HashMap<String, (&str, &str, &str)> =
            std::collections::HashMap::new();
        let mut valid = 0usize;
        for &org in &slugs {
            for &project in &slugs {
                for &env in &envs {
                    // Only VALID triples derive names; `--`/`_` slugs never reach here.
                    if validate_project_env_cdc(org, project, &env.parse().unwrap()).is_err() {
                        continue;
                    }
                    valid += 1;
                    let triple = (org, project, env);
                    let db =
                        project_env_database_name(org, project, &env.parse().unwrap(), INSTANCE);
                    if let Some(prev) = by_db.insert(db.clone(), triple) {
                        assert_eq!(prev, triple, "database name {db:?} collides two triples");
                    }
                    let cdc = cdc_object_name(org, project, &env.parse().unwrap(), INSTANCE);
                    if let Some(prev) = by_cdc.insert(cdc.clone(), triple) {
                        assert_eq!(prev, triple, "cdc object name {cdc:?} collides two triples");
                    }
                }
            }
        }
        assert!(valid > 0, "the fixture must include valid coordinates");
        assert_eq!(by_db.len(), valid, "database names are injective");
        assert_eq!(by_cdc.len(), valid, "cdc object names are injective");
    }

    #[test]
    fn validate_project_env_cdc_bounds_the_object_name() {
        assert!(validate_project_env_cdc("acme", "billing", &"prod".parse().unwrap()).is_ok());
        // The base project-env validation still applies (slug + reserved rules).
        assert!(matches!(
            validate_project_env_cdc("acme", "Bad", &"dev".parse().unwrap()),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        for (org_len, accepted) in [(36, true), (37, false)] {
            let org = "o".repeat(org_len);
            let result = validate_project_env_cdc(&org, "pp", &"de".parse().unwrap());
            if accepted {
                assert!(result.is_ok());
            } else {
                assert!(matches!(
                    result,
                    Err(ProvisionError::NameTooLong {
                        max: MAX_DB_NAME_LEN,
                        ..
                    })
                ));
            }
        }
    }
}
