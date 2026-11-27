//! Kubernetes operations for installing an org.

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, net::IpAddr, path::PathBuf, time::Duration};
use wamn_kubernetes::{Target, helm, job, org_resources};
use wamn_postgres::database::{ConnectionConfig, DATABASE_DIR};
use wamn_registry::{EnvName, OrgId, name};

use crate::{
    env_install::EnvSqlStep,
    org::{OrgInputs, SYSTEM_DATABASE_CA, control_image, field},
    provision_org::{CONTROL_AUDIENCE, INVITATION_ACCOUNT, OrgActivity, SQL_ACCOUNT},
};

pub(crate) const PLATFORM: &str = "platform";
const ORG_INPUTS: &str = "wamn-org-inputs";
const SYSTEM_SQL_CONFIG: &str = "wamn-system-sql";
pub(crate) const PROVISION_JOB: &str = "wamn-provision-org";
const SQL_CONFIG_PATH: &str = "/etc/wamn/system-sql/database.json";
pub(crate) const DEADLINE: Duration = Duration::from_secs(300);

/// The org chart and the cluster selected by the administrator.
#[derive(Debug)]
pub struct OrgInstaller {
    chart: PathBuf,
    target: Target,
}

impl OrgInstaller {
    /// Locate the org chart and connect to the selected cluster.
    pub async fn developer() -> anyhow::Result<Self> {
        let chart = org_chart()?;
        let target = Target::developer(PLATFORM).await?;
        Ok(Self { chart, target })
    }
}

impl OrgInstaller {
    /// Run the activity command through the platform SQL Job in the selected cluster.
    ///
    /// # Errors
    /// Returns an error if the cluster inputs cannot be read or the Job fails.
    pub async fn set_activity(org: &OrgId, activity: OrgActivity) -> anyhow::Result<()> {
        let target = Target::developer(PLATFORM).await?;
        let (inputs, config) = tokio::try_join!(
            org_resources::config_map(&target, PLATFORM, ORG_INPUTS),
            org_resources::config_map(&target, PLATFORM, SYSTEM_SQL_CONFIG),
        )?;
        let image = control_image(&inputs["data"])?;
        let database = serde_json::from_str(field(&config["data"], "database.json")?)
            .context("decode the platform SQL Job connection")?;
        let verb = match activity {
            OrgActivity::Active => "activate-org",
            OrgActivity::Inactive => "deactivate-org",
        };
        let manifest = sql_job(org, image, &database, verb);
        job::run_exclusive(&target, &manifest, DEADLINE + Duration::from_secs(30)).await?;
        Ok(())
    }

    pub(crate) async fn inputs(&self) -> anyhow::Result<OrgInputs> {
        provisioning_inputs(&self.target).await
    }

    pub(crate) async fn install_release(&self, org: &OrgId, values: &Value) -> anyhow::Result<()> {
        let target = &self.target;
        let values_file =
            tempfile::NamedTempFile::new().context("create a file for public org values")?;
        serde_json::to_writer(values_file.as_file(), values).context("write public org values")?;

        let storage = helm::org_release_namespace(org.as_str());
        org_resources::ensure_helm_storage_namespace(target, &storage).await?;
        helm::recover_pending(target, &storage, "wamn-org", helm::Wait::None).await?;
        helm::upgrade(
            target,
            &storage,
            &self.chart,
            "wamn-org",
            &[values_file.path()],
            helm::Wait::None,
            helm::Install::ExistingNamespace,
        )
        .await
    }

    pub(crate) async fn write_registry_credentials(
        &self,
        org: &OrgId,
        credentials: &[u8],
    ) -> anyhow::Result<()> {
        org_resources::write_registry_credentials(&self.target, org, credentials).await
    }

    pub(crate) async fn provision_logins(
        &self,
        org: &OrgId,
        image: &str,
        database: &ConnectionConfig,
    ) -> anyhow::Result<String> {
        job::run_exclusive(
            &self.target,
            &provision_job(org, image, database),
            DEADLINE + Duration::from_secs(30),
        )
        .await
    }

    pub(crate) async fn wait_for_trust(&self, org: &OrgId) -> anyhow::Result<()> {
        wait_for_trust(&self.target, org.as_str()).await
    }

    pub(crate) async fn reload_control(
        &self,
        org: &OrgId,
        sql_job_uid: &str,
    ) -> anyhow::Result<()> {
        org_resources::reload_control(&self.target, org, sql_job_uid, DEADLINE).await
    }

    pub(crate) async fn invite_owner(
        &self,
        org: &OrgId,
        image: &str,
        owner: &str,
    ) -> anyhow::Result<()> {
        org_resources::run_job(
            &self.target,
            org,
            &invitation_job(org, image, owner),
            DEADLINE,
        )
        .await
    }
}

pub(crate) async fn provisioning_inputs(target: &Target) -> anyhow::Result<OrgInputs> {
    let (inputs, database_config, api_server) = tokio::try_join!(
        org_resources::config_map(target, PLATFORM, ORG_INPUTS),
        org_resources::config_map(target, PLATFORM, SYSTEM_SQL_CONFIG),
        async { api_destinations(org_resources::api_addresses(target.client.client()).await?) },
    )?;
    let database: ConnectionConfig =
        serde_json::from_str(field(&database_config["data"], "database.json")?)
            .context("decode the platform SQL Job connection")?;
    Ok(OrgInputs {
        values: inputs,
        database,
        api_server,
    })
}

pub(crate) async fn wait_for_trust(target: &Target, namespace: &str) -> anyhow::Result<()> {
    tokio::try_join!(
        org_resources::wait_for_public_root(target, namespace, "identity-ca", DEADLINE),
        org_resources::wait_for_public_root(target, namespace, "nats-ca", DEADLINE),
        org_resources::wait_for_public_root(target, namespace, SYSTEM_DATABASE_CA, DEADLINE),
    )?;
    Ok(())
}

fn org_chart() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .map(|dir| dir.join("deploy/charts/org"))
        .chain([PathBuf::from("/usr/local/share/wamn/deploy/charts/org")])
        .find(|dir| dir.join("Chart.yaml").is_file())
        .context("locate deploy/charts/org in the repository or installed chart directory")
}

/// Build NetworkPolicy destinations from the Kubernetes API addresses.
pub fn api_destinations(destinations: BTreeSet<(IpAddr, i32)>) -> anyhow::Result<Vec<Value>> {
    ensure!(
        !destinations.is_empty(),
        "no Kubernetes API address for NetworkPolicy"
    );
    Ok(destinations
        .into_iter()
        .map(|(ip, port)| {
            let mask = if ip.is_ipv4() { 32 } else { 128 };
            json!({"cidr": format!("{ip}/{mask}"), "port": port})
        })
        .collect())
}

/// Build the platform Job that provisions an org's database logins.
pub fn provision_job(org: &OrgId, image: &str, database: &ConnectionConfig) -> Value {
    let mut sql = sql_job(org, image, database, "provision-org");
    sql["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
    sql
}

pub(crate) const ENV_SQL_COMPONENT: &str = "wamn-env-sql";
pub(crate) const ENV_SQL_RESULT_PATH: &str = "/dev/termination-log";

pub(crate) fn env_sql_account(namespace: &str) -> String {
    format!("{ENV_SQL_COMPONENT}-{namespace}")
}

/// Build one of the platform Jobs that provision an env.
pub fn env_provision_job(
    org: &OrgId,
    env: &EnvName,
    prod: bool,
    image: &str,
    database: &ConnectionConfig,
    step: EnvSqlStep,
) -> Value {
    let verb = match step {
        EnvSqlStep::Record => "provision-env",
        EnvSqlStep::PublishWorker => "provision-env-worker",
    };
    let mut args = vec![
        verb,
        org.as_str(),
        env.as_str(),
        "--database",
        SQL_CONFIG_PATH,
    ];
    if prod {
        args.push("--prod");
    }
    let mut job = platform_sql_job(ENV_SQL_COMPONENT, image, database, &args);
    job["metadata"]["namespace"] = PLATFORM.into();
    let spec = &mut job["spec"]["template"]["spec"];
    spec["serviceAccountName"] = match step {
        EnvSqlStep::Record => "default".into(),
        EnvSqlStep::PublishWorker => env_sql_account(&name::env_key(org, env)).into(),
    };
    spec["containers"][0]["terminationMessagePath"] = ENV_SQL_RESULT_PATH.into();
    spec["automountServiceAccountToken"] = matches!(step, EnvSqlStep::PublishWorker).into();
    job
}

fn sql_job(org: &OrgId, image: &str, database: &ConnectionConfig, verb: &str) -> Value {
    let mut job = platform_sql_job(
        SQL_ACCOUNT,
        image,
        database,
        &[verb, org.as_str(), "--database", SQL_CONFIG_PATH],
    );
    job["metadata"] = json!({"name": PROVISION_JOB});
    job
}

fn platform_sql_job(
    component: &str,
    image: &str,
    database: &ConnectionConfig,
    args: &[&str],
) -> Value {
    let login = &database.credentials.name;
    let mut sql = org_job(component, image, args);
    let spec = &mut sql["spec"]["template"]["spec"];
    spec["volumes"] = json!([
        {"name": "configuration", "configMap": {"name": SYSTEM_SQL_CONFIG}},
        {"name": "database", "projected": {"defaultMode": 256, "sources": [
            {"secret": {"name": login, "items": [
                {"key": "tls.crt", "path": format!("{login}/tls.crt")},
                {"key": "tls.key", "path": format!("{login}/tls.key")},
            ]}},
            {"secret": {"name": database.server_ca.name, "items": [
                {"key": "ca.crt", "path": format!("{login}/ca.crt")},
            ]}},
        ]}},
    ]);
    spec["containers"][0]["volumeMounts"] = json!([
        {"name": "configuration", "mountPath": "/etc/wamn/system-sql", "readOnly": true},
        {"name": "database", "mountPath": DATABASE_DIR, "readOnly": true},
    ]);
    sql
}

/// Build the org Job that sends the first owner's invitation.
pub fn invitation_job(org: &OrgId, image: &str, owner: &str) -> Value {
    let mut invitation = org_job(
        INVITATION_ACCOUNT,
        image,
        &[
            "invite-org-owner",
            org.as_str(),
            &format!("--owner={owner}"),
        ],
    );
    invitation["spec"]["backoffLimit"] = 5.into();
    let spec = &mut invitation["spec"]["template"]["spec"];
    spec["volumes"] = json!([
        {"name": "control-token", "projected": {"defaultMode": 256, "sources": [
            {"serviceAccountToken": {"audience": CONTROL_AUDIENCE, "expirationSeconds": 600, "path": "token"}},
        ]}},
    ]);
    spec["containers"][0]["volumeMounts"] = json!([
        {"name": "control-token", "mountPath": "/var/run/secrets/wamn-control", "readOnly": true},
    ]);
    invitation
}

fn org_job(account: &str, image: &str, args: &[&str]) -> Value {
    json!({
        "apiVersion": "batch/v1", "kind": "Job", "metadata": {"generateName": format!("{account}-")},
        "spec": {
            "activeDeadlineSeconds": DEADLINE.as_secs(), "ttlSecondsAfterFinished": 3600,
            "backoffLimit": 0,
            "template": {
                "metadata": {"labels": {"app.kubernetes.io/component": account}},
                "spec": {
                    "serviceAccountName": account, "automountServiceAccountToken": false,
                    "restartPolicy": "Never",
                    "securityContext": job::pod_security_context(),
                    "containers": [{
                        "name": account, "image": image, "args": args,
                        "securityContext": job::container_security_context(),
                        "resources": job::container_resources(),
                    }],
                },
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{invitation_job, provision_job};
    use wamn_kubernetes::job;
    use wamn_postgres::database::configuration;

    #[test]
    fn org_jobs_use_the_wamn_job_security_contexts_and_resources() -> anyhow::Result<()> {
        let org = "acme".parse()?;
        let image = "registry.example/control";
        let database = configuration(
            "platform",
            "database",
            "database".into(),
            5432,
            "system".into(),
            "postgres".parse()?,
            "database-login".into(),
        );

        for manifest in [
            provision_job(&org, image, &database),
            invitation_job(&org, image, "owner@example.com"),
        ] {
            let pod = &manifest["spec"]["template"]["spec"];
            assert_eq!(pod["securityContext"], job::pod_security_context());
            let containers = pod["containers"].as_array().expect("Job containers");
            assert!(!containers.is_empty());
            for container in containers {
                assert_eq!(
                    container["securityContext"],
                    job::container_security_context()
                );
                assert_eq!(container["resources"], job::container_resources());
            }
        }
        Ok(())
    }
}
