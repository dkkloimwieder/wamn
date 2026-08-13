//! Live gate for the [11.7] publish gate (wamn-12g).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** maintenance url of a throwaway
//! Postgres (recipe: docs/archive/build-and-test.md [11.7]); skipped cleanly when unset.
//! Drives the REAL `copy-project-env --include definition` verb against the REAL
//! storage SQL (deploy/sql/{system,catalog,run-state,flows,flow-tests}.sql),
//! proving the four properties §11.7 actually promises:
//!
//!   * POLICY: an env whose org default does not require green suites promotes,
//!     and the ledger still records the rule that was in force;
//!   * REFUSAL: a gated env with NO durable suite report REFUSES, mutating
//!     nothing on the destination;
//!   * FRESHNESS: a report that really passed, but against superseded flow
//!     bytes, is NOT accepted — the artifact-hash pin is what makes a pass
//!     evidence about what is being shipped;
//!   * AUDIT: passes AND refusals land in `catalog.publish_gate_audit` with the
//!     evidence pointer, and the ledger refuses to be rewritten.
//!
//! Evidence is seeded through the REAL `authoring_suite_reports` triggers
//! (reservation → case fact → report), so the gate is proven against rows only a
//! conforming producer could have written — not against a convenient fixture.
//!
//! Hermetic: every scenario drops+recreates its own system, src and dst
//! databases, so a re-run starts clean and teardown leaves nothing behind.

use tokio_postgres::{Client, NoTls};

use wamn_control_provision::project_env_database_name;
use wamn_ctl::copy_project_env::{self, CopyProjectEnvArgs, IncludeArg};

const SYSTEM_SCHEMA: &str = include_str!("../../../deploy/sql/system-schema.sql");
const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS: &str = include_str!("../../../deploy/sql/flows.sql");
const FLOW_TESTS: &str = include_str!("../../../deploy/sql/flow-tests.sql");

const PROJECT: &str = "gate";
const SRC_ENV: &str = "dev";
const DST_ENV: &str = "prod";
const TENANT: &str = "t1";
const CATALOG_ID: &str = "shop";
const FLOW_ID: &str = "escalate-holds";
const SUITE_ID: &str = "smoke";
const DATA_SCHEMA: &str = "public";
const FLOW_SCHEMA: &str = "wamn_run";
/// The artifact hash of a SUPERSEDED build — well-formed, but not the hash this
/// release pins, so a real pass recorded against it is about bytes that are no
/// longer being shipped.
const SUPERSEDED_HASH: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";

/// The flow graph the release ships — a real `wamn-flow` document, because the
/// copy re-parses every immutable artifact it carries.
fn flow_graph_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FLOW_ID}","version":1,"nodes":[{{"id":"request","type":"request","config":{{"input-schema":true}}}},{{"id":"respond","type":"respond","config":{{"status":200}}}}],"edges":[{{"from":"request","to":"respond"}}]}}"#
    )
}

/// The catalog document the src env has applied and the promotion carries.
fn catalog_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","catalog-id":"{CATALOG_ID}","version":1,"entities":[{{"id":"touched","name":"orders","fields":[{{"id":"status","name":"status","type":{{"kind":"text"}}}}]}}]}}"#
    )
}

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

fn write_tmp(name: &str, content: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&p, content).expect("write fixture");
    p
}

fn swap_db(url: &str, db: &str) -> String {
    let (base, tail) = url.rsplit_once('/').expect("url has a path");
    match tail.split_once('?') {
        Some((_, query)) => format!("{base}/{db}?{query}"),
        None => format!("{base}/{db}"),
    }
}

/// One scenario's isolated world: its own system DB plus a src/dst project-env
/// database pair, all named from `org` so scenarios never collide.
struct World {
    admin_url: String,
    org: String,
    system_db: String,
    src_db: String,
    dst_db: String,
}

impl World {
    fn new(admin_url: &str, org: &str) -> Self {
        Self {
            admin_url: admin_url.to_string(),
            org: org.to_string(),
            system_db: format!("wamn_system_{org}"),
            src_db: project_env_database_name(org, PROJECT, SRC_ENV),
            dst_db: project_env_database_name(org, PROJECT, DST_ENV),
        }
    }

    fn system_url(&self) -> String {
        swap_db(&self.admin_url, &self.system_db)
    }
    fn src_url(&self) -> String {
        swap_db(&self.admin_url, &self.src_db)
    }
    fn dst_url(&self) -> String {
        swap_db(&self.admin_url, &self.dst_db)
    }

    async fn reset(&self) {
        let su = connect(&self.admin_url).await;
        for (role, extra) in [
            ("wamn_app", "NOBYPASSRLS"),
            ("wamn_scenario_author", "NOBYPASSRLS"),
            ("wamn_system", ""),
        ] {
            su.batch_execute(&format!(
                "DO $$ BEGIN \
                   PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') \
                   THEN CREATE ROLE {role} LOGIN PASSWORD '{role}' NOSUPERUSER {extra}; \
                 END IF; END $$;"
            ))
            .await
            .expect("ensure role");
        }
        for db in [&self.system_db, &self.src_db, &self.dst_db] {
            su.batch_execute(&format!("DROP DATABASE IF EXISTS \"{db}\" WITH (FORCE)"))
                .await
                .expect("drop db");
            su.batch_execute(&format!("CREATE DATABASE \"{db}\""))
                .await
                .expect("create db");
        }
    }

    async fn drop_all(&self) {
        let su = connect(&self.admin_url).await;
        for db in [&self.system_db, &self.src_db, &self.dst_db] {
            let _ = su
                .batch_execute(&format!("DROP DATABASE IF EXISTS \"{db}\" WITH (FORCE)"))
                .await;
        }
    }

    /// Stamp the T1 registry: the org, its project, both env policies, and the
    /// project-envs. `env_requires_green` is the ORG-WIDE default on the
    /// destination env's policy.
    async fn seed_registry(&self, env_requires_green: bool, project_override: Option<bool>) {
        let sys = connect(&self.system_url()).await;
        sys.batch_execute(SYSTEM_SCHEMA)
            .await
            .expect("apply system-schema.sql");
        // Applied as superuser, so the TABLES are superuser-owned while the
        // schemas carry `AUTHORIZATION wamn_system`. The copy saga runs as
        // `wamn_system`, so grant it what the T1 cluster's owner role holds.
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
            &[&self.org],
        )
        .await
        .expect("seed org");
        sys.execute(
            "INSERT INTO registry.projects (org, id) VALUES ($1, $2)",
            &[&self.org, &PROJECT],
        )
        .await
        .expect("seed project");
        for (env, rank, requires) in [(SRC_ENV, 0i32, false), (DST_ENV, 1i32, env_requires_green)] {
            sys.execute(
                "INSERT INTO registry.env_policies \
                   (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, \
                    image, requires_green_suite) \
                 VALUES ($1, $2, '\"own\"'::jsonb, $3, 1, '1Gi', '100m', '256Mi', 'pg:18', $4)",
                &[&self.org, &env, &rank, &requires],
            )
            .await
            .expect("seed env policy");
            sys.execute(
                "INSERT INTO registry.project_envs (org, project, env, secret_name) \
                 VALUES ($1, $2, $3, $4)",
                &[&self.org, &PROJECT, &env, &format!("wamn-db-{}", self.org)],
            )
            .await
            .expect("seed project env");
        }
        if let Some(requires) = project_override {
            sys.execute(
                wamn_control_registry::sql::upsert_project_publish_policy_sql(),
                &[&self.org, &PROJECT, &DST_ENV, &requires],
            )
            .await
            .expect("seed project publish override");
        }
    }

    /// The destination is a provisioned project-env with NO applied catalog (so
    /// the promotion is a first materialization) but WITH the flow version
    /// already in its legacy test-plane registry — the precondition the 11.2
    /// suite-orphan guard imposes on any suite-carrying definition copy.
    async fn seed_dst(&self) {
        let dst = connect(&self.dst_url()).await;
        dst.batch_execute(CATALOG_SCHEMA)
            .await
            .expect("apply catalog-schema.sql on dst");
        for ddl in [RUN_STATE, FLOWS, FLOW_TESTS] {
            dst.batch_execute(ddl)
                .await
                .expect("apply run-plane DDL on dst");
        }
        dst.execute(
            "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES ($1, $2, 1, true, '{}'::jsonb) \
             ON CONFLICT DO NOTHING",
            &[&TENANT, &FLOW_ID],
        )
        .await
        .expect("pre-register flow v1 on dst");
    }

    /// The source carries an applied v1 catalog, an immutable release pinning
    /// one flow artifact, an event registration binding that flow to the
    /// catalog's entity (so the impact report NAMES its suite), and the suite.
    async fn seed_src(&self) {
        let src = connect(&self.src_url()).await;
        src.batch_execute(CATALOG_SCHEMA)
            .await
            .expect("apply catalog-schema.sql on src");
        for ddl in [RUN_STATE, FLOWS, FLOW_TESTS] {
            src.batch_execute(ddl).await.expect("apply run-plane DDL");
        }
        // The applied catalog + its immutable release are written by the REAL
        // `publish-catalog --flow` writer, not by hand: the release's artifact
        // hash IS the freshness authority this gate leans on, so a fixture that
        // invented one would prove nothing about the real identity rules.
        let catalog_path = write_tmp(&format!("{}-catalog.json", self.org), &catalog_json());
        let flow_path = write_tmp(&format!("{}-flow.json", self.org), &flow_graph_json());
        wamn_ctl::publish_catalog::run(wamn_ctl::publish_catalog::PublishCatalogArgs {
            catalog: catalog_path,
            admin_database_url: Some(self.src_url()),
            tenant: TENANT.to_string(),
            project_config: None,
            schema: DATA_SCHEMA.to_string(),
            provision: true,
            runstate: true,
            seed_dataset: None,
            flow: vec![flow_path],
            exposure: None,
            skip_reconcile_replica_identity: true,
        })
        .await
        .expect("publish the src release");
        src.execute(
            "UPDATE catalog.catalogs SET state = 'applied', environment = $3 \
             WHERE tenant_id = $1 AND catalog_id = $2 AND version = 1",
            &[&TENANT, &CATALOG_ID, &SRC_ENV],
        )
        .await
        .expect("mark the src catalog applied");
        // The dependency edge that makes the impact report name this suite.
        src.execute(
            "INSERT INTO catalog.event_registrations \
               (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
             VALUES ($1, $2, 'r1', $3, 'touched', '{}'::jsonb)",
            &[&TENANT, &CATALOG_ID, &FLOW_ID],
        )
        .await
        .expect("seed event registration");
        src.execute(
            "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES ($1, $2, 1, true, '{}'::jsonb) \
             ON CONFLICT DO NOTHING",
            &[&TENANT, &FLOW_ID],
        )
        .await
        .expect("register flow v1");
        src.execute(
            "INSERT INTO wamn_run.test_suites (tenant_id, flow_id, flow_version, suite_id, name) \
             VALUES ($1, $2, 1, $3, 'smoke suite')",
            &[&TENANT, &FLOW_ID, &SUITE_ID],
        )
        .await
        .expect("seed suite");
    }

    /// Seed ONE durable suite report through the real reservation → case fact →
    /// report trigger chain, pinned to `artifact_hash`.
    async fn seed_suite_report(&self, report_id: &str, artifact_hash: &str, passed: bool) {
        let src = connect(&self.src_url()).await;
        let lineage = format!(
            r#"{{"kind":"release","artifact-hash":"{artifact_hash}","catalog-id":"{CATALOG_ID}","catalog-version":1,"environment":"{SRC_ENV}"}}"#
        );
        // The trigger compares the report's lineage to its reservation's by
        // both value and hash, so the same text must be used for both.
        let lineage_hash = format!("hash-of-{artifact_hash}");
        let command = format!(
            r#"{{"cases":[{{"case-id":"c1","run-id":"{report_id}-run","execution-schema":"s","case-content-hash":"cc1"}}]}}"#
        );
        src.execute(
            "INSERT INTO wamn_run.authoring_report_reservations \
               (tenant_id, report_id, execution_id, flow_id, suite_flow_version, suite_id, \
                command_json, command_hash, lineage_json, lineage_hash) \
             VALUES ($1, $2, $3, $4, 1, $5, $6::text::jsonb, 'ch', $7::text::jsonb, $8)",
            &[
                &TENANT,
                &report_id,
                &format!("{report_id}-exec"),
                &FLOW_ID,
                &SUITE_ID,
                &command,
                &lineage,
                &lineage_hash,
            ],
        )
        .await
        .expect("seed report reservation");
        src.execute(
            "INSERT INTO wamn_run.authoring_suite_case_facts \
               (tenant_id, report_id, ordinal, case_id, run_id, passed, status, outcome) \
             VALUES ($1, $2, 0, 'c1', $3, $4, 'completed', '{}'::jsonb)",
            &[&TENANT, &report_id, &format!("{report_id}-run"), &passed],
        )
        .await
        .expect("seed case fact");
        src.execute(
            "INSERT INTO wamn_run.authoring_suite_reports \
               (tenant_id, report_id, execution_id, flow_id, suite_flow_version, suite_id, \
                passed, lineage_json, lineage_hash) \
             VALUES ($1, $2, $3, $4, 1, $5, $6, $7::text::jsonb, $8)",
            &[
                &TENANT,
                &report_id,
                &format!("{report_id}-exec"),
                &FLOW_ID,
                &SUITE_ID,
                &passed,
                &lineage,
                &lineage_hash,
            ],
        )
        .await
        .expect("seed suite report");
    }

    fn copy_args(&self) -> CopyProjectEnvArgs {
        CopyProjectEnvArgs {
            src_org: self.org.clone(),
            src_project: PROJECT.into(),
            src_env: SRC_ENV.into(),
            dst_org: self.org.clone(),
            dst_project: PROJECT.into(),
            dst_env: DST_ENV.into(),
            include: IncludeArg::Definition,
            cutover: false,
            deprovision_old: false,
            confirm: false,
            src_admin_url: Some(self.admin_url.clone()),
            dst_admin_url: None,
            system_database_url: Some(self.system_url()),
            tenant: Some(TENANT.into()),
            data_schema: DATA_SCHEMA.into(),
            flow_schema: FLOW_SCHEMA.into(),
            dump_root: std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dump"),
            confirm_with_backup: false,
            plan: false,
            saga_id: None,
        }
    }

    /// The artifact hash the src's release actually pins for the flow — read
    /// back from the release the production writer minted, never invented.
    async fn release_artifact_hash(&self) -> String {
        let src = connect(&self.src_url()).await;
        src.query_one(
            "SELECT artifact_hash FROM catalog.flow_artifacts \
             WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = 1",
            &[&TENANT, &FLOW_ID],
        )
        .await
        .expect("the release pins one flow artifact")
        .get(0)
    }

    /// `(decision, findings_json)` of the single recorded gate verdict.
    async fn verdict(&self) -> (String, String) {
        let dst = connect(&self.dst_url()).await;
        let row = dst
            .query_one(
                "SELECT decision, findings_json::text FROM catalog.publish_gate_audit",
                &[],
            )
            .await
            .expect("exactly one gate verdict recorded");
        (row.get(0), row.get(1))
    }

    async fn dst_applied_catalogs(&self) -> i64 {
        let dst = connect(&self.dst_url()).await;
        dst.query_one("SELECT count(*) FROM catalog.catalogs", &[])
            .await
            .expect("count dst catalogs")
            .get(0)
    }
}

fn admin_url() -> Option<String> {
    std::env::var("WAMN_CTL_PG_URL").ok()
}

/// Build a world with the given policy, seeded src and dst, ready to promote.
async fn world(org: &str, env_requires_green: bool, over: Option<bool>) -> Option<World> {
    let url = admin_url()?;
    let w = World::new(&url, org);
    w.reset().await;
    w.seed_registry(env_requires_green, over).await;
    w.seed_src().await;
    w.seed_dst().await;
    Some(w)
}

/// An ungated destination promotes, and the ledger still records the rule that
/// was in force — a change-control log that only appears when it blocks cannot
/// answer "was this env gated at the time?".
#[tokio::test]
async fn an_ungated_env_promotes_and_records_the_rule() {
    let Some(w) = world("pg12ga", false, None).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    copy_project_env::run(w.copy_args())
        .await
        .expect("ungated promotion proceeds");
    let (decision, _) = w.verdict().await;
    assert_eq!(decision, "not-required");
    assert_eq!(w.dst_applied_catalogs().await, 1, "the catalog promoted");
    w.drop_all().await;
}

/// THE gate. A gated destination with no durable suite report REFUSES, and the
/// destination is untouched — evidence-free is not the same as green.
#[tokio::test]
async fn a_gated_env_refuses_a_promotion_with_no_suite_report() {
    let Some(w) = world("pg12gb", true, None).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    let err = copy_project_env::run(w.copy_args())
        .await
        .expect_err("a gated promotion without evidence must refuse");
    let msg = format!("{err:#}");
    assert!(msg.contains("no-report"), "refusal names the defect: {msg}");
    assert!(msg.contains(SUITE_ID), "refusal names the suite: {msg}");
    assert_eq!(
        w.dst_applied_catalogs().await,
        0,
        "a refused promotion must mutate nothing on the destination"
    );
    w.drop_all().await;
}

/// A refusal is durable evidence in its own right, and the ledger cannot be
/// rewritten to hide it.
#[tokio::test]
async fn a_refusal_is_recorded_in_the_append_only_ledger() {
    let Some(w) = world("pg12gc", true, None).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    copy_project_env::run(w.copy_args())
        .await
        .expect_err("gated promotion refuses");
    let (decision, findings) = w.verdict().await;
    assert_eq!(decision, "refused", "the refusal itself must be recorded");
    assert!(findings.contains("no-report"), "findings: {findings}");
    assert!(findings.contains(SUITE_ID), "findings: {findings}");

    let dst = connect(&w.dst_url()).await;
    for statement in [
        "UPDATE catalog.publish_gate_audit SET decision = 'passed'",
        "DELETE FROM catalog.publish_gate_audit",
    ] {
        let err = dst
            .batch_execute(statement)
            .await
            .expect_err("the ledger is append-only");
        assert!(
            format!("{err:?}").contains("immutable"),
            "expected an immutability refusal, got: {err:?}"
        );
    }
    w.drop_all().await;
}

/// A pass recorded against the exact bytes the release ships satisfies the gate,
/// and the ledger keeps the report id that proved it.
#[tokio::test]
async fn fresh_release_evidence_satisfies_the_gate() {
    let Some(w) = world("pg12gd", true, None).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    let pinned = w.release_artifact_hash().await;
    w.seed_suite_report("rep-fresh", &pinned, true).await;
    copy_project_env::run(w.copy_args())
        .await
        .expect("proven-green promotion proceeds");
    let (decision, findings) = w.verdict().await;
    assert_eq!(decision, "passed");
    assert!(
        findings.contains("rep-fresh"),
        "the ledger records the evidence pointer: {findings}"
    );
    assert_eq!(w.dst_applied_catalogs().await, 1, "the catalog promoted");
    w.drop_all().await;
}

/// FRESHNESS. The suite really passed — but against flow bytes this release does
/// not ship. A pass about superseded code is not evidence about the deploy.
#[tokio::test]
async fn a_pass_against_superseded_flow_bytes_is_refused() {
    let Some(w) = world("pg12ge", true, None).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    w.seed_suite_report("rep-stale", SUPERSEDED_HASH, true)
        .await;
    let err = copy_project_env::run(w.copy_args())
        .await
        .expect_err("stale evidence must not satisfy the gate");
    let msg = format!("{err:#}");
    assert!(msg.contains("stale-artifact"), "refusal: {msg}");
    let pinned = w.release_artifact_hash().await;
    assert!(msg.contains(&pinned), "refusal names the pin: {msg}");
    assert_eq!(
        w.dst_applied_catalogs().await,
        0,
        "a refused promotion must mutate nothing"
    );
    let (decision, _) = w.verdict().await;
    assert_eq!(decision, "refused");
    w.drop_all().await;
}

/// A recorded FAILURE against exactly this release refuses — the gate reads the
/// verdict, not merely the existence of a report.
#[tokio::test]
async fn a_recorded_failure_refuses_the_promotion() {
    let Some(w) = world("pg12gf", true, None).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    let pinned = w.release_artifact_hash().await;
    w.seed_suite_report("rep-red", &pinned, false).await;
    let err = copy_project_env::run(w.copy_args())
        .await
        .expect_err("a failed suite must not satisfy the gate");
    assert!(format!("{err:#}").contains("failed"));
    assert_eq!(w.dst_applied_catalogs().await, 0);
    w.drop_all().await;
}

/// The per-project override is the "per-project rules" half of §11.7: it gates a
/// single project inside an org whose env default does not.
#[tokio::test]
async fn a_project_override_gates_a_project_in_an_ungated_env() {
    let Some(w) = world("pg12gg", false, Some(true)).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    let err = copy_project_env::run(w.copy_args())
        .await
        .expect_err("the project override must gate this promotion");
    assert!(format!("{err:#}").contains("no-report"));
    let (decision, _) = w.verdict().await;
    assert_eq!(decision, "refused");
    w.drop_all().await;
}

/// The override is authoritative in both directions: a documented exemption
/// releases one project from an org-wide gate, and the ledger says so.
#[tokio::test]
async fn a_project_override_can_exempt_a_project_from_a_gated_env() {
    let Some(w) = world("pg12gh", true, Some(false)).await else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping");
        return;
    };
    copy_project_env::run(w.copy_args())
        .await
        .expect("an exempted project promotes");
    let (decision, _) = w.verdict().await;
    assert_eq!(decision, "not-required");
    let dst = connect(&w.dst_url()).await;
    let recorded: String = dst
        .query_one("SELECT policy_source FROM catalog.publish_gate_audit", &[])
        .await
        .expect("verdict")
        .get(0);
    assert_eq!(
        recorded, "project-override",
        "the ledger names the layer that granted the exemption"
    );
    w.drop_all().await;
}
