//! Focused contracts of retained platform resources.

use serde_json::json;
use std::path::Path;

#[test]
fn project_retains_only_the_operator_policy_for_each_cluster() -> anyhow::Result<()> {
    use wamn_environment::environment::chart_values::set_operation_values;
    use wamn_environment::platform_environment::database_values;
    use wamn_kubernetes::platform_environment::entry;
    use wamn_postgres::database::connection_for;
    use wamn_registry::name;

    let triple = wamn_registry::Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
    let release = name::environment_resource_name(&triple.org, &triple.project, &triple.env);
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/project");
    let mut names = std::collections::BTreeSet::new();
    for instance in ["abcdefgh", "ijklmnop"] {
        let connection = connection_for(&triple, &instance.parse()?);
        let cluster = name::project_cluster(&connection.database);
        let mut values = database_values(
            &connection,
            1,
            json!({"instances": 1, "storage": "2Gi", "cpu": "200m", "memory": "256Mi"}),
            &json!({"image": "registry.example/postgresql:test"}),
        )?;
        set_operation_values(
            &mut values,
            &triple,
            &cluster,
            &connection.database,
            std::iter::empty(),
        )?;
        let entry = entry(&chart, triple.org.as_str(), &release, values)?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let mut kept = objects.iter().filter(|object| {
            object["kind"] == "NetworkPolicy"
                && object["metadata"]["annotations"]["helm.sh/resource-policy"] == "keep"
        });
        let policy = kept.next().expect("operator policy must survive uninstall");
        assert!(
            kept.next().is_none(),
            "only the operator policy survives uninstall"
        );
        let policy_name = policy["metadata"]["name"].as_str().expect("policy name");
        assert_eq!(policy_name, format!("{cluster}-allow-cnpg-operator"));
        assert_eq!(
            policy["spec"]["podSelector"]["matchLabels"]["cnpg.io/cluster"],
            cluster
        );
        assert!(
            names.insert(policy_name.to_owned()),
            "policy name must be unique per Cluster"
        );
    }
    Ok(())
}

#[test]
fn operation_worker_bindings_use_their_own_accounts_and_namespace_resources() -> anyhow::Result<()>
{
    let document = wamn_registry::Triple::new(
        "acme".parse().expect("fixed identifier"),
        "dev".parse().unwrap(),
        "billing".parse().expect("fixed identifier"),
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cluster = "database-owner";
    let database = "recorded-environment-database";
    let application = "shop".parse()?;
    let mut values = json!({});
    wamn_environment::environment::chart_values::set_operation_values(
        &mut values,
        &document,
        cluster,
        database,
        std::iter::once(&application),
    )?;
    values["namespace"] = json!(document.org);
    let entry = wamn_kubernetes::platform_environment::entry(
        &repository.join("deploy/charts/project"),
        document.org.as_str(),
        "fixture",
        values,
    )?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    let values = &entry["values"]["operations"];
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
        let binding = objects
            .iter()
            .find(|object| object["kind"] == "RoleBinding" && object["metadata"]["name"] == name);
        if name == values["capture"] {
            assert!(binding.is_none());
            assert!(
                !objects.iter().any(|object| {
                    object["kind"] == "Role" && object["metadata"]["name"] == name
                })
            );
            continue;
        }
        let binding = binding.unwrap();
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

        assert_eq!(
            name,
            format!("{}-operation", values["name"].as_str().unwrap())
        );
        let allowed = json!({ "secrets": values["diffSecrets"], "workloaddeployments": values["diffWorkloads"] });
        let mut observed = std::collections::BTreeSet::new();
        for rule in role["rules"].as_array().unwrap() {
            assert_eq!(rule["verbs"], json!(["get"]));
            for resource in rule["resources"].as_array().unwrap() {
                let resource = resource.as_str().unwrap();
                let api = match resource {
                    "secrets" => "",
                    "workloaddeployments" => "runtime.wasmcloud.dev",
                    _ => panic!("unexpected {resource} access"),
                };
                assert_eq!(rule["apiGroups"], json!([api]));
                assert!(observed.insert(resource));
                assert_eq!(rule["resourceNames"], allowed[resource]);
            }
        }
        assert_eq!(
            observed,
            allowed
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect()
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
        let (org, project, env) = coordinates[index];
        let entry = wamn_kubernetes::platform_environment::entry(
            &repository.join("deploy/charts/environment"),
            org,
            &accounts[index],
            json!({
                "org":org, "env":env, "hostName":accounts[index], "serviceAccount":accounts[index],
                "image":format!("registry.example/wamn-host:test@sha256:{}", "a".repeat(64)),
                "drainBoundSeconds":60,
                "identityIssuer":"https://identity.platform.svc.cluster.local",
                "natsUrl":"nats://wamn-nats.platform.svc.cluster.local:4222",
                "environmentNats":{
                    "data":"data-nats", "native":"native-nats",
                    "workflow":{"secret":"workflow-nats", "inbox":"_INBOX_workflow"},
                    "ingress":{"secret":"ingress-nats", "inbox":"_INBOX_ingress"},
                    "subjects":"evt.fixture.>",
                },
                "projects":{(project):{
                    "host":"project-rw", "port":5432, "database":"project", "cluster":"project",
                    "logins":{"guest-sql":"project-app"}, "serverCa":"project-ca",
                    "credentialSecret":secrets[index], "stream":"EVT_FIXTURE",
                }},
            }),
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
