//! Compare COMMIT latency with and without a transaction identity.

use anyhow::{Context as _, Result, ensure};
use sqlx::{Connection as _, PgConnection};
use std::num::NonZeroU32;
use std::time::Instant;

const WARMUP_PAIRS: u64 = 10;

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    /// URL for the private database with the transaction identity schema installed.
    #[arg(env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,
    /// Number of measured pairs after warm-up.
    #[arg(long, default_value = "200")]
    samples: NonZeroU32,
}

pub(super) async fn run(args: &Args) -> Result<()> {
    let mut connection = PgConnection::connect(&args.database_url)
        .await
        .context("connect to the private measurement database")?;
    let (number, version, database, login, synchronous_commit, synchronous_standby_names) =
        sqlx::query_as::<_, (i32, String, String, String, String, String)>(
            "SELECT current_setting('server_version_num')::int, version(), current_database(), \
             current_user, current_setting('synchronous_commit'), \
             current_setting('synchronous_standby_names')",
        )
        .fetch_one(&mut connection)
        .await?;
    ensure!(
        number / 10_000 == 18,
        "PostgreSQL 18 is required. Connected to {version}."
    );
    println!(
        "{version}\ndatabase={database} login={login} synchronous_commit={synchronous_commit} \
         synchronous_standby_names={synchronous_standby_names:?}"
    );

    let principal = "00000000-0000-4000-8000-000000000001".parse()?;
    let count = args.samples.get();
    let mut samples = [Vec::new(), Vec::new()];
    for pair in 0..WARMUP_PAIRS + u64::from(count) {
        // Alternate order to reduce drift. Discard the warm-up pairs.
        let identity_first = pair % 2 == 0;
        for identity in [identity_first, !identity_first] {
            let show_wal = pair == 0 && identity;
            if show_wal {
                print_wal(&mut connection, "before_identity").await?;
            }
            let mut transaction = connection
                .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
                .await?;
            if identity {
                wamn_postgres::bind_transaction_identity(
                    &mut transaction,
                    &principal,
                    "measurement",
                    "measurement",
                    Some("admin:measurement"),
                    None,
                )
                .await
                .context("insert the measurement identity")?;
            }
            sqlx::raw_sql("SET TRANSACTION READ ONLY")
                .execute(&mut *transaction)
                .await?;
            sqlx::raw_sql("SELECT 1").execute(&mut *transaction).await?;
            let start = Instant::now();
            transaction
                .commit()
                .await
                .context("commit measurement transaction")?;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            if show_wal {
                print_wal(&mut connection, "after_identity").await?;
            }
            if pair >= WARMUP_PAIRS {
                samples[usize::from(identity)].push(elapsed_ms);
            }
        }
    }

    for (identity, times) in [false, true].into_iter().zip(&mut samples) {
        times.sort_unstable_by(f64::total_cmp);
        let len = times.len();
        let mean = times.iter().sum::<f64>() / f64::from(count);
        let median = f64::midpoint(times[(len - 1) / 2], times[len / 2]);
        // ceil(0.95 * len) is len - floor(len / 20).
        let p95 = times[len - len / 20 - 1];
        println!(
            "identity={identity} samples={len} mean_ms={mean:.3} median_ms={median:.3} p95_ms={p95:.3}"
        );
    }
    Ok(())
}

async fn print_wal(connection: &mut PgConnection, label: &str) -> Result<()> {
    let (wal, flush, io) =
        sqlx::query_as::<_, (serde_json::Value, String, Option<serde_json::Value>)>(
            "SELECT to_jsonb(w), pg_current_wal_flush_lsn()::text, \
         (SELECT jsonb_agg(to_jsonb(i)) FROM pg_stat_io i WHERE object = 'wal') \
         FROM pg_stat_wal w",
        )
        .fetch_one(connection)
        .await
        .context("read PostgreSQL WAL counters")?;
    let io = io.unwrap_or(serde_json::Value::Null);
    println!("{label} pg_stat_wal={wal} flush_lsn={flush} pg_stat_io={io}");
    Ok(())
}
