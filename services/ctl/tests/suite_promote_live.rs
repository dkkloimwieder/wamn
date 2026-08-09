//! Live-promote gate for 11.2 test cases as catalog data (wamn-828).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** maintenance url (path `/wamn` or
//! `/postgres`) of a throwaway Postgres (recipe: docs/build-and-test.md
//! [11.2/wamn-828]); skipped cleanly when unset. Drives the REAL
//! `copy-project-env --include definition` verb across two freshly-created
//! project-env DATABASES, proving:
//!
//!   * PROMOTE: a suite + its cases arrive on a destination that ALREADY holds
//!     the pinned flow version, version-bound (`flow_version = 1`) with matching
//!     counts. The destination's flow registration is a PRECONDITION, not
//!     something the copy installs: immutable release publication owns flow
//!     installation (5wd1.46), so the definition copy never writes the mutable
//!     `flows` registry;
//!   * RLS: a second tenant's claim sees ZERO suite rows on the dst;
//!   * FK (version binding): deleting flow v1 on the dst CASCADES its suite +
//!     cases (the `test_suites/test_cases → flows` ON DELETE CASCADE);
//!   * GUARD: a copy carrying a suite pinned to a flow version the destination
//!     does not hold is REFUSED, naming only that orphan, and the refusal leaves
//!     every definition-copy target on the dst at its pre-copy row count.
//!
//! Hermetic: each scenario drops+recreates its two databases, so a re-run starts
//! clean and teardown leaves nothing behind.

use tokio_postgres::{Client, NoTls};

use wamn_ctl::copy_project_env::{self, CopyProjectEnvArgs, IncludeArg};

use wamn_control_provision::project_env_database_name;

const RUN_STATE: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS: &str = include_str!("../../../deploy/sql/flows.sql");
const FLOW_TESTS: &str = include_str!("../../../deploy/sql/flow-tests.sql");
const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const SYSTEM_SCHEMA: &str = include_str!("../../../deploy/sql/system-schema.sql");

/// The T1 system database this gate stands up so `copy-project-env` can resolve
/// its [11.7] publish policy (wamn-12g). Both envs are left UNGATED — the org
/// default — because this gate proves 11.2 promotion, not the publish gate.
const SYSTEM_DB: &str = "wamn_system_lane828";

const ORG: &str = "lane828";
const PROJECT: &str = "promote";
const SRC_ENV: &str = "dev";
const DST_ENV: &str = "prod";
const TENANT: &str = "t1";
const FLOW_ID: &str = "escalate-holds";

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    (client, task)
}

/// Replace the database segment of a maintenance url with `db` (the copy verb's
/// own `swap_db`, duplicated here so the gate builds the same per-db urls).
fn swap_db(url: &str, db: &str) -> String {
    let (base, tail) = url.rsplit_once('/').expect("url has a path");
    match tail.split_once('?') {
        Some((_, query)) => format!("{base}/{db}?{query}"),
        None => format!("{base}/{db}"),
    }
}

/// Ensure the runtime role, then drop+create the two project-env databases.
async fn reset_databases(admin_url: &str, src_db: &str, dst_db: &str) {
    let (su, task) = connect(admin_url).await;
    su.batch_execute(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         END IF; END $$;",
    )
    .await
    .expect("ensure wamn_app role");
    su.batch_execute(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
           THEN CREATE ROLE wamn_system LOGIN PASSWORD 'wamn_system' NOSUPERUSER; \
         END IF; END $$;",
    )
    .await
    .expect("ensure wamn_system role");
    for db in [src_db, dst_db, SYSTEM_DB] {
        su.batch_execute(&format!("DROP DATABASE IF EXISTS \"{db}\" WITH (FORCE)"))
            .await
            .expect("drop db");
        su.batch_execute(&format!("CREATE DATABASE \"{db}\""))
            .await
            .expect("create db");
    }
    drop(su);
    let _ = task.await;
}

/// Stand up the T1 registry the [11.7] gate resolves its policy from: the org,
/// its project, and both env policies (neither requiring green suites).
async fn seed_registry(admin_url: &str) {
    let (sys, task) = connect(&swap_db(admin_url, SYSTEM_DB)).await;
    sys.batch_execute(SYSTEM_SCHEMA)
        .await
        .expect("apply system-schema.sql");
    // Applied as superuser, so the TABLES are superuser-owned while the schemas
    // carry `AUTHORIZATION wamn_system`. The copy saga runs as `wamn_system`.
    sys.batch_execute(
        "GRANT USAGE ON SCHEMA registry, provisioning, identity TO wamn_system; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA registry, provisioning, \
           identity TO wamn_system;",
    )
    .await
    .expect("grant the system owner role");
    sys.execute(
        "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
         VALUES ($1, 'pooled', 'wamn-pg')",
        &[&ORG],
    )
    .await
    .expect("seed org");
    sys.execute(
        "INSERT INTO registry.projects (org, id) VALUES ($1, $2)",
        &[&ORG, &PROJECT],
    )
    .await
    .expect("seed project");
    for (env, rank) in [(SRC_ENV, 0i32), (DST_ENV, 1i32)] {
        sys.execute(
            "INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
             VALUES ($1, $2, '\"own\"'::jsonb, $3, 1, '1Gi', '100m', '256Mi', 'pg:18')",
            &[&ORG, &env, &rank],
        )
        .await
        .expect("seed env policy");
        sys.execute(
            "INSERT INTO registry.project_envs (org, project, env, secret_name) \
             VALUES ($1, $2, $3, 'wamn-db-lane828')",
            &[&ORG, &PROJECT, &env],
        )
        .await
        .expect("seed project env");
    }
    drop(sys);
    let _ = task.await;
}

async fn drop_databases(admin_url: &str, src_db: &str, dst_db: &str) {
    let (su, task) = connect(admin_url).await;
    for db in [src_db, dst_db, SYSTEM_DB] {
        let _ = su
            .batch_execute(&format!("DROP DATABASE IF EXISTS \"{db}\" WITH (FORCE)"))
            .await;
    }
    drop(su);
    let _ = task.await;
}

/// Provision the DST: catalog metadata + run-plane DDL, and PRE-REGISTER flow
/// v1 (the `publish_gate_live.rs::seed_dst` shape). Holding the pinned flow
/// version is the destination-side PRECONDITION of a suite-carrying definition
/// copy: immutable release publication is the authoritative flow path (5wd1.46),
/// so the copy never installs the mutable `wamn_run.flows` registry and a suite
/// promotes only onto a version the destination already has.
async fn provision_dst(url: &str) {
    let (c, task) = connect(url).await;
    c.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply catalog-schema.sql on dst");
    for ddl in [RUN_STATE, FLOWS, FLOW_TESTS] {
        c.batch_execute(ddl)
            .await
            .expect("apply run-plane DDL on dst");
    }
    c.execute(
        "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
         VALUES ($1, $2, 1, true, '{}'::jsonb)",
        &[&TENANT, &FLOW_ID],
    )
    .await
    .expect("pre-register flow v1 on dst");
    drop(c);
    let _ = task.await;
}

/// Provision the SRC: run-state + flows + flow-tests DDL, register flow v1, seed
/// one suite + two cases (superuser bypasses RLS; explicit tenant).
async fn provision_src(url: &str) {
    let (c, task) = connect(url).await;
    c.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply catalog-schema.sql on src");
    for ddl in [RUN_STATE, FLOWS, FLOW_TESTS] {
        c.batch_execute(ddl).await.expect("apply run-plane DDL");
    }
    c.execute(
        "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
         VALUES ($1, $2, 1, true, '{}'::jsonb)",
        &[&TENANT, &FLOW_ID],
    )
    .await
    .expect("register flow v1");
    c.execute(
        "INSERT INTO wamn_run.test_suites (tenant_id, flow_id, flow_version, suite_id, name) \
         VALUES ($1, $2, 1, 'smoke', 'smoke suite')",
        &[&TENANT, &FLOW_ID],
    )
    .await
    .expect("seed suite");
    // Case bodies are valid scenario-model TestCases (11.4 vocabulary) so they
    // satisfy the 828 validate-on-write reconcile; still opaque jsonb to the copy.
    c.execute(
        "INSERT INTO wamn_run.test_cases \
           (tenant_id, flow_id, flow_version, suite_id, case_id, ordinal, case_body) VALUES \
           ($1, $2, 1, 'smoke', 'c1', 0, \
             '{\"schema-version\":\"0.1\",\"name\":\"c1\",\"node-ref\":{},\"input\":{},\"expect\":[{\"error-class\":{\"node-error\":\"invalid-input\"}}]}'::jsonb), \
           ($1, $2, 1, 'smoke', 'c2', 1, \
             '{\"schema-version\":\"0.1\",\"name\":\"c2\",\"flow-ref\":{\"flow-id\":\"escalate-holds\",\"version\":1},\"input\":{},\"expect\":[{\"run-outcome\":{\"status\":\"completed\"}}]}'::jsonb)",
        &[&TENANT, &FLOW_ID],
    )
    .await
    .expect("seed cases");
    drop(c);
    let _ = task.await;
}

fn copy_args(admin_url: &str) -> CopyProjectEnvArgs {
    CopyProjectEnvArgs {
        src_org: ORG.into(),
        src_project: PROJECT.into(),
        src_env: SRC_ENV.into(),
        dst_org: ORG.into(),
        dst_project: PROJECT.into(),
        dst_env: DST_ENV.into(),
        include: IncludeArg::Definition,
        cutover: false,
        deprovision_old: false,
        confirm: false,
        src_admin_url: Some(admin_url.to_string()),
        dst_admin_url: None,
        system_database_url: Some(swap_db(admin_url, SYSTEM_DB)),
        tenant: Some(TENANT.into()),
        data_schema: "public".into(),
        flow_schema: "wamn_run".into(),
        dump_root: std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dump"),
        confirm_with_backup: false,
        plan: false,
        saga_id: None,
    }
}

async fn count(c: &Client, sql: &str) -> i64 {
    c.query_one(sql, &[]).await.expect("count").get(0)
}

/// Every destination relation a `--include definition` copy writes, one per
/// block: catalogs + heads (1), the immutable release slice (2a), RLS policy
/// rows (3), event registrations (4), suites + cases (5) — plus `flows`, which
/// the copy deliberately does NOT write. Counted before and after a refused
/// copy: the guard's promise is that a refusal moves none of them.
const DEFINITION_COPY_TARGETS: [&str; 10] = [
    "catalog.catalogs",
    "catalog.catalog_heads",
    "catalog.flow_artifacts",
    "catalog.release_manifests",
    "catalog.release_flows",
    "catalog.rls_policies",
    "catalog.event_registrations",
    "wamn_run.flows",
    "wamn_run.test_suites",
    "wamn_run.test_cases",
];

async fn definition_copy_counts(c: &Client) -> Vec<(&'static str, i64)> {
    let mut counts = Vec::new();
    for relation in DEFINITION_COPY_TARGETS {
        counts.push((
            relation,
            count(c, &format!("SELECT count(*) FROM {relation}")).await,
        ));
    }
    counts
}

/// The suite rows a role sees under `tenant` on the dst (RLS via SET ROLE
/// wamn_app — non-superuser, non-BYPASSRLS, so the FORCE-RLS policy applies).
async fn suites_seen_as_app(c: &Client, tenant: &str) -> i64 {
    c.batch_execute("SET ROLE wamn_app; SET search_path = wamn_run")
        .await
        .expect("assume wamn_app");
    c.query("SELECT set_config('app.tenant', $1, false)", &[&tenant])
        .await
        .expect("set tenant claim");
    let n: i64 = c
        .query_one("SELECT count(*) FROM test_suites", &[])
        .await
        .expect("count as app")
        .get(0);
    c.batch_execute("RESET ROLE; SELECT set_config('app.tenant', '', false)")
        .await
        .expect("reset role");
    n
}

#[tokio::test]
async fn suite_promotes_with_its_flow_and_the_guard_refuses_orphans() {
    let Some(admin_url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the 11.2 suite-promote gate");
        return;
    };
    promote_scenario(&admin_url).await;
    guard_scenario(&admin_url).await;
}

/// PROMOTE + RLS + FK cascade.
async fn promote_scenario(admin_url: &str) {
    let src_db = project_env_database_name(ORG, PROJECT, SRC_ENV);
    let dst_db = project_env_database_name(ORG, PROJECT, DST_ENV);
    reset_databases(admin_url, &src_db, &dst_db).await;
    seed_registry(admin_url).await;
    provision_src(&swap_db(admin_url, &src_db)).await;
    provision_dst(&swap_db(admin_url, &dst_db)).await;

    copy_project_env::run(copy_args(admin_url))
        .await
        .expect("definition copy src -> dst");

    // --- PROMOTE: the suite/cases arrived on the pre-held flow v1, version-bound. ---
    let (dst, task) = connect(&swap_db(admin_url, &dst_db)).await;
    assert_eq!(
        count(&dst, "SELECT count(*) FROM wamn_run.test_suites").await,
        1,
        "suite arrived"
    );
    assert_eq!(
        count(&dst, "SELECT count(*) FROM wamn_run.test_cases").await,
        2,
        "both cases arrived"
    );
    // Version binding: every suite/case row pins flow_version = 1.
    assert_eq!(
        count(
            &dst,
            "SELECT count(*) FROM wamn_run.test_suites WHERE flow_version = 1"
        )
        .await,
        1,
        "suite is bound to flow v1"
    );
    assert_eq!(
        count(
            &dst,
            "SELECT count(*) FROM wamn_run.test_cases WHERE flow_version = 1"
        )
        .await,
        2,
        "cases are bound to flow v1"
    );
    // The opaque case body survived verbatim.
    let body: String = dst
        .query_one(
            "SELECT case_body::text FROM wamn_run.test_cases WHERE case_id = 'c1'",
            &[],
        )
        .await
        .expect("read case body")
        .get(0);
    assert!(body.contains("\"expect\""), "case body preserved: {body}");

    // --- RLS: a second tenant sees ZERO suite rows; the owner sees its one. ---
    assert_eq!(
        suites_seen_as_app(&dst, "t2").await,
        0,
        "a foreign tenant sees no suites (RLS)"
    );
    assert_eq!(
        suites_seen_as_app(&dst, TENANT).await,
        1,
        "the owning tenant sees its suite"
    );

    // --- FK cascade (version binding is structural): drop flow v1 → suite gone. ---
    dst.execute(
        "DELETE FROM wamn_run.flows WHERE tenant_id = $1 AND flow_id = $2 AND version = 1",
        &[&TENANT, &FLOW_ID],
    )
    .await
    .expect("delete flow v1");
    assert_eq!(
        count(&dst, "SELECT count(*) FROM wamn_run.test_suites").await,
        0,
        "dropping the flow version cascaded its suite"
    );
    assert_eq!(
        count(&dst, "SELECT count(*) FROM wamn_run.test_cases").await,
        0,
        "and its cases"
    );

    drop(dst);
    let _ = task.await;
    drop_databases(admin_url, &src_db, &dst_db).await;
}

/// GUARD: a src carrying a suite pinned to a version the destination does not
/// hold is refused before any mutation. The dst is a real provisioned project-env
/// holding flow v1, so the ONLY orphan is the drifted v99 pin — and "mutating
/// nothing" is proven as a row-count delta, not as an absent table.
async fn guard_scenario(admin_url: &str) {
    let src_db = project_env_database_name(ORG, PROJECT, SRC_ENV);
    let dst_db = project_env_database_name(ORG, PROJECT, DST_ENV);
    reset_databases(admin_url, &src_db, &dst_db).await;
    seed_registry(admin_url).await;
    provision_src(&swap_db(admin_url, &src_db)).await;
    provision_dst(&swap_db(admin_url, &dst_db)).await;

    // Seed a DRIFTED orphan suite: it pins v99, which no flow row backs. The FK
    // forbids this, so bypass it with session_replication_role (superuser only)
    // — modelling a pre-existing drift the guard must SURFACE, not carry.
    let (src, task) = connect(&swap_db(admin_url, &src_db)).await;
    src.batch_execute(
        "SET session_replication_role = replica; \
         INSERT INTO wamn_run.test_suites (tenant_id, flow_id, flow_version, suite_id, name) \
           VALUES ('t1', 'escalate-holds', 99, 'orphan', 'orphan suite'); \
         SET session_replication_role = origin;",
    )
    .await
    .expect("seed orphan suite via FK bypass");
    drop(src);
    let _ = task.await;

    // The pre-copy state of every relation the definition copy writes.
    let (dst, dtask) = connect(&swap_db(admin_url, &dst_db)).await;
    let before = definition_copy_counts(&dst).await;

    let err = copy_project_env::run(copy_args(admin_url))
        .await
        .expect_err("an orphaning definition copy must be refused");
    let msg = err.to_string();
    for needle in ["orphan", "escalate-holds", "99"] {
        assert!(msg.contains(needle), "refusal names {needle:?}: {msg}");
    }
    assert!(
        !msg.contains("smoke"),
        "the suite whose pinned version the dst holds is not named: {msg}"
    );

    // NOTHING mutated. The dst is provisioned, so this can no longer be "the
    // tables do not exist": it is a DELTA — every definition-copy target still
    // holds exactly the rows it held before the refusal.
    let after = definition_copy_counts(&dst).await;
    assert_eq!(
        after, before,
        "the refused copy wrote no new rows to the dst"
    );
    // Sentinel, scoped to the one row a guard-less copy would have committed:
    // 'smoke' pins v1, which the dst HOLDS, so block 5 would have inserted it
    // (autocommit) before the FK rejected the v99 orphan. Its absence is what
    // proves the refusal happened FIRST rather than part-way through.
    assert_eq!(
        count(
            &dst,
            "SELECT count(*) FROM wamn_run.test_suites WHERE suite_id = 'smoke'"
        )
        .await,
        0,
        "the refused copy landed no suite row on the dst"
    );
    drop(dst);
    let _ = dtask.await;
    drop_databases(admin_url, &src_db, &dst_db).await;
}
