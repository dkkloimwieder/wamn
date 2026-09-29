//! The `recover-capture-gap` and `close-capture-gap` verbs (`wamn-59z6`,
//! docs/plan/cdc-reader-slot.md 4.3).
//!
//! A CDC reader whose slot is missing or invalidated stays running and stopped.
//! `recover-capture-gap` drops the lost slot, creates it again under the same
//! name, and records the gap in `registry.capture_gap`. The publication and the
//! replication role keep that name and stay untouched. The open row keeps the
//! reader stopped on the healthy new slot. `close-capture-gap` sets `resync_at`
//! on the newest row, and the reader then resumes in the same process.
//!
//! The row's start comes from the newest CDC event on the source stream, the
//! newest event whose `Nats-Msg-Id` is `<project>_<env>:<lsn>`. Derived events
//! share the stream with the id `derived:<hash>` and carry no LSN, so the read
//! skips them. The read uses the observer credential, because reading a stream
//! is the observer's job.

use std::path::PathBuf;

use anyhow::{Context as _, bail, ensure};
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::stream::RawMessageErrorKind;
use chrono::{DateTime, Utc};
use tokio_postgres::{Client, NoTls};

use wamn_control_provision::{
    cdc_object_name, project_env_database_name, sql, validate_project_env_cdc,
};
use wamn_control_registry::Triple;
use wamn_control_registry::sql::{
    close_capture_gap_sql, insert_capture_gap_sql, lock_newest_capture_gap_sql,
    select_event_reader_origin_sql,
};
use wamn_event_wire::{Envelope, project_env};

/// The reason a missing slot records.
pub const MISSING_SLOT_REASON: &str = "missing";

/// Inputs of one capture gap recovery.
#[derive(Debug)]
pub struct RecoverCaptureGapRequest {
    pub org: String,
    pub project: String,
    pub env: String,

    /// Superuser URL of the system database: the instance suffix, the reader
    /// registration, and the gap row.
    pub system_database_url: String,

    /// Superuser URL of the project-env database, which holds the slot.
    pub admin_database_url: String,

    /// Event broker of the environment.
    pub nats_url: String,

    /// Observer username of the environment.
    pub nats_username: String,

    /// Private file that holds the observer password.
    pub nats_password_file: PathBuf,
}

/// Inputs of one capture gap close.
#[derive(Debug)]
pub struct CloseCaptureGapRequest {
    pub org: String,
    pub project: String,
    pub env: String,

    /// Superuser URL of the system database.
    pub system_database_url: String,
}

/// The gap row `recover-capture-gap` wrote.
#[derive(Debug)]
pub struct RecoveredCaptureGap {
    pub triple: Triple,
    pub slot: String,
    pub reason: String,
    pub start_lsn: Option<String>,
    pub start_at: DateTime<Utc>,
    pub end_lsn: String,
    pub created_at: DateTime<Utc>,
}

/// The gap row `close-capture-gap` closed.
#[derive(Debug)]
pub struct ClosedCaptureGap {
    pub triple: Triple,
    pub slot: String,
    pub created_at: DateTime<Utc>,
    pub resync_at: DateTime<Utc>,
}

/// Drop the lost slot, create it again under the same name, and record the gap.
pub async fn recover_capture_gap(
    args: &RecoverCaptureGapRequest,
) -> anyhow::Result<RecoveredCaptureGap> {
    validate_project_env_cdc(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("cdc names: {e}"))?;
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());
    let broker_options =
        crate::event_streams::connection_options(&args.nats_username, &args.nats_password_file)?;
    let instance =
        crate::provision_project_env::read_project_env_instance(&args.system_database_url, &triple)
            .await?;
    let slot = cdc_object_name(&args.org, &args.project, &args.env, &instance);
    let database = project_env_database_name(&args.org, &args.project, &args.env, &instance);

    let system = connect_system(&args.system_database_url).await?;
    let env = triple.env.as_str();
    let registration = system
        .query_opt(
            select_event_reader_origin_sql(),
            &[&triple.org, &triple.project, &env],
        )
        .await
        .context("read the registry.event_readers row")?;
    let Some(registration) = registration else {
        bail!("{triple} has no CDC reader registration");
    };
    let registered_slot: String = registration.get(0);
    let stream: String = registration.get(1);
    let registered_at: DateTime<Utc> = registration.get(2);
    ensure!(
        registered_slot == slot,
        "the registration of {triple} names slot {registered_slot}, not {slot}"
    );

    let admin = connect(&args.admin_database_url).await?;
    let current: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .context("read the database of --admin-database-url")?
        .get(0);
    ensure!(
        current == database,
        "--admin-database-url reaches database {current}, not the project-env database {database}"
    );
    let state = admin
        .query_opt(
            "SELECT active, confirmed_flush_lsn::text, wal_status::text, \
                    invalidation_reason::text \
             FROM pg_replication_slots WHERE slot_name = $1",
            &[&slot],
        )
        .await
        .context("read the slot")?;
    let (reason, confirmed) = match state {
        None => (MISSING_SLOT_REASON.to_owned(), None),
        Some(row) => {
            let active: bool = row.get(0);
            let confirmed: Option<String> = row.get(1);
            let wal_status: Option<String> = row.get(2);
            let invalidation: Option<String> = row.get(3);
            if active {
                bail!("slot {slot} is active: a reader still streams from it");
            }
            if invalidation.is_none() && wal_status.as_deref() != Some("lost") {
                bail!("slot {slot} is healthy: {triple} has no capture gap");
            }
            (
                invalidation.unwrap_or_else(|| "wal_status lost".to_owned()),
                confirmed,
            )
        }
    };

    let broker = async_nats::jetstream::new(
        broker_options
            .connect(&args.nats_url)
            .await
            .context("connect the event observer credential")?,
    );
    let last = last_cdc_event(&broker, &stream, &args.project, &args.env).await?;
    let start_lsn = if reason == MISSING_SLOT_REASON {
        last.map(|(lsn, _)| pg_lsn(lsn))
    } else {
        confirmed
    };
    let start_at = last.map_or(registered_at, |(_, at)| at);

    admin
        .batch_execute(&sql::drop_replication_slot_sql(&slot))
        .await
        .context("drop the lost slot")?;
    admin
        .batch_execute(&sql::create_failover_slot_sql(&slot))
        .await
        .context("create the slot again")?;
    let end_lsn: String = admin
        .query_one(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
            &[&slot],
        )
        .await
        .context("read the new slot's first position")?
        .get(0);

    let created_at: DateTime<Utc> = system
        .query_one(
            insert_capture_gap_sql(),
            &[
                &triple.org,
                &triple.project,
                &env,
                &slot,
                &start_lsn,
                &start_at,
                &reason,
                &end_lsn,
            ],
        )
        .await
        .context("write the registry.capture_gap row")?
        .get(0);

    Ok(RecoveredCaptureGap {
        triple,
        slot,
        reason,
        start_lsn,
        start_at,
        end_lsn,
        created_at,
    })
}

/// Set `resync_at` on the newest gap row of one registration.
pub async fn close_capture_gap(args: &CloseCaptureGapRequest) -> anyhow::Result<ClosedCaptureGap> {
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());
    let mut system = connect_system(&args.system_database_url).await?;
    let env = triple.env.as_str();
    let transaction = system
        .transaction()
        .await
        .context("begin the close transaction")?;
    let newest = transaction
        .query_opt(
            lock_newest_capture_gap_sql(),
            &[&triple.org, &triple.project, &env],
        )
        .await
        .context("read the newest registry.capture_gap row")?;
    let Some(newest) = newest else {
        bail!("{triple} has no capture gap row");
    };
    let created_at: DateTime<Utc> = newest.get(0);
    let slot: String = newest.get(1);
    let closed: bool = newest.get(2);
    if closed {
        bail!("the newest capture gap row of {triple} already has resync_at");
    }
    let resync_at: DateTime<Utc> = transaction
        .query_one(
            close_capture_gap_sql(),
            &[&triple.org, &triple.project, &env, &created_at],
        )
        .await
        .context("set resync_at")?
        .get(0);
    transaction
        .commit()
        .await
        .context("commit the close transaction")?;
    Ok(ClosedCaptureGap {
        triple,
        slot,
        created_at,
        resync_at,
    })
}

/// The LSN and `commit_ts` of the newest CDC event on the source stream. `None`
/// when the stream holds no CDC event.
async fn last_cdc_event(
    broker: &async_nats::jetstream::Context,
    stream: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<Option<(u64, DateTime<Utc>)>> {
    let source = broker
        .get_stream(stream)
        .await
        .with_context(|| format!("read the source stream {stream}"))?;
    let state = source.cached_info().state.clone();
    if state.messages == 0 {
        return Ok(None);
    }
    let prefix = format!("{}:", project_env(project, env));
    let mut sequence = state.last_sequence;
    while sequence >= state.first_sequence.max(1) {
        match source.get_raw_message(sequence).await {
            Ok(message) => {
                let lsn = message
                    .headers
                    .get(NATS_MESSAGE_ID)
                    .and_then(|id| id.as_str().strip_prefix(&prefix))
                    .and_then(|lsn| lsn.parse::<u64>().ok());
                if let Some(lsn) = lsn {
                    let envelope: Envelope = serde_json::from_slice(&message.payload)
                        .with_context(|| format!("decode the CDC event at sequence {sequence}"))?;
                    return Ok(Some((lsn, envelope.commit_ts)));
                }
            }
            Err(error) if error.kind() == RawMessageErrorKind::NoMessageFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read the source stream at sequence {sequence}"));
            }
        }
        sequence -= 1;
    }
    Ok(None)
}

/// The text form of a `pg_lsn`.
fn pg_lsn(lsn: u64) -> String {
    format!("{:X}/{:X}", lsn >> 32, lsn & 0xFFFF_FFFF)
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("database connect")?;
    tokio::spawn(connection);
    Ok(client)
}

/// Connect as the superuser and `SET ROLE wamn_system`, the registry owner.
async fn connect_system(url: &str) -> anyhow::Result<Client> {
    let client = connect(url).await.context("system db connect")?;
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::pg_lsn;

    #[test]
    fn pg_lsn_is_the_server_text_form() {
        assert_eq!(pg_lsn(0), "0/0");
        assert_eq!(pg_lsn(0x0000_0001_E900_4398), "1/E9004398");
    }
}
