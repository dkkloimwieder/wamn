//! The system Poolers admit org logins, queue at the shared connection limit.
//! Cargo runs this target separately from the certificate renewal target.

mod common;

use std::cell::Cell;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use common::request::until;
use common::{Env, Org, PortForward, exec, secret_key};
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
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
const APPLY_TRANSACTION: &str = "apply-transaction";
/// Time for org creation and environment apply through their CLI waits.
const PROVISION_DEADLINE: Duration = Duration::from_mins(15);
/// Time for a Pooler pod to report Ready or for all held sessions to open.
const READY_DEADLINE: Duration = Duration::from_secs(60);
/// Time for credential reads, a port-forward setup, or an individual request.
const REQUEST_DEADLINE: Duration = Duration::from_secs(15);
/// Time for the extra client to appear in the Pooler metrics.
const QUEUE_DEADLINE: Duration = Duration::from_secs(30);
/// Time to release a backend and receive the queued answer.
const RESUME_DEADLINE: Duration = Duration::from_secs(10);

common::journey! {
    /// A second org waits at the shared cap. It proceeds after a transaction ends.
    async fn orgs_share_the_transaction_pooler_budget() -> anyhow::Result<()> {
        let holder = provision().await?;
        let waiter = provision().await?;
        let pods = platform_pods(&holder.org);
        let pod = ready_pooler(&pods, APPLY_TRANSACTION).await?;
        let limit = backend_limit(&pods, &pod).await?;
        assert_queue(&holder.org, &waiter.org, &pods, &pod, limit).await
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

async fn backend_limit(pods: &Api<Pod>, pod: &str) -> anyhow::Result<usize> {
    // CloudNativePG configures peer authentication on the local admin console.
    let config = timeout(
        REQUEST_DEADLINE,
        exec(
            pods,
            pod,
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
    let limit = config
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find_map(|(key, columns)| (key == "max_db_connections").then_some(columns))
        .and_then(|columns| columns.split('\t').next())
        .context("the Pooler reports its database connection limit")?
        .parse()?;
    Ok(limit)
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
    let role = if family == CONTROL { CONTROL } else { APPLY };
    let secret = format!("wamn-{role}-db");
    let roots = Api::<ConfigMap>::namespaced(client.clone(), namespace);
    let (username, password, ca, forward) = timeout(REQUEST_DEADLINE, async {
        tokio::try_join!(
            secret_key(client, namespace, &secret, "username"),
            secret_key(client, namespace, &secret, "password"),
            async { Ok(roots.get("wamn-pg-ca").await?) },
            PortForward::start(pods.clone(), pod, POSTGRES_PORT),
        )
    })
    .await
    .with_context(|| {
        format!("read {namespace}/{secret} and forward to {pod} within {REQUEST_DEADLINE:?}")
    })??;
    let username = String::from_utf8(username).context("Pooler login username must be UTF-8")?;
    let password = String::from_utf8(password).context("Pooler login password must be UTF-8")?;
    let ca = ca
        .data
        .and_then(|mut data| data.remove("ca.crt"))
        .context("org database CA ConfigMap has no ca.crt")?;
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
        .ssl_root_cert_from_pem(ca.into_bytes())
        .ssl_mode(PgSslMode::VerifyFull);
    Ok(PoolerLogin {
        options,
        _relay: relay,
        _forward: forward,
        _scratch: scratch,
    })
}

async fn assert_queue(
    holder: &Org,
    waiter: &Org,
    pods: &Api<Pod>,
    pod: &str,
    limit: usize,
) -> anyhow::Result<()> {
    let holder = pooler_login(holder, pods, pod, APPLY_TRANSACTION).await?;
    let waiter = pooler_login(waiter, pods, pod, APPLY_TRANSACTION).await?;
    let metrics = timeout(
        REQUEST_DEADLINE,
        PortForward::start(pods.clone(), pod, METRICS_PORT),
    )
    .await
    .context("start the Pooler metrics port-forward")??;
    let url = format!("http://{}/metrics", metrics.address);
    let http = HttpClient::builder().timeout(REQUEST_DEADLINE).build()?;
    let mut connection = timeout(REQUEST_DEADLINE, waiter.options.connect()).await??;
    timeout(
        REQUEST_DEADLINE,
        sqlx::query("SELECT 1").execute(&mut connection),
    )
    .await
    .context("prepare the waiting client's statement before saturating the pool")??;
    let mut held = Vec::with_capacity(limit);
    timeout(READY_DEADLINE, async {
        for index in 0..limit {
            let session = hold_session(&holder.options)
                .await
                .with_context(|| format!("open held session {}", index + 1))?;
            held.push(session);
        }
        anyhow::Ok(())
    })
    .await
    .with_context(|| format!("open all {limit} held sessions within {READY_DEADLINE:?}"))??;

    let username = waiter.options.get_username();
    let mut extra = JoinSet::new();
    extra.spawn(select_one(connection));
    let waiting = format!("{username} queues while the other org holds the backend budget");
    let last_counts = Cell::new((0, 0));
    let queued = until(QUEUE_DEADLINE, &waiting, || async {
        let counts = queue_counts(&http, &url, username).await?;
        last_counts.set(counts);
        Ok((counts == (1, 0)).then_some(()))
    });
    tokio::select! {
        biased;
        result = extra.join_next() => {
            result.context("extra session task")??.context("extra session finished before queueing")?;
            bail!("extra session answered while the other org held the backend budget");
        }
        result = timeout(QUEUE_DEADLINE, queued) => {
            result.context("Pooler metrics polling timed out").flatten().with_context(|| {
                let counts = last_counts.get();
                format!("observe {waiting} within {QUEUE_DEADLINE:?}; last waiting and active counts: {counts:?}")
            })?;
        }
    }

    timeout(RESUME_DEADLINE, async {
        // Keep the client open. Transaction pooling releases its backend at rollback.
        sqlx::raw_sql("ROLLBACK")
            .execute(held.last_mut().context("a held transaction")?)
            .await
            .context("release one backend")?;
        extra.join_next().await.context("extra session task")??
    })
    .await
    .with_context(|| {
        format!("extra query answers within {RESUME_DEADLINE:?} after ending one transaction")
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
