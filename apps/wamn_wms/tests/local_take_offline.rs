//! Online take-offline counts, saved bytes, and fill time for 20,000 added locations.
//!
//! Build WMS and web/sync-bench first. Uses the local_outbox host and inputs.

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node, Chrome"]
async fn take_offline_fills_packaging_and_locations() -> anyhow::Result<()> {
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
