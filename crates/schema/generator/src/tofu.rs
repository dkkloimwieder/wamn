//! Render a validated project document as OpenTofu JSON without I/O.

use anyhow::{Context as _, bail};
use serde::Deserialize;
use serde_json::{Map, json};

// This version supports the GKE fields used below.
// https://github.com/hashicorp/terraform-provider-google/tree/v7.1.0/website/docs/r
const GOOGLE_PROVIDER_VERSION: &str = "7.1.0";

/// Render validated project JSON as the bytes of `main.tf.json`.
///
/// The caller owns project validation and the existing GCS state bucket.
/// It writes no import block for the cluster or pools, because an import of
/// a missing cluster or pool refuses the plan. It imports the kind registry,
/// the DNS zone and the guard's state binding, which outlive a teardown.
/// Lifecycle guards refuse their destruction and the replacement of the
/// cluster and pools. This function performs no I/O.
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
    let (region, _) = project
        .zone
        .rsplit_once('-')
        .expect("validated zone has a zone suffix");
    let cluster = json!({
        "name": project.cluster_name,
        "project": project.project_id,
        "location": project.zone,
        "release_channel": {"channel": project.release_channel.to_ascii_uppercase()},
        "network": format!("projects/{}/global/networks/{}", project.project_id, project.network),
        "subnetwork": format!("projects/{}/regions/{region}/subnetworks/{}", project.project_id, project.subnetwork),
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
        "lifecycle": {"prevent_destroy": true, "ignore_changes": ["initial_node_count", "remove_default_node_pool"]},
    });
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
            "lifecycle": {"prevent_destroy": true, "ignore_changes": ["node_count", "node_config[0].taint"]},
        });
        if pool.spot {
            resource["upgrade_settings"] = json!({"max_surge": 0, "max_unavailable": 1});
        }
        pools.insert(pool.name, resource);
    }
    let registry = &project.kind_registry;
    let zone = &project.dns_zone;
    let guard = &project.guard_state;
    let domain = zone.dns_name.trim_end_matches('.');
    let guard_member = format!("serviceAccount:{}", guard.service_account);
    let imports = json!([
        {
            "to": "google_artifact_registry_repository.kind",
            "id": format!(
                "projects/{}/locations/{}/repositories/{}",
                project.project_id, registry.location, registry.repository_id
            ),
        },
        {
            "to": "google_dns_managed_zone.zone",
            "id": format!("projects/{}/managedZones/{}", project.project_id, zone.name),
        },
        {
            "to": "google_storage_bucket_iam_member.guard_state",
            "id": format!(
                "b/{} roles/storage.objectUser {guard_member} guard-state",
                guard.bucket
            ),
        },
    ]);
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
            "google_compute_health_check": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
                "tcp_health_check": {"port_specification": "USE_SERVING_PORT"},
            }},
            "google_certificate_manager_dns_authorization": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
                "domain": domain,
            }},
            "google_certificate_manager_certificate": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
                "managed": {
                    "domains": [domain, format!("*.{domain}")],
                    "dns_authorizations": ["${google_certificate_manager_dns_authorization.edge.id}"],
                },
            }},
            "google_certificate_manager_certificate_map": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
            }},
            "google_certificate_manager_certificate_map_entry": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
                "map": "${google_certificate_manager_certificate_map.edge.name}",
                "certificates": ["${google_certificate_manager_certificate.edge.id}"],
                "matcher": "PRIMARY",
            }},
            // A new address, so that the cutover creates it while the TCP
            // proxy still holds the old backend service.
            "google_compute_backend_service": {"edge_https": {
                "name": "wamn-edge", "project": project.project_id,
                "protocol": "HTTPS", "load_balancing_scheme": "EXTERNAL_MANAGED",
                "health_checks": ["${google_compute_health_check.edge.id}"],
                "backend": [{
                    "group": "${data.google_compute_network_endpoint_group.edge.self_link}",
                    "balancing_mode": "RATE", "max_rate_per_endpoint": 1000,
                }],
                "enable_cdn": true,
                // The provider needs a cache key policy. These are the API
                // defaults; the host keeps each route host's entries apart.
                "cdn_policy": {
                    "cache_mode": "USE_ORIGIN_HEADERS",
                    "cache_key_policy": {
                        "include_host": true, "include_protocol": true, "include_query_string": true,
                    },
                },
            }},
            "google_compute_url_map": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
                "default_service": "${google_compute_backend_service.edge_https.id}",
            }},
            "google_compute_target_https_proxy": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
                "url_map": "${google_compute_url_map.edge.id}",
                "certificate_map": "//certificatemanager.googleapis.com/${google_certificate_manager_certificate_map.edge.id}",
            }},
            "google_compute_global_address": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
            }},
            "google_compute_global_forwarding_rule": {"edge": {
                "name": "wamn-edge", "project": project.project_id,
                "ip_address": "${google_compute_global_address.edge.address}",
                "target": "${google_compute_target_https_proxy.edge.id}",
                "port_range": "443", "ip_protocol": "TCP",
                "load_balancing_scheme": "EXTERNAL_MANAGED",
            }},
            "google_artifact_registry_repository": {"kind": {
                "project": project.project_id,
                "location": registry.location,
                "repository_id": registry.repository_id,
                "description": registry.description,
                "format": "DOCKER",
                "docker_config": {"immutable_tags": true},
                "cleanup_policies": [{
                    "id": format!("delete-older-than-{}-days", registry.cleanup_older_than_days),
                    "action": "DELETE",
                    "condition": {
                        "tag_state": "ANY",
                        "older_than": format!("{}s", u64::from(registry.cleanup_older_than_days) * 86_400),
                    },
                }],
                "lifecycle": {"prevent_destroy": true},
            }},
            "google_dns_managed_zone": {"zone": {
                "project": project.project_id,
                "name": zone.name,
                "dns_name": zone.dns_name,
                "description": zone.description,
                "visibility": "public",
                "lifecycle": {"prevent_destroy": true},
            }},
            "google_storage_bucket_iam_member": {"guard_state": {
                "bucket": guard.bucket,
                "role": "roles/storage.objectUser",
                "member": guard_member,
                "condition": {
                    "title": "guard-state",
                    "expression": format!(
                        "resource.name.startsWith(\"projects/_/buckets/{}/objects/guard/\")",
                        guard.bucket
                    ),
                },
                "lifecycle": {"prevent_destroy": true},
            }},
        },
        "import": imports,
        // The DNS authorization's CNAME, which the zone needs once.
        "output": {
            "edge_dns_authorization_name": {
                "value": "${google_certificate_manager_dns_authorization.edge.dns_resource_record[0].name}",
            },
            "edge_dns_authorization_data": {
                "value": "${google_certificate_manager_dns_authorization.edge.dns_resource_record[0].data}",
            },
        },
        "data": {
            "google_compute_network_endpoint_group": {"edge": {
                "name": project.edge.neg_name, "zone": project.edge.neg_zone,
                "project": project.project_id,
            }},
        },
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
    edge: Edge,
    kind_registry: KindRegistry,
    dns_zone: DnsZone,
    guard_state: GuardState,
}

#[derive(Deserialize)]
struct KindRegistry {
    repository_id: String,
    location: String,
    description: String,
    cleanup_older_than_days: u32,
}

#[derive(Deserialize)]
struct DnsZone {
    name: String,
    dns_name: String,
    description: String,
}

#[derive(Deserialize)]
struct GuardState {
    bucket: String,
    service_account: String,
}

#[derive(Deserialize)]
struct Edge {
    neg_name: String,
    neg_zone: String,
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
    fn edge_https_proxy_serves_the_service_neg_through_the_cdn() {
        let mut source: Value = serde_json::from_slice(&project()).unwrap();
        source["edge"]["neg_zone"] = json!("us-east1-b");
        let rendered: Value = serde_json::from_slice(
            &render_project_tofu(&serde_json::to_vec(&source).unwrap(), "state", "dev").unwrap(),
        )
        .unwrap();
        assert_eq!(
            rendered["data"]["google_compute_network_endpoint_group"]["edge"],
            json!({
                "name": "wamn-edge", "zone": "us-east1-b", "project": "wamn-dev"
            })
        );
        let resources = &rendered["resource"];
        assert_eq!(
            resources["google_compute_health_check"]["edge"]["tcp_health_check"],
            json!({"port_specification": "USE_SERVING_PORT"})
        );
        let backends = resources["google_compute_backend_service"]
            .as_object()
            .unwrap();
        assert_eq!(backends.len(), 1);
        let backend = &backends["edge_https"];
        assert_eq!(backend["name"], "wamn-edge");
        assert_eq!(backend["protocol"], "HTTPS");
        assert_eq!(backend["load_balancing_scheme"], "EXTERNAL_MANAGED");
        assert_eq!(
            backend["health_checks"],
            json!(["${google_compute_health_check.edge.id}"])
        );
        assert_eq!(
            backend["backend"],
            json!([{
                "group": "${data.google_compute_network_endpoint_group.edge.self_link}",
                "balancing_mode": "RATE", "max_rate_per_endpoint": 1000,
            }])
        );
        assert_eq!(backend["enable_cdn"], true);
        assert_eq!(
            backend["cdn_policy"],
            json!({
                "cache_mode": "USE_ORIGIN_HEADERS",
                "cache_key_policy": {
                    "include_host": true, "include_protocol": true, "include_query_string": true,
                },
            })
        );
        assert_eq!(
            resources["google_compute_url_map"]["edge"],
            json!({
                "name": "wamn-edge", "project": "wamn-dev",
                "default_service": "${google_compute_backend_service.edge_https.id}",
            })
        );
        assert_eq!(
            resources["google_compute_target_https_proxy"]["edge"],
            json!({
                "name": "wamn-edge", "project": "wamn-dev",
                "url_map": "${google_compute_url_map.edge.id}",
                "certificate_map": "//certificatemanager.googleapis.com/${google_certificate_manager_certificate_map.edge.id}",
            })
        );
        let forwarding = &resources["google_compute_global_forwarding_rule"]["edge"];
        assert_eq!(
            forwarding["target"],
            "${google_compute_target_https_proxy.edge.id}"
        );
        assert_eq!(
            forwarding["ip_address"],
            "${google_compute_global_address.edge.address}"
        );
        assert_eq!(forwarding["port_range"], "443");
        assert_eq!(
            forwarding["load_balancing_scheme"],
            backend["load_balancing_scheme"]
        );
        for absent in [
            "google_compute_target_tcp_proxy",
            "google_compute_ssl_certificate",
            "google_compute_managed_ssl_certificate",
            "google_compute_backend_bucket",
            "google_compute_network_endpoint_group",
        ] {
            assert!(resources.get(absent).is_none(), "{absent}");
        }
        for rule in ["host_rule", "path_matcher"] {
            assert!(
                resources["google_compute_url_map"]["edge"]
                    .get(rule)
                    .is_none()
            );
        }
    }

    #[test]
    fn edge_certificate_covers_the_zone_domain_and_its_wildcard() {
        let rendered: Value =
            serde_json::from_slice(&render_project_tofu(&project(), "state", "dev").unwrap())
                .unwrap();
        let resources = &rendered["resource"];
        assert_eq!(
            resources["google_certificate_manager_dns_authorization"]["edge"],
            json!({"name": "wamn-edge", "project": "wamn-dev", "domain": "wamn.dev"})
        );
        assert_eq!(
            resources["google_certificate_manager_certificate"]["edge"],
            json!({
                "name": "wamn-edge", "project": "wamn-dev",
                "managed": {
                    "domains": ["wamn.dev", "*.wamn.dev"],
                    "dns_authorizations": ["${google_certificate_manager_dns_authorization.edge.id}"],
                },
            })
        );
        assert_eq!(
            resources["google_certificate_manager_certificate_map"]["edge"],
            json!({"name": "wamn-edge", "project": "wamn-dev"})
        );
        assert_eq!(
            resources["google_certificate_manager_certificate_map_entry"]["edge"],
            json!({
                "name": "wamn-edge", "project": "wamn-dev",
                "map": "${google_certificate_manager_certificate_map.edge.name}",
                "certificates": ["${google_certificate_manager_certificate.edge.id}"],
                "matcher": "PRIMARY",
            })
        );
        assert_eq!(
            rendered["output"],
            json!({
                "edge_dns_authorization_name": {
                    "value": "${google_certificate_manager_dns_authorization.edge.dns_resource_record[0].name}",
                },
                "edge_dns_authorization_data": {
                    "value": "${google_certificate_manager_dns_authorization.edge.dns_resource_record[0].data}",
                },
            })
        );
    }

    #[test]
    fn committed_project_renders_cluster_pools_and_backend_without_imports() {
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
        assert_eq!(resources.as_object().unwrap().len(), 15);
        let clusters = resources["google_container_cluster"].as_object().unwrap();
        assert_eq!(clusters.len(), 1);
        let cluster = &clusters["cluster"];
        assert_eq!(cluster["name"], "wamn");
        assert_eq!(cluster["project"], "wamn-dev");
        assert_eq!(cluster["location"], "us-central1-a");
        assert_eq!(cluster["network"], "projects/wamn-dev/global/networks/wamn");
        assert_eq!(
            cluster["subnetwork"],
            "projects/wamn-dev/regions/us-central1/subnetworks/wamn-us-central1"
        );
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
        assert_eq!(
            cluster["lifecycle"]["ignore_changes"],
            json!(["initial_node_count", "remove_default_node_pool"])
        );

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
            assert_eq!(
                pool["lifecycle"]["ignore_changes"],
                json!(["node_count", "node_config[0].taint"])
            );
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
        // An import of a missing cluster or pool refuses the plan (wamn-02nkn),
        // so only the three project resources that outlive a teardown import.
        assert_eq!(
            rendered["import"],
            json!([
                {
                    "to": "google_artifact_registry_repository.kind",
                    "id": "projects/wamn-dev/locations/us-central1/repositories/wamn-kind",
                },
                {
                    "to": "google_dns_managed_zone.zone",
                    "id": "projects/wamn-dev/managedZones/wamn-dev",
                },
                {
                    "to": "google_storage_bucket_iam_member.guard_state",
                    "id": "b/wamn-dev-tofu-state roles/storage.objectUser \
                           serviceAccount:wamn-guard@wamn-dev.iam.gserviceaccount.com guard-state",
                },
            ])
        );
    }

    #[test]
    fn project_resources_match_the_live_objects_and_refuse_destruction() {
        let rendered: Value =
            serde_json::from_slice(&render_project_tofu(&project(), "state", "dev").unwrap())
                .unwrap();
        let resources = &rendered["resource"];
        assert_eq!(
            resources["google_artifact_registry_repository"]["kind"],
            json!({
                "project": "wamn-dev", "location": "us-central1", "repository_id": "wamn-kind",
                "description": "kind test pushes of upgrade-environment", "format": "DOCKER",
                "docker_config": {"immutable_tags": true},
                "cleanup_policies": [{
                    "id": "delete-older-than-7-days", "action": "DELETE",
                    "condition": {"tag_state": "ANY", "older_than": "604800s"},
                }],
                "lifecycle": {"prevent_destroy": true},
            })
        );
        assert_eq!(
            resources["google_dns_managed_zone"]["zone"],
            json!({
                "project": "wamn-dev", "name": "wamn-dev", "dns_name": "wamn.dev.",
                "description": "wamn.dev public zone", "visibility": "public",
                "lifecycle": {"prevent_destroy": true},
            })
        );
        assert_eq!(
            resources["google_storage_bucket_iam_member"]["guard_state"],
            json!({
                "bucket": "wamn-dev-tofu-state", "role": "roles/storage.objectUser",
                "member": "serviceAccount:wamn-guard@wamn-dev.iam.gserviceaccount.com",
                "condition": {
                    "title": "guard-state",
                    "expression": "resource.name.startsWith(\"projects/_/buckets/wamn-dev-tofu-state/objects/guard/\")",
                },
                "lifecycle": {"prevent_destroy": true},
            })
        );
        // The state bucket stays out of OpenTofu: it holds OpenTofu's own state.
        assert!(resources.get("google_storage_bucket").is_none());
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
