//! Kubernetes objects owned by operator env provisioning.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_registry::{OrgId, name};

use crate::{
    org_install::{ENV_SQL_COMPONENT, PLATFORM, env_sql_account},
    provision_org::APPLY_SECRET,
};

/// The namespace label that identifies envs for admission.
pub const ENV_LABEL: &str = "wamn.dev/env";

/// The namespace label that carries the org used in public app host names.
pub use wamn_kubernetes::web_release::ROUTE_ORG_LABEL as ORG_NAME_LABEL;

/// The ServiceAccount used for environment-wide jobs.
pub const ENV_ACCOUNT: &str = "wamn-env";

/// The committed resource budgets and container defaults for one env.
#[derive(Debug, Deserialize, Serialize)]
pub struct Quota {
    pods: i32,
    jobs: i32,
    persistent_volume_claims: i32,
    secrets: i32,
    config_maps: i32,
    services: i32,
    clusters: i32,
    workload_deployments: i32,
    storage_bytes: i64,
    cpu_millicores: i64,
    memory_request_bytes: i64,
    memory_limit_bytes: i64,
    default_cpu_millicores: i64,
    default_memory_request_bytes: i64,
    default_memory_limit_bytes: i64,
}

pub(super) fn namespaces(org: &OrgId, namespace: &str, storage: &str) -> (Value, Value) {
    let labeled = |name| {
        json!({
            "apiVersion":"v1", "kind":"Namespace",
            "metadata":{"name":name,"labels":{
                "app.kubernetes.io/part-of": "wamn",
                (ENV_LABEL): "true",
                (ORG_NAME_LABEL): org,
                "pod-security.kubernetes.io/enforce": "restricted",
                "pod-security.kubernetes.io/warn": "restricted",
            }},
        })
    };
    let mut workloads = labeled(namespace);
    workloads["metadata"]["labels"]["wamn.dev/org-workloads"] = "true".into();
    workloads["metadata"]["labels"]["wamn.dev/identity-trust"] = "true".into();
    let storage = labeled(storage);
    (workloads, storage)
}

/// Build the account and its named Secret access before the env SQL Job starts.
pub fn sql_prerequisites(namespace: &str) -> [Value; 4] {
    let account = env_sql_account(namespace);
    let subject = json!({"kind":"ServiceAccount","name":account,"namespace":PLATFORM});
    let [role, binding] = bind_role(
        namespace,
        ENV_SQL_COMPONENT,
        &[json!({
            "apiGroups":[""], "resources":["secrets"], "resourceNames":[APPLY_SECRET],
            "verbs":["get","update","patch"],
        })],
        &subject,
    );

    [
        json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":account,"namespace":PLATFORM}}),
        // Basic-auth validation requires one key. This shell holds no credential.
        json!({"apiVersion":"v1","kind":"Secret","type":"kubernetes.io/basic-auth",
            "metadata":{"name":APPLY_SECRET,"namespace":namespace},"data":{"username":"cGVuZGluZw=="}}),
        role,
        binding,
    ]
}

pub(super) fn boundary(org: &OrgId, namespace: &str, storage: &str, quota: &Quota) -> Vec<Value> {
    let worker = json!({"kind":"ServiceAccount","name":ENV_ACCOUNT,"namespace":namespace});
    let control =
        json!({"kind":"ServiceAccount","name":name::control_service_account(org),"namespace":org});
    let secret_verbs = ["get", "create", "patch", "delete"];
    let read = ["get", "list", "watch"];
    let manage = [
        "get", "list", "watch", "create", "update", "patch", "delete",
    ];
    let worker_rules = [
        rule("", &["secrets"], &secret_verbs),
        rule("", &["configmaps", "services", "serviceaccounts"], &manage),
        rule("", &["pods", "events", "pods/log"], &read),
        rule(
            "apps",
            &["deployments", "replicasets", "statefulsets"],
            &manage,
        ),
        rule("batch", &["jobs"], &manage),
        rule(
            "rbac.authorization.k8s.io",
            &["roles", "rolebindings"],
            &manage,
        ),
        rule(
            "postgresql.cnpg.io",
            &["clusters", "databases", "publications", "scheduledbackups"],
            &manage,
        ),
        rule(
            "barmancloud.cnpg.io",
            &["objectstores"],
            &["get", "create", "update", "patch", "delete"],
        ),
        rule("cert-manager.io", &["certificates", "issuers"], &manage),
        rule("networking.k8s.io", &["networkpolicies"], &manage),
        rule("runtime.wasmcloud.dev", &["workloaddeployments"], &manage),
        rule("runtime.wasmcloud.dev", &["workloadreplicasets"], &["get"]),
        rule("discovery.k8s.io", &["endpointslices"], &["list"]),
    ];
    let control_rules = [
        json!({
            "apiGroups":[""], "resources":["secrets"], "resourceNames":[crate::provision_org::OAUTH_SECRET],
            "verbs":["get"],
        }),
        rule("batch", &["jobs"], &["create", "get"]),
        rule("", &["pods"], &read),
        rule("", &["pods/log"], &["get"]),
        rule("", &["configmaps"], &["get", "create"]),
        rule("apps", &["deployments"], &read),
        rule("runtime.wasmcloud.dev", &["workloaddeployments"], &read),
    ];
    let quota_name = "wamn-env";
    let mut objects = vec![
        json!({"apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
            "metadata":{"name":"default-deny","namespace":namespace},
            "spec":{"podSelector":{},"policyTypes":["Ingress","Egress"]}}),
        json!({"apiVersion":"v1","kind":"ResourceQuota",
        "metadata":{"name":quota_name,"namespace":namespace},
        "spec":{"hard":{
            "count/pods":quota.pods.to_string(),
            "count/jobs.batch":quota.jobs.to_string(),
            "count/persistentvolumeclaims":quota.persistent_volume_claims.to_string(),
            "count/secrets":quota.secrets.to_string(),
            "count/configmaps":quota.config_maps.to_string(),
            "count/services":quota.services.to_string(),
            "count/clusters.postgresql.cnpg.io":quota.clusters.to_string(),
            "count/workloaddeployments.runtime.wasmcloud.dev":quota.workload_deployments.to_string(),
            "requests.storage":quota.storage_bytes.to_string(),
            "requests.cpu":format!("{}m", quota.cpu_millicores),
            "requests.memory":quota.memory_request_bytes.to_string(),
            "limits.memory":quota.memory_limit_bytes.to_string(),
        }}}),
        json!({"apiVersion":"v1","kind":"LimitRange",
        "metadata":{"name":quota_name,"namespace":namespace},
        "spec":{"limits":[{
            "type":"Container",
            "defaultRequest":{"cpu":format!("{}m", quota.default_cpu_millicores),
                "memory":quota.default_memory_request_bytes.to_string()},
            "default":{"memory":quota.default_memory_limit_bytes.to_string()},
        }]}}),
        json!({"apiVersion":"v1","kind":"ServiceAccount",
            "metadata":{"name":ENV_ACCOUNT,"namespace":namespace}}),
    ];
    objects.extend(bind_role(namespace, ENV_ACCOUNT, &worker_rules, &worker));
    objects.extend(bind_role(
        namespace,
        "wamn-control-operations",
        &control_rules,
        &control,
    ));
    objects.extend(bind_role(
        storage,
        "wamn-helm-releases",
        &[rule(
            "",
            &["secrets"],
            &["get", "list", "create", "update", "delete"],
        )],
        &worker,
    ));
    objects
}

fn rule(group: &str, resources: &[&str], verbs: &[&str]) -> Value {
    json!({"apiGroups":[group],"resources":resources,"verbs":verbs})
}

fn bind_role(namespace: &str, name: &str, rules: &[Value], subject: &Value) -> [Value; 2] {
    let group = "rbac.authorization.k8s.io";
    let api = "rbac.authorization.k8s.io/v1";
    [
        json!({"apiVersion":api,"kind":"Role","metadata":{"name":name,"namespace":namespace},"rules":rules}),
        json!({"apiVersion":api,"kind":"RoleBinding","metadata":{"name":name,"namespace":namespace},
            "roleRef":{"apiGroup":group,"kind":"Role","name":name},"subjects":[subject]}),
    ]
}

pub(super) fn network_policies(
    namespace: &str,
    apply_poolers: [&str; 2],
    database_port: u16,
    api_server: &[Value],
    registry: &[Value],
) -> Vec<Value> {
    let selected = |key: &str, value: &str| json!({"matchLabels":{(key):value}});
    let peer = |namespace: &str, pod_selector: Value| {
        json!({
            "namespaceSelector":selected("kubernetes.io/metadata.name", namespace),"podSelector":pod_selector,
        })
    };
    let worker = selected("app.kubernetes.io/component", "wamn-worker");
    let host = selected("wasmcloud.com/name", "hostgroup");
    let cdc = selected("app.kubernetes.io/name", "wamn-cdc-reader");
    let postgres = json!({"matchExpressions":[{"key":"cnpg.io/cluster","operator":"Exists"}]});
    let tunnel_rules = |destinations: &[Value]| -> Vec<Value> {
        destinations
            .iter()
            .map(|entry| {
                json!({
                    "to":[{"ipBlock":{"cidr":entry["cidr"]}}],
                    "ports":[{"port":entry["port"],"protocol":"TCP"}],
                })
            })
            .collect()
    };
    let platform_rule = |labels, port| {
        json!({
            "to":[peer(PLATFORM, json!({"matchLabels":labels}))],
            "ports":[{"port":port,"protocol":"TCP"}],
        })
    };
    let egress = |name, selector: &Value, rules: &[Value]| {
        json!({
            "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
            "metadata":{"name":name,"namespace":namespace},
            "spec":{"podSelector":selector,"policyTypes":["Egress"],"egress":rules},
        })
    };
    let mut policies = vec![
        egress(
            "allow-dns",
            &json!({}),
            &[json!({
                "to":[peer("kube-system",selected("k8s-app","kube-dns"))],
                "ports":[{"port":53,"protocol":"TCP"},{"port":53,"protocol":"UDP"}],
            })],
        ),
        egress(
            "allow-worker-to-platform",
            &worker,
            &[
                platform_rule(json!({"app.kubernetes.io/name":"wamn-identity"}), 8443),
                platform_rule(
                    json!({"app.kubernetes.io/name":"nats","app.kubernetes.io/instance":"wamn"}),
                    4222,
                ),
                platform_rule(
                    json!({"cnpg.io/poolerName":apply_poolers[0]}),
                    database_port,
                ),
                platform_rule(
                    json!({"cnpg.io/poolerName":apply_poolers[1]}),
                    database_port,
                ),
            ],
        ),
    ];
    let api_rules = tunnel_rules(api_server);
    let registry_rules = tunnel_rules(registry);
    for (name, selector) in [
        ("allow-worker-to-api-server", &worker),
        ("allow-host-to-api-server", &host),
        ("allow-cdc-to-api-server", &cdc),
        ("allow-postgres-to-api-server", &postgres),
    ] {
        policies.push(egress(name, selector, &api_rules));
    }
    for (name, selector) in [
        ("allow-worker-to-registry", &worker),
        ("allow-host-to-registry", &host),
    ] {
        policies.push(egress(name, selector, &registry_rules));
    }
    policies
}
