//! Case 2 reopens a persistent Chrome profile offline and uploads through a second shell.
//!
//! Uses the local_outbox host and inputs. The driver builds both shells in scratch.

/// The final reload keeps the 60 s wait for worker activation, wfe gap wamn-c361p.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node, Chrome"]
async fn case2_offline_reopens_upload_three_rows_across_a_release() -> anyhow::Result<()> {
    super::local_outbox::run("case2").await
}
