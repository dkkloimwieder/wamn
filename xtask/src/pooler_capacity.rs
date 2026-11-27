//! Measure the transaction Poolers under a fixed load per provisioned kind org.

use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use futures_util::future::join_all;
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use kube::api::AttachParams;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use sqlx::{ConnectOptions as _, PgConnection};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout, timeout_at};
use wamn_registry::{EnvName, OrgId, name};

use crate::journey_support::pooler::{APPLY_TRANSACTION, CONTROL, Pooler, ready_pooler};
use crate::journey_support::{Cluster, OUTPUTS, PLATFORM, exec};
use crate::{journey_cluster, process};

const ENV: &str = "dev";
const CLIENTS_PER_LOGIN: usize = 4;
const WINDOW: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const QUERY: &str = "SELECT pg_sleep(0.02); COMMIT";

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    /// The bundle passed to journey-cluster for the installed kind platform.
    #[arg(long)]
    chart: PathBuf,
    /// Increasing org counts, starting at one.
    #[arg(long, value_delimiter = ',', default_value = "1,2,4,8,12,16,24")]
    steps: Vec<NonZeroUsize>,
    /// Maximum number of orgs this run can create.
    #[arg(long, default_value = "24")]
    max_step: NonZeroUsize,
    /// Time limit for one step, including org and env creation.
    #[arg(long, default_value = "900")]
    step_timeout_secs: NonZeroU32,
}

#[derive(Serialize)]
struct Run {
    bundle: PathBuf,
    steps_requested: Vec<NonZeroUsize>,
    max_step: NonZeroUsize,
    step_timeout_seconds: u64,
    window_seconds: u64,
    clients_per_login: usize,
    request_timeout_seconds: u64,
    query: &'static str,
    existing_orgs: Option<i64>,
    provisioning: Vec<Provisioning>,
    steps: Vec<Step>,
    threshold: Threshold,
    error: Option<String>,
}

#[derive(Serialize)]
struct Provisioning {
    org: OrgId,
    org_create_ms: u128,
    org_created: bool,
    env_create_ms: Option<u128>,
    env_created: bool,
}

struct Endpoint {
    family: &'static str,
    pod: String,
    pooler: Pooler,
    logins: Vec<PgConnectOptions>,
}

#[derive(Serialize)]
struct Step {
    orgs: usize,
    endpoints: [EndpointLoad; 2],
    observations: Vec<Observation>,
    observation_errors: Vec<String>,
}

#[derive(Serialize)]
struct EndpointLoad {
    pooler: &'static str,
    acquire_us: Option<Percentiles>,
    login: Tally,
    acquire: Tally,
    query: Tally,
}

#[derive(Default, Serialize)]
struct Tally {
    ok: usize,
    errors: usize,
    timeouts: usize,
    first_error: Option<String>,
}

impl Tally {
    fn record<T>(
        &mut self,
        result: Result<sqlx::Result<T>, tokio::time::error::Elapsed>,
    ) -> Option<T> {
        match result {
            Ok(Ok(value)) => {
                self.ok += 1;
                Some(value)
            }
            Ok(Err(error)) => {
                self.errors += 1;
                self.first_error.get_or_insert_with(|| error.to_string());
                None
            }
            Err(_) => {
                self.timeouts += 1;
                None
            }
        }
    }

    fn merge(&mut self, other: Self) {
        self.ok += other.ok;
        self.errors += other.errors;
        self.timeouts += other.timeouts;
        self.first_error = self.first_error.take().or(other.first_error);
    }
}

#[derive(Default)]
struct ClientLoad {
    samples: Vec<AcquireTime>,
    login: Tally,
    acquire: Tally,
    query: Tally,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Percentiles {
    p50: AcquireTime,
    p95: AcquireTime,
    max: AcquireTime,
}

/// Once a client times out, its full wait is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct AcquireTime {
    us: u64,
    at_least: bool,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
struct Threshold {
    largest_within_2x: Option<usize>,
    first_above_2x: Option<usize>,
}

#[derive(Serialize)]
struct Observation {
    elapsed_ms: u128,
    pools: [Vec<PoolRow>; 2],
    pg_stat_activity: serde_json::Value,
}

#[derive(Deserialize, Serialize)]
struct PoolRow {
    database: String,
    user: String,
    cl_waiting: u64,
    maxwait: u64,
    maxwait_us: u64,
}

pub(super) async fn run(root: &Path, args: Args) -> anyhow::Result<()> {
    ensure!(
        args.steps.first().map(|step| step.get()) == Some(1)
            && args.steps.windows(2).all(|pair| pair[0] < pair[1])
            && args.steps.last().is_some_and(|last| *last <= args.max_step),
        "--steps must start at 1, increase, and stay within --max-step"
    );
    let step_timeout = Duration::from_secs(u64::from(args.step_timeout_secs.get()));
    ensure!(
        step_timeout > WINDOW + REQUEST_TIMEOUT,
        "--step-timeout-secs must allow the load window and its final request to finish"
    );
    let bundle = args
        .chart
        .canonicalize()
        .context("resolve --chart bundle")?;
    let cluster = timeout(REQUEST_TIMEOUT, Cluster::load(root))
        .await
        .context("load the journey cluster")??;
    ensure!(
        cluster.outputs.context == journey_cluster::CONTEXT,
        "the journey outputs must name the kind context {}",
        journey_cluster::CONTEXT
    );
    ensure!(
        bundle == cluster.outputs.bundle.canonicalize()?,
        "--chart must be the bundle installed by journey-cluster at {}",
        cluster.outputs.bundle.display()
    );
    let run_id = uuid::Uuid::now_v7().simple().to_string();
    let results = std::env::var_os("WAMN_RESULTS")
        .filter(|path| !path.is_empty())
        .map_or_else(|| root.join(OUTPUTS), PathBuf::from);
    std::fs::create_dir_all(&results)?;
    let mut file = tempfile::NamedTempFile::new_in(&results)?;
    let mut report = Run {
        bundle,
        steps_requested: args.steps,
        max_step: args.max_step,
        step_timeout_seconds: step_timeout.as_secs(),
        window_seconds: WINDOW.as_secs(),
        clients_per_login: CLIENTS_PER_LOGIN,
        request_timeout_seconds: REQUEST_TIMEOUT.as_secs(),
        query: QUERY,
        existing_orgs: None,
        provisioning: Vec::new(),
        steps: Vec::new(),
        threshold: Threshold::default(),
        error: None,
    };
    let result = measure(root, &cluster, &run_id, step_timeout, &mut report).await;
    report.threshold = threshold(report.steps.iter().map(|step| {
        let [control, worker] = &step.endpoints;
        (
            step.orgs,
            [control, worker].map(|load| load.acquire_us.as_ref().map(|times| times.p95)),
        )
    }));
    let result = result.and_then(|()| {
        ensure!(
            report.threshold.largest_within_2x.is_some(),
            "no complete one-org p95 baseline"
        );
        ensure!(
            report
                .steps
                .iter()
                .all(|step| !step.observations.is_empty()),
            "at least one step has no Pooler and PostgreSQL snapshots"
        );
        Ok(())
    });
    report.error = result.as_ref().err().map(|error| format!("{error:#}"));
    print_report(&report);
    serde_json::to_writer_pretty(&mut file, &report)?;
    let output = results.join(format!("pooler-capacity-{run_id}.json"));
    file.persist_noclobber(&output)?;
    println!("Results written to {}", output.display());
    result
}

async fn measure(
    root: &Path,
    cluster: &Cluster,
    run_id: &str,
    step_timeout: Duration,
    report: &mut Run,
) -> anyhow::Result<()> {
    let pods = Api::<Pod>::namespaced(cluster.client.clone(), PLATFORM);
    let sockets = tempfile::tempdir()?;
    let (mut observer, mut control, mut worker) = tokio::try_join!(
        async {
            timeout(REQUEST_TIMEOUT, cluster.observe_system())
                .await
                .context("connect to the system database as observer")?
        },
        endpoint(&pods, CONTROL, sockets.path()),
        endpoint(&pods, APPLY_TRANSACTION, sockets.path()),
    )?;
    report.existing_orgs = Some(
        timeout(
            REQUEST_TIMEOUT,
            sqlx::query_scalar("SELECT count(*) FROM registry.org")
                .fetch_one(&mut observer.connection),
        )
        .await
        .context("count existing orgs")??,
    );
    let env: EnvName = ENV.parse()?;
    for count in report.steps_requested.iter().map(|step| step.get()) {
        println!("Measuring {count} load orgs");
        let deadline = Instant::now() + step_timeout;
        for index in report.provisioning.len() + 1..=count {
            let org: OrgId = format!("pc-{}-{index}", &run_id[20..]).parse()?;
            report.provisioning.push(Provisioning {
                org,
                org_create_ms: 0,
                org_created: false,
                env_create_ms: None,
                env_created: false,
            });
            let record = report.provisioning.last_mut().context("new org record")?;
            provision(root, cluster, &env, deadline, record).await?;
            let worker_namespace = name::env_key(&record.org, &env);
            let (control_login, worker_login) = timeout_at(deadline, async {
                tokio::try_join!(
                    control.pooler.login(&cluster.client, record.org.as_str()),
                    worker.pooler.login(&cluster.client, &worker_namespace),
                )
            })
            .await
            .with_context(|| {
                format!("read the Pooler logins for {} within the step", record.org)
            })??;
            control.logins.push(control_login);
            worker.logins.push(worker_login);
        }
        let start = Instant::now();
        ensure!(
            start + WINDOW + REQUEST_TIMEOUT < deadline,
            "step {count} ran out of time for the load window"
        );
        let (control_load, worker_load, (observations, observation_errors)) =
            timeout_at(deadline, async {
                tokio::join!(
                    load_endpoint(&control, start + WINDOW),
                    load_endpoint(&worker, start + WINDOW),
                    observe(&pods, &control, &worker, &mut observer.connection, start),
                )
            })
            .await
            .with_context(|| format!("measure step {count} within {step_timeout:?}"))?;
        report.steps.push(Step {
            orgs: count,
            endpoints: [control_load, worker_load],
            observations,
            observation_errors,
        });
    }
    Ok(())
}

async fn endpoint(
    pods: &Api<Pod>,
    family: &'static str,
    sockets: &Path,
) -> anyhow::Result<Endpoint> {
    let pod = ready_pooler(pods, family).await?;
    let pooler = timeout(
        REQUEST_TIMEOUT,
        Pooler::start(pods.clone(), &pod, family, sockets),
    )
    .await
    .with_context(|| format!("start forwarding to {pod}"))??;
    Ok(Endpoint {
        family,
        pod,
        pooler,
        logins: Vec::new(),
    })
}

async fn provision(
    root: &Path,
    cluster: &Cluster,
    env: &EnvName,
    deadline: Instant,
    record: &mut Provisioning,
) -> anyhow::Result<()> {
    println!("Provisioning {}/{env}", record.org);
    let (org_command, _) = cluster.org_create_command(&record.org);
    let start = Instant::now();
    let result = process::execute_before(
        tokio::process::Command::from(org_command).current_dir(root),
        deadline,
    )
    .await;
    record.org_create_ms = start.elapsed().as_millis();
    result.with_context(|| format!("create org {}", record.org))?;
    record.org_created = true;

    let start = Instant::now();
    let result = process::execute_before(
        tokio::process::Command::from(cluster.env_create_command(&record.org, env))
            .current_dir(root),
        deadline,
    )
    .await;
    record.env_create_ms = Some(start.elapsed().as_millis());
    result.with_context(|| format!("create env {}/{env}", record.org))?;
    record.env_created = true;
    Ok(())
}

async fn load_endpoint(endpoint: &Endpoint, end: Instant) -> EndpointLoad {
    let clients = endpoint
        .logins
        .iter()
        .flat_map(|login| (0..CLIENTS_PER_LOGIN).map(move |_| load_client(login, end)));
    let mut login = Tally::default();
    let mut acquire = Tally::default();
    let mut query = Tally::default();
    let samples = join_all(clients).await.into_iter().map(|client| {
        login.merge(client.login);
        acquire.merge(client.acquire);
        query.merge(client.query);
        client.samples
    });
    EndpointLoad {
        pooler: endpoint.family,
        acquire_us: percentiles(samples),
        login,
        acquire,
        query,
    }
}

async fn load_client(login: &PgConnectOptions, end: Instant) -> ClientLoad {
    let mut load = ClientLoad::default();
    while Instant::now() < end {
        if let Some(mut connection) = load
            .login
            .record(timeout(REQUEST_TIMEOUT, login.connect()).await)
        {
            while Instant::now() < end {
                let start = Instant::now();
                // BEGIN waits for a server connection in transaction mode. Measure
                // that round trip before running the sleep on the same backend.
                let deadline = start + REQUEST_TIMEOUT;
                let acquire = timeout_at(
                    deadline,
                    sqlx::raw_sql("BEGIN READ ONLY").execute(&mut connection),
                )
                .await;
                let at_least = acquire.is_err();
                let acquired = load.acquire.record(acquire).is_some();
                if acquired || at_least {
                    load.samples.push(AcquireTime {
                        us: u64::try_from(start.elapsed().min(REQUEST_TIMEOUT).as_micros())
                            .expect("a request lasts at most 15 seconds"),
                        at_least,
                    });
                }
                if !acquired {
                    break;
                }
                let query =
                    timeout_at(deadline, sqlx::raw_sql(QUERY).execute(&mut connection)).await;
                if load.query.record(query).is_none() {
                    break;
                }
            }
        }
        // On error, drop the session and give the Pooler a moment before reconnecting.
        sleep_until((Instant::now() + Duration::from_millis(100)).min(end)).await;
    }
    load
}

async fn observe(
    pods: &Api<Pod>,
    control: &Endpoint,
    worker: &Endpoint,
    observer: &mut PgConnection,
    start: Instant,
) -> (Vec<Observation>, Vec<String>) {
    let end = start + WINDOW;
    let mut interval = interval_at(start, SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut samples = Vec::new();
    let mut errors = Vec::new();
    while let Ok(scheduled) = timeout_at(end, interval.tick()).await {
        if Instant::now() >= end {
            break;
        }
        let elapsed_ms = start.elapsed().as_millis();
        let sample = timeout_at(
            (scheduled + REQUEST_TIMEOUT).min(end),
            async {
                tokio::try_join!(
                    show_pools(pods, &control.pod),
                    show_pools(pods, &worker.pod),
                    async {
                        sqlx::query_scalar(
                            "SELECT coalesce(jsonb_agg(to_jsonb(activity)), '[]'::jsonb)
                             FROM (SELECT datname, usename, backend_type, state, count(*) AS connections
                                   FROM pg_stat_activity GROUP BY 1, 2, 3, 4 ORDER BY 1, 2, 3, 4) AS activity",
                        )
                        .fetch_one(&mut *observer)
                        .await
                        .context("read pg_stat_activity as observer")
                    },
                )
            },
        )
        .await;
        match sample {
            Ok(Ok((control, worker_transaction, pg_stat_activity))) => samples.push(Observation {
                elapsed_ms,
                pools: [control, worker_transaction],
                pg_stat_activity,
            }),
            Ok(Err(error)) => errors.push(format!("{elapsed_ms} ms: {error:#}")),
            Err(_) => errors.push(format!("{elapsed_ms} ms: observation timed out")),
        }
    }
    (samples, errors)
}

async fn show_pools(pods: &Api<Pod>, pod: &str) -> anyhow::Result<Vec<PoolRow>> {
    // The local console uses peer authentication for the pgbouncer container user.
    let csv = exec(
        pods,
        pod,
        &AttachParams::default().container("pgbouncer"),
        [
            "psql",
            "-Xtw",
            "--csv",
            "-h",
            "/controller/run",
            "-p",
            "5432",
            "-U",
            "pgbouncer",
            "-d",
            "pgbouncer",
            "-c",
            "SHOW POOLS",
        ],
    )
    .await
    .with_context(|| format!("SHOW POOLS on {pod}"))?;
    // CSV column names select the measured fields independently of other Pooler columns.
    csv::Reader::from_reader(csv.as_bytes())
        .deserialize::<PoolRow>()
        .collect::<Result<_, _>>()
        .with_context(|| format!("read SHOW POOLS on {pod}"))
}

fn percentiles(clients: impl Iterator<Item = Vec<AcquireTime>>) -> Option<Percentiles> {
    // Give each client equal weight so that fast clients cannot hide client waits.
    // Fixed-point weights avoid floating-point drift at an exact rank boundary.
    const CLIENT_WEIGHT: u128 = 10_u128.pow(18);
    let mut samples = Vec::new();
    let mut weight = 0_u128;
    for client in clients.filter(|client| !client.is_empty()) {
        let count = client.len() as u128;
        // Spread rounding across the client's samples and preserve its total weight.
        let quotient = CLIENT_WEIGHT / count;
        let remainder = CLIENT_WEIGHT % count;
        samples.extend(
            client
                .into_iter()
                .enumerate()
                .map(|(index, time)| (time, quotient + u128::from((index as u128) < remainder))),
        );
        weight += CLIENT_WEIGHT;
    }
    samples.sort_unstable_by_key(|(time, _)| *time);
    let max = samples.last()?.0;
    let mut cumulative = 0;
    let mut p50 = None;
    for (time, sample_weight) in samples {
        cumulative += sample_weight;
        if cumulative >= weight / 2 {
            p50.get_or_insert(time);
        }
        if cumulative >= weight - weight / 20 {
            return Some(Percentiles {
                p50: p50.unwrap_or(time),
                p95: time,
                max,
            });
        }
    }
    None
}

fn threshold(mut steps: impl Iterator<Item = (usize, [Option<AcquireTime>; 2])>) -> Threshold {
    let mut result = Threshold::default();
    let Some((1, [Some(control), Some(worker)])) = steps.next() else {
        return result;
    };
    if control.at_least || worker.at_least {
        return result;
    }
    let limits = [control.us, worker.us].map(|base| base.saturating_mul(2));
    result.largest_within_2x = Some(1);
    for (orgs, p95) in steps {
        let mut pairs = p95.into_iter().zip(limits);
        if pairs
            .clone()
            .any(|(time, limit)| time.is_some_and(|time| time.us > limit))
        {
            result.first_above_2x.get_or_insert(orgs);
        } else if pairs
            .all(|(time, limit)| time.is_some_and(|time| !time.at_least && time.us <= limit))
        {
            result.largest_within_2x = Some(orgs);
        }
    }
    result
}

fn print_report(report: &Run) {
    println!(
        "{:>4}  {:<17} {:>10} {:>10} {:>10} {:>9} {:>8} {:>8} {:>10} {:>12}",
        "Orgs",
        "Pooler",
        "p50 us",
        "p95 us",
        "max us",
        "Queries",
        "Errors",
        "Timeouts",
        "Waiting",
        "Max wait us"
    );
    for step in &report.steps {
        for (index, load) in step.endpoints.iter().enumerate() {
            let pools = step.observations.iter().map(|sample| &sample.pools[index]);
            let waiting = pools
                .clone()
                .map(|rows| rows.iter().map(|row| row.cl_waiting).sum::<u64>())
                .max();
            let max_wait = pools
                .flatten()
                .map(|row| row.maxwait * 1_000_000 + row.maxwait_us)
                .max();
            let show =
                |metric: Option<u64>| metric.map_or_else(|| "-".to_owned(), |n| n.to_string());
            let latency = |time: Option<AcquireTime>| {
                time.map_or_else(
                    || "-".to_owned(),
                    |time| format!("{}{}", if time.at_least { "≥" } else { "" }, time.us),
                )
            };
            let times = load.acquire_us.as_ref();
            println!(
                "{:>4}  {:<17} {:>10} {:>10} {:>10} {:>9} {:>8} {:>8} {:>10} {:>12}",
                step.orgs,
                load.pooler,
                latency(times.map(|time| time.p50)),
                latency(times.map(|time| time.p95)),
                latency(times.map(|time| time.max)),
                load.query.ok,
                load.login.errors + load.acquire.errors + load.query.errors,
                load.login.timeouts + load.acquire.timeouts + load.query.timeouts,
                show(waiting),
                show(max_wait),
            );
        }
    }
    let step =
        |value: Option<usize>| value.map_or_else(|| "none measured".to_owned(), |n| n.to_string());
    println!(
        "Largest step within 2x the one-org p95: {}",
        step(report.threshold.largest_within_2x)
    );
    println!(
        "First step above 2x the one-org p95: {}",
        step(report.threshold.first_above_2x)
    );
    for record in &report.provisioning {
        let summary = if record.org_created {
            "left in place"
        } else {
            "creation unconfirmed"
        };
        println!("Org {} {summary}", record.org);
    }
}

#[cfg(test)]
mod tests {
    use super::{AcquireTime, Percentiles, Threshold, percentiles, threshold};

    fn exact(us: u64) -> AcquireTime {
        AcquireTime {
            us,
            at_least: false,
        }
    }

    #[test]
    fn acquire_percentiles_use_nearest_rank() {
        let samples = (1..=20).rev().map(exact).collect();
        assert_eq!(
            percentiles([samples].into_iter()),
            Some(Percentiles {
                p50: exact(10),
                p95: exact(19),
                max: exact(20)
            })
        );
        assert_eq!(percentiles(std::iter::empty()), None);
    }

    #[test]
    fn acquire_percentiles_give_each_client_equal_weight() {
        let timed_out = AcquireTime {
            us: 15_000_000,
            at_least: true,
        };
        let fast = vec![exact(100); 100];
        assert_eq!(
            percentiles([fast, vec![timed_out]].into_iter()),
            Some(Percentiles {
                p50: exact(100),
                p95: timed_out,
                max: timed_out
            })
        );
    }

    #[test]
    fn threshold_compares_each_pooler_to_its_own_baseline() {
        let step = |orgs, control, worker| (orgs, [Some(exact(control)), Some(exact(worker))]);
        let steps = [
            step(1, 100, 200),
            step(2, 200, 400),
            step(4, 200, 401),
            step(8, 180, 300),
            (12, [Some(exact(180)), None]),
            step(16, 201, 390),
        ];
        assert_eq!(
            threshold(steps.into_iter()),
            Threshold {
                largest_within_2x: Some(8),
                first_above_2x: Some(4)
            }
        );
        assert_eq!(threshold(steps[1..].iter().copied()), Threshold::default());
    }

    #[test]
    fn a_timeout_can_prove_a_step_exceeds_the_threshold() {
        let timeout = |us| Some(AcquireTime { us, at_least: true });
        let steps = [
            (1, [Some(exact(100)), Some(exact(200))]),
            (2, [Some(exact(200)), timeout(400)]),
            (4, [Some(exact(200)), timeout(401)]),
        ];
        assert_eq!(
            threshold(steps.into_iter()),
            Threshold {
                largest_within_2x: Some(1),
                first_above_2x: Some(4)
            }
        );
    }
}
