//! Provision a kind environment with the platform operator's kubeconfig.

use anyhow::{Context as _, bail, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::ErrorKind, path::Path};
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
    provision_org::{
        APPLY_SECRET, OAUTH_ENCRYPTION_INPUT, OAUTH_SECRET, create_env_worker_login, record_env,
    },
    provision_system::connect_system_database,
};

mod manifest;

pub use manifest::{ENV_ACCOUNT, ENV_LABEL, ORG_NAME_LABEL, Quota, sql_prerequisites};
use manifest::{boundary, namespaces, network_policies};

/// Optional file in the platform SQL Job's mounted ConfigMap.
pub const ENV_DEFAULT_CPU_MILLICORES_FILE: &str = "env-default-cpu-millicores";

/// Provision the env mapping, quota, worker login and Kubernetes boundary.
///
/// # Errors
/// Returns an error if the platform is not kind, SQL refuses the request, or Kubernetes apply fails.
/// Retry with the same prod choice to finish a partial apply.
pub async fn create(org: &OrgId, env: &EnvName, prod: bool) -> anyhow::Result<()> {
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
    let sql_job = env_provision_job(org, env, prod, image, &database, EnvSqlStep::Record);
    let (committed, _) = run_sql_job(&target, &sql_job)
        .await
        .with_context(|| format!("provision env {org}/{env} in the platform SQL Job"))?;
    ensure!(
        committed.namespace == namespace,
        "SQL Job returned another env namespace"
    );
    let namespace = namespace.as_str();
    async {
        for object in manifests(
            org,
            env,
            apply_poolers,
            database.port,
            &api_server,
            &registry,
            &committed,
        )? {
            if object["kind"] == "Namespace" {
                let name = object["metadata"]["name"]
                    .as_str()
                    .context("env Namespace has no name")?;
                org_resources::apply_namespace(&target, name, &object).await?;
            } else {
                env_resources::apply(&target, &object).await?;
            }
        }
        for object in sql_prerequisites(namespace) {
            if object["kind"] == "Secret" {
                env_resources::create_secret(&target, &object).await?;
            } else {
                env_resources::apply(&target, &object).await?;
            }
        }
        let worker_job = env_provision_job(org, env, committed.prod, image, &database, EnvSqlStep::PublishWorker);
        let (_, login) = run_sql_job(&target, &worker_job).await?;
        for object in worker_configuration(inputs, &database, org, namespace, &login, &api_server)? {
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
    .with_context(|| {
        let prod_arg = if committed.prod { " --prod" } else { "" };
        format!("env {org}/{env} is recorded. Run wamn env create {org}/{env}{prod_arg} again to finish provisioning")
    })
}

async fn run_sql_job(
    target: &Target,
    manifest: &Value,
) -> anyhow::Result<(EnvSettings, wamn_role::LoginName)> {
    let result = job::run_with_termination_message(target, manifest, DEADLINE).await?;
    match serde_json::from_str(&result).context("decode the env provisioning result")? {
        EnvSqlResult::Committed(settings, login) => Ok((settings, login)),
        EnvSqlResult::Refused(message) => bail!("{message}"),
    }
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
    env: &EnvName,
    apply_pooler_hosts: [&str; 2],
    database_port: u16,
    api_server: &[Value],
    registry: &[Value],
    settings: &EnvSettings,
) -> anyhow::Result<Vec<Value>> {
    let namespace = name::env_key(org, env);
    let storage = helm::org_release_namespace(&namespace);
    let (workloads, releases) = namespaces(org, env, &namespace, &storage, settings.prod);
    let [session_pooler, transaction_pooler] = apply_pooler_hosts;
    let apply_poolers = [
        service_name(session_pooler)?,
        service_name(transaction_pooler)?,
    ];
    let mut objects = vec![workloads, releases];
    objects.extend(boundary(org, &namespace, &storage, &settings.quota));
    objects.extend(network_policies(
        &namespace,
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

/// Read the initial environment CPU request from the platform SQL configuration.
///
/// A missing file leaves the database column default in effect.
///
/// # Errors
/// Returns an error if the file cannot be read or does not contain a positive integer.
pub async fn read_env_default_cpu_millicores(database: &Path) -> anyhow::Result<Option<i64>> {
    let path = database.with_file_name(ENV_DEFAULT_CPU_MILLICORES_FILE);
    let value = match tokio::fs::read_to_string(&path).await {
        Ok(value) => value,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let millicores: i64 = value
        .trim()
        .parse()
        .with_context(|| format!("read CPU millicores from {}", path.display()))?;
    ensure!(
        millicores > 0,
        "CPU millicores must be positive in {}",
        path.display()
    );
    Ok(Some(millicores))
}

/// The two platform SQL Jobs that provision an env.
#[derive(Debug, Clone, Copy)]
pub enum EnvSqlStep {
    /// Record the environment before the operator applies its Namespace.
    Record,
    /// Rotate and publish the worker login after the operator prepares its Secret.
    PublishWorker,
}

/// Run one env provisioning step and return its result to the operator.
///
/// The CPU request override applies only when the Job creates the quota row.
pub async fn provision_in_cluster(
    database: &str,
    org: &OrgId,
    env: &EnvName,
    prod: bool,
    default_cpu_millicores: Option<i64>,
    step: EnvSqlStep,
) -> anyhow::Result<()> {
    validate_names(org, env)?;
    let mut connection = connect_system_database(database).await?;
    let result = match step {
        EnvSqlStep::Record => {
            record_env(&mut connection, org, env, prod, default_cpu_millicores).await
        }
        EnvSqlStep::PublishWorker => {
            publish_worker(&mut connection, org, env, prod, default_cpu_millicores).await
        }
    };
    let result = match result {
        Ok((login, settings)) => EnvSqlResult::Committed(settings, login),
        Err(error) => match error
            .downcast_ref::<sqlx::Error>()
            .and_then(sqlx::Error::as_database_error)
        {
            Some(database)
                if matches!(
                    database.constraint(),
                    Some("env_one_prod_per_org" | "env_prod_immutable")
                ) =>
            {
                EnvSqlResult::Refused(database.message().to_owned())
            }
            _ => return Err(error),
        },
    };
    drop(connection);
    tokio::fs::write(ENV_SQL_RESULT_PATH, serde_json::to_vec(&result)?)
        .await
        .context("publish the env provisioning Job result")
}

async fn publish_worker(
    connection: &mut sqlx::PgConnection,
    org: &OrgId,
    env: &EnvName,
    prod: bool,
    default_cpu_millicores: Option<i64>,
) -> anyhow::Result<(wamn_role::LoginName, EnvSettings)> {
    let api = KubernetesApi::in_cluster()?;
    // Keep the same session lock through publication, after the SQL transaction commits.
    lock_env_login(connection, org, env).await?;
    let worker =
        create_env_worker_login(connection, org, env, prod, default_cpu_millicores).await?;
    env_resources::publish_login(
        &api,
        &worker.settings.namespace,
        APPLY_SECRET,
        worker.login.as_str(),
        &worker.password,
    )
    .await?;
    Ok((worker.login, worker.settings))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
enum EnvSqlResult {
    Committed(EnvSettings, wamn_role::LoginName),
    Refused(String),
}

/// The Namespace and admission settings recorded at env provisioning.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvSettings {
    pub(crate) namespace: String,
    pub(crate) prod: bool,
    pub(crate) quota: Quota,
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
        let record_job = env_provision_job(&org, &env, true, image, &database, EnvSqlStep::Record);
        let record_pod = &record_job["spec"]["template"]["spec"];
        assert_eq!(record_job["metadata"]["namespace"], PLATFORM);
        assert_eq!(record_pod["automountServiceAccountToken"], false);
        assert_eq!(record_pod["containers"][0]["args"][0], "provision-env");
        assert!(
            record_pod["containers"][0]["args"]
                .as_array()
                .unwrap()
                .contains(&json!("--prod"))
        );
        let env_job = env_provision_job(
            &org,
            &env,
            true,
            image,
            &database,
            EnvSqlStep::PublishWorker,
        );
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
    fn prod_label_only_marks_the_prod_workload_namespace() -> anyhow::Result<()> {
        let org: OrgId = "acme".parse()?;
        let env: EnvName = "blue".parse()?;
        let namespace = name::env_key(&org, &env);
        let storage = helm::org_release_namespace(&namespace);
        for prod in [false, true] {
            let (workloads, releases) = namespaces(&org, &env, &namespace, &storage, prod);
            let label = prod.then(|| json!("true"));
            assert_eq!(
                workloads["metadata"]["labels"].get("wamn.dev/prod"),
                label.as_ref()
            );
            assert!(
                releases["metadata"]["labels"]
                    .get("wamn.dev/prod")
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn env_boundary_applies_committed_budgets_and_namespaced_access() -> anyhow::Result<()> {
        let org: OrgId = "acme".parse()?;
        let env = "dev".parse()?;
        let namespace = name::env_key(&org, &env);
        let quota = serde_json::from_value(json!({
            "pods": 23, "jobs": 11, "persistent_volume_claims": 12,
            "secrets": 81, "config_maps": 29, "services": 13,
            "clusters": 6, "workload_deployments": 9, "storage_bytes": 1 << 30,
            "cpu_millicores": 9000, "memory_request_bytes": 4_i64 << 30,
            "memory_limit_bytes": 8_i64 << 30, "default_cpu_millicores": 120,
            "default_memory_request_bytes": 128 << 20, "default_memory_limit_bytes": 512 << 20
        }))?;
        let settings = EnvSettings {
            namespace,
            prod: false,
            quota,
        };
        let namespace = settings.namespace.as_str();
        let storage = helm::org_release_namespace(namespace);
        let manifests = manifests(
            &org,
            &env,
            ["pooler.platform.svc", "pooler-transaction.platform.svc"],
            5432,
            &[],
            &[],
            &settings,
        )?;
        let find = |kind, name: &str| {
            manifests
                .iter()
                .find(|object| object["kind"] == kind && object["metadata"]["name"] == name)
                .with_context(|| format!("find {kind}/{name}"))
        };

        for name in [namespace, storage.as_ref()] {
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
        assert_eq!(
            find("Namespace", namespace)?["metadata"]["labels"][manifest::ENV_HOST_GROUP_LABEL],
            name::env_host_name(&org, &env)
        );
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
        let releases = find("Role", "wamn-helm-releases")?;
        assert_eq!(releases["metadata"]["namespace"], storage.as_ref());
        assert_eq!(releases["rules"][0]["resources"], json!(["secrets"]));
        assert_eq!(
            releases["rules"][0]["verbs"],
            json!(["get", "list", "create", "update", "delete"])
        );
        for object in manifests
            .iter()
            .filter(|object| object["kind"] != "Namespace")
        {
            let namespace_label = &object["metadata"]["namespace"];
            assert!(namespace_label == namespace || namespace_label == storage.as_ref());
        }
        Ok(())
    }
}
