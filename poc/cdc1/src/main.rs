//! S-CDC-1: pg_walstream diligence spike (wamn-l5i9.2, event-plane v3 §7).
//!
//! Drives the five checklist items against the throwaway 2-instance CNPG
//! cluster `cdc1` (see cdc1-cluster.yaml). Modes:
//!
//!   setup               create spike tables, publication, failover slot
//!   message             (e) pg_logical_emit_message → EventType::Message
//!   toast               (c) unchanged-TOAST marker distinguishable from NULL
//!   stream --rows N     (d) N-row single txn under 4MB work_mem; RSS profile
//!   soak --secs N       (a) idle keepalive/feedback; canary write at the end
//!   switchover --secs N (b) writer+consumer across a promotion; no-gap check
//!   teardown            drop publication + slot
//!
//! Env: CDC1_URL = postgresql://postgres:<pw>@<node-ip>:<nodeport>/app
//! (no query string; the harness appends sslmode/replication itself).
//!
//! This is diligence, not a gate of record: verdicts land in the bead notes
//! and feed wamn-l5i9.6 [BUILD-VS-BUY].

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use pg_walstream::{
    CancellationToken, ChangeEvent, ColumnValue, EventStream, EventType, LogicalReplicationStream,
    ReplicationStreamConfig, RetryConfig, RowData, StreamingMode,
};
use tokio_postgres::NoTls;

const SLOT: &str = "cdc1_spike";
const PUBLICATION: &str = "cdc1_pub";

fn base_url() -> Result<String> {
    std::env::var("CDC1_URL").context("CDC1_URL not set")
}

async fn sql_connect() -> Result<tokio_postgres::Client> {
    let url = format!("{}?sslmode=disable", base_url()?);
    let (client, conn) = tokio_postgres::connect(&url, NoTls).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(client)
}

fn repl_config() -> ReplicationStreamConfig {
    let mut cfg = ReplicationStreamConfig::new(
        SLOT.to_string(),
        PUBLICATION.to_string(),
        2,
        StreamingMode::On,
        Duration::from_secs(5),
        Duration::from_secs(30),
        Duration::from_secs(30),
        RetryConfig::default(),
    );
    cfg.messages = true; // pg_logical_emit_message → EventType::Message
    // FINDING F1 (spike, 2026-07-18): slot_options.failover = true is BROKEN in
    // pg_walstream 0.8.0 against PG17+ — the crate emits the legacy
    // space-separated `CREATE_REPLICATION_SLOT … FAILOVER`, but FAILOVER only
    // exists in the parenthesized option grammar (proven live: legacy form →
    // 42601, `(SNAPSHOT 'nothing', FAILOVER)` → ok). Workaround: setup()
    // creates the slot via SQL pg_create_logical_replication_slot(…,
    // failover => true); ensure_replication_slot() then tolerates
    // "already exists". A vendor/fork patch (wamn-l5i9.8) is ~3 lines.
    cfg.slot_options.failover = false;
    cfg
}

async fn open_stream_with(token: CancellationToken) -> Result<EventStream> {
    let url = format!("{}?sslmode=disable&replication=database", base_url()?);
    let mut stream = LogicalReplicationStream::new(&url, repl_config()).await?;
    stream.ensure_replication_slot().await?;
    stream.start(None).await?;
    Ok(stream.into_stream(token))
}

async fn open_stream() -> Result<(EventStream, CancellationToken)> {
    let token = CancellationToken::new();
    Ok((open_stream_with(token.clone()).await?, token))
}

fn vm_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn text_of(v: &ColumnValue) -> Option<String> {
    match v {
        ColumnValue::Text(b) => Some(String::from_utf8_lossy(b).into_owned()),
        ColumnValue::Binary(b) => Some(format!("<{}B binary>", b.len())),
        ColumnValue::Null => None,
    }
}

// ---------------------------------------------------------------- setup

async fn setup() -> Result<()> {
    let c = sql_connect().await?;
    c.batch_execute(
        "CREATE TABLE IF NOT EXISTS spike (
             id bigint PRIMARY KEY,
             val text,
             big text,
             maybe_null text
         );
         ALTER TABLE spike ALTER COLUMN big SET STORAGE EXTERNAL;
         CREATE TABLE IF NOT EXISTS bulk (id int PRIMARY KEY, val text);",
    )
    .await?;
    // No CREATE PUBLICATION IF NOT EXISTS in PG — tolerate duplicate_object.
    match c
        .batch_execute(&format!(
            "CREATE PUBLICATION {PUBLICATION} FOR TABLE spike, bulk"
        ))
        .await
    {
        Ok(()) => println!("publication {PUBLICATION} created"),
        Err(e) if e.code() == Some(&tokio_postgres::error::SqlState::DUPLICATE_OBJECT) => {
            println!("publication {PUBLICATION} already exists");
        }
        Err(e) => return Err(e.into()),
    }
    // Create the failover-enabled slot via SQL (see FINDING F1 in
    // repl_config: the crate's own CREATE_REPLICATION_SLOT … FAILOVER path
    // emits legacy syntax PG17+ rejects).
    let exists = c
        .query_opt(
            "SELECT 1 FROM pg_replication_slots WHERE slot_name = $1",
            &[&SLOT],
        )
        .await?
        .is_some();
    if !exists {
        c.execute(
            &format!(
                "SELECT pg_create_logical_replication_slot('{SLOT}', 'pgoutput', \
                 temporary => false, twophase => false, failover => true)"
            ),
            &[],
        )
        .await?;
    }
    let row = c
        .query_one(
            "SELECT failover, slot_type FROM pg_replication_slots WHERE slot_name = $1",
            &[&SLOT],
        )
        .await?;
    let failover: bool = row.get(0);
    println!(
        "slot {SLOT} created: type={} failover={failover}",
        row.get::<_, String>(1)
    );
    if !failover {
        bail!("slot was not created with failover=true");
    }
    Ok(())
}

// ---------------------------------------------------------------- message

async fn message() -> Result<()> {
    let (mut stream, token) = open_stream().await?;
    let c = sql_connect().await?;
    c.execute(
        "SELECT pg_logical_emit_message(true, 'cdc1', 'txn-hello')",
        &[],
    )
    .await?;
    c.execute(
        "SELECT pg_logical_emit_message(false, 'cdc1', 'now-hello')",
        &[],
    )
    .await?;
    c.execute(
        "INSERT INTO spike (id, val) VALUES (1, 'sanity') ON CONFLICT (id) DO UPDATE SET val = 'sanity'",
        &[],
    )
    .await?;

    let mut got_txn = false;
    let mut got_now = false;
    let mut got_write = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !(got_txn && got_now && got_write) {
        let ev = match next_with_timeout(&mut stream, deadline).await? {
            Some(ev) => ev,
            None => break,
        };
        match &ev.event_type {
            EventType::Message {
                flags,
                prefix,
                content,
                ..
            } if prefix.as_ref() == "cdc1" => {
                let body = String::from_utf8_lossy(content).into_owned();
                println!("message event: flags={flags} prefix={prefix} content={body}");
                if body == "txn-hello" && *flags == 1 {
                    got_txn = true;
                }
                if body == "now-hello" && *flags == 0 {
                    got_now = true;
                }
            }
            EventType::Insert { table, .. } | EventType::Update { table, .. }
                if table.as_ref() == "spike" =>
            {
                got_write = true;
            }
            _ => {}
        }
        stream.update_applied_lsn(ev.lsn.value());
    }
    token.cancel();
    let _ = stream.shutdown().await;
    println!("MESSAGE: transactional={got_txn} non_transactional={got_now} row_sanity={got_write}");
    if !(got_txn && got_now && got_write) {
        bail!("message mode FAIL");
    }
    println!("MESSAGE PASS");
    Ok(())
}

// ---------------------------------------------------------------- toast

async fn toast() -> Result<()> {
    let c = sql_connect().await?;
    // Incompressible-enough 22.4KB value; STORAGE EXTERNAL forces out-of-line.
    c.execute(
        "INSERT INTO spike (id, val, big, maybe_null)
         SELECT 2, 'orig', string_agg(md5(i::text), ''), NULL
           FROM generate_series(1, 700) i
         ON CONFLICT (id) DO UPDATE SET val = 'orig', big = EXCLUDED.big, maybe_null = NULL",
        &[],
    )
    .await?;
    c.batch_execute("ALTER TABLE spike REPLICA IDENTITY DEFAULT")
        .await?;

    let (mut stream, token) = open_stream().await?;
    // Update NOT touching `big` under default replica identity.
    c.execute("UPDATE spike SET val = 'updated' WHERE id = 2", &[])
        .await?;

    let (old1, new1) = wait_for_update(&mut stream, "spike", "updated").await?;
    let a_absent = new1.get("big").is_none();
    let a_null_present = matches!(new1.get("maybe_null"), Some(ColumnValue::Null));
    let a_old_none = old1.is_none();
    println!(
        "toast/default: big-absent-in-new={a_absent} maybe_null-present-as-Null={a_null_present} old_data-none={a_old_none}"
    );

    // Same update under REPLICA IDENTITY FULL — the old image must carry the
    // real TOAST value (the l5i9.31 per-entity-knob mechanics).
    c.batch_execute("ALTER TABLE spike REPLICA IDENTITY FULL")
        .await?;
    c.execute("UPDATE spike SET val = 'updated2' WHERE id = 2", &[])
        .await?;
    let (old2, new2) = wait_for_update(&mut stream, "spike", "updated2").await?;
    let b_absent = new2.get("big").is_none();
    let old_big_len = old2
        .as_ref()
        .and_then(|o| o.get("big"))
        .and_then(text_of)
        .map(|s| s.len())
        .unwrap_or(0);
    println!("toast/full: big-absent-in-new={b_absent} old-big-len={old_big_len}");

    c.batch_execute("ALTER TABLE spike REPLICA IDENTITY DEFAULT")
        .await?;
    token.cancel();
    let _ = stream.shutdown().await;

    if !(a_absent && a_null_present && a_old_none && b_absent && old_big_len == 700 * 32) {
        bail!("toast mode FAIL");
    }
    println!(
        "TOAST PASS: unchanged-TOAST column ABSENT from new_data while a real NULL is present \
         as ColumnValue::Null (distinguishable); REPLICA IDENTITY FULL old image carries the \
         full {old_big_len}B value"
    );
    Ok(())
}

async fn wait_for_update(
    stream: &mut EventStream,
    want_table: &str,
    want_val: &str,
) -> Result<(Option<RowData>, RowData)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ev = next_with_timeout(stream, deadline)
            .await?
            .context("timed out waiting for update event")?;
        stream.update_applied_lsn(ev.lsn.value());
        if let EventType::Update {
            table,
            old_data,
            new_data,
            ..
        } = ev.event_type
            && table.as_ref() == want_table
            && new_data.get("val").and_then(text_of).as_deref() == Some(want_val)
        {
            return Ok((old_data, new_data));
        }
    }
}

// ------------------------------------------------- stream (1M-row streamed txn)

async fn stream_mode(rows: u64) -> Result<()> {
    let c = sql_connect().await?;
    c.batch_execute("TRUNCATE bulk").await?;

    let (mut stream, token) = open_stream().await?;
    let rss_before = vm_rss_kib();

    let insert = format!("INSERT INTO bulk SELECT i, 'v' || i FROM generate_series(1, {rows}) i");
    let writer = tokio::spawn(async move { c.batch_execute(&insert).await });

    let mut inserts: u64 = 0;
    let mut stream_starts: u64 = 0;
    let mut stream_stops: u64 = 0;
    let mut stream_commit = false;
    let mut plain_commit = false;
    let mut rss_peak = rss_before;
    let started = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(600);
    while Instant::now() < deadline {
        let ev = next_with_timeout(&mut stream, deadline)
            .await?
            .context("timed out draining the bulk transaction")?;
        stream.update_applied_lsn(ev.lsn.value());
        match &ev.event_type {
            EventType::Insert { table, .. } if table.as_ref() == "bulk" => {
                inserts += 1;
                if inserts.is_multiple_of(100_000) {
                    rss_peak = rss_peak.max(vm_rss_kib());
                    println!(
                        "  {inserts} inserts, VmRSS {} KiB, {:.1}s",
                        vm_rss_kib(),
                        started.elapsed().as_secs_f32()
                    );
                }
            }
            EventType::StreamStart { .. } => stream_starts += 1,
            EventType::StreamStop => {
                stream_stops += 1;
                rss_peak = rss_peak.max(vm_rss_kib());
            }
            EventType::StreamCommit { .. } => {
                stream_commit = true;
                break;
            }
            EventType::Commit { .. } if inserts > 0 => {
                plain_commit = true;
                break;
            }
            _ => {}
        }
    }
    let rss_after = vm_rss_kib();
    writer.await??;
    token.cancel();
    let _ = stream.shutdown().await;

    println!(
        "STREAM: rows={inserts}/{rows} segments(start={stream_starts},stop={stream_stops}) \
         stream_commit={stream_commit} plain_commit={plain_commit} wall={:.1}s \
         VmRSS KiB before={rss_before} peak={rss_peak} after={rss_after} (delta-peak={} KiB)",
        started.elapsed().as_secs_f32(),
        rss_peak.saturating_sub(rss_before)
    );
    if inserts != rows {
        bail!("stream mode FAIL: row count mismatch");
    }
    if !stream_commit {
        bail!("stream mode FAIL: txn was not streamed (no StreamCommit; work_mem too high?)");
    }
    if rss_peak.saturating_sub(rss_before) > 500 * 1024 {
        bail!("stream mode FAIL: harness buffered the transaction (peak RSS delta > 500 MiB)");
    }
    println!("STREAM PASS");
    Ok(())
}

// ---------------------------------------------------------------- soak

async fn soak(secs: u64) -> Result<()> {
    let c = sql_connect().await?;
    let (stream, token) = open_stream().await?;

    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let recv_clone = received.clone();
    let consumer = tokio::spawn(async move {
        let mut stream = stream;
        loop {
            match stream.next_event().await {
                Ok(ev) => {
                    if let EventType::Insert { table, data, .. } = &ev.event_type
                        && table.as_ref() == "spike"
                        && let Some(id) = data
                            .get("id")
                            .and_then(text_of)
                            .and_then(|s| s.parse().ok())
                    {
                        recv_clone.lock().unwrap().push(id);
                    }
                    stream.update_applied_lsn(ev.lsn.value());
                }
                Err(e) => {
                    println!("soak consumer exiting: {e}");
                    break;
                }
            }
        }
        let _ = stream.shutdown().await;
    });

    let started = Instant::now();
    let mut last_reply: Option<String> = None;
    let mut reply_advances = 0u32;
    let mut checks = 0u32;
    while started.elapsed() < Duration::from_secs(secs) {
        tokio::time::sleep(Duration::from_secs(30)).await;
        checks += 1;
        let slot = c
            .query_one(
                "SELECT active, confirmed_flush_lsn::text, restart_lsn::text,
                        pg_size_pretty(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn))
                   FROM pg_replication_slots WHERE slot_name = $1",
                &[&SLOT],
            )
            .await?;
        // Our walsender: the logical one on this slot (CNPG's own physical
        // walsenders carry the instance name as application_name).
        let walsender = c
            .query_opt(
                "SELECT s.state, s.reply_time::text
                   FROM pg_stat_replication s
                   JOIN pg_replication_slots r ON r.active_pid = s.pid
                  WHERE r.slot_name = $1",
                &[&SLOT],
            )
            .await?;
        let (state, reply) = walsender
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .unwrap_or_else(|| ("MISSING".into(), "-".into()));
        if last_reply.as_deref() != Some(reply.as_str()) && reply != "-" {
            reply_advances += 1;
            last_reply = Some(reply.clone());
        }
        println!(
            "[soak {:>5}s] slot active={} confirmed={} restart={} retained={} walsender={} reply={}",
            started.elapsed().as_secs(),
            slot.get::<_, bool>(0),
            slot.get::<_, String>(1),
            slot.get::<_, String>(2),
            slot.get::<_, String>(3),
            state,
            reply
        );
    }

    // Canary: the stream must still deliver after the idle window.
    let canary_id: i64 = 900;
    c.execute(
        "INSERT INTO spike (id, val) VALUES ($1, 'canary') ON CONFLICT (id) DO UPDATE SET val = 'canary'",
        &[&canary_id],
    )
    .await?;
    let mut canary_seen = false;
    let canary_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < canary_deadline {
        if received.lock().unwrap().contains(&canary_id) {
            canary_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    token.cancel();
    let _ = consumer.await;

    println!(
        "SOAK: idle={secs}s checks={checks} walsender_reply_advances={reply_advances} canary_after_idle={canary_seen}"
    );
    if !canary_seen {
        bail!("soak FAIL: canary write not delivered after the idle window");
    }
    println!("SOAK PASS");
    Ok(())
}

// ---------------------------------------------------------------- switchover

async fn switchover(secs: u64) -> Result<()> {
    let token = CancellationToken::new();

    let received: Arc<Mutex<BTreeSet<i64>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let lsn_trail: Arc<Mutex<Vec<(i64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let recv_clone = received.clone();
    let trail_clone = lsn_trail.clone();
    let consumer_token = token.clone();
    // Consumer with a harness-level reconnect loop: the crate's inner retry
    // window (RetryConfig::default ≈ 31s) can be shorter than a promotion, and
    // a production reader would re-open the session anyway. Re-opens count as
    // evidence the drill actually severed the stream.
    let consumer = tokio::spawn(async move {
        let mut reopens: u32 = 0;
        'outer: loop {
            let mut stream = loop {
                if consumer_token.is_cancelled() {
                    return reopens;
                }
                match open_stream_with(consumer_token.clone()).await {
                    Ok(s) => break s,
                    Err(e) => {
                        println!("[consumer] open failed ({e}); retrying in 1s");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            };
            loop {
                match stream.next_event().await {
                    Ok(ev) => {
                        if let EventType::Insert { table, data, .. } = &ev.event_type
                            && table.as_ref() == "spike"
                            && let Some(id) = data
                                .get("id")
                                .and_then(text_of)
                                .and_then(|s| s.parse().ok())
                        {
                            recv_clone.lock().unwrap().insert(id);
                            trail_clone.lock().unwrap().push((id, ev.lsn.value()));
                        }
                        stream.update_applied_lsn(ev.lsn.value());
                    }
                    Err(e) => {
                        let _ = stream.shutdown().await;
                        if consumer_token.is_cancelled() {
                            break 'outer;
                        }
                        reopens += 1;
                        println!("[consumer] stream error ({e}); reopening (n={reopens})");
                        break;
                    }
                }
            }
        }
        reopens
    });

    // Writer: one committed row every 200ms, reconnecting through the promotion.
    let committed: Arc<Mutex<BTreeSet<i64>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let committed_clone = committed.clone();
    let writer = tokio::spawn(async move {
        // Per-run id base: a repeated run must not collide with prior rows —
        // an ON CONFLICT no-op insert emits NO row event, so a recycled id
        // would be counted committed yet never delivered (a false gap).
        let mut id: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            * 100_000;
        let started = Instant::now();
        let mut client: Option<tokio_postgres::Client> = None;
        while started.elapsed() < Duration::from_secs(secs) {
            if client.is_none() {
                match sql_connect().await {
                    Ok(c) => {
                        println!(
                            "[writer {:>5.1}s] connected",
                            started.elapsed().as_secs_f32()
                        );
                        client = Some(c);
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                }
            }
            id += 1;
            let res = client
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO spike (id, val) VALUES ($1, 'sw') ON CONFLICT (id) DO NOTHING",
                    &[&id],
                )
                .await;
            match res {
                // Count committed ONLY when a row was actually inserted — an
                // ON CONFLICT no-op (0 rows) produces no CDC event.
                Ok(1) => {
                    committed_clone.lock().unwrap().insert(id);
                }
                Ok(_) => {}
                Err(e) => {
                    // Commit outcome unknown → NOT counted as committed.
                    println!(
                        "[writer {:>5.1}s] write error (reconnecting): {e}",
                        started.elapsed().as_secs_f32()
                    );
                    client = None;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    println!("switchover drill running {secs}s — PROMOTE THE STANDBY NOW (kubectl)");
    writer.await?;

    // Let the consumer catch up, then compare.
    let catchup_deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let missing: Vec<i64> = {
            let com = committed.lock().unwrap();
            let rec = received.lock().unwrap();
            com.difference(&rec).copied().collect()
        };
        if missing.is_empty() || Instant::now() > catchup_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    token.cancel();
    let reopens = consumer.await.unwrap_or(0);

    let com = committed.lock().unwrap().clone();
    let rec = received.lock().unwrap().clone();
    let missing: Vec<i64> = com.difference(&rec).copied().collect();
    let trail = lsn_trail.lock().unwrap();
    let (min_lsn, max_lsn) = trail.iter().fold((u64::MAX, 0u64), |(lo, hi), (_, l)| {
        (lo.min(*l), hi.max(*l))
    });
    // LSN regressions in arrival order = the at-least-once redelivery window.
    let mut regressions = 0u32;
    for w in trail.windows(2) {
        if w[1].1 < w[0].1 {
            regressions += 1;
        }
    }
    println!(
        "SWITCHOVER: committed={} received={} missing={:?} reopens={} lsn=[{:X}..{:X}] lsn_regressions={}",
        com.len(),
        rec.len(),
        missing,
        reopens,
        min_lsn,
        max_lsn,
        regressions
    );
    if !missing.is_empty() {
        bail!("switchover FAIL: gap — committed rows never delivered: {missing:?}");
    }
    println!("SWITCHOVER PASS: no gap (dupes within at-least-once are acceptable)");
    Ok(())
}

// ---------------------------------------------------------------- teardown

async fn teardown() -> Result<()> {
    let c = sql_connect().await?;
    let _ = c
        .batch_execute(&format!("DROP PUBLICATION IF EXISTS {PUBLICATION}"))
        .await;
    let _ = c
        .execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = $1",
            &[&SLOT],
        )
        .await;
    println!("teardown done (publication + slot dropped)");
    Ok(())
}

// ---------------------------------------------------------------- glue

async fn next_with_timeout(
    stream: &mut EventStream,
    deadline: Instant,
) -> Result<Option<ChangeEvent>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(None);
    }
    match tokio::time::timeout(remaining, stream.next_event()).await {
        Ok(Ok(ev)) => Ok(Some(ev)),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Ok(None),
    }
}

fn arg_u64(flag: &str, default: u64) -> u64 {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "setup" => setup().await,
        "message" => message().await,
        "toast" => toast().await,
        "stream" => stream_mode(arg_u64("--rows", 1_000_000)).await,
        "soak" => soak(arg_u64("--secs", 1800)).await,
        "switchover" => switchover(arg_u64("--secs", 60)).await,
        "teardown" => teardown().await,
        other => {
            bail!("unknown mode {other:?} (setup|message|toast|stream|soak|switchover|teardown)")
        }
    }
}
