//! Provisioning validation and database names.

use crate::error::ProvisionError;

/// Refuse coordinates whose database or namespace name exceeds its limit.
pub fn validate_project_env(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), ProvisionError> {
    wamn_platform_install::env::validate_project_env(org, project, env).map_err(|error| {
        ProvisionError::NameTooLong {
            name: error.name,
            max: error.max,
        }
    })
}

/// Refuse coordinates whose CDC object name exceeds the database limit.
pub fn validate_project_env_cdc(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), ProvisionError> {
    wamn_platform_install::env::validate_project_env_cdc(org, project, env).map_err(|error| {
        ProvisionError::NameTooLong {
            name: error.name,
            max: error.max,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_registry::name::{
        MAX_DB_NAME_LEN, MAX_NAMESPACE_STEM_LEN, cdc_object_name, project_env_database_name,
    };

    const INSTANCE: &str = "k3m9x2p7";

    #[test]
    fn project_coordinates_require_a_bounded_resource_name() {
        assert!(
            validate_project_env(
                &"acme".parse().unwrap(),
                &"billing".parse().unwrap(),
                &"prod".parse().unwrap()
            )
            .is_ok()
        );
        assert!(
            validate_project_env(
                &"o".repeat(40).parse().unwrap(),
                &"p".repeat(40).parse().unwrap(),
                &"canary".parse().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn both_global_name_bounds_account_for_the_instance_suffix() {
        // The database sits exactly on 63 bytes with 41 component bytes.
        let org = "o".repeat(37);
        let database = project_env_database_name(
            &org.parse().unwrap(),
            &"pp".parse().unwrap(),
            &"de".parse().unwrap(),
            &INSTANCE.parse().unwrap(),
        );
        assert_eq!(database.len(), MAX_DB_NAME_LEN);
        assert!(
            validate_project_env(
                &org.parse().unwrap(),
                &"pp".parse().unwrap(),
                &"de".parse().unwrap()
            )
            .is_ok()
        );

        // One more component byte is refused only because the separator and
        // eight-byte instance suffix consume ten bytes of the identifier budget.
        let over_database = "o".repeat(38);
        assert!(
            wamn_registry::name::project_env_database_stem(
                &over_database.parse().unwrap(),
                &"pp".parse().unwrap(),
                &"de".parse().unwrap()
            )
            .len()
                <= MAX_DB_NAME_LEN
        );
        assert_eq!(
            project_env_database_name(
                &over_database.parse().unwrap(),
                &"pp".parse().unwrap(),
                &"de".parse().unwrap(),
                &INSTANCE.parse().unwrap()
            )
            .len(),
            MAX_DB_NAME_LEN + 1
        );
        match validate_project_env(
            &over_database.parse().unwrap(),
            &"pp".parse().unwrap(),
            &"de".parse().unwrap(),
        ) {
            Err(ProvisionError::NameTooLong { max, .. }) => {
                assert_eq!(max, MAX_DB_NAME_LEN);
            }
            other => panic!("suffixed database candidate was not refused: {other:?}"),
        }

        // The namespace stem limit reserves the same ten bytes.
        let org = "o".repeat(40);
        let stem = wamn_registry::name::project_env_namespace_stem(
            &org.parse().unwrap(),
            &"pp".parse().unwrap(),
            &"de".parse().unwrap(),
        );
        assert_eq!(stem.len(), MAX_NAMESPACE_STEM_LEN);
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
                    let (Ok(parsed_org), Ok(parsed_project)) = (
                        org.parse::<wamn_registry::OrgId>(),
                        project.parse::<wamn_registry::ProjectId>(),
                    ) else {
                        continue;
                    };
                    // Only VALID triples derive names; `--`/`_` slugs never reach here.
                    if validate_project_env_cdc(&parsed_org, &parsed_project, &env.parse().unwrap())
                        .is_err()
                    {
                        continue;
                    }
                    valid += 1;
                    let triple = (org, project, env);
                    let db = project_env_database_name(
                        &parsed_org,
                        &parsed_project,
                        &env.parse().unwrap(),
                        &INSTANCE.parse().unwrap(),
                    );
                    if let Some(prev) = by_db.insert(db.clone(), triple) {
                        assert_eq!(prev, triple, "database name {db:?} collides two triples");
                    }
                    let cdc = cdc_object_name(
                        &parsed_org,
                        &parsed_project,
                        &env.parse().unwrap(),
                        &INSTANCE.parse().unwrap(),
                    );
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
        assert!(
            validate_project_env_cdc(
                &"acme".parse().unwrap(),
                &"billing".parse().unwrap(),
                &"prod".parse().unwrap()
            )
            .is_ok()
        );
        for (org_len, accepted) in [(36, true), (37, false)] {
            let org = "o".repeat(org_len);
            let result = validate_project_env_cdc(
                &org.parse().unwrap(),
                &"pp".parse().unwrap(),
                &"de".parse().unwrap(),
            );
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
