//! Online take-offline counts, saved bytes, and fill time for 20,000 added locations.
//!
//! Uses the local_outbox host and its prepared inputs.

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn take_offline_fills_packaging_and_locations() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome",
    );
    super::local_outbox::run("take-offline").await
}

pub(super) async fn seed(project: &tokio_postgres::Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "BEGIN;
             SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true),
                    set_config('app.operation', 'admin:seed-offline-locations', true);
             INSERT INTO wms.location (id, location_code)
             SELECT ('10000000-0000-4000-8000-' || lpad(i::text, 12, '0'))::uuid,
                    'OFFLINE-LOC-' || lpad(i::text, 5, '0')
             FROM generate_series(1, 20000) AS i;
             COMMIT;",
        )
        .await?;
    Ok(())
}
