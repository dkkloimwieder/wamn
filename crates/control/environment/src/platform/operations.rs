//! Prepared inputs and namespace grants for server-authorized operation Jobs.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_release_control::release_chart::Target;

use super::{
    artifact_plan::SelectedChart,
    document::{ClusterDocument, Provider},
    resources,
};
use crate::environment::{Platform, document::EnvironmentDocument};

/// Store the image selection, chart values, and registry inputs for the worker.
///
/// # Errors
/// Returns filesystem, credential, or Kubernetes errors.
pub(super) async fn prepare(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
) -> anyhow::Result<()> {
    let registry = if document.provider == Provider::Kind {
        resources::POD_REGISTRY
    } else {
        document.registry.as_str()
    };
    let configuration = json!({"insecure_registry":document.provider == Provider::Kind,
        "package_base":format!("{registry}/wamn/packages"),"component_base":format!("{registry}/wamn/components")});
    let data = json!({"image":chart.image("ctl")?,
        "release-base":format!("{registry}/wamn/releases"),"configuration.json":configuration.to_string(),
        "Chart.yaml":std::fs::read_to_string(chart.directory().join("Chart.yaml"))?,
        "values.yaml":std::fs::read_to_string(chart.directory().join("values.yaml"))?});
    let digest = wamn_digest::bytes_sha256(&wamn_digest::canonical_json_bytes(&data));
    let inputs = format!("wamn-operation-{}-{}", document.org, &digest[7..23]);
    let object = json!({"apiVersion":"v1","kind":"ConfigMap","immutable":true,
        "metadata":{"name":inputs,"namespace":"platform"},"data":data});
    if document.provider == Provider::Kind {
        let secret: super::bootstrap::SecretData = wamn_kubernetes::kubectl::get(
            &target.kubeconfig,
            &target.context,
            "hosts",
            "secret",
            "wamn-registry-pull",
        )
        .await?
        .context("the platform registry credential is absent")?;
        let registry_json = super::bootstrap::secret_text(&secret, ".dockerconfigjson")?;
        resources::apply_object(target,&json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":format!("wamn-operation-registry-{}",document.org),"namespace":"platform"},"stringData":{"config.json":registry_json}})).await?;
    }
    resources::apply_object(target, &object).await?;
    resources::apply_object(
        target,
        &json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":{"name":format!("wamn-operation-{}",document.org),"namespace":"platform"},
        "data":{"inputs":inputs}}),
    )
    .await
}

/// Prepare the chart carried by the ctl image with the server-selected values.
///
/// # Errors
/// Returns an error if the image payload or selected chart metadata cannot be read.
pub(crate) fn worker_chart(repository: &Path, inputs: &Path, chart: &Path) -> anyhow::Result<()> {
    crate::platform_build::copy_package_output(&repository.join("deploy/platform/release"), chart)?;
    crate::platform_build::prepare_payload(repository, chart)?;
    for file in ["Chart.yaml", "values.yaml"] {
        std::fs::copy(inputs.join(file), chart.join(file))?;
    }
    Ok(())
}

/// The accounts and Roles of one environment's code and privileged SQL workers.
/// Their credentials reuse the existing platform identities.
///
/// # Errors
/// Returns an error for an invalid environment coordinate.
pub fn target_objects(
    document: &EnvironmentDocument,
    cluster: &str,
    database: &str,
) -> anyhow::Result<Vec<Value>> {
    let coordinate = wamn_control_provision::environment_names::environment_resource_name(
        &document.org,
        &document.project,
        &document.env,
    )?;
    let mut objects = vec![
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":format!("{coordinate}-operations"),"namespace":"platform"},"data":{"cluster":cluster}}),
    ];
    for privileged in [false, true] {
        let name = format!("{coordinate}-{}", if privileged { "sql" } else { "code" });
        objects.push(json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":name,"namespace":"platform"}}));
        let mut host_rules = vec![
            // Helm Secret storage reads revisions, writes status, and prunes history.
            json!({"apiGroups":[""],"resources":["secrets"],"verbs":["get","list","create","update","delete"]}),
            // Chart web ConfigMaps: Helm lifecycle and waits; edge conflict listing.
            json!({"apiGroups":[""],"resources":["configmaps"],"verbs":["get","list","watch","create","patch","delete"]}),
            json!({"apiGroups":[""],"resources":["services"],"verbs":["get","list","watch","create","update","patch","delete"]}),
            json!({"apiGroups":[""],"resources":["pods","events"],"verbs":["get","list","watch"]}),
            json!({"apiGroups":["apps"],"resources":["deployments"],"verbs":["get","list","watch","create","update","patch","delete"]}),
            json!({"apiGroups":["apps"],"resources":["replicasets"],"verbs":["get","list","watch"]}),
            // Environment and release charts render one NetworkPolicy each.
            json!({"apiGroups":["networking.k8s.io"],"resources":["networkpolicies"],"verbs":["get","list","watch","create","update","patch","delete"]}),
            // Runtime deployment refuses to start while the platform edge has no ready endpoint.
            json!({"apiGroups":["discovery.k8s.io"],"resources":["endpointslices"],"verbs":["list"]}),
            json!({"apiGroups":["runtime.wasmcloud.dev"],"resources":["workloaddeployments"],"verbs":["get","list","watch","create","update","patch","delete"]}),
        ];
        if privileged {
            host_rules.extend([
                // SQL Job input and emitted credential handoff use server-side apply.
                json!({"apiGroups":[""],"resources":["secrets"],"verbs":["patch"]}),
                json!({"apiGroups":["batch"],"resources":["jobs"],"verbs":["create","get","list","watch","patch","delete"]}),
                json!({"apiGroups":[""],"resources":["pods/log"],"verbs":["get"]}),
                json!({"apiGroups":[""],"resources":["pods/exec"],"verbs":["get","create"]}),
                json!({"apiGroups":["apps"],"resources":["deployments/scale"],"verbs":["get","update","patch"]}),
            ]);
        }
        if !privileged {
            // Only deploy/rollback persist the selected release pair by replacement.
            host_rules.push(json!({"apiGroups":[""],"resources":["configmaps"],"resourceNames":[coordinate],"verbs":["update"]}));
        }
        role(&mut objects, "hosts", &name, &host_rules);
        if privileged {
            role(
                &mut objects,
                "platform",
                &name,
                &[
                    json!({"apiGroups":[""],"resources":["secrets"],"resourceNames":["wamn-apply",format!("{cluster}-superuser")],"verbs":["get"]}),
                    json!({"apiGroups":["postgresql.cnpg.io"],"resources":["databases"],"resourceNames":[database],"verbs":["get","list","watch","patch"]}),
                ],
            );
        }
    }
    Ok(objects)
}

fn role(objects: &mut Vec<Value>, namespace: &str, name: &str, rules: &[Value]) {
    objects.push(json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role","metadata":{"name":name,"namespace":namespace},"rules":rules}));
    objects.push(json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":{"name":name,"namespace":namespace},
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":name},
        "subjects":[{"kind":"ServiceAccount","name":name,"namespace":"platform"}]}));
}

pub(crate) async fn provision(
    platform: &Platform,
    document: &EnvironmentDocument,
    cluster: &str,
) -> anyhow::Result<()> {
    let instance = crate::provision_project_env::read_project_env_instance(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    let database = wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    for object in target_objects(document, cluster, &database)? {
        resources::apply_object(&platform.target, &object).await?;
    }
    Ok(())
}

/// Read the existing administrator credential mounted only in privileged workers.
///
/// # Errors
/// Returns an error when the mounted credential or target cluster is absent.
pub(crate) fn administrator(cluster: &str) -> anyhow::Result<String> {
    let password = std::fs::read_to_string(Path::new("/credentials/administrator/password"))?;
    let mut url = url::Url::parse(&format!(
        "postgresql://postgres@{cluster}-rw.platform.svc.cluster.local/wamn_system"
    ))?;
    url.set_password(Some(&password.replace('%', "%25")))
        .map_err(|()| anyhow::anyhow!("encode the administrator credential"))?;
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_prepares_its_image_chart_with_the_selected_values() -> anyhow::Result<()> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let selected = super::super::artifact_plan::test_util::prepared_chart(&repository)?;
        let inputs = selected.0.join("worker-inputs");
        std::fs::create_dir(&inputs)?;
        for file in ["Chart.yaml", "values.yaml"] {
            std::fs::copy(selected.0.join(file), inputs.join(file))?;
        }
        let output = selected.0.join("worker-chart");
        worker_chart(&repository, &inputs, &output)?;
        let prepared = super::super::artifact_plan::select(&output)?;
        let source = super::super::artifact_plan::select(&selected.0)?;
        assert_eq!(prepared.image("ctl")?, source.image("ctl")?);
        assert_eq!(prepared.version, source.version);
        assert!(prepared.operator_archive.is_file());
        Ok(())
    }
}
