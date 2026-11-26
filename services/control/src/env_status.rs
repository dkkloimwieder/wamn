//! `env.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Shared host rollout and project-owned workload conditions come from Kubernetes.
//! Workload annotations give the package digest and content revision.
//! Application history remains in Helm, independently of runtime placement.
use anyhow::Context as _;
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Pod};
use kube::{
    Api,
    api::{DynamicObject, ListParams},
};
use wamn_app::app_config::AppConfig;
use wamn_env::status::{self, HostDeployment, HostRollout, Workload};
use wamn_kubernetes::client::{KubernetesApi, workloads};
use wamn_registry::{EnvName, OrgId, ProjectId};
use wamn_routes::host_wire::EnvStatusResult;

use crate::control_route::Refusal;

/// Read runtime status through the control pod's Kubernetes ServiceAccount.
pub(crate) async fn read(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> anyhow::Result<EnvStatusResult> {
    let api = KubernetesApi::in_cluster()?;
    let namespace = &api.namespace;
    let params = ListParams::default();
    let deployments = Api::<Deployment>::namespaced(api.client(), namespace);
    let deployments = tokio::time::timeout(
        wamn_kubernetes::client::REQUEST_TIMEOUT,
        deployments.list(&params),
    )
    .await??;
    let workload_api = workloads(api.client(), namespace);
    let workload_list = tokio::time::timeout(
        wamn_kubernetes::client::REQUEST_TIMEOUT,
        workload_api.list(&params),
    )
    .await??;
    // Keep the Pod permission check in the status read.
    let pods = Api::<Pod>::namespaced(api.client(), namespace);
    drop(
        tokio::time::timeout(wamn_kubernetes::client::REQUEST_TIMEOUT, pods.list(&params))
            .await??,
    );

    let deployments = deployments.items.iter().map(|deployment| {
        let env = deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|meta| meta.annotations.as_ref())
            .and_then(|values| values.get("wamn.env"))
            .map(String::as_str);
        let rollout = deployment
            .status
            .as_ref()
            .map_or_else(HostRollout::default, |status| HostRollout {
                replicas: status.replicas,
                updated_replicas: status.updated_replicas,
                ready_replicas: status.ready_replicas,
                available_replicas: status.available_replicas,
            });
        HostDeployment {
            name: deployment.metadata.name.as_deref(),
            env,
            rollout,
        }
    });
    status::runtime_status(
        org,
        project,
        env,
        deployments,
        workload_list.items.iter().map(workload_data),
    )
}

/// Read application bindings for connection deletion, refusing unready workloads.
pub(crate) async fn apps(
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
) -> Result<Vec<AppConfig>, Refusal> {
    let api =
        KubernetesApi::in_cluster().context("connect to Kubernetes for application bindings")?;
    let workloads = workloads(api.client(), org.as_str());
    let list = tokio::time::timeout(
        wamn_kubernetes::client::REQUEST_TIMEOUT,
        workloads.list(&ListParams::default()),
    )
    .await
    .context("time out listing application workloads")?
    .context("list application workloads")?;
    apps_for_delete(org, project, env, list.items.iter().map(workload_data))
}

fn apps_for_delete<'a>(
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
    workloads: impl IntoIterator<Item = Workload<'a>>,
) -> Result<Vec<AppConfig>, Refusal> {
    status::apps(org, project, env, workloads)?.ok_or_else(|| Refusal::Declared {
        code: "workload_not_ready",
        detail: serde_json::json!({"field":"env"}),
    })
}

fn workload_data(item: &DynamicObject) -> Workload<'_> {
    Workload {
        name: item.metadata.name.as_deref(),
        annotations: item.metadata.annotations.as_ref(),
        data: &item.data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use wamn_app::app_config::APP_CONFIG;

    #[test]
    fn connection_delete_refuses_an_unready_workload_in_the_selected_env() {
        let app = AppConfig {
            org: "test-org".parse().unwrap(),
            project: "billing".parse().unwrap(),
            env: "dev".parse().unwrap(),
            app: "billing".parse().unwrap(),
            grant_cache_seconds: 1,
            components: Vec::new(),
        };
        let coordinate = wamn_registry::name::host_status(&app.org, &app.project, &app.env);
        let annotations = [("wamn.env".to_owned(), coordinate)].into();
        let other_coordinate =
            wamn_registry::name::host_status(&app.org, &"other-project".parse().unwrap(), &app.env);
        let other_annotations = [("wamn.env".to_owned(), other_coordinate)].into();
        let mut data = json!({"spec":{"template":{"spec":{"components":[
            {"localResources":{"config":{(APP_CONFIG):serde_json::to_string(&app).unwrap()}}}
        ]}}}});
        let read = |data: &Value| {
            apps_for_delete(
                &app.org,
                &app.project,
                &app.env,
                [
                    Workload {
                        name: Some("other-project"),
                        annotations: Some(&other_annotations),
                        data: &Value::Null,
                    },
                    Workload {
                        name: Some("billing-http"),
                        annotations: Some(&annotations),
                        data,
                    },
                ],
            )
        };
        for conditions in [json!([]), json!([{"type":"Ready","status":"False"}])] {
            data["status"] = json!({"conditions":conditions});
            let refusal = read(&data).unwrap_err();
            assert!(
                matches!(refusal, Refusal::Declared { code: "workload_not_ready", detail }
                if detail == json!({"field":"env"}))
            );
        }
        data["status"] = json!({"conditions":[{"type":"Ready","status":"True"}]});
        assert_eq!(read(&data).unwrap(), [app]);

        let api = wamn_routes::HostRouteSet::Control.openapi();
        let errors =
            api["paths"]["/wamn_control/connection/delete"]["post"]["x-wamn-contract"]["errors"]
                .as_array()
                .expect("connection.delete declares its refusal codes");
        assert!(
            errors
                .iter()
                .any(|error| error["literal"] == "workload_not_ready")
        );
    }
}
