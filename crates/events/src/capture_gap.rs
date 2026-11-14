//! Repair lost replication slots and record operator resynchronization.
//! Recovery keeps the reader stopped. Control restarts it after closing the gap.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
pub(super) mod plan;
use plan::pg_lsn;

use anyhow::{Context as _, bail, ensure};
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::stream::RawMessageErrorKind;
use chrono::{DateTime, Utc};

use wamn_control_provision::{
    cdc_object_name, project_env_database_name, sql, validate_project_env_cdc,
};
use wamn_event_wire::{SourceEvent, verified_source_event_id};
use wamn_registry::Triple;
use wamn_registry::sql::{
    close_capture_gap_sql, insert_capture_gap_sql, lock_newest_capture_gap_sql,
    select_event_reader_origin_sql,
};

/// The reason a missing slot records.
pub const MISSING_SLOT_REASON: &str = "missing";

/// Inputs of one capture gap recovery.
#[derive(Debug)]
pub struct RecoverCaptureGapRequest {
    pub org: String,
    pub project: String,
    pub env: String,

    /// Owning connection of the system database: the instance suffix, the reader
    /// registration, and the gap row.
    pub system_database_url: String,

    /// Capture-role connection of the project database that holds the slot.
    pub admin_database_url: String,

    /// The NATS of the event streams, with the client certificate.
    pub nats: wamn_broker::nats::PlatformNats,
}

/// Inputs of one capture gap close.
#[derive(Debug)]
pub struct CloseCaptureGapRequest {
    pub org: String,
    pub project: String,
    pub env: String,

    /// Owning connection of the system database.
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
    instance: &str,
    stop_reader: impl AsyncFnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<RecoveredCaptureGap> {
    validate_project_env_cdc(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("cdc names: {e}"))?;
    let triple = Triple::new(&args.org, args.env.as_str(), &args.project);
    let slot = cdc_object_name(&args.org, &args.project, &args.env, instance);
    let database = project_env_database_name(&args.org, &args.project, &args.env, instance);

    let mut system = connect_system(&args.system_database_url).await?;
    let env = triple.env.as_str();
    let registration = sqlx::query(sqlx::AssertSqlSafe(
        (select_event_reader_origin_sql()).to_string(),
    ))
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(env)
    .fetch_optional(&mut system)
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

    let mut admin = connect(&args.admin_database_url).await?;
    let current: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await
        .context("read the database of --admin-database-url")?
        .get(0);
    ensure!(
        current == database,
        "--admin-database-url reaches database {current}, not the project-env database {database}"
    );
    let state = sqlx::query(
        "SELECT active, confirmed_flush_lsn::text, wal_status::text, \
                    invalidation_reason::text \
             FROM pg_replication_slots WHERE slot_name = $1",
    )
    .bind(&slot)
    .fetch_optional(&mut admin)
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
        args.nats
            .connect()
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

    // Validate the recovery before stopping a healthy reader. The callback must
    // stop the Deployment before a repaired slot can become visible to it.
    stop_reader().await?;

    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::drop_replication_slot_sql(&slot)))
        .execute(&mut admin)
        .await
        .map(drop)
        .context("drop the lost slot")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::create_failover_slot_sql(&slot)))
        .execute(&mut admin)
        .await
        .map(drop)
        .context("create the slot again")?;
    let end_lsn: String = sqlx::query(
        "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
    )
    .bind(&slot)
    .fetch_one(&mut admin)
    .await
    .context("read the new slot's first position")?
    .get(0);

    let mut transaction = provisioning_transaction(&mut system).await?;
    let created_at: DateTime<Utc> =
        sqlx::query(sqlx::AssertSqlSafe((insert_capture_gap_sql()).to_string()))
            .bind(&triple.org)
            .bind(&triple.project)
            .bind(env)
            .bind(&slot)
            .bind(&start_lsn)
            .bind(start_at)
            .bind(&reason)
            .bind(&end_lsn)
            .fetch_one(&mut *transaction)
            .await
            .context("write the registry.capture_gap row")?
            .get(0);

    transaction.commit().await?;

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
    close_gap(args, false)
        .await?
        .context("capture gap was not closed")
}

/// Close an open gap after database reconstruction, retaining an already closed or absent gap.
pub async fn close_open_capture_gap(args: &CloseCaptureGapRequest) -> anyhow::Result<()> {
    close_gap(args, true).await?;
    Ok(())
}

async fn close_gap(
    args: &CloseCaptureGapRequest,
    optional: bool,
) -> anyhow::Result<Option<ClosedCaptureGap>> {
    let triple = Triple::new(&args.org, args.env.as_str(), &args.project);
    let mut system = connect_system(&args.system_database_url).await?;
    let env = triple.env.as_str();
    let mut transaction = provisioning_transaction(&mut system).await?;
    let newest = sqlx::query(sqlx::AssertSqlSafe(
        (lock_newest_capture_gap_sql()).to_string(),
    ))
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(env)
    .fetch_optional(&mut *transaction)
    .await
    .context("read the newest registry.capture_gap row")?;
    let Some(newest) = newest else {
        if optional {
            return Ok(None);
        }
        bail!("{triple} has no capture gap row");
    };
    let created_at: DateTime<Utc> = newest.get(0);
    let slot: String = newest.get(1);
    let closed: bool = newest.get(2);
    if closed {
        if optional {
            return Ok(None);
        }
        bail!("the newest capture gap row of {triple} already has resync_at");
    }
    let resync_at: DateTime<Utc> =
        sqlx::query(sqlx::AssertSqlSafe((close_capture_gap_sql()).to_string()))
            .bind(&triple.org)
            .bind(&triple.project)
            .bind(env)
            .bind(created_at)
            .fetch_one(&mut *transaction)
            .await
            .context("set resync_at")?
            .get(0);
    transaction
        .commit()
        .await
        .context("commit the close transaction")?;
    Ok(Some(ClosedCaptureGap {
        triple,
        slot,
        created_at,
        resync_at,
    }))
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
    let mut sequence = state.last_sequence;
    while sequence >= state.first_sequence.max(1) {
        match source.get_raw_message(sequence).await {
            Ok(message) => {
                let ids = message
                    .headers
                    .get_all(NATS_MESSAGE_ID)
                    .map(async_nats::HeaderValue::as_str)
                    .collect::<Vec<_>>();
                // Derived records and old coordinates do not establish a CDC position.
                if let Ok(SourceEvent::Cdc(envelope)) = SourceEvent::decode(&message.payload)
                    && verified_source_event_id(project, env, &envelope, &ids).is_some()
                {
                    return Ok(Some((envelope.identity.commit_lsn, envelope.commit_ts)));
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

async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url)
        .await
        .context("database connect")?;

    Ok(client)
}

/// Select `wamn_system`, the registry owner, on its owning connection.
async fn connect_system(url: &str) -> anyhow::Result<Client> {
    let mut client = connect(url).await.context("system db connect")?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .context("SET ROLE wamn_system")?;
    Ok(client)
}

async fn provisioning_transaction(
    client: &mut Client,
) -> anyhow::Result<sqlx::Transaction<'_, sqlx::Postgres>> {
    use sqlx::Connection as _;
    let mut transaction = client.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *transaction)
    .await?;
    Ok(transaction)
}

/// Refuse to restart capture while the operator's newest gap remains open.
pub async fn require_resynchronized(system_url: &str, triple: &Triple) -> anyhow::Result<()> {
    let mut system = wamn_postgres::connect(system_url).await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(
        wamn_registry::sql::select_capture_gap_sql().to_owned(),
    ))
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(triple.env.as_str())
    .fetch_optional(&mut system)
    .await?;
    if let Some(row) = row {
        ensure!(
            row.get::<bool, _>(1),
            "capture gap is open; resynchronize and close it before restarting the reader"
        );
    }
    Ok(())
}
