//! Render a validated project document as OpenTofu JSON without I/O.

use anyhow::{Context as _, bail};
use serde::Deserialize;
use serde_json::{Map, json};

// This version supports the GKE fields and import IDs used below.
// https://github.com/hashicorp/terraform-provider-google/tree/v7.1.0/website/docs/r
const GOOGLE_PROVIDER_VERSION: &str = "7.1.0";

/// Render validated project JSON as the bytes of `main.tf.json`.
///
/// The caller owns project validation and the existing GCS state bucket.
/// Imports adopt the declared cluster and pools. Lifecycle guards refuse
/// replacement of those resources. This function performs no I/O.
///
/// # Errors
///
/// Returns an error when JSON cannot be decoded or a label or taint is malformed.
pub fn render_project_tofu(
    project_json: &[u8],
    state_bucket: &str,
    state_prefix: &str,
) -> anyhow::Result<Vec<u8>> {
    let project: ProjectInput =
        serde_json::from_slice(project_json).context("decode project JSON")?;
    let cluster_id = format!(
        "projects/{}/locations/{}/clusters/{}",
        project.project_id, project.zone, project.cluster_name
    );
    let cluster = json!({
        "name": project.cluster_name,
        "project": project.project_id,
        "location": project.zone,
        "release_channel": {"channel": project.release_channel.to_ascii_uppercase()},
        "network": project.network,
        "subnetwork": project.subnetwork,
        "ip_allocation_policy": {
            "cluster_secondary_range_name": project.pod_secondary_range_name,
            "services_secondary_range_name": project.service_secondary_range_name,
        },
        "workload_identity_config": {
            "workload_pool": format!("{}.svc.id.goog", project.project_id),
        },
        "logging_config": {"enable_components": ["SYSTEM_COMPONENTS"]},
        "monitoring_config": {
            "enable_components": ["SYSTEM_COMPONENTS"],
            "managed_prometheus": {"enabled": false},
        },
        "remove_default_node_pool": true,
        "initial_node_count": 1,
        "deletion_protection": true,
        "lifecycle": {"prevent_destroy": true},
    });
    let mut imports = vec![json!({"to": "google_container_cluster.cluster", "id": cluster_id})];
    let mut pools = Map::new();
    for pool in project.node_pools {
        let mut labels = Map::new();
        if !pool.label.is_empty() {
            let (key, value) = pool
                .label
                .split_once('=')
                .context("node label needs key=value")?;
            labels.insert(key.to_owned(), json!(value));
        }
        let mut taints = Vec::new();
        if !pool.taint.is_empty() {
            let (label, effect) = pool
                .taint
                .split_once(':')
                .context("node taint needs an effect")?;
            let (key, value) = label
                .split_once('=')
                .context("node taint needs key=value")?;
            let effect = match effect {
                "NoSchedule" => "NO_SCHEDULE",
                "PreferNoSchedule" => "PREFER_NO_SCHEDULE",
                "NoExecute" => "NO_EXECUTE",
                _ => bail!("unknown node taint effect {effect:?}"),
            };
            taints.push(json!({"key": key, "value": value, "effect": effect}));
        }
        let mut resource = json!({
            "name": pool.name,
            "project": project.project_id,
            "location": project.zone,
            "cluster": "${google_container_cluster.cluster.name}",
            "node_count": pool.size,
            "node_config": {
                "machine_type": pool.machine_type,
                "spot": pool.spot,
                "disk_type": pool.disk_type,
                "disk_size_gb": pool.disk_size,
                "labels": labels,
                "taint": taints,
                "service_account": project.node_service_account,
            },
            "lifecycle": {"prevent_destroy": true},
        });
        if pool.spot {
            resource["upgrade_settings"] = json!({"max_surge": 0, "max_unavailable": 1});
        }
        imports.push(json!({
            "to": format!("google_container_node_pool.{}", pool.name),
            "id": format!("{}/{}/{}/{}", project.project_id, project.zone, project.cluster_name, pool.name),
        }));
        pools.insert(pool.name, resource);
    }
    let document = json!({
        "terraform": {
            "required_providers": {
                "google": {"source": "hashicorp/google", "version": GOOGLE_PROVIDER_VERSION},
            },
            "backend": {"gcs": {"bucket": state_bucket, "prefix": state_prefix}},
        },
        "resource": {
            "google_container_cluster": {"cluster": cluster},
            "google_container_node_pool": pools,
        },
        "import": imports,
    });
    let mut bytes = serde_json::to_vec_pretty(&document).context("encode OpenTofu JSON")?;
    bytes.push(b'\n');
    Ok(bytes)
}

// The control crate owns validation. These private types decode its JSON
// without adding a dependency from the generator back to control.
#[derive(Deserialize)]
struct ProjectInput {
    project_id: String,
    zone: String,
    cluster_name: String,
    release_channel: String,
    network: String,
    subnetwork: String,
    pod_secondary_range_name: String,
    service_secondary_range_name: String,
    node_service_account: String,
    node_pools: Vec<NodePool>,
}

#[derive(Deserialize)]
struct NodePool {
    name: String,
    machine_type: String,
    spot: bool,
    disk_type: String,
    disk_size: u32,
    size: u32,
    #[serde(default)]
    label: String,
    #[serde(default)]
    taint: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::Path;

    fn project() -> Vec<u8> {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/gcp/project.k");
        crate::compile_schema_file(&file, "project").expect("compile the committed project")
    }

    #[test]
    fn committed_project_renders_cluster_pools_backend_and_imports() {
        let source = project();
        let declared: Value = serde_json::from_slice(&source).unwrap();
        let bytes = render_project_tofu(&source, "owner-state", "gcp/dev").unwrap();
        let rendered: Value = serde_json::from_slice(&bytes).expect("rendered JSON");
        assert_eq!(
            rendered["terraform"]["backend"],
            json!({
                "gcs": {"bucket": "owner-state", "prefix": "gcp/dev"},
            })
        );
        assert_eq!(
            rendered["terraform"]["required_providers"]["google"],
            json!({
                "source": "hashicorp/google", "version": "7.1.0",
            })
        );
        let resources = &rendered["resource"];
        assert_eq!(resources.as_object().unwrap().len(), 2);
        let clusters = resources["google_container_cluster"].as_object().unwrap();
        assert_eq!(clusters.len(), 1);
        let cluster = &clusters["cluster"];
        assert_eq!(cluster["name"], "wamn");
        assert_eq!(cluster["project"], "wamn-dev");
        assert_eq!(cluster["location"], "us-central1-a");
        assert_eq!(cluster["network"], "wamn");
        assert_eq!(cluster["subnetwork"], "wamn-us-central1");
        assert_eq!(cluster["release_channel"]["channel"], "REGULAR");
        assert_eq!(
            cluster["ip_allocation_policy"],
            json!({
                "cluster_secondary_range_name": "pods", "services_secondary_range_name": "services",
            })
        );
        assert_eq!(
            cluster["workload_identity_config"]["workload_pool"],
            "wamn-dev.svc.id.goog"
        );
        assert_eq!(
            cluster["logging_config"]["enable_components"],
            json!(["SYSTEM_COMPONENTS"])
        );
        assert_eq!(
            cluster["monitoring_config"],
            json!({
                "enable_components": ["SYSTEM_COMPONENTS"], "managed_prometheus": {"enabled": false},
            })
        );
        assert_eq!(cluster["remove_default_node_pool"], true);
        assert_eq!(cluster["initial_node_count"], 1);
        assert_eq!(cluster["deletion_protection"], true);
        assert_eq!(cluster["lifecycle"]["prevent_destroy"], true);

        let pools = resources["google_container_node_pool"].as_object().unwrap();
        let declared_pools = declared["node_pools"].as_array().unwrap();
        assert_eq!(pools.len(), declared_pools.len());
        for source in declared_pools {
            let pool = &pools[source["name"].as_str().unwrap()];
            assert_eq!(pool["name"], source["name"]);
            assert_eq!(pool["project"], "wamn-dev");
            assert_eq!(pool["location"], "us-central1-a");
            assert_eq!(pool["cluster"], "${google_container_cluster.cluster.name}");
            assert_eq!(pool["node_count"], source["size"]);
            assert!(pool.get("initial_node_count").is_none());
            assert!(pool.get("autoscaling").is_none());
            assert_eq!(pool["lifecycle"]["prevent_destroy"], true);
            for field in ["machine_type", "spot", "disk_type"] {
                assert_eq!(pool["node_config"][field], source[field]);
            }
            assert_eq!(pool["node_config"]["disk_size_gb"], source["disk_size"]);
            assert_eq!(
                pool["node_config"]["service_account"],
                declared["node_service_account"]
            );
            if source["spot"] == true {
                assert_eq!(
                    pool["upgrade_settings"],
                    json!({"max_surge": 0, "max_unavailable": 1})
                );
            } else {
                assert!(pool.get("upgrade_settings").is_none());
            }
        }
        assert_eq!(pools["main"]["node_config"]["labels"], json!({}));
        assert_eq!(pools["main"]["node_config"]["taint"], json!([]));
        for (name, role) in [("bench", "bench-host"), ("pg", "postgres")] {
            assert_eq!(pools[name]["node_config"]["labels"], json!({"role": role}));
            assert_eq!(
                pools[name]["node_config"]["taint"],
                json!([
                    {"key": "role", "value": role, "effect": "NO_SCHEDULE"},
                ])
            );
        }
        assert_eq!(
            rendered["import"],
            json!([
                {"to": "google_container_cluster.cluster", "id": "projects/wamn-dev/locations/us-central1-a/clusters/wamn"},
                {"to": "google_container_node_pool.main", "id": "wamn-dev/us-central1-a/wamn/main"},
                {"to": "google_container_node_pool.bench", "id": "wamn-dev/us-central1-a/wamn/bench"},
                {"to": "google_container_node_pool.pg", "id": "wamn-dev/us-central1-a/wamn/pg"},
            ])
        );
    }

    #[test]
    fn zero_size_and_each_taint_effect_survive_rendering() {
        let mut source: Value = serde_json::from_slice(&project()).unwrap();
        for (effect, expected) in [
            ("PreferNoSchedule", "PREFER_NO_SCHEDULE"),
            ("NoExecute", "NO_EXECUTE"),
        ] {
            source["node_pools"][0]["size"] = json!(0);
            source["node_pools"][0]["taint"] = json!(format!("role=main:{effect}"));
            let bytes =
                render_project_tofu(&serde_json::to_vec(&source).unwrap(), "state", "dev").unwrap();
            let rendered: Value = serde_json::from_slice(&bytes).unwrap();
            let pool = &rendered["resource"]["google_container_node_pool"]["main"];
            assert_eq!(pool["node_count"], 0);
            assert_eq!(pool["node_config"]["taint"][0]["effect"], expected);
        }
    }
}
