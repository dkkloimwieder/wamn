//! Provisioning validation and database names.

use crate::error::ProvisionError;
use crate::workload_role::WorkloadRoleFamily;

/// The shared role of generated application statements.
pub const APP_ROLE: &str = "wamn_app";
/// The owner of project databases and platform tables.
pub const DB_OWNER_ROLE: &str = "wamn_db_owner";
pub use wamn_registry::identifiers::MAX_PROJECT_ID_LEN;
pub use wamn_registry::name::{
    CDC_OBJECT_PREFIX, CDC_SECRET_PREFIX, DB_PREFIX, GUEST_SECRET_PREFIX, INSTANCE_SUFFIX_LEN,
    MAX_DB_NAME_LEN, MAX_NAMESPACE_LEN, MAX_NAMESPACE_STEM_LEN, NAMESPACE_PREFIX, cdc_object_name,
    control_administration_key, control_administration_secret_name, package_owner_role,
    project_env_cdc_secret_name, project_env_database_name, project_env_namespace,
    project_env_namespace_stem, project_env_secret_name,
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
    env: &str,
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
pub fn project_env_guest_secret_name(org: &str, project: &str, env: &str) -> String {
    workload_secret_name(WorkloadRoleFamily::App, org, project, env)
}

/// Refuse coordinates whose database or namespace name exceeds its limit.
pub fn validate_project_env(org: &str, project: &str, env: &str) -> Result<(), ProvisionError> {
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
    if let Some(reason) = slug_reason(env) {
        return Err(ProvisionError::InvalidComponent {
            component: "env",
            value: env.to_string(),
            reason,
        });
    }
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
pub fn validate_project_env_cdc(org: &str, project: &str, env: &str) -> Result<(), ProvisionError> {
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
    fn valid_project_ids_pass() {
        let max = "x".repeat(MAX_PROJECT_ID_LEN);
        for id in [
            "a",
            "acme",
            "acme-corp",
            "proj-1",
            "p9",
            "a-b-c",
            max.as_str(),
        ] {
            assert!(validate_project_id(id).is_ok(), "{id:?} should be valid");
        }
    }

    #[test]
    fn consecutive_hyphens_are_rejected() {
        // wamn-R27: `--` is the component separator in project_env_database_name
        // (and `__` in cdc_object_name), so an interior `--` run inside a slug is
        // an identity-collision vector and must be rejected. A SINGLE interior
        // hyphen stays valid.
        for bad in ["a--b", "x---y", "a--b--c", "ab--", "--ab"] {
            let err = validate_project_id(bad).unwrap_err();
            // "ab--"/"--ab" trip the start/end-alnum boundary first; the interior
            // cases trip the consecutive-hyphen rule — either way, rejected.
            assert!(
                matches!(err, ProvisionError::InvalidProjectId { .. }),
                "{bad:?} should be rejected"
            );
        }
        // The interior-run rule reports its own reason.
        assert!(matches!(
            validate_project_id("a--b"),
            Err(ProvisionError::InvalidProjectId {
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
        assert!(validate_project_id("a-b").is_ok());
    }

    #[test]
    fn invalid_project_ids_are_rejected_with_a_reason() {
        // empty / too long / charset / boundary
        assert!(matches!(
            validate_project_id(""),
            Err(ProvisionError::InvalidProjectId {
                reason: "empty",
                ..
            })
        ));
        assert!(matches!(
            validate_project_id(&"x".repeat(41)),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        for bad in [
            "Acme",
            "under_score",
            "has.dot",
            "space bar",
            "sql;--",
            "tab\tx",
        ] {
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::InvalidProjectId {
                        reason: "only lowercase letters, digits, and hyphens are allowed",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        for bad in ["-lead", "trail-", "-", "9-"] {
            // "9-" ends on a hyphen; "-lead"/"-" start on one.
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::InvalidProjectId {
                        reason: "must start and end with a lowercase letter or digit",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn reserved_wamn_prefix_is_rejected() {
        for bad in ["wamn", "wamn-db", "wamn-proj", "wamn-anything"] {
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::ReservedProjectId { .. })
                ),
                "{bad:?} should be reserved"
            );
        }
        // The boundary is a hyphen: `wamn` + non-hyphen is a normal project.
        assert!(validate_project_id("wamning").is_ok());
        assert!(validate_project_id("wamnable").is_ok());
    }

    #[test]
    fn the_registry_and_provisioning_decide_a_project_id_alike() {
        // wamn-0h0g.9.16: one rule, two readers. The registry owns the rule and
        // this crate translates it, so neither side can accept what the other
        // refuses. Before the move the registry took 64 bytes of
        // [A-Za-z0-9_-] and this crate took 40 bytes of [a-z0-9-].
        let at_cap = "x".repeat(MAX_PROJECT_ID_LEN);
        let over_cap = "x".repeat(MAX_PROJECT_ID_LEN + 1);
        for id in [
            "a",
            "acme",
            "acme-corp",
            at_cap.as_str(),
            "",
            "Acme",
            "under_score",
            "-lead",
            "trail-",
            "a--b",
            over_cap.as_str(),
            "wamn",
            "wamn-db",
            "wamning",
        ] {
            assert_eq!(
                wamn_registry::identifiers::valid_project(id),
                validate_project_id(id).is_ok(),
                "the two sides disagree about {id:?}"
            );
        }
    }

    #[test]
    fn project_env_names_encode_the_full_triple() {
        // The org is encoded (unlike the 2.3 project-only name) so identically
        // named projects across orgs never collide on the shared T3 pool.

        // canary and prod are distinct names (they co-reside on <org>-prod, so the
        // env MUST be in the name to keep them apart).
        assert_ne!(
            project_env_database_name("acme", "billing", "canary", INSTANCE),
            project_env_database_name("acme", "billing", "prod", INSTANCE)
        );
        // Two orgs, same project+env → different db names (the pool collision fix).
        assert_ne!(
            project_env_database_name("org-a", "demo", "dev", INSTANCE),
            project_env_database_name("org-b", "demo", "dev", INSTANCE)
        );
        // The namespace-targeted Secret keeps the stable triple-only lookup key.
        assert_ne!(
            project_env_secret_name("acme", "billing", "dev"),
            project_env_database_name("acme", "billing", "dev", INSTANCE)
        );
    }

    #[test]
    fn validate_project_env_slug_requires_a_bounded_name() {
        assert!(validate_project_env("acme", "billing", "prod").is_ok());
        // A bad project id is rejected (the reason flows from validate_project_id).
        assert!(matches!(
            validate_project_env("acme", "Bad", "dev"),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        assert!(matches!(
            validate_project_env("acme", "wamn-x", "dev"),
            Err(ProvisionError::ReservedProjectId { .. })
        ));
        // wamn-R27: the org and env are slug-validated too (not only the project).
        assert!(matches!(
            validate_project_env("Bad_Org", "billing", "dev"),
            Err(ProvisionError::InvalidComponent {
                component: "org",
                ..
            })
        ));
        assert!(matches!(
            validate_project_env("acme", "billing", "Bad_Env"),
            Err(ProvisionError::InvalidComponent {
                component: "env",
                ..
            })
        ));
        // The assembled name must fit the Postgres / DNS-1123 63-byte limit.
        let long_org = "o".repeat(40);
        let long_proj = "p".repeat(40);
        let err = validate_project_env(&long_org, &long_proj, "canary").unwrap_err();
        assert!(matches!(err, ProvisionError::NameTooLong { max: 63, .. }));
        // 41 combined bytes — the longest triple the suffixed database admits.
        assert!(validate_project_env(&"o".repeat(20), &"p".repeat(17), "prod").is_ok());
    }

    #[test]
    fn the_environment_namespace_carries_the_minted_instance_suffix() {
        // One environment = one namespace (wamn-0h0g.15.3): the same `--`
        // separated triple the database name carries, under the `wamn-` prefix,
        // plus the provision-minted instance suffix (wamn-0h0g.13.57).

        // canary and prod are distinct namespaces, as they are distinct databases.
        assert_ne!(
            project_env_namespace("acme", "billing", "canary", "k3m9x2p7"),
            project_env_namespace("acme", "billing", "prod", "k3m9x2p7")
        );
        // THE POINT OF THE SUFFIX: the SAME triple provisioned a second time
        // lands in a DIFFERENT namespace, so a recreated environment cannot
        // inherit whatever survived the deleted one. Drop the suffix from the
        // derivation and these two collide.
        assert_ne!(
            project_env_namespace("acme", "billing", "prod", "k3m9x2p7"),
            project_env_namespace("acme", "billing", "prod", "q80zdw41")
        );
    }

    #[test]
    fn the_environment_namespace_is_a_valid_dns_1123_label() {
        // A Kubernetes namespace name is a DNS-1123 LABEL: at most 63 bytes,
        // `[a-z0-9-]`, starting and ending alphanumeric. The database's suffixed
        // 63-byte bound is tighter; take its longest admitted triple.
        let org = "o".repeat(37);
        assert!(validate_project_env(&org, "pp", "de").is_ok());
        let namespace = project_env_namespace(&org, "pp", "de", "0z9a8b7c");
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
        // The two rejection reasons are distinct and stable.
        assert!(matches!(
            validate_instance_suffix("short"),
            Err(ProvisionError::InvalidComponent {
                reason: "must be exactly 8 bytes",
                ..
            })
        ));
        assert!(matches!(
            validate_instance_suffix("k3m9x2p-"),
            Err(ProvisionError::InvalidComponent {
                reason: "only lowercase letters and digits are allowed",
                ..
            })
        ));
    }

    #[test]
    fn both_global_name_bounds_account_for_the_instance_suffix() {
        // The database sits exactly on 63 bytes with 41 component bytes.
        let org = "o".repeat(37);
        let database = project_env_database_name(&org, "pp", "de", INSTANCE);
        assert_eq!(database.len(), MAX_DB_NAME_LEN);
        assert!(validate_project_env(&org, "pp", "de").is_ok());

        // One more component byte is refused only because the separator and
        // eight-byte instance suffix consume ten bytes of the identifier budget.
        let over_database = "o".repeat(38);
        assert!(
            wamn_registry::name::project_env_database_stem(&over_database, "pp", "de").len()
                <= MAX_DB_NAME_LEN
        );
        assert_eq!(
            project_env_database_name(&over_database, "pp", "de", INSTANCE).len(),
            MAX_DB_NAME_LEN + 1
        );
        match validate_project_env(&over_database, "pp", "de") {
            Err(ProvisionError::NameTooLong { name, max }) => {
                let probe = "0".repeat(INSTANCE_SUFFIX_LEN);
                assert_eq!(
                    name,
                    project_env_database_name(&over_database, "pp", "de", &probe)
                );
                assert_eq!(max, MAX_DB_NAME_LEN);
            }
            other => panic!("suffixed database candidate was not refused: {other:?}"),
        }

        // The namespace formula independently budgets the same ten bytes.
        let org = "o".repeat(40);
        let stem = project_env_namespace_stem(&org, "pp", "de");
        assert_eq!(stem.len(), MAX_NAMESPACE_STEM_LEN);
        assert_eq!(
            project_env_namespace(&org, "pp", "de", INSTANCE).len(),
            MAX_NAMESPACE_LEN
        );
    }

    #[test]
    fn cdc_object_bound_accounts_for_the_instance_suffix() {
        // At the database limit the one-byte-longer CDC prefix crosses 63 only
        // after its `__` separator plus eight-byte suffix are included.
        let org = "o".repeat(37);
        assert_eq!(
            project_env_database_name(&org, "pp", "de", INSTANCE).len(),
            MAX_DB_NAME_LEN
        );
        assert_eq!(
            cdc_object_name(&org, "pp", "de", INSTANCE).len(),
            MAX_DB_NAME_LEN + 1
        );
        match validate_project_env_cdc(&org, "pp", "de") {
            Err(ProvisionError::NameTooLong { name, max }) => {
                let probe = "0".repeat(INSTANCE_SUFFIX_LEN);
                assert_eq!(name, cdc_object_name(&org, "pp", "de", &probe));
                assert_eq!(max, MAX_DB_NAME_LEN);
            }
            other => panic!("suffixed CDC candidate was not refused: {other:?}"),
        }
    }

    #[test]
    fn double_hyphen_collision_pair_is_rejected_in_every_component() {
        // The wamn-R27 collision: `(a, x--p, dev)` and `(a--x, p, dev)` both
        // derive `wamn-db-a--x--p--dev` (and `wamn_cdc_a__x__p__dev`). The
        // derivations still alias — that is why the VALIDATOR is the fix: both
        // triples are now rejected before they can be provisioned.
        // Project carries the `--`: rejected via validate_project_id.
        assert!(matches!(
            validate_project_env("a", "x--p", "dev"),
            Err(ProvisionError::InvalidProjectId {
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
        // Org carries the `--`: rejected as the org component.
        assert!(matches!(
            validate_project_env("a--x", "p", "dev"),
            Err(ProvisionError::InvalidComponent {
                component: "org",
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
        // Env carries the `--`: rejected as the env component.
        assert!(matches!(
            validate_project_env("a", "p", "d--v"),
            Err(ProvisionError::InvalidComponent {
                component: "env",
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
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
                    if validate_project_env_cdc(org, project, env).is_err() {
                        continue;
                    }
                    valid += 1;
                    let triple = (org, project, env);
                    let db = project_env_database_name(org, project, env, INSTANCE);
                    if let Some(prev) = by_db.insert(db.clone(), triple) {
                        assert_eq!(prev, triple, "database name {db:?} collides two triples");
                    }
                    let cdc = cdc_object_name(org, project, env, INSTANCE);
                    if let Some(prev) = by_cdc.insert(cdc.clone(), triple) {
                        assert_eq!(prev, triple, "cdc object name {cdc:?} collides two triples");
                    }
                }
            }
        }
        // The `--` slugs (2 of 6 org/project values) are filtered by the ban, so
        // exactly the 4 hyphen-run-free slugs survive per id position × 3 envs.
        // The count being BELOW the full product shows the filter actually ran.
        assert!(
            valid < slugs.len() * slugs.len() * envs.len(),
            "the `--` filter ran"
        );
        assert_eq!(
            valid,
            4 * 4 * 3,
            "4 valid org × 4 valid project × 3 valid env"
        );
        assert_eq!(by_db.len(), valid, "database names are injective");
        assert_eq!(by_cdc.len(), valid, "cdc object names are injective");
    }

    #[test]
    fn underscore_is_not_a_second_collision_vector() {
        // cdc_object_name maps `-`→`_`; a `_` INSIDE a slug would be a second way
        // to forge the `__` separator. The slug charset admits no underscore, so
        // the vector is closed at validation — confirm it here.
        assert!(matches!(
            validate_project_id("a_b"),
            Err(ProvisionError::InvalidProjectId {
                reason: "only lowercase letters, digits, and hyphens are allowed",
                ..
            })
        ));
        assert!(matches!(
            validate_project_env("a_b", "p", "dev"),
            Err(ProvisionError::InvalidComponent {
                component: "org",
                ..
            })
        ));
        assert!(matches!(
            validate_project_env("a", "p", "d_v"),
            Err(ProvisionError::InvalidComponent {
                component: "env",
                ..
            })
        ));
    }

    #[test]
    fn cdc_names_encode_the_triple_in_both_domains() {
        // The Secret keeps the hyphenated K8s convention…

        // …while the Postgres objects (slot charset `[a-z0-9_]`) are underscored,
        // slug hyphens mapped to `_`, `__` as the separator.

        // A hyphen inside a slug maps to a SINGLE `_`, the separator is DOUBLE —
        // ("org-a","demo") and ("org","a-demo") stay distinct.
        assert_ne!(
            cdc_object_name("org-a", "demo", "dev", INSTANCE),
            cdc_object_name("org", "a-demo", "dev", INSTANCE)
        );
    }

    #[test]
    fn validate_project_env_cdc_bounds_the_object_name() {
        assert!(validate_project_env_cdc("acme", "billing", "prod").is_ok());
        // The base project-env validation still applies (slug + reserved rules).
        assert!(matches!(
            validate_project_env_cdc("acme", "Bad", "dev"),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        let org_ok = "o".repeat(36);
        assert!(validate_project_env_cdc(&org_ok, "pp", "de").is_ok());
        assert!(cdc_object_name(&org_ok, "pp", "de", INSTANCE).len() <= MAX_DB_NAME_LEN);
    }
}
