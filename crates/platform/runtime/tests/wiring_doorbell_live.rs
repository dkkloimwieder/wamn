//! Ignored live gate for the pointer-flip doorbell (wamn-0h0g.16.15).
//!
//! The unit tests in `wiring_doorbell.rs` prove what the subscriber DECIDES.
//! This proves the wire: a flip performed by wamn-0h0g.18.2's real activation
//! statement, against the real DDL trigger, reaches a real `LISTEN` connection
//! and makes the router's `WiringCache` serve the new active version on the next
//! resolution. Nothing here is a mock — the flip is `wamn_catalog`'s own
//! `flip_activation()` through `PREPARE`/`EXECUTE`, the read is its own
//! `resolve_active_wiring()`, and the subscriber is the production
//! `WiringDoorbellListener::postgres`.
//!
//! Run it against a THROWAWAY superuser database — it rewrites the `catalog`
//! schema and creates cluster-wide roles:
//!
//! ```text
//! docker run -d --name pg -e POSTGRES_PASSWORD=pw -p 5433:5432 postgres:18
//! WAMN_CATALOG_PG_URL=postgresql://postgres:pw@127.0.0.1:5433/postgres \
//!   cargo test -p wamn-runtime --test wiring_doorbell_live -- --ignored
//! ```

use std::io::Write as _;
use std::num::NonZeroUsize;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio_postgres::NoTls;
use wamn_catalog::{flip_activation, resolve_active_wiring};
use wamn_router::{CacheInsert, Wiring, WiringCache, WiringNode};
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_runtime::wiring_doorbell::WiringDoorbellListener;

const TENANT: &str = "t1";
const CATALOG: &str = "shop";
const ENVIRONMENT: &str = "prod";
const WIRING: &str = "orders-create";

fn hash(letter: char) -> String {
    format!("sha256:{}", String::from(letter).repeat(64))
}

fn psql(url: &str, script: &str) -> String {
    let mut child = Command::new("psql")
        .args(["-X", "-Atq", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run psql");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    let output = child.wait_with_output().expect("wait for psql");
    let stdout = String::from_utf8(output.stdout).expect("psql stdout is utf-8");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success() && !stderr.contains("ERROR:"),
        "psql failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

/// Load the real catalog DDL — trigger, doorbell function and all — over a
/// throwaway database, with three gated wiring versions to flip between.
fn install_schema(url: &str) {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    psql(
        url,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$;\n\
             DROP SCHEMA IF EXISTS catalog CASCADE;\n\
             {catalog}\n\
             SET app.tenant = '{TENANT}';\n\
             INSERT INTO catalog.catalogs \
                    (tenant_id, catalog_id, version, environment, schema_version, state) \
             VALUES ('{TENANT}','{CATALOG}',1,'{ENVIRONMENT}','1','applied');\n\
             INSERT INTO catalog.catalog_heads \
                    (tenant_id, catalog_id, environment, applied_catalog_version) \
             VALUES ('{TENANT}','{CATALOG}','{ENVIRONMENT}',1);\n\
             INSERT INTO catalog.wirings (tenant_id, catalog_id, wiring_id, version, \
                    gated_catalog_version, graph_json, wiring_hash) \
             VALUES ('{TENANT}','{CATALOG}','{WIRING}',1,1,'{{\"n\":1}}','{a}'), \
                    ('{TENANT}','{CATALOG}','{WIRING}',2,1,'{{\"n\":2}}','{b}');\n",
            a = hash('a'),
            b = hash('b'),
        ),
    );
}

/// One activation through the REAL builder, committed so the doorbell rings.
fn flip(url: &str, definition: &str, enabled: bool) {
    psql(
        url,
        &format!(
            "SET app.tenant = '{TENANT}';\n\
             PREPARE flip (text,text,text,text,boolean) AS {statement};\n\
             BEGIN;\n\
             EXECUTE flip('{CATALOG}','{ENVIRONMENT}','{WIRING}','{definition}',{enabled});\n\
             COMMIT;\n",
            statement = flip_activation(),
        ),
    );
}

/// The env-hot read, through the REAL statement the serving path uses.
async fn resolve(url: &str) -> Option<u32> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect the resolution reader");
    let driver = tokio::spawn(connection);
    client
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the reader to its tenant");
    let row = client
        .query_opt(resolve_active_wiring(), &[&CATALOG, &ENVIRONMENT, &WIRING])
        .await
        .expect("resolve the active wiring");
    driver.abort();
    row.map(|row| u32::try_from(row.get::<_, i32>(0)).expect("a version is non-negative"))
}

fn wiring(entry: &str) -> Wiring {
    Wiring::compile(
        entry,
        vec![WiringNode {
            id: entry.to_string(),
            component: "echo".to_string(),
            config: Value::Null,
            connection: None,
            terminal: None,
        }],
        vec![],
    )
    .expect("fixture wiring compiles")
}

async fn eventually(what: &str, predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} did not happen"));
}

/// Complete one resolution under the token its miss handed out. A version is
/// immutable, so its graph hash is a function of its identity; the cache refuses
/// a second, different hash for the same version.
fn install(
    cache: &WiringCache,
    wiring_id: &str,
    version: u32,
    graph: Wiring,
    token: wamn_router::ResolutionToken,
) {
    assert!(
        matches!(
            cache.insert(
                TENANT,
                CATALOG,
                ENVIRONMENT,
                wiring_id,
                version,
                format!("sha256:{wiring_id}-v{version}"),
                graph,
                (),
                token,
            ),
            CacheInsert::Installed(_)
        ),
        "no flip raced this resolution"
    );
}

#[tokio::test]
#[ignore = "requires WAMN_CATALOG_PG_URL and a throwaway PostgreSQL database"]
async fn a_pointer_flip_makes_the_cache_serve_the_new_active_version() {
    let url = std::env::var("WAMN_CATALOG_PG_URL")
        .expect("set WAMN_CATALOG_PG_URL to the throwaway superuser database");
    install_schema(&url);
    flip(&url, &hash('a'), true);

    let cache = Arc::new(WiringCache::new(
        NonZeroUsize::new(8).expect("bound is non-zero"),
    ));

    // A sentinel pointer the FIRST established LISTEN must drop: it is how this
    // gate knows the subscriber is on the wire before it flips anything, and it
    // is the reconnect obligation running on the very first connection.
    let sentinel = cache
        .get(TENANT, CATALOG, ENVIRONMENT, "sentinel")
        .miss()
        .expect("a fresh cache holds nothing");
    install(&cache, "sentinel", 1, wiring("sentinel"), sentinel);
    // The subscriber rides the ordinary platform pool (wamn-0h0g.16.24), so the
    // gate builds the same plugin production does rather than a private URL.
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(url.clone()),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 5_000,
            statement_timeout_ms: 10_000,
            row_limit: 10_000,
        })
        .expect("build the platform pool the doorbell listens on"),
    );
    let listener = WiringDoorbellListener::postgres(postgres, None, Arc::clone(&cache))
        .expect("subscribe the doorbell through the platform pool");
    eventually("the doorbell established its LISTEN", || {
        cache
            .get(TENANT, CATALOG, ENVIRONMENT, "sentinel")
            .hit()
            .is_none()
    })
    .await;

    // Resolve once, then serve from memory: the hot path this whole seam exists
    // to keep out of Postgres. The token is taken BEFORE the store read, which
    // is the only order that lets a flip during the read be detected.
    let token = cache
        .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
        .miss()
        .expect("the first delivery misses");
    let version = resolve(&url).await.expect("v1 is active");
    assert_eq!(version, 1);
    install(&cache, WIRING, version, wiring("v1"), token);
    assert_eq!(
        cache
            .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
            .hit()
            .expect("resident after the first resolution")
            .version,
        1
    );

    // THE FLIP. Nothing here touches the cache; the only path from this
    // statement to that memory is the DDL trigger's pg_notify.
    flip(&url, &hash('b'), true);
    eventually("the doorbell invalidated the flipped pointer", || {
        cache
            .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
            .hit()
            .is_none()
    })
    .await;

    let token = cache
        .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
        .miss()
        .expect("the flipped pointer was dropped");
    let version = resolve(&url).await.expect("v2 is active");
    assert_eq!(version, 2, "the store now serves the flipped version");
    install(&cache, WIRING, version, wiring("v2"), token);
    let served = cache
        .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
        .hit()
        .expect("the re-read repopulated the pointer");
    assert_eq!(served.version, 2);
    assert_eq!(served.wiring.entry(), "v2");

    // ROLLBACK is the same flip with an older argument, and it lands on the
    // graph that was never evicted.
    flip(&url, &hash('a'), true);
    eventually("the doorbell invalidated the rolled-back pointer", || {
        cache
            .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
            .hit()
            .is_none()
    })
    .await;
    assert_eq!(cache.len(), 3, "no graph was dropped by either flip");
    let token = cache
        .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
        .miss()
        .expect("the rolled-back pointer was dropped");
    assert_eq!(resolve(&url).await.expect("v1 is active again"), 1);

    // Taking the wiring dark is the same statement again, and is as much an
    // invalidation: the read path stops serving it entirely.
    install(&cache, WIRING, 1, wiring("v1"), token);
    flip(&url, &hash('a'), false);
    eventually("the doorbell invalidated the darkened pointer", || {
        cache
            .get(TENANT, CATALOG, ENVIRONMENT, WIRING)
            .hit()
            .is_none()
    })
    .await;
    assert_eq!(
        resolve(&url).await,
        None,
        "a dark wiring resolves to nothing"
    );

    drop(listener);
}
