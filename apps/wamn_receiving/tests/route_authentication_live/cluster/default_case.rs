//! The complete Receiving route, event, startup, and environment-isolation case.
//!
//! It runs every stage of `stages` in one process on one cluster.

use super::{materializer_case, route_cases, stages, start_with};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn released_routes_materializer_startup_and_environment_isolation() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, run(&evidence))).await
}

async fn run(evidence: &std::path::Path) -> anyhow::Result<()> {
    let mut cluster = start_with(evidence, false, true).await?;
    let result = async {
        let ready = stages::setup(&mut cluster).await?;
        stages::materializer(
            &mut cluster,
            &ready,
            &materializer_case::TriggerOrder::seeded(),
        )
        .await?;
        stages::startup(&cluster, &ready).await?;
        stages::outage(&cluster).await?;
        super::assert_source_unchanged(&cluster.resources).await
    }
    .await;
    route_cases::finish(&mut cluster, result).await
}
