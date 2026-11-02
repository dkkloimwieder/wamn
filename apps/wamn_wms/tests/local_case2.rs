//! Case 2 reopens a persistent Chrome profile offline and uploads through a second shell.
//!
//! Uses the local_outbox host and inputs. The driver builds both shells in scratch.

/// The final reload keeps the 60 s wait for worker activation, wfe gap wamn-c361p.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn case2_offline_reopens_upload_three_rows_across_a_release() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome",
    );
    super::local_outbox::run("case2").await
}
