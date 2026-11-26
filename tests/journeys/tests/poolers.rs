//! The system Poolers admit org logins, queue at the per-login limit, and expose their settings.
//! Cargo runs this target separately from the certificate renewal target.

mod common;

use std::cell::Cell;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use common::request::until;
use common::{Env, Org, PortForward, exec, scale_deployment, secret_key};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use kube::api::{AttachParams, ListParams};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{ConnectOptions as _, Connection as _, PgConnection};
use tokio::net::{TcpStream, UnixListener};
use tokio::task::JoinSet;
use tokio::time::timeout;
use wamn_http_client::HttpClient;
use wamn_test_infrastructure::scratch::ScratchRoot;

const PLATFORM: &str = "platform";
const DATABASE: &str = "wamn_system";
const POSTGRES_PORT: u16 = 5432;
const METRICS_PORT: u16 = 9127;
const CONTROL: &str = "control";
const APPLY: &str = "apply";
/// Time for org creation and environment apply through their CLI waits.
const PROVISION_DEADLINE: Duration = Duration::from_mins(15);
/// Time for scaling control or for finished work to leave the org.
const CONTROL_DEADLINE: Duration = Duration::from_secs(120);
/// Time for a Pooler pod to report Ready or for all held sessions to open.
const READY_DEADLINE: Duration = Duration::from_secs(60);
/// Time for credential reads, a port-forward setup, or an individual request.
const REQUEST_DEADLINE: Duration = Duration::from_secs(15);
/// Time for the extra client to appear in the Pooler metrics.
const QUEUE_DEADLINE: Duration = Duration::from_secs(30);
/// Time to close a held session and receive the queued answer.
const RESUME_DEADLINE: Duration = Duration::from_secs(10);

common::journey! {
    /// C7. The next client waits at each per-login limit and proceeds when one session closes.
    async fn each_login_queues_at_its_connection_limit() -> anyhow::Result<()> {
        let env = provision().await?;
        let org = &env.org;
        let mut control = RestoreControl {
            org,
            name: wamn_registry::name::control_resource(&org.name),
            armed: true,
        };
        let result = async {
            control.scale(0).await?;
            wait_for_idle_org(org, &control.name).await?;
            let pods = platform_pods(org);
            for (family, limit) in [(CONTROL, 8), (APPLY, 16)] {
                let pod = ready_pooler(&pods, family).await?;
                assert_queue(org, &pods, &pod, family, limit)
                    .await
                    .with_context(|| format!("check the {family} login limit on {pod}"))?;
            }
            Ok(())
        }
        .await;
        let restored = control.restore().await;
        match (result, restored) {
            (result, Ok(())) | (Ok(()), result) => result,
            (Err(test), Err(restore)) => Err(test.context(format!("restore control: {restore:#}"))),
        }
    }
}

common::journey! {
    /// C8. Each Pooler admits the matching provisioned org login through its auth query.
    async fn each_pooler_admits_its_provisioned_login() -> anyhow::Result<()> {
        let env = provision().await?;
        let org = &env.org;
        let pods = platform_pods(org);
        for family in [CONTROL, APPLY] {
            let pod = ready_pooler(&pods, family).await?;
            let login = pooler_login(org, &pods, &pod, family).await?;
            let probe = async {
                let connection = login
                    .options
                    .connect()
                    .await
                    .context("connect to the Pooler")?;
                select_one(connection).await
            };
            timeout(REQUEST_DEADLINE, probe)
                .await
                .with_context(|| {
                    format!("{family} login probe through {pod} exceeded {REQUEST_DEADLINE:?}")
                })?
                .with_context(|| format!("{family} login answers SELECT 1 through {pod}"))?;
        }
        Ok(())
    }
}

common::journey! {
    /// C9. The local admin console reports the live query and apply login wait settings.
    async fn each_pooler_reports_its_wait_settings() -> anyhow::Result<()> {
        let env = provision().await?;
        let pods = platform_pods(&env.org);
        for (family, query_wait) in [(CONTROL, 120), (APPLY, 900)] {
            let pod = ready_pooler(&pods, family).await?;
            // CloudNativePG configures peer authentication on this local console.
            let config = timeout(
                REQUEST_DEADLINE,
                exec(
                    &pods,
                    &pod,
                    &AttachParams::default().container("pgbouncer"),
                    [
                        "psql",
                        "-XAtw",
                        "-F",
                        "\t",
                        "-h",
                        "/controller/run",
                        "-p",
                        "5432",
                        "-U",
                        "pgbouncer",
                        "-d",
                        "pgbouncer",
                        "-c",
                        "SHOW CONFIG",
                    ],
                ),
            )
            .await
            .with_context(|| format!("SHOW CONFIG on {pod} within {REQUEST_DEADLINE:?}"))??;
            assert_setting(&pod, &config, "query_wait_timeout", query_wait)?;
            if family == APPLY {
                assert_setting(&pod, &config, "client_login_timeout", 900)?;
            }
        }
        Ok(())
    }
}

async fn provision() -> anyhow::Result<Env> {
    timeout(PROVISION_DEADLINE, async {
        Org::create().await?.apply(common::host::FIXTURE).await
    })
    .await
    .with_context(|| format!("apply the test environment within {PROVISION_DEADLINE:?}"))?
}

fn platform_pods(org: &Org) -> Api<Pod> {
    Api::namespaced(org.cluster.client.clone(), PLATFORM)
}

async fn ready_pooler(pods: &Api<Pod>, family: &str) -> anyhow::Result<String> {
    let name = format!("wamn-pg-{family}");
    let selector = ListParams::default().labels(&format!("cnpg.io/poolerName={name}"));
    let waiting = format!("a ready pod for {PLATFORM}/{name}");
    timeout(
        READY_DEADLINE,
        until(READY_DEADLINE, &waiting, || async {
            pods.list(&selector)
                .await?
                .items
                .into_iter()
                .find(|pod| pod.metadata.deletion_timestamp.is_none() && common::host::ready(pod))
                .map(|pod| pod.metadata.name.context("ready Pooler pod name"))
                .transpose()
        }),
    )
    .await
    .with_context(|| format!("find {waiting} within {READY_DEADLINE:?}"))?
}

async fn wait_for_idle_org(org: &Org, control: &str) -> anyhow::Result<()> {
    let namespace = org.name.as_str();
    let pods = Api::<Pod>::namespaced(org.cluster.client.clone(), namespace);
    let jobs = Api::<Job>::namespaced(org.cluster.client.clone(), namespace);
    let selector = ListParams::default().labels(&format!("app.kubernetes.io/name={control}"));
    let waiting = format!("control pods to exit and Jobs to finish in {namespace}");
    timeout(
        CONTROL_DEADLINE,
        until(CONTROL_DEADLINE, &waiting, || async {
            let all_jobs = ListParams::default();
            let (pods, jobs) = tokio::try_join!(pods.list(&selector), jobs.list(&all_jobs))?;
            let finished = jobs.items.iter().all(|job| {
                job.status
                    .iter()
                    .flat_map(|status| status.conditions.iter().flatten())
                    .any(|condition| {
                        condition.status == "True"
                            && matches!(condition.type_.as_str(), "Complete" | "Failed")
                    })
            });
            Ok((pods.items.is_empty() && finished).then_some(()))
        }),
    )
    .await
    .with_context(|| format!("wait for {waiting} within {CONTROL_DEADLINE:?}"))?
}

struct RestoreControl<'a> {
    org: &'a Org,
    name: String,
    armed: bool,
}

impl RestoreControl<'_> {
    async fn scale(&self, replicas: i32) -> anyhow::Result<()> {
        scale_deployment(
            &self.org.cluster.client,
            self.org.name.as_str(),
            &self.name,
            replicas,
            CONTROL_DEADLINE,
        )
        .await
    }

    async fn restore(&mut self) -> anyhow::Result<()> {
        self.scale(1).await?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for RestoreControl<'_> {
    fn drop(&mut self) {
        if self.armed {
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.scale(1))
            });
            if let Err(error) = result {
                eprintln!("restore {} to one replica: {error:#}", self.name);
            }
        }
    }
}

struct PoolerLogin {
    options: PgConnectOptions,
    _relay: JoinSet<anyhow::Result<()>>,
    _forward: PortForward,
    _scratch: ScratchRoot,
}

async fn pooler_login(
    org: &Org,
    pods: &Api<Pod>,
    pod: &str,
    family: &str,
) -> anyhow::Result<PoolerLogin> {
    let namespace = org.name.as_str();
    let client = &org.cluster.client;
    let secret = format!("wamn-{family}-db");
    let (username, password, ca, forward) = timeout(REQUEST_DEADLINE, async {
        tokio::try_join!(
            secret_key(client, namespace, &secret, "username"),
            secret_key(client, namespace, &secret, "password"),
            secret_key(client, namespace, "wamn-pg-ca", "ca.crt"),
            PortForward::start(pods.clone(), pod, POSTGRES_PORT),
        )
    })
    .await
    .with_context(|| {
        format!("read {namespace}/{secret} and forward to {pod} within {REQUEST_DEADLINE:?}")
    })??;
    let username = String::from_utf8(username).context("Pooler login username must be UTF-8")?;
    let password = String::from_utf8(password).context("Pooler login password must be UTF-8")?;
    ensure!(
        !username.is_empty() && !password.is_empty() && !ca.is_empty(),
        "empty Pooler login or CA in {namespace}/{secret}"
    );

    // SQLx uses the TCP host as the TLS name. A local Unix relay selects the port-forward
    // independently, so VerifyFull still checks the Pooler Service DNS name.
    let scratch = ScratchRoot::create()?;
    let address = forward.address;
    let socket = scratch.path().join(format!(".s.PGSQL.{}", address.port()));
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("bind the SQLx relay at {}", socket.display()))?;
    let mut relay = JoinSet::new();
    relay.spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut local, _) = listener
                .accept()
                .await
                .context("accept a SQLx relay connection")?;
            connections.spawn(async move {
                let result = async {
                    let mut tcp = TcpStream::connect(address).await?;
                    tokio::io::copy_bidirectional(&mut local, &mut tcp).await?;
                    std::io::Result::Ok(())
                }
                .await;
                if let Err(error) = result {
                    eprintln!("relay SQLx to {address}: {error}");
                }
            });
        }
    });
    let options = PgConnectOptions::new_without_pgpass()
        .host(&format!("wamn-pg-{family}.{PLATFORM}.svc.cluster.local"))
        .port(address.port())
        .socket(scratch.path())
        .database(DATABASE)
        .username(&username)
        .password(&password)
        .ssl_root_cert_from_pem(ca)
        .ssl_mode(PgSslMode::VerifyFull);
    Ok(PoolerLogin {
        options,
        _relay: relay,
        _forward: forward,
        _scratch: scratch,
    })
}

async fn assert_queue(
    org: &Org,
    pods: &Api<Pod>,
    pod: &str,
    family: &str,
    limit: usize,
) -> anyhow::Result<()> {
    let login = pooler_login(org, pods, pod, family).await?;
    let metrics = timeout(
        REQUEST_DEADLINE,
        PortForward::start(pods.clone(), pod, METRICS_PORT),
    )
    .await
    .context("start the Pooler metrics port-forward")??;
    let url = format!("http://{}/metrics", metrics.address);
    let http = HttpClient::builder().timeout(REQUEST_DEADLINE).build()?;
    let mut held = Vec::with_capacity(limit);
    timeout(READY_DEADLINE, async {
        for index in 0..limit {
            let session = hold_session(&login.options)
                .await
                .with_context(|| format!("open held session {}", index + 1))?;
            held.push(session);
        }
        anyhow::Ok(())
    })
    .await
    .with_context(|| format!("open all {limit} held sessions within {READY_DEADLINE:?}"))??;

    let mut released = held.pop().context("a held session to close")?;
    // Session pooling keeps this backend linked after ROLLBACK. Closing an idle
    // session lets PgBouncer reuse its backend for the queued client.
    timeout(
        REQUEST_DEADLINE,
        sqlx::raw_sql("ROLLBACK").execute(&mut released),
    )
    .await
    .context("end the transaction in the session that will close")??;

    let username = login.options.get_username();
    let counts = queue_counts(&http, &url, username).await?;
    ensure!(
        counts == (0, limit),
        "{username} waiting and active counts before the extra client: {counts:?}, expected (0, {limit})"
    );

    let connection = timeout(REQUEST_DEADLINE, login.options.connect())
        .await
        .context("connect the extra session within the request deadline")?
        .context("connect the extra session")?;
    let mut extra = JoinSet::new();
    extra.spawn(select_one(connection));
    let waiting = format!("{username} has one waiting client and {limit} active servers");
    let last_counts = Cell::new(counts);
    let queued = until(QUEUE_DEADLINE, &waiting, || async {
        let counts = queue_counts(&http, &url, username).await?;
        last_counts.set(counts);
        Ok((counts == (1, limit)).then_some(()))
    });
    tokio::select! {
        biased;
        result = extra.join_next() => {
            result.context("extra session task")??.context("extra session finished before queueing")?;
            bail!("extra session answered while all {limit} sessions held a server");
        }
        result = timeout(QUEUE_DEADLINE, queued) => {
            result.context("Pooler metrics polling timed out").flatten().with_context(|| {
                let counts = last_counts.get();
                format!("observe {waiting} within {QUEUE_DEADLINE:?}; last waiting and active counts: {counts:?}")
            })?;
        }
    }

    timeout(RESUME_DEADLINE, async {
        released.close().await.context("close one held session")?;
        extra.join_next().await.context("extra session task")??
    })
    .await
    .with_context(|| {
        format!("extra query answers within {RESUME_DEADLINE:?} after closing one session")
    })??;
    timeout(REQUEST_DEADLINE, async {
        for connection in held {
            connection
                .close()
                .await
                .context("close the remaining held session")?;
        }
        anyhow::Ok(())
    })
    .await
    .context("close the remaining held sessions")??;
    Ok(())
}

async fn hold_session(options: &PgConnectOptions) -> anyhow::Result<PgConnection> {
    let mut connection = options.connect().await.context("connect to the Pooler")?;
    sqlx::raw_sql("BEGIN READ ONLY; SELECT 1")
        .execute(&mut connection)
        .await
        .context("hold a server connection in a read-only transaction")?;
    Ok(connection)
}

async fn select_one(mut connection: PgConnection) -> anyhow::Result<()> {
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut connection)
        .await
        .context("execute SELECT 1")?;
    ensure!(one == 1, "SELECT 1 returned {one}");
    connection
        .close()
        .await
        .context("close the SELECT 1 session")
}

async fn queue_counts(
    http: &HttpClient,
    url: &str,
    username: &str,
) -> anyhow::Result<(usize, usize)> {
    let response = http
        .get(url)
        .send()
        .await
        .context("scrape Pooler metrics")?;
    ensure!(
        response.status() == hyper::StatusCode::OK,
        "Pooler metrics at {url} returned {}",
        response.status()
    );
    let text = response.text().await.context("read Pooler metrics")?;
    let count = |name| -> anyhow::Result<usize> {
        let sample =
            format!(r#"cnpg_pgbouncer_pools_{name}{{database="{DATABASE}",user="{username}"}} "#);
        text.lines()
            .find_map(|line| line.strip_prefix(&sample))
            .with_context(|| {
                format!(
                    "Pooler metrics at {url} have no {} sample",
                    sample.trim_end()
                )
            })?
            .trim()
            .parse()
            .with_context(|| format!("parse the {name} count for {username}"))
    };
    Ok((count("cl_waiting")?, count("sv_active")?))
}

fn assert_setting(pod: &str, config: &str, key: &str, seconds: u64) -> anyhow::Result<()> {
    let value = config
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(name, _)| *name == key)
        .and_then(|(_, columns)| columns.split('\t').next())
        .with_context(|| format!("{pod} SHOW CONFIG has no {key} value"))?;
    let setting = || format!("{pod} {key} seconds: {value}");
    let duration =
        Duration::try_from_secs_f64(value.parse().with_context(setting)?).with_context(setting)?;
    ensure!(
        duration == Duration::from_secs(seconds),
        "{pod} reports {key}={value}, expected {seconds} seconds"
    );
    Ok(())
}
