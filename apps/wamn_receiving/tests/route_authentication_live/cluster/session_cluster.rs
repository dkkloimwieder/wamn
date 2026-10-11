//! The session issuer of a Receiving cluster case.

use wamn_control::print_release_env::ReleaseCarrier;
use wamn_gate_harness::session_issuer::{self, IssuerCluster, image_loaded};

use super::super::identity;
use super::ReceivingCluster;

/// The issuer cluster of this Receiving run.
fn issuer_cluster(cluster: &ReceivingCluster) -> IssuerCluster<'_> {
    let resources = &cluster.resources;
    IssuerCluster {
        name: &resources.name,
        namespace: &resources.name,
        work: &resources.work,
        evidence: &resources.evidence,
        repository: &resources.repository,
        lifecycle: &resources.lifecycle,
        source: &resources.source,
        system_database_url: &cluster.inputs.system_pg_url,
        identity_image: resources.identity_image.as_deref(),
        org: identity().org.as_str(),
        project: identity().project.as_str(),
        environment: identity().environment.as_str(),
        tenant: identity().tenant.as_str(),
    }
}

/// Supply the real issuer required by the published application's session routes.
pub(super) async fn prepare_application(
    cluster: &ReceivingCluster,
    carrier: &ReleaseCarrier,
) -> anyhow::Result<(String, String)> {
    load_gates(cluster).await?;
    session_issuer::prepare_application(
        &issuer_cluster(cluster),
        &carrier.manifest_digest.to_string(),
    )
    .await
}

/// Load the gates image of a case that uses it, beside the issuer.
async fn load_gates(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    if let Some(gates_image) = cluster.resources.gates_image.as_deref() {
        image_loaded(&issuer_cluster(cluster), gates_image, "gates", true).await?;
    }
    Ok(())
}
