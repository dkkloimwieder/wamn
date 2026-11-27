//! Provision a kind environment with the platform operator's kubeconfig.

use anyhow::{Context as _, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_kubernetes::{Target, client::KubernetesApi, env_resources, helm, job, org_resources};
use wamn_registry::{EnvName, OrgId, name};

use crate::{
    org::{
        LOCAL_PROVIDER, OrgInputs, control_image, field, internal_registry, registry_destinations,
    },
    org_install::{
        DEADLINE, ENV_SQL_RESULT_PATH, PLATFORM, env_provision_job, provisioning_inputs,
    },
    platform_database::lock_env_login,
    platform_registry::docker_configuration,
    provision_org::{APPLY_SECRET, OAUTH_ENCRYPTION_INPUT, OAUTH_SECRET, create_env_worker_login},
    provision_system::connect_system_database,
};

mod manifest;

pub use manifest::{ENV_ACCOUNT, ENV_LABEL, ORG_NAME_LABEL};
pub use manifest::{Quota, sql_prerequisites};
use manifest::{boundary, namespaces, network_policies};

/// Provision the env mapping, quota, worker login and Kubernetes boundary.
///
/// # Errors
/// Returns an error if the names are too long, the platform is not kind, or
/// the database or Kubernetes apply fails. Rerun the command to finish a partial apply.
pub async fn create(org: &OrgId, env: &EnvName) -> anyhow::Result<()> {
    validate_names(org, env)?;
    let target = Target::developer(PLATFORM).await?;
    let OrgInputs {
        values,
        database,
        api_server,
    } = provisioning_inputs(&target).await?;
    let inputs = &values["data"];
    ensure!(
        field(inputs, "provider")? == LOCAL_PROVIDER,
        "env create requires a kind platform"
    );
    let encryption: Value = serde_json::from_str(field(inputs, OAUTH_ENCRYPTION_INPUT)?)
        .context("decode the platform OAuth encryption configuration")?;
    let registry = field(inputs, "registry")?;
    let credentials = docker_configuration(registry, &internal_registry(registry))?;
    let registry = registry_destinations(inputs)?;
    let apply_poolers = [
        field(inputs, "applyDatabaseHost")?,
        field(inputs, "applyTransactionDatabaseHost")?,
    ];

    let image = control_image(inputs)?;
    let namespace = name::env_key(org, env);
    let storage = helm::org_release_namespace(&namespace);
    let (workloads, _) = namespaces(org, &namespace, &storage);
    org_resources::apply_namespace(&target, &namespace, &workloads).await?;
    for object in sql_prerequisites(&namespace) {
        if object["kind"] == "Secret" {
            env_resources::create_secret(&target, &object).await?;
        } else {
            env_resources::apply(&target, &object).await?;
        }
    }
    let sql_job = env_provision_job(org, env, image, &database);
    let result = job::run_with_termination_message(&target, &sql_job, DEADLINE)
        .await
        .with_context(|| format!("provision env {org}/{env} in the platform SQL Job"))?;
    let committed: EnvSqlResult =
        serde_json::from_str(&result).context("decode the committed env provisioning settings")?;
    ensure!(
        committed.namespace == namespace,
        "SQL Job returned another env namespace"
    );
    let namespace = namespace.as_str();
    async {
        for object in manifests(
            org,
            namespace,
            apply_poolers,
            database.port,
            &api_server,
            &registry,
            &committed.quota,
        )? {
            if object["kind"] == "Namespace" {
                let name = object["metadata"]["name"]
                    .as_str()
                    .context("env Namespace has no name")?;
                if name != namespace {
                    org_resources::apply_namespace(&target, name, &object).await?;
                }
            } else {
                env_resources::apply(&target, &object).await?;
            }
        }
        for object in worker_configuration(inputs, &database, org, namespace, &committed.login, &api_server)? {
            env_resources::apply(&target, &object).await?;
        }
        org_resources::write_registry_credentials_in(&target, namespace, &credentials).await?;
        if encryption["provider"] == "local" {
            let mut key = [0_u8; 32];
            SystemRandom::new()
                .fill(&mut key)
                .map_err(|_| anyhow::anyhow!("generate the OAuth encryption key for {org}/{env}"))?;
            env_resources::ensure_oauth_key(&target, namespace, OAUTH_SECRET, &key).await?;
        }
        crate::org_install::wait_for_trust(&target, namespace).await
    }
    .await
    .with_context(|| format!("env {org}/{env} is recorded. Run wamn env create {org}/{env} again to finish provisioning"))
}

/// Build the settings and PostgreSQL inputs mounted or read by env Jobs.
pub fn worker_configuration(
    inputs: &Value,
    database: &wamn_postgres::database::ConnectionConfig,
    org: &OrgId,
    namespace: &str,
    login: &wamn_role::LoginName,
    api_server: &[Value],
) -> anyhow::Result<[Value; 2]> {
    let mut values = crate::org::org_values(inputs, database, org, api_server)?;
    let mut configuration = values["control"]["configuration"].take();
    configuration
        .as_object_mut()
        .context("missing worker settings")?
        .remove("database");
    configuration["public_origin"] = format!("https://{org}.{}", field(inputs, "base")?).into();
    configuration["apply_transaction_host"] = field(inputs, "applyTransactionDatabaseHost")?.into();
    configuration["apply_database"] = json!({
        "host":field(inputs, "applyDatabaseHost")?,
        "port":database.port,
        "database":database.database,
        "username":login,
        "credentials":{"namespace":namespace,"name":APPLY_SECRET},
        "server_ca":{"namespace":namespace,"name":crate::org::SYSTEM_DATABASE_CA},
    });
    let config_map = |name: &str, file: &str, value: &Value| {
        json!({"apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":name,"namespace":namespace},
            "data":{(file):value.to_string()},
        })
    };
    Ok([
        config_map(
            &format!("{}-settings", name::control_resource(org)),
            "settings.json",
            &configuration,
        ),
        config_map(
            "wamn-platform",
            "postgres.json",
            &json!({"image": field(inputs, "postgresImage")?}),
        ),
    ])
}

/// Render the env's namespaces and admission, access and network rules.
pub fn manifests(
    org: &OrgId,
    namespace: &str,
    apply_pooler_hosts: [&str; 2],
    database_port: u16,
    api_server: &[Value],
    registry: &[Value],
    quota: &Quota,
) -> anyhow::Result<Vec<Value>> {
    let storage = helm::org_release_namespace(namespace);
    let (workloads, releases) = namespaces(org, namespace, &storage);
    let [session_pooler, transaction_pooler] = apply_pooler_hosts;
    let apply_poolers = [
        service_name(session_pooler)?,
        service_name(transaction_pooler)?,
    ];
    let mut objects = vec![workloads, releases];
    objects.extend(boundary(org, namespace, &storage, quota));
    objects.extend(network_policies(
        namespace,
        apply_poolers,
        database_port,
        api_server,
        registry,
    ));
    Ok(objects)
}

fn validate_names(org: &OrgId, env: &EnvName) -> anyhow::Result<()> {
    let namespace = name::env_key(org, env);
    let storage = helm::org_release_namespace(&namespace);
    for name in [namespace.as_str(), storage.as_ref()] {
        ensure!(
            name.len() <= name::MAX_NAMESPACE_LEN,
            "namespace {name} exceeds the {}-byte limit",
            name::MAX_NAMESPACE_LEN,
        );
    }
    // Use the shortest project slug. Actual projects check their own names on apply.
    crate::env::validate_project_env_cdc(org, &"p".parse()?, env).with_context(|| {
        format!("env {org}/{env} leaves no room for a project in the database or CDC names")
    })?;
    Ok(())
}

fn service_name(host: &str) -> anyhow::Result<&str> {
    host.split('.')
        .next()
        .filter(|name| !name.is_empty())
        .context("database host has no service name")
}

/// Provision the committed env data and publish its worker login from the SQL Job.
pub async fn provision_in_cluster(
    database: &str,
    org: &OrgId,
    env: &EnvName,
) -> anyhow::Result<()> {
    validate_names(org, env)?;
    let api = KubernetesApi::in_cluster()?;
    let mut connection = connect_system_database(database).await?;
    // Keep the same session lock through publication, after the SQL transaction commits.
    lock_env_login(&mut connection, org, env).await?;
    let worker = create_env_worker_login(&mut connection, org, env).await?;
    env_resources::publish_login(
        &api,
        &worker.namespace,
        APPLY_SECRET,
        worker.login.as_str(),
        &worker.password,
    )
    .await?;
    drop(connection);
    let committed = EnvSqlResult {
        namespace: worker.namespace,
        login: worker.login,
        quota: worker.quota,
    };
    tokio::fs::write(ENV_SQL_RESULT_PATH, serde_json::to_vec(&committed)?)
        .await
        .context("publish the committed env settings as the Job result")
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EnvSqlResult {
    namespace: String,
    login: wamn_role::LoginName,
    quota: Quota,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn env_names_leave_room_for_helm_and_a_project_database() -> anyhow::Result<()> {
        let org: OrgId = "a".repeat(30).parse()?;
        validate_names(&org, &"b".repeat(9).parse()?)?;
        assert!(validate_names(&org, &"b".repeat(10).parse()?).is_err());
        assert!(validate_names(&org, &"b".repeat(23).parse()?).is_err());
        Ok(())
    }

    #[test]
    fn env_sql_uses_the_platform_job_and_only_its_worker_secret() -> anyhow::Result<()> {
        let org: OrgId = "acme".parse()?;
        let env = "dev".parse()?;
        let namespace = name::env_key(&org, &env);
        let database = wamn_postgres::database::configuration(
            PLATFORM,
            "database",
            "database".into(),
            5432,
            "system".into(),
            "postgres".parse()?,
            "database-login".into(),
        );
        let image = "registry.example/control";
        let env_job = env_provision_job(&org, &env, image, &database);
        let org_job = crate::org_install::provision_job(&org, image, &database);
        let env_pod = &env_job["spec"]["template"]["spec"];
        let org_pod = &org_job["spec"]["template"]["spec"];
        let env_container = &env_pod["containers"][0];
        let org_container = &org_pod["containers"][0];
        assert_eq!(env_job["metadata"]["namespace"], PLATFORM);
        assert_eq!(env_pod["securityContext"], org_pod["securityContext"]);
        for field in ["image", "securityContext", "resources"] {
            assert_eq!(env_container[field], org_container[field]);
        }
        assert_eq!(env_pod["volumes"][1], org_pod["volumes"][1]);
        assert_eq!(env_container["terminationMessagePath"], ENV_SQL_RESULT_PATH);

        let [account, secret, role, binding] = sql_prerequisites(&namespace);
        assert_eq!(env_pod["serviceAccountName"], account["metadata"]["name"]);
        assert_eq!(binding["subjects"][0]["name"], account["metadata"]["name"]);
        assert_eq!(binding["subjects"][0]["namespace"], PLATFORM);
        assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
        assert_eq!(
            role["metadata"]["namespace"],
            secret["metadata"]["namespace"]
        );
        assert!(secret["data"]["password"].is_null());
        assert_eq!(
            role["rules"],
            json!([{
                "apiGroups":[""], "resources":["secrets"],
                "resourceNames":[secret["metadata"]["name"]], "verbs":["get","update","patch"],
            }])
        );
        Ok(())
    }

    #[test]
    fn env_boundary_applies_committed_budgets_and_namespaced_access() -> anyhow::Result<()> {
        let org: OrgId = "acme".parse()?;
        let namespace = name::env_key(&org, &"dev".parse()?);
        let storage = helm::org_release_namespace(&namespace);
        let quota = serde_json::from_value(json!({
            "pods": 23, "jobs": 11, "persistent_volume_claims": 12,
            "secrets": 81, "config_maps": 29, "services": 13,
            "clusters": 6, "workload_deployments": 9, "storage_bytes": 1 << 30,
            "cpu_millicores": 9000, "memory_request_bytes": 4_i64 << 30,
            "memory_limit_bytes": 8_i64 << 30, "default_cpu_millicores": 120,
            "default_memory_request_bytes": 128 << 20, "default_memory_limit_bytes": 512 << 20
        }))?;
        let manifests = manifests(
            &org,
            &namespace,
            ["pooler.platform.svc", "pooler-transaction.platform.svc"],
            5432,
            &[],
            &[],
            &quota,
        )?;
        let find = |kind, name: &str| {
            manifests
                .iter()
                .find(|object| object["kind"] == kind && object["metadata"]["name"] == name)
                .with_context(|| format!("find {kind}/{name}"))
        };

        for name in [namespace.as_str(), storage.as_ref()] {
            let labels = &find("Namespace", name)?["metadata"]["labels"];
            assert_eq!(labels[ENV_LABEL], "true");
            assert_eq!(labels[ORG_NAME_LABEL], org.as_str());
            assert_eq!(labels["pod-security.kubernetes.io/enforce"], "restricted");
            assert_eq!(labels["pod-security.kubernetes.io/warn"], "restricted");
        }
        assert!(
            find("Namespace", &storage)?["metadata"]["labels"]["wamn.dev/org-workloads"].is_null()
        );
        let worker_egress = find("NetworkPolicy", "allow-worker-to-platform")?["spec"]["egress"]
            .as_array()
            .context("worker egress rules")?;
        for pooler in ["pooler", "pooler-transaction"] {
            assert!(worker_egress.iter().any(|rule| {
                rule["to"][0]["podSelector"]["matchLabels"]["cnpg.io/poolerName"] == pooler
            }));
        }
        let deny = &find("NetworkPolicy", "default-deny")?["spec"];
        assert_eq!(deny["podSelector"], json!({}));
        assert_eq!(deny["policyTypes"], json!(["Ingress", "Egress"]));
        assert!(deny["ingress"].is_null() && deny["egress"].is_null());

        let hard = &find("ResourceQuota", "wamn-env")?["spec"]["hard"];
        assert_eq!(hard["count/pods"], "23");
        assert_eq!(hard["requests.cpu"], "9000m");
        assert_eq!(hard["requests.memory"], "4294967296");
        assert_eq!(hard["limits.memory"], "8589934592");
        let limits = &find("LimitRange", "wamn-env")?["spec"]["limits"][0];
        assert_eq!(
            limits["defaultRequest"],
            json!({"cpu":"120m","memory":"134217728"})
        );
        assert_eq!(limits["default"], json!({"memory":"536870912"}));

        let account = &find("ServiceAccount", ENV_ACCOUNT)?["metadata"];
        let role = find("Role", ENV_ACCOUNT)?;
        let binding = find("RoleBinding", ENV_ACCOUNT)?;
        assert_eq!(role["metadata"]["namespace"], account["namespace"]);
        assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
        assert_eq!(binding["subjects"][0]["name"], account["name"]);
        assert_eq!(binding["subjects"][0]["namespace"], account["namespace"]);
        let rules = role["rules"]
            .as_array()
            .context("the env Role has no rules")?;
        assert!(!rules.iter().any(
            |rule| rule["resources"].as_array().is_some_and(|resources| {
                resources
                    .iter()
                    .any(|resource| resource == "pods/exec" || resource == "*")
            })
        ));
        for object in manifests
            .iter()
            .filter(|object| object["kind"] != "Namespace")
        {
            let namespace_label = &object["metadata"]["namespace"];
            assert!(namespace_label == namespace.as_str() || namespace_label == storage.as_ref());
        }
        Ok(())
    }
}
