//! The web and Kubernetes authorization cases for the kind smoke run.

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

use anyhow::{Context as _, ensure};
use common::{Org, host::FIXTURE, service_account_can};
use futures_util::{StreamExt as _, TryStreamExt as _, stream};
use k8s_openapi::api::authorization::v1::{ResourceAttributes, SubjectAccessReview};
use k8s_openapi::api::core::v1::{Pod, ServiceAccount};
use k8s_openapi::api::rbac::v1::{ClusterRole, PolicyRule, Role};
use kube::{
    Api, Client,
    api::{ListParams, PostParams},
    discovery::{Discovery, Scope},
};
use serde::Deserialize as _;
use serde_json::{Value, json};
use tokio::time::timeout;
use wamn_env::app_chart::plan::workload_name;
use wamn_http_client::HttpClient;
use wamn_kubernetes::{client::resource, helm::org_release_namespace};
use wamn_registry::name;

const APP: &str = "wamn_receiving";

common::journey! {
    /// An owner deploys one application, and web answers 401 without a token and 200 with
    /// the owner token, for an HTTP app call and a trusted HTTPS control call.
    async fn the_owner_reaches_an_app_and_control_through_web() -> anyhow::Result<()> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps")
            .join(APP);
        let env = Org::create().await?.apply(APP).await?;
        env.deploy(&source, APP).await?;

        let org = &env.org;
        let outputs = &org.cluster.outputs;
        let file: Value = serde_json::from_slice(&std::fs::read(&env.file)?)?;
        let label = file["projects"][0]["apps"][APP]["label"]
            .as_str()
            .context("the environment file has no application label")?;
        let app = name::app_host(label, &org.name, &outputs.base_domain.parse()?);
        let app_url = format!("http://{}/api/location/list", outputs.web_address);
        let mut control = url::Url::parse(&org.control_url())?;
        control.set_scheme("https").expect("HTTPS control URL");
        control.set_path("/wamn_control/control/mine");
        let control_host = control.host_str().context("control host")?;

        let ca = common::secret_key(&org.cluster.client, "hosts", "wamn-edge-ca", "tls.crt").await?;
        let certificates = resource(
            org.cluster.client.clone(), "hosts", "cert-manager.io", "v1", "Certificate", "certificates",
        );
        let certificate = certificates.get(&name::control_resource(&org.name)).await?;
        let wildcard = certificate.data["spec"]["dnsNames"]
            .as_array()
            .context("the org certificate has no DNS names")?
            .iter()
            .filter_map(Value::as_str)
            .find(|host| host.starts_with("*."))
            .context("the org certificate has no wildcard host")?;
        let env_host = wildcard.replacen('*', common::ENV, 1);
        let mut tls_address = outputs.web_address;
        tls_address.set_port(443);
        let client = HttpClient::builder()
            .roots(&ca)?
            .resolve(control_host, tls_address)
            .resolve(&env_host, tls_address)
            .timeout(Duration::from_secs(60))
            .build()?;

        let health = client.get(format!("https://{env_host}/healthz")).send().await?;
        ensure!(health.status().is_success(), "wildcard host returned {}", health.status());
        for (host, url) in [
            (app.as_str(), app_url.as_str()),
            (control_host, control.as_str()),
        ] {
            let refused = client.get(url).header("Host", host).send().await?;
            let status = refused.status();
            ensure!(
                status == hyper::StatusCode::UNAUTHORIZED,
                "{host} at {url} without a token answered {status}: {}",
                refused.text().await?
            );
            let answered = client
                .get(url)
                .header("Host", host)
                .bearer_auth(&org.token)
                .send()
                .await?;
            let status = answered.status();
            let text = answered.text().await?;
            ensure!(
                status == hyper::StatusCode::OK,
                "{host} at {url} with the owner token answered {status}: {text}"
            );
            serde_json::from_str::<Value>(&text)
                .with_context(|| format!("{host} at {url} answered no JSON: {text}"))?;
        }
        Ok(())
    }
}

common::journey! {
    async fn org_service_accounts_cannot_act_in_another_org() -> anyhow::Result<()> {
        let (a, b) = timeout(Duration::from_mins(20), async {
            let a = Org::create().await?.apply(FIXTURE).await?;
            let b = Org::create().await?.apply(FIXTURE).await?;
            anyhow::Ok((a, b))
        })
        .await
        .context("apply the org environments for the authorization check")??;
        let client = &a.org.cluster.client;
        let namespaces = |env: &common::Env| {
            let namespace = env.namespace();
            [
                env.org.name.to_string(),
                org_release_namespace(env.org.name.as_str()).into_owned(),
                org_release_namespace(&namespace).into_owned(),
                namespace,
            ]
        };
        let source = namespaces(&a);
        let target = namespaces(&b);
        timeout(Duration::from_mins(5), check_authorization(client, &source, &target))
            .await
            .context("review the org service-account permissions")?
    }
}

async fn check_authorization(
    client: &Client,
    source: &[String; 4],
    target: &[String; 4],
) -> anyhow::Result<()> {
    let mut accounts = Vec::new();
    for namespace in source {
        let items = Api::<ServiceAccount>::namespaced(client.clone(), namespace)
            .list(&ListParams::default())
            .await
            .with_context(|| format!("list service accounts in {namespace}"))?
            .items;
        ensure!(!items.is_empty(), "no service accounts in {namespace}");
        for item in items {
            let name = item
                .metadata
                .name
                .context("service account without a name")?;
            accounts.push((namespace.as_str(), name));
        }
    }

    let reviews = Api::<SubjectAccessReview>::all(client.clone());
    // The env worker watches env pods and lists the env's Helm records.
    for (namespace, resource) in [
        (source[3].as_str(), "pods"),
        (source[2].as_str(), "secrets"),
    ] {
        let mut allowed = false;
        for (account_namespace, account) in &accounts {
            let attributes = ResourceAttributes {
                namespace: Some(namespace.to_owned()),
                resource: Some(resource.to_owned()),
                verb: Some("list".to_owned()),
                ..ResourceAttributes::default()
            };
            allowed = service_account_can(&reviews, account_namespace, account, attributes)
                .await
                .with_context(|| {
                    format!("{account_namespace}/{account} list {resource} in {namespace}")
                })?;
            if allowed {
                break;
            }
        }
        ensure!(
            allowed,
            "no source account can list {resource} in {namespace}"
        );
    }

    let resources = discover_resources(client).await?;
    ensure!(
        !resources.is_empty(),
        "discovery returned no namespaced resources"
    );
    let mut policies = Vec::new();
    for namespace in source.iter().chain(target) {
        let roles = Api::<Role>::namespaced(client.clone(), namespace)
            .list(&ListParams::default())
            .await
            .with_context(|| format!("list roles in {namespace}"))?;
        policies.extend(
            roles
                .items
                .into_iter()
                .flat_map(|role| role.rules.into_iter().flatten()),
        );
    }
    let roles = Api::<ClusterRole>::all(client.clone())
        .list(&ListParams::default())
        .await?;
    policies.extend(
        roles
            .items
            .into_iter()
            .flat_map(|role| role.rules.into_iter().flatten()),
    );
    let mut checks = Vec::new();
    for (resource, discovered_verbs) in &resources {
        let matching = policies.iter().filter(|rule| resource.matches(rule));
        // Discovery omits authorization verbs such as impersonate.
        let verbs: BTreeSet<_> = discovered_verbs
            .iter()
            .map(String::as_str)
            .chain(
                matching
                    .clone()
                    .flat_map(|rule| rule.verbs.iter().map(String::as_str))
                    .filter(|verb| *verb != "*"),
            )
            .collect();
        for verb in verbs {
            let names: BTreeSet<_> = matching
                .clone()
                .filter(|rule| rule.verbs.iter().any(|entry| entry == verb || entry == "*"))
                .flat_map(|rule| rule.resource_names.iter().flatten().map(String::as_str))
                .collect();
            // An unnamed review misses grants scoped to resourceNames.
            checks.push((resource, verb, None));
            checks.extend(names.into_iter().map(|name| (resource, verb, Some(name))));
        }
    }

    for namespace in target {
        let requests = accounts
            .iter()
            .flat_map(|account| checks.iter().map(move |check| (account, check)));
        stream::iter(requests)
            .map(|((account_namespace, account), &(resource, verb, name))| {
                let reviews = &reviews;
                async move {
                    let attributes = resource.attributes(namespace, verb, name);
                    let description = || {
                        format!("{account_namespace}/{account} {verb} {resource:?} {name:?} in {namespace}")
                    };
                    let allowed =
                        service_account_can(reviews, account_namespace, account, attributes)
                            .await
                            .with_context(description)?;
                    ensure!(!allowed, "unexpected access for {}", description());
                    Ok(())
                }
            })
            .buffer_unordered(16)
            .try_for_each(|()| async { Ok(()) })
            .await?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Resource {
    group: String,
    resource: String,
}

impl Resource {
    fn matches(&self, rule: &PolicyRule) -> bool {
        let subresource = self.resource.split_once('/').map(|(_, part)| part);
        rule.api_groups
            .iter()
            .flatten()
            .any(|group| group == &self.group || group == "*")
            && rule.resources.iter().flatten().any(|resource| {
                resource == &self.resource
                    || resource == "*"
                    || resource
                        .strip_prefix("*/")
                        .is_some_and(|part| Some(part) == subresource)
            })
    }

    fn attributes(&self, namespace: &str, verb: &str, name: Option<&str>) -> ResourceAttributes {
        let (resource, subresource) = self.resource.split_once('/').unzip();
        ResourceAttributes {
            namespace: Some(namespace.to_owned()),
            group: Some(self.group.clone()),
            resource: Some(resource.unwrap_or(&self.resource).to_owned()),
            subresource: subresource.map(str::to_owned),
            verb: Some(verb.to_owned()),
            name: name.map(str::to_owned),
            ..ResourceAttributes::default()
        }
    }
}

async fn discover_resources(
    client: &Client,
) -> anyhow::Result<BTreeMap<Resource, BTreeSet<String>>> {
    let discovery = Discovery::new(client.clone()).run().await?;
    let mut resources = BTreeMap::<_, BTreeSet<_>>::new();
    for group in discovery.groups() {
        for version in group.versions() {
            for (resource, caps) in group.versioned_resources(version) {
                if caps.scope != Scope::Namespaced {
                    continue;
                }
                for (subresource, caps) in caps.subresources {
                    resources
                        .entry(Resource {
                            group: group.name().to_owned(),
                            resource: format!("{}/{}", resource.plural, subresource.plural),
                        })
                        .or_default()
                        .extend(caps.operations);
                }
                resources
                    .entry(Resource {
                        group: group.name().to_owned(),
                        resource: resource.plural,
                    })
                    .or_default()
                    .extend(caps.operations);
            }
        }
    }
    Ok(resources)
}

common::journey! {
    /// The org admits a compliant Pod and refuses the same Pod with privilege enabled.
    async fn an_org_refuses_a_privileged_pod() -> anyhow::Result<()> {
        let org = Org::create().await?;
        let pods = Api::<Pod>::namespaced(org.cluster.client.clone(), org.name.as_str());
        let params = PostParams { dry_run: true, ..Default::default() };
        let mut pod = json!({
            "metadata": {"generateName": "pod-security-"},
            "spec": {
                "securityContext": {
                    "runAsNonRoot": true,
                    "runAsUser": 65532,
                    "runAsGroup": 65532,
                    "fsGroup": 65532,
                    "seccompProfile": {"type": "RuntimeDefault"},
                },
                "containers": [{
                    "name": "probe",
                    "image": "registry.k8s.io/pause:3.10",
                    "securityContext": {
                        "allowPrivilegeEscalation": false,
                        "capabilities": {"drop": ["ALL"]},
                        "privileged": false,
                    },
                }],
            },
        });
        pods.create(&params, &Pod::deserialize(&pod)?)
            .await
            .context("the compliant pod must be admitted")?;
        let security_context = pod["spec"]["containers"][0]["securityContext"]
            .as_object_mut()
            .expect("the probe container security context is an object");
        security_context.insert("privileged".into(), true.into());
        security_context.remove("allowPrivilegeEscalation");
        let result = pods.create(&params, &serde_json::from_value(pod)?).await;
        ensure!(
            matches!(&result, Err(kube::Error::Api(status))
                if status.code == 403
                    && status.reason == "Forbidden"
                    && status.message.contains(r#"violates PodSecurity "restricted"#)),
            "the privileged pod must be forbidden in {}: {:?}",
            org.name,
            result.map(|_| ()),
        );
        Ok(())
    }
}

common::journey! {
    /// Deny admits the rendered workload and refuses a Secret reference as the deploy account.
    async fn project_admission_refuses_workload_secret_references() -> anyhow::Result<()> {
        let env = Org::create().await?.apply(FIXTURE).await?;
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps").join(FIXTURE);
        env.deploy(&source, FIXTURE).await?;
        let triple = &env.triple;
        let namespace = env.namespace();
        let resource = kube::api::ApiResource::from_gvk_with_plural(
            &kube::api::GroupVersionKind::gvk("runtime.wasmcloud.dev", "v1alpha1", "WorkloadDeployment"),
            "workloaddeployments",
        );
        let workloads = Api::<kube::api::DynamicObject>::namespaced_with(
            env.org.cluster.client.clone(), &namespace, &resource,
        );
        let release = wamn_registry::resource_names::app_release_name(
            &triple.org, &triple.project, &triple.env, &FIXTURE.parse()?,
        );
        let mut installed = workloads.get(&workload_name(&release)).await?;
        let outputs = &env.org.cluster.outputs;
        let mut config = kube::Config::from_custom_kubeconfig(
            kube::config::Kubeconfig::read_from(&outputs.kubeconfig)?,
            &kube::config::KubeConfigOptions {
                context: Some(outputs.context.clone()),
                ..Default::default()
            },
        ).await?;
        let account = wamn_registry::name::project_operation_account(
            &triple.org, &triple.project, &triple.env,
        );
        config.auth_info.impersonate = Some(format!("system:serviceaccount:{namespace}:{account}"));
        let project = Api::<kube::api::DynamicObject>::namespaced_with(
            Client::try_from(config)?, &namespace, &resource,
        );
        let name = "admission-probe-http";
        let mut candidate = kube::api::DynamicObject::new(name, &resource).within(&namespace);
        candidate.data = json!({"spec": installed.data["spec"].take()});
        let workload = &mut candidate.data["spec"]["template"]["spec"];
        workload["kubernetes"]["service"]["name"] = name.into();
        let params = PostParams { dry_run: true, ..Default::default() };
        project.create(&params, &candidate).await.context("admit the compliant workload as the project account")?;
        candidate.data["spec"]["template"]["spec"]["hostInterfaces"][0]["secretFrom"] =
            json!([{"name":"admission-probe"}]);
        let result = project.create(&params, &candidate).await;
        ensure!(
            matches!(&result, Err(kube::Error::Api(status))
                if status.code == 403 && status.message.contains("wamn-project-operation")),
            "expected denial by the project admission policy, got {:?}",
            result.map(|_| ()),
        );
        Ok(())
    }
}
