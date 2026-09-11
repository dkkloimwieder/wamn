//! The production CDC reader with the application's declared connection inputs.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::json;
use tokio_postgres::Client;
use wamn_cdc_reader::EventReaderArgs;

pub(super) fn args(
    work: &Path,
    nats_url: &str,
    source: &async_nats::jetstream::stream::Config,
    username: &str,
    password_file: &Path,
) -> anyhow::Result<EventReaderArgs> {
    use wamn_ctl::dev::environment::secret_value;
    Ok(EventReaderArgs {
        org: crate::environment::ORG.into(),
        project: crate::environment::PROJECT.into(),
        env: crate::environment::ENVIRONMENT.into(),
        system_database_url: secret_value(&work.join("registry-reader.json"), "url")?,
        cdc_url: secret_value(&work.join("cdc-reader.json"), "url")?,
        nats_url: nats_url.to_owned(),
        nats_username: Some(username.to_owned()),
        nats_password_file: Some(password_file.to_path_buf()),
        sslmode: "disable".into(),
        stream_replicas: source.num_replicas,
        dup_window_secs: source.duplicate_window.as_secs(),
        feedback_secs: 1,
        stall_threshold_secs: 30,
        slot_poll_secs: 0,
        slot_safe_wal_warn_bytes: 268_435_456,
    })
}

pub(super) async fn wait_streaming(
    project: &Client,
    cdc_url: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    let config: tokio_postgres::Config = cdc_url
        .parse()
        .context("parse the private CDC connection")?;
    let user = config
        .get_user()
        .context("the CDC connection names its scoped role")?;
    for _ in 0..120 {
        let sessions: i64 = project.query_one(
            "SELECT count(*)::bigint FROM pg_stat_replication WHERE usename = $1 AND state = 'streaming'",
            &[&user],
        ).await?.get(0);
        ensure!(
            sessions <= 1,
            "the owned CDC role has more than one streaming session"
        );
        if sessions == 1 {
            crate::wms_runtime_live::write_result(
                evidence,
                "cdc-reader-ready.json",
                &json!({
                    "streaming_sessions":sessions,"state":"streaming",
                }),
            )?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!(
        "the production CDC reader did not open its replication session within 120 seconds"
    )
}
