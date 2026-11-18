//! Focused contracts of retained platform resources.

use serde_json::json;
use std::path::Path;

#[test]
fn operation_worker_bindings_use_their_own_accounts_and_namespace_resources() -> anyhow::Result<()>
{
    use crate::platform::operations;
    let document = wamn_registry::Triple::new(
        "acme".parse().expect("fixed identifier"),
        "dev".parse().unwrap(),
        "billing".parse().expect("fixed identifier"),
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cluster = "database-owner";
    let database = "recorded-environment-database";
    let capture_secret = wamn_registry::name::project_login_secret(database, wamn_role::CDC_LOGIN);
    let entry = crate::environment::entry(
        &repository.join("deploy/charts/project"),
        document.org.as_str(),
        "fixture",
        operations::values(&document, cluster, database, std::iter::empty())?,
    )?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    let mut identities = std::collections::BTreeSet::new();
    for object in &objects {
        assert!(
            identities.insert((
                object["apiVersion"].to_string(),
                object["kind"].to_string(),
                object["metadata"]["namespace"].to_string(),
                object["metadata"]["name"].to_string()
            )),
            "duplicate resource identity would replace its rules"
        );
    }
    for account in objects
        .iter()
        .filter(|object| object["kind"] == "ServiceAccount")
    {
        assert_eq!(account["metadata"]["namespace"], document.org.as_str());
        let name = account["metadata"]["name"].as_str().unwrap();
        let privileged = name.ends_with("-operation");
        let capture = wamn_provision::cdc_reader_name(
            &document.org,
            &document.project,
            &document.env,
        );
        for binding in objects
            .iter()
            .filter(|object| object["kind"] == "RoleBinding" && object["metadata"]["name"] == name)
        {
            assert_eq!(binding["subjects"][0]["name"], name);
            assert_eq!(
                binding["subjects"][0]["namespace"],
                account["metadata"]["namespace"]
            );
            let role = objects
                .iter()
                .find(|object| {
                    object["kind"] == "Role"
                        && object["metadata"]["name"] == binding["roleRef"]["name"]
                        && object["metadata"]["namespace"] == binding["metadata"]["namespace"]
                })
                .unwrap();
            assert_eq!(role["metadata"]["namespace"], document.org.as_str());
            if name != capture {
                assert!(role["rules"].as_array().unwrap().iter().any(|rule| {
                    rule["apiGroups"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|group| group == "")
                        && rule["resources"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|resource| resource == "pods/proxy")
                        && rule["verbs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|verb| verb == "get")
                }));
            }
            for rule in role["rules"].as_array().unwrap() {
                if name == capture {
                    assert!(
                        rule["resources"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|resource| resource == "secrets")
                    );
                    assert!(
                        rule["verbs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|verb| verb == "get")
                    );
                    let allowed = [&capture_secret, &format!("{cluster}-ca")];
                    assert!(
                        rule["resourceNames"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|resource| allowed
                                .iter()
                                .any(|name| resource.as_str() == Some(name.as_str())))
                    );
                    assert!(allowed.iter().all(|name| {
                        rule["resourceNames"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|resource| resource.as_str() == Some(name.as_str()))
                    }));
                }

                if role["metadata"]["namespace"] == document.org.as_str() {
                    let resources = rule["resources"].as_array().unwrap();
                    let verbs = rule["verbs"].as_array().unwrap();
                    if resources.iter().any(|resource| resource == "secrets") {
                        assert!(!verbs.iter().any(|verb| verb == "watch"));
                        if verbs.iter().any(|verb| verb == "patch") {
                            assert!(privileged, "only the project worker patches Secrets");
                        }
                    }
                }
                for field in ["resources", "verbs", "apiGroups"] {
                    assert!(
                        !rule[field]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|value| value == "*")
                    );
                }
                if rule["apiGroups"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|group| group == "postgresql.cnpg.io")
                {
                    assert!(privileged);
                    let target = if rule["resources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == "databases")
                    {
                        database
                    } else {
                        cluster
                    };
                    {
                        let observed = &(rule["resourceNames"]);
                        assert_eq!(observed.as_array().expect("array").len(), 1);
                        assert_eq!(observed[0], serde_json::json!(target));
                    };
                    assert!(
                        !rule["verbs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|verb| verb == "create" || verb == "delete")
                    );
                }
            }
        }
        assert_eq!(
            objects.iter().any(|object| object["kind"] == "Role"
                && object["metadata"]["name"] == name
                && object["rules"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|rule| rule["apiGroups"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == "postgresql.cnpg.io"))),
            privileged
        );
    }
    Ok(())
}

#[test]
fn colliding_coordinates_read_only_their_own_credentials() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let coordinates = [("a", "b-c", "d"), ("a", "b", "c-d")];
    let accounts = coordinates.map(|(org, _project, env)| {
        wamn_registry::name::environment_service_account(
            &org.parse().unwrap(),
            &env.parse().unwrap(),
        )
    });
    let secrets = coordinates.map(|(org, project, env)| {
        wamn_registry::credential_secret_name(
            &org.parse().unwrap(),
            &project.parse().unwrap(),
            &env.parse().unwrap(),
        )
    });
    assert_ne!(accounts[0], accounts[1]);
    assert_ne!(secrets[0], secrets[1]);
    for index in 0..coordinates.len() {
        let entry = crate::environment::entry(
            &repository.join("deploy/charts/environment"),
            coordinates[index].0,
            &accounts[index],
            json!({"credentialAccess":{"account":accounts[index],"secrets":[secrets[index]]}}),
        )?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let binding = objects
            .iter()
            .find(|object| object["kind"] == "RoleBinding")
            .unwrap();
        let role = objects
            .iter()
            .find(|object| {
                object["kind"] == "Role" && object["metadata"]["name"] == binding["roleRef"]["name"]
            })
            .unwrap();
        for subject in binding["subjects"].as_array().unwrap() {
            assert_eq!(subject["name"], accounts[index]);
            assert_eq!(subject["namespace"], role["metadata"]["namespace"]);
        }
        for rule in role["rules"].as_array().unwrap().iter().filter(|rule| {
            rule["resources"]
                .as_array()
                .is_some_and(|resources| resources.iter().any(|resource| resource == "secrets"))
        }) {
            let allowed = rule["resourceNames"].as_array().unwrap();
            assert!(allowed.iter().any(|name| name == &secrets[index]));
            assert!(!allowed.iter().any(|name| name == &secrets[1 - index]));
            assert!(
                rule["verbs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|verb| verb == "get")
            );
        }
    }
    Ok(())
}
