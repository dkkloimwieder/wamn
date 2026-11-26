//! Focused contracts of retained platform resources.

use serde_json::json;
use std::path::Path;

#[path = "support/pod_resources.rs"]
mod pod_resources;
#[path = "support/pod_security.rs"]
mod pod_security;

#[test]
fn platform_and_infrastructure_pods_declare_resources() -> anyhow::Result<()> {
    use wamn_kubernetes::{helm, platform_env::entry};

    let charts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts");
    for provider in ["kind", "gcp"] {
        let values = charts.join(format!("platform/values-{provider}.yaml"));
        let values = std::fs::read(values)?;
        let platform = helm::template(&entry(
            &charts.join("platform"),
            "platform",
            "wamn",
            serde_yaml::from_slice(&values)?,
        )?)?;
        pod_resources::assert_pod_resources(&platform);
        for cluster in platform.iter().filter(|object| object["kind"] == "Cluster") {
            pod_resources::assert_compute_resources(
                &cluster["spec"]["resources"],
                cluster["metadata"]["name"].as_str().expect("Cluster name"),
            );
        }
        let infrastructure = helm::template(&entry(
            &charts.join("infrastructure"),
            "platform",
            "wamn-infrastructure",
            serde_yaml::from_slice(&values)?,
        )?)?;
        pod_resources::assert_pod_resources(&infrastructure);
    }
    Ok(())
}

#[test]
fn system_poolers_leave_postgres_headroom_and_bound_queueing() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use wamn_kubernetes::{helm, platform_env::entry};

    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/platform");
    for file in ["values-kind.yaml", "values-gcp.yaml"] {
        let values = serde_yaml::from_slice(&std::fs::read(chart.join(file))?)?;
        let objects = helm::template(&entry(&chart, "platform", "wamn", values)?)?;
        let cluster = objects
            .iter()
            .find(|object| object["kind"] == "Cluster")
            .context("system database Cluster")?;
        let maximum: u64 = cluster["spec"]["postgresql"]["parameters"]["max_connections"]
            .as_str()
            .context("fixed PostgreSQL connection budget")?
            .parse()?;
        let mut backends = 0;
        for pooler in objects.iter().filter(|object| {
            object["kind"] == "Pooler"
                && object["spec"]["cluster"]["name"] == cluster["metadata"]["name"]
        }) {
            assert_eq!(pooler["spec"]["deploymentStrategy"]["type"], "Recreate");
            let instances = pooler["spec"]["instances"]
                .as_u64()
                .context("pooler replicas")?;
            let pgbouncer = &pooler["spec"]["pgbouncer"];
            let setting = |key: &str| -> anyhow::Result<u64> {
                Ok(pgbouncer["parameters"][key]
                    .as_str()
                    .context(key.to_owned())?
                    .parse()?)
            };
            let cap = setting("max_db_connections")?;
            let auth = cap.min(setting("default_pool_size")?);
            backends += instances * (cap + auth);
            assert!(pgbouncer["parameters"]["max_user_connections"].is_null());
            let wait = setting("query_wait_timeout")?;
            assert!(setting("client_login_timeout")? >= wait);
            assert!((1..wait).contains(&setting("server_idle_timeout")?));
            if pgbouncer["poolMode"] == "transaction" {
                assert!(setting("max_prepared_statements")? > 0);
            }
        }
        assert!(backends > 0 && backends < maximum);
    }
    Ok(())
}

#[test]
fn platform_wamn_workloads_render_restricted_pods() -> anyhow::Result<()> {
    use wamn_kubernetes::{helm, platform_env::entry};

    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/platform");
    for file in ["values-kind.yaml", "values-gcp.yaml"] {
        let values: serde_json::Value = serde_yaml::from_slice(&std::fs::read(chart.join(file))?)?;
        let registry = values["registry"].as_str().expect("platform registry");
        let wamn_images = format!("{registry}/wamn-");
        let objects = helm::template(&entry(&chart, "platform", "wamn", values)?)?;
        let hosts = objects
            .iter()
            .find(|object| object["kind"] == "Namespace" && object["metadata"]["name"] == "hosts")
            .expect("the platform chart must create the hosts namespace");
        assert_restricted_namespace(hosts);
        pod_security::assert_restricted_pods(objects.iter().filter(|object| {
            object["spec"]["template"]["spec"]["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|container| {
                    container["image"]
                        .as_str()
                        .is_some_and(|image| image.starts_with(&wamn_images))
                })
        }));
    }
    Ok(())
}

fn assert_restricted_namespace(namespace: &serde_json::Value) {
    let labels = &namespace["metadata"]["labels"];
    for policy in ["enforce", "warn"] {
        assert_eq!(
            labels[format!("pod-security.kubernetes.io/{policy}")],
            "restricted",
            "Namespace/{} pod security {policy}",
            namespace["metadata"]["name"]
        );
    }
}

#[test]
fn the_org_chart_creates_a_restricted_namespace() -> anyhow::Result<()> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/org");
    let values: serde_json::Value =
        serde_json::from_str(include_str!("support/org-chart-values.json"))?;
    let org = values["org"].as_str().expect("org chart value").to_owned();
    let entry = wamn_kubernetes::platform_env::entry(&chart, &org, &org, values)?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    let namespace = objects
        .iter()
        .find(|object| object["kind"] == "Namespace" && object["metadata"]["name"] == org)
        .expect("the org chart must create the org namespace");
    assert_restricted_namespace(namespace);
    Ok(())
}

#[test]
fn platform_ingress_and_edge_egress_match_the_provider_boundary() -> anyhow::Result<()> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/platform");
    let cidr = "172.18.0.3/32";
    let api_port = 6443;
    for provider in ["kind", "gcp"] {
        let file = std::fs::read(chart.join(format!("values-{provider}.yaml")))?;
        let mut values: serde_json::Value = serde_yaml::from_slice(&file)?;
        values["apiServer"] = json!([{"cidr": cidr, "port": api_port}]);
        let entry = wamn_kubernetes::platform_env::entry(&chart, "platform", "wamn", values)?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let find = |kind, name| {
            objects
                .iter()
                .find(|object| object["kind"] == kind && object["metadata"]["name"] == name)
                .unwrap_or_else(|| panic!("missing rendered {kind}/{name} on {provider}"))
        };
        let policies = objects
            .iter()
            .filter(|object| object["kind"] == "NetworkPolicy");
        let platform = policies
            .clone()
            .filter(|policy| policy["metadata"]["namespace"] == "platform");
        let deny: Vec<_> = platform
            .clone()
            .filter(|policy| policy["spec"]["podSelector"] == json!({}))
            .collect();
        let [deny] = deny.as_slice() else {
            panic!("expected one platform default-deny on {provider}");
        };
        assert_eq!(deny["spec"]["policyTypes"], json!(["Ingress"]));
        assert!(deny["spec"]["ingress"].as_array().is_none_or(Vec::is_empty));
        let operator = &find("Deployment", "runtime-operator")["spec"]["selector"];
        assert!(
            !platform
                .clone()
                .any(|policy| policy["spec"]["podSelector"] == *operator)
        );

        let edge = find("Deployment", "wamn-edge");
        let edge_namespace = &edge["metadata"]["namespace"];
        let edge_name = &edge["spec"]["template"]["metadata"]["labels"]["app.kubernetes.io/name"];
        let identity = find("Deployment", "identity");
        let password = identity["spec"]["template"]["spec"]["containers"][0]["ports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|port| port["name"] == "password")
            .unwrap();
        let password_port = &password["containerPort"];
        let identity_ingress = find("NetworkPolicy", "allow-identity-clients")["spec"]["ingress"]
            .as_array()
            .unwrap();
        let mut password_rules = identity_ingress.iter().filter(|rule| {
            rule["ports"]
                .as_array()
                .unwrap()
                .iter()
                .any(|port| port["port"] == *password_port)
        });
        assert_eq!(
            password_rules.next().expect("password ingress")["from"],
            json!([{
                "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": edge_namespace}},
                "podSelector": {"matchLabels": {"app.kubernetes.io/name": edge_name}}
            }])
        );
        assert!(
            password_rules.next().is_none(),
            "only the edge reaches the password listener"
        );

        let edge_egress: Vec<_> = policies
            .filter(|policy| {
                policy["metadata"]["namespace"] == *edge_namespace
                    && policy["spec"]["podSelector"] == edge["spec"]["selector"]
                    && policy["spec"]["policyTypes"] == json!(["Egress"])
            })
            .collect();
        if provider == "gcp" {
            assert!(edge_egress.is_empty());
            continue;
        }
        let [egress] = edge_egress.as_slice() else {
            panic!("expected one kind edge egress policy");
        };
        let rules = egress["spec"]["egress"].as_array().unwrap();
        assert!(
            rules
                .iter()
                .any(|rule| rule["to"] == json!([{"ipBlock": {"cidr": cidr}}])
                    && rule["ports"] == json!([{"port": api_port, "protocol": "TCP"}])),
            "kind edge must reach the Kubernetes API"
        );
    }
    Ok(())
}

#[test]
fn web_and_owner_invitation_policies_use_the_control_pod_port() -> anyhow::Result<()> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/org");
    let values = serde_json::from_str(include_str!("support/org-chart-values.json"))?;
    let entry = wamn_kubernetes::platform_env::entry(&chart, "acme", "acme", values)?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    let control = &objects
        .iter()
        .find(|object| object["kind"] == "Deployment")
        .expect("control Deployment")["spec"];
    let control_selector = &control["selector"];
    let pod_port = &control["template"]["spec"]["containers"][0]["ports"][0]["containerPort"];
    let invitation = json!({"matchLabels": {"app.kubernetes.io/component": "wamn-org-invitation"}});
    let web = json!({"matchLabels": {"app.kubernetes.io/name": "wamn-edge"}});
    let expected_ports = json!([{"port": pod_port, "protocol": "TCP"}]);
    for (subject, direction, peers, peer) in [
        (control_selector, "ingress", "from", &web),
        (control_selector, "ingress", "from", &invitation),
        (&invitation, "egress", "to", control_selector),
    ] {
        let mut rules = objects
            .iter()
            .filter(|object| {
                object["kind"] == "NetworkPolicy" && object["spec"]["podSelector"] == *subject
            })
            .filter_map(|object| object["spec"][direction].as_array())
            .flatten()
            .filter(|rule| {
                rule[peers]
                    .as_array()
                    .is_some_and(|peers| peers.iter().any(|entry| entry["podSelector"] == *peer))
            })
            .peekable();
        assert!(
            rules.peek().is_some(),
            "missing {direction} rule for {peer}"
        );
        for rule in rules {
            assert_eq!(rule["ports"], expected_ports, "{direction} rule for {peer}");
        }
    }
    Ok(())
}

#[test]
fn nats_file_store_and_identity_share_the_platform_byte_budget() -> anyhow::Result<()> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/platform");
    for provider in ["kind", "gcp"] {
        let values = serde_yaml::from_slice(&std::fs::read(
            chart.join(format!("values-{provider}.yaml")),
        )?)?;
        let entry = wamn_kubernetes::platform_env::entry(&chart, "platform", "wamn", values)?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let config = |name: &str, key: &str| -> anyhow::Result<&str> {
            objects
                .iter()
                .find(|object| object["kind"] == "ConfigMap" && object["metadata"]["name"] == name)
                .and_then(|object| object["data"][key].as_str())
                .ok_or_else(|| anyhow::anyhow!("{provider} must render {name}/{key}"))
        };
        let broker_bytes: u64 = config("wamn-nats-config", "nats.conf")?
            .lines()
            .find_map(|line| line.trim().strip_prefix("\"max_file_store\": "))
            .ok_or_else(|| anyhow::anyhow!("{provider} must configure JetStream file storage"))?
            .trim_end_matches(',')
            .parse()?;
        let identity: serde_json::Value =
            serde_json::from_str(config("identity-settings", "settings.json")?)?;
        assert_eq!(broker_bytes, 16 * 1024 * 1024 * 1024, "{provider}");
        assert_eq!(
            identity["nats"]["file_store_bytes"], broker_bytes,
            "{provider}"
        );

        let statefulset = objects
            .iter()
            .find(|object| {
                object["kind"] == "StatefulSet" && object["metadata"]["name"] == "wamn-nats"
            })
            .ok_or_else(|| anyhow::anyhow!("{provider} must render the broker"))?;
        let volume = statefulset["spec"]["template"]["spec"]["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|container| container["volumeMounts"].as_array().into_iter().flatten())
            .find(|mount| mount["mountPath"] == "/data")
            .ok_or_else(|| anyhow::anyhow!("{provider} must mount the file store"))?;
        let claim = statefulset["spec"]["volumeClaimTemplates"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|claim| claim["metadata"]["name"] == volume["name"])
            .ok_or_else(|| anyhow::anyhow!("{provider} must provide a file store claim"))?;
        assert_eq!(
            claim["spec"]["resources"]["requests"]["storage"], "20Gi",
            "{provider}"
        );
    }
    Ok(())
}

#[test]
fn nats_and_the_collector_use_restricted_pod_contexts() -> anyhow::Result<()> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/platform");
    for provider in ["kind", "gcp"] {
        let values = serde_yaml::from_slice(&std::fs::read(
            chart.join(format!("values-{provider}.yaml")),
        )?)?;
        let entry = wamn_kubernetes::platform_env::entry(&chart, "platform", "wamn", values)?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        for (kind, name) in [
            ("StatefulSet", "wamn-nats"),
            ("Deployment", "otel-collector"),
        ] {
            let workload = objects
                .iter()
                .find(|object| object["kind"] == kind && object["metadata"]["name"] == name)
                .unwrap_or_else(|| panic!("{provider} must render {kind} {name}"));
            let pod = &workload["spec"]["template"]["spec"];
            let security = &pod["securityContext"];
            let uid = security["runAsUser"].as_u64().filter(|&uid| uid > 0);
            assert!(uid.is_some(), "{provider}/{name} must set a non-root UID");
            assert_eq!(security["runAsNonRoot"], true, "{provider}/{name}");
            assert_eq!(
                security["seccompProfile"]["type"], "RuntimeDefault",
                "{provider}/{name}"
            );
            assert!(
                security["fsGroup"].as_u64().is_some_and(|group| group > 0),
                "{provider}/{name} must set a non-root volume group"
            );
            for volume in pod["volumes"].as_array().into_iter().flatten() {
                assert!(volume["hostPath"].is_null(), "{provider}/{name}: {volume}");
            }

            for field in ["containers", "initContainers"] {
                for container in pod[field].as_array().into_iter().flatten() {
                    let container_name = &container["name"];
                    let security = &container["securityContext"];
                    let location = format!("{provider}/{name}/{container_name}");
                    assert_eq!(security["allowPrivilegeEscalation"], false, "{location}");
                    assert!(
                        security["capabilities"]["drop"]
                            .as_array()
                            .is_some_and(|drop| drop.iter().any(|capability| capability == "ALL")),
                        "{location} must drop all capabilities"
                    );
                    let effective_uid = security["runAsUser"].as_u64().or(uid);
                    assert_ne!(effective_uid, Some(0), "{location} must run as non-root");
                    if name == "wamn-nats"
                        && container["volumeMounts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|mount| mount["name"] == "pid")
                    {
                        assert_eq!(effective_uid, uid, "{location} shares the NATS PID volume");
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn project_retains_only_the_operator_policy_for_each_cluster() -> anyhow::Result<()> {
    use wamn_env::env::chart_values::set_operation_values;
    use wamn_env::platform_env::database_values;
    use wamn_kubernetes::platform_env::entry;
    use wamn_postgres::database::connection_for;
    use wamn_registry::name;

    let triple = wamn_registry::Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
    let release = name::project_env_resource_name(&triple.org, &triple.project, &triple.env);
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
    let app = "shop".parse()?;
    let mut values = json!({});
    wamn_env::env::chart_values::set_operation_values(
        &mut values,
        &document,
        cluster,
        database,
        std::iter::once(&app),
    )?;
    values["namespace"] = json!(document.org);
    let entry = wamn_kubernetes::platform_env::entry(
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
        let allowed = json!({
            "secrets": values["diffSecrets"],
            "configmaps": values["diffConfigMaps"],
            "workloaddeployments": values["diffWorkloads"],
        });
        let mut observed = std::collections::BTreeSet::new();
        for rule in role["rules"].as_array().unwrap() {
            assert_eq!(rule["verbs"], json!(["get"]));
            for resource in rule["resources"].as_array().unwrap() {
                let resource = resource.as_str().unwrap();
                let api = match resource {
                    "secrets" | "configmaps" => "",
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
        wamn_registry::name::env_service_account(&org.parse().unwrap(), &env.parse().unwrap())
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
        let entry = wamn_kubernetes::platform_env::entry(
            &repository.join("deploy/charts/env"),
            org,
            &accounts[index],
            json!({
                "org":org, "env":env, "hostName":accounts[index], "serviceAccount":accounts[index],
                "image":format!("registry.example/wamn-host:test@sha256:{}", "a".repeat(64)),
                "drainBoundSeconds":60,
                "identityIssuer":"https://identity.platform.svc.cluster.local",
                "natsUrl":"nats://wamn-nats.platform.svc.cluster.local:4222",
                "envNats":{
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

#[test]
fn trust_manager_uses_the_platform_sources_and_cert_manager_webhook_tls() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use serde::Deserialize as _;
    use serde_json::Value;

    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/infrastructure");
    let output = std::process::Command::new("helm")
        .args(["template", "wamn-infrastructure"])
        .arg(chart)
        .args(["-n", "platform", "--include-crds"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let objects = serde_yaml::Deserializer::from_slice(&output.stdout)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    let crd = objects
        .iter()
        .find(|object| {
            object["kind"] == "CustomResourceDefinition"
                && object["spec"]["group"] == "trust.cert-manager.io"
        })
        .context("Bundle CRD")?;
    assert_eq!(crd["spec"]["names"]["kind"], "Bundle");
    let webhook = objects
        .iter()
        .find(|object| {
            object["kind"] == "ValidatingWebhookConfiguration"
                && object["webhooks"][0]["rules"][0]["apiGroups"]
                    == json!(["trust.cert-manager.io"])
        })
        .context("Bundle admission webhook")?;
    let service = &webhook["webhooks"][0]["clientConfig"]["service"];
    let namespace = &service["namespace"];
    assert_eq!(namespace, "cert-manager");
    let deployment = objects
        .iter()
        .find(|object| {
            object["kind"] == "Deployment"
                && object["metadata"]["name"] == service["name"]
                && object["metadata"]["namespace"] == *namespace
        })
        .context("trust-manager Deployment")?;
    let pod = &deployment["spec"]["template"]["spec"];
    let container = &pod["containers"][0];
    assert!(
        container["args"]
            .as_array()
            .unwrap()
            .contains(&json!("--trust-namespace=platform"))
    );
    assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
    assert!(
        pod["securityContext"]["runAsUser"]
            .as_u64()
            .is_some_and(|uid| uid > 0)
    );
    assert_eq!(
        container["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(
        container["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert!(
        container["securityContext"]["capabilities"]["drop"]
            .as_array()
            .unwrap()
            .contains(&json!("ALL"))
    );
    let tls_source = webhook["metadata"]["annotations"]["cert-manager.io/inject-ca-from-secret"]
        .as_str()
        .context("webhook trust injection")?;
    let (_, secret) = tls_source
        .split_once('/')
        .context("namespaced webhook TLS source")?;
    assert!(objects.iter().any(|object| object["kind"] == "Certificate"
        && object["metadata"]["namespace"] == *namespace
        && object["spec"]["secretName"] == secret));
    assert!(!objects.iter().any(|object| object["kind"] == "Secret"
        && object["metadata"]["namespace"] == *namespace
        && object["metadata"]["name"] == secret));

    let uses_secrets = |role: &Value| {
        role["rules"].as_array().is_some_and(|rules| {
            rules.iter().any(|rule| {
                rule["resources"]
                    .as_array()
                    .is_some_and(|resources| resources.contains(&json!("secrets")))
            })
        })
    };
    let account = &pod["serviceAccountName"];
    let bindings = objects.iter().filter(|object| {
        matches!(
            object["kind"].as_str(),
            Some("ClusterRoleBinding" | "RoleBinding")
        ) && object["subjects"].as_array().is_some_and(|subjects| {
            subjects.iter().any(|subject| {
                subject["kind"] == "ServiceAccount"
                    && subject["namespace"] == *namespace
                    && subject["name"] == *account
            })
        })
    });
    let mut source_access = false;
    for binding in bindings {
        let role_ref = &binding["roleRef"];
        let role = objects
            .iter()
            .find(|object| {
                object["kind"] == role_ref["kind"]
                    && object["metadata"]["name"] == role_ref["name"]
                    && object["metadata"]["namespace"] == binding["metadata"]["namespace"]
            })
            .context("trust-manager Role or ClusterRole")?;
        if uses_secrets(role) {
            assert_eq!(role["kind"], "Role");
            assert_eq!(role["metadata"]["namespace"], "platform");
            source_access = true;
        }
    }
    assert!(source_access);
    Ok(())
}
