//! Deploy the org control host from selected artifacts and bootstrap credentials.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

use wamn_kubernetes::kubectl;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::document::{ClusterDocument, Provider};

use wamn_registry::name::REGISTRY_PULL;

/// Wait until every control host observes the environment's administration login change.
pub async fn wait_administration(
    target: &Target,
    triple: &wamn_registry::Triple,
    present: bool,
    budget: std::time::Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(budget, async {
        let name = wamn_control_provision::control_administration_secret_name(&triple.org);
        let Some(secret) = kubectl::get::<Value>(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "configmap",
            &name,
        )
        .await?
        else {
            return Ok(());
        };
        let key = wamn_control_provision::control_administration_key(
            &triple.project,
            triple.env.as_str(),
        );
        let login = present
            .then(|| {
                secret["data"][&key]
                    .as_str()
                    .map(str::to_owned)
                    .context("administration configuration is absent")
            })
            .transpose()?;
        let path = format!("/etc/wamn/control-administration/{key}");
        let selector = format!(
            "app.kubernetes.io/name={}",
            wamn_registry::name::control_resource(&triple.org)
        );
        let script = if present {
            r#"test -r "$1" && cmp -s "$1" -"#
        } else {
            r#"test ! -e "$1""#
        };
        loop {
            let output = kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &["get", "pods", "-l", &selector, "-o", "json"],
                None,
            )
            .await?;
            let pods: Value = serde_json::from_str(&output).context("decode control host pods")?;
            let pods = pods["items"].as_array().context("control host pod list")?;
            let mut projected = !present || !pods.is_empty();
            for pod in pods {
                let pod = pod["metadata"]["name"]
                    .as_str()
                    .context("control host pod name")?;
                projected &= kubectl::kubectl(
                    &target.kubeconfig,
                    &target.context,
                    &target.namespace,
                    &[
                        "exec", "-i", pod, "-c", "control", "--", "sh", "-c", script, "--", &path,
                    ],
                    Some(login.as_deref().unwrap_or_default().as_bytes()),
                )
                .await
                .is_ok();
            }
            if projected {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "control administration login projection for {triple} exceeded the readiness budget"
        )
    })?
}

/// Render the platform registry credential for hosts using the local registry Service.
pub fn registry_pull_secret(document: &ClusterDocument, auth_file: &Path) -> anyhow::Result<Value> {
    let authority = document.registry.split('/').next().unwrap_or_default();
    let credential = wamn_package_artifact::registry_credentials::read_registry_credentials(
        auth_file, authority,
    )?;
    let mut config: Value = serde_json::from_slice(&std::fs::read(auth_file)?)?;
    config["auths"][super::resources::POD_REGISTRY] = json!({
        "username":credential.username(), "password":credential.password(),
    });
    Ok(json!({
        "apiVersion":"v1", "kind":"Secret", "type":"kubernetes.io/dockerconfigjson",
        "metadata":{"name":REGISTRY_PULL, "namespace":document.org},
        "stringData":{".dockerconfigjson":serde_json::to_string(&config)?},
    }))
}

/// Read bootstrap credentials and copy the operator Secret through the deploy client.
pub async fn initialize(target: &Target, document: &ClusterDocument) -> anyhow::Result<()> {
    let configuration: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        &document.org,
        "configmap",
        &wamn_registry::name::control_administration_secret_name(&document.org),
    )
    .await?;
    anyhow::ensure!(
        configuration
            .as_ref()
            .is_some_and(|value| value["data"]["control.json"].is_string()),
        "control database configuration is absent"
    );
    super::oauth::ensure_key(target, document).await?;
    Ok(())
}

/// Render the org host values shared by the cluster and local control host.
pub fn host_values(org: &str) -> Value {
    let name = wamn_registry::name::resource_suffix("c", org);
    let mut values = json!({"runtime-operator": {"runtime": {
        "terminationGracePeriodSeconds": 70,
        "serviceAccount": {"create":true,"name":wamn_registry::control_nats_user(org)},
        // Control operation routes create their Jobs with this account's token.
        "automountServiceAccountToken": true,
        "hostGroups": [{
            "name": name, "namespace": org, "replicas": 1,
            "schedulerNatsUrl": "nats://wamn-nats.platform.svc.cluster.local:4222",
            "dataNatsUrl": "nats://wamn-nats.platform.svc.cluster.local:4222",
            "envFrom": [{"configMapRef":{"name":"wamn-host-settings"}}],
            "env": [
                {"name": "WAMN_DRAIN_BOUND_SECONDS", "value": "70"},
                {"name": "WAMN_CONTROL_URL", "value": "/etc/wamn/control-administration/control.json"},
                {"name": "WAMN_PAT_ISSUER", "value": format!("https://identity.{org}.svc.cluster.local")},
                {"name": "WAMN_PAT_CLIENT_CERT", "value": "/etc/identity-operator/tls.crt"},
                {"name": "WAMN_PAT_CLIENT_KEY", "value": "/etc/identity-operator/tls.key"},
                {"name": "WAMN_PAT_SERVER_CA", "value": "/etc/identity-ca/ca.crt"},
                {"name": "WAMN_CONTROL_ADMINISTRATION_DIR", "value": "/etc/wamn/control-administration"}
            ],
            "volumes": [
                {"name": "identity-operator", "secret": {"secretName": wamn_registry::name::resource_suffix("operator",org)}},
                {"name": "control-administration", "configMap": {"name": wamn_registry::name::control_administration_secret_name(org)}}
            ],
            "volumeMounts": [
                {"name": "identity-operator", "mountPath": "/etc/identity-operator", "readOnly": true},
                {"name": "control-administration", "mountPath": "/etc/wamn/control-administration", "readOnly": true}
            ],
            "http": {"enabled": true, "port": 80},
            "resources": {"requests": {"memory": "256Mi", "cpu": "250m"},
                "limits": {"memory": "4Gi", "cpu": "2"}}
        }]
    }}});
    // The control host's data connection is its org's NATS user, which covers
    // every environment of the org (docs/plan/alignment.md R7).
    values["global"] = json!({"certificates": {"dataCertSecretName": wamn_registry::name::control_nats_certificate(org)}});
    values
}

pub(super) fn resource_entry(
    document: &ClusterDocument,
    chart: &SelectedChart,
    _installed: Option<&Value>,
) -> anyhow::Result<Value> {
    use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity};
    let org = &document.org;
    let name = wamn_registry::name::control_resource(org);
    let configuration = format!("{name}-configuration");
    let login = wamn_control_provision::workload_login(
        WorkloadRoleFamily::Control,
        WorkloadRoleIdentity::Org {
            org,
            database: "wamn_system",
        },
    )?;
    let connection = |username, secret| {
        crate::credentials::configuration(
            org,
            "wamn-pg",
            format!("wamn-pg-rw.{org}.svc.cluster.local"),
            5432,
            "wamn_system".into(),
            username,
            secret,
        )
    };
    let scheme = if document.provider == Provider::Kind {
        "http"
    } else {
        "https"
    };
    let registry = if document.provider == Provider::Kind {
        super::resources::POD_REGISTRY
    } else {
        &document.registry
    };
    let nats = json!({"url":super::identity::NATS_URL,"ca":"/credentials/nats/ca.crt","cert":"/credentials/nats/tls.crt","key":"/credentials/nats/tls.key","server_name":null});
    let platform_ui = chart
        .values()
        .get("platformUi")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !platform_ui.is_empty() {
        wamn_package_artifact::ensure_digest(platform_ui)?;
    }
    let config = json!({"org":org,"http_addr":"0.0.0.0:80","public_origin":format!("{scheme}://{}",document.route_hosts.control),
        "database":connection(login,wamn_control_provision::org_workload_secret_name(WorkloadRoleFamily::Control,org)),
        "apply_database":connection("wamn_apply".into(),"wamn-apply".into()),
        "session_issuer":super::identity::issuer(org),"session_jwks_ca":"/etc/identity-ca/ca.crt",
        "administration_dir":"/etc/wamn/control-administration","worker":{"image":chart.image("control")?,"configuration":configuration,"service_account":wamn_registry::name::operation(org)},
        "chart":chart.reference(),"release_artifact_base":format!("{registry}/wamn/releases"),"package_artifact_base":format!("{registry}/wamn/packages"),"component_artifact_base":format!("{registry}/wamn/components"),"integration_artifact_base":format!("{registry}/wamn/integrations"),
        "registry_auth_file":"/credentials/registry/config.json","registry_token_metadata":document.provider!=Provider::Kind,"insecure_registry":document.provider==Provider::Kind,
        "nats":nats,"events":{"stream_replicas":1,"dup_window_secs":120},
        "event_observer":null,"identity":{"endpoint":super::identity::issuer(org),"client_cert":"/etc/identity-operator/tls.crt","client_key":"/etc/identity-operator/tls.key","server_ca":"/etc/identity-ca/ca.crt"},"oauth_encryption":super::oauth::configuration(document)});
    Ok(
        json!({"chart":chart.directory(),"version":chart.version,"name":name,"namespace":org,
        "values":{"controlService":true,"org":org,"platformUi":platform_ui,"routeHost":document.route_hosts.control,"roles":[],"runtime-operator":{"runtime":{"hostGroups":[]}},
        "control":{"image":chart.image("control")?,"configuration":config,"natsSecret":wamn_registry::name::control_nats_certificate(org),"operatorSecret":wamn_registry::name::resource_suffix("operator",org),"administration":wamn_registry::name::control_administration_secret_name(org),"account":wamn_registry::control_nats_user(org),"registrySecret":if document.provider==Provider::Kind {REGISTRY_PULL}else{""},"oauthSecret":if document.provider==Provider::Kind {"wamn-oauth"}else{""}},
        "platform":{"chart":chart.reference()}}}),
    )
}

/// Require the control route to refuse a request without credentials.
///
/// # Errors
/// Returns a forwarding, HTTP, or authorization-boundary failure.
pub async fn probe(target: &Target, document: &ClusterDocument) -> anyhow::Result<()> {
    let service = format!(
        "service/{}",
        wamn_registry::name::resource_suffix(
            &wamn_registry::name::control_resource(&document.org),
            "http"
        )
    );
    let forward =
        wamn_kubernetes::forward::port_forward(target, &document.org, &service, 80).await?;
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?
        .get(format!(
            "http://127.0.0.1:{}/wamn_control/control/mine",
            forward.port
        ))
        .header(reqwest::header::HOST, &document.route_hosts.control)
        .send()
        .await
        .context("probe the unauthenticated control route")?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::UNAUTHORIZED,
        "the unauthenticated control route returned {}; expected 401",
        response.status()
    );
    // The hint uses the exported context and the native control Deployment.
    println!(
        "Local control access: kubectl --context '{}' -n {} port-forward --address=127.0.0.1 deployment/{} 8080:80\nHost: {}",
        target.context.replace('\'', "'\\''"),
        document.org,
        wamn_registry::name::control_resource(&document.org),
        document.route_hosts.control,
    );
    Ok(())
}
