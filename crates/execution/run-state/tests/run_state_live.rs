//! Ignored live gate for the fenced run-state transitions.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql as provision_sql,
    workload_generation_role,
};
use wamn_run_state::queue::{
    advance_claim_attempts_sql, clear_pre_effect_state_sql, grant_production_claim_sql,
    renew_production_lease_sql, select_exhausted_production_sql,
    terminalize_exhausted_production_sql,
};
use wamn_run_state::transitions::{release_caller_sql, terminalize_sql};

fn psql(url: &str, script: &str) -> Output {
    let mut child = Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", "-1", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start psql");
    let mut stdin = child.stdin.take().expect("open psql stdin");
    if let Err(error) = stdin.write_all(script.as_bytes()) {
        eprintln!("psql closed stdin before the full script was written: {error}");
    }
    drop(stdin);
    child.wait_with_output().expect("run psql")
}

fn success(url: &str, script: &str) -> String {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "psql failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("psql stdout is utf-8")
}

fn app_preamble(app_login: &str) -> String {
    format!("BEGIN; SET LOCAL ROLE {app_login}; SET LOCAL search_path TO wamn_run;")
}

/// The generation login the fenced transitions actually run as.
///
/// `FENCED_PREFIX` and `grant_production_claim_sql` both open with
/// `require_executor_platform_authority()`, which raises `42501` unless
/// `CURRENT_USER` is a MEMBER of `wamn_executor_platform`; and `wamn_app` holds
/// only `SELECT, DELETE` on `wamn_run.runs` plus a column-scoped `SELECT` on
/// `wamn_run.run_queue`. A transition driven under [`app_preamble`] is
/// therefore refused twice over — at the grant, before the authority guard ever
/// evaluates — and cannot reach the semantics under test. The App generation's
/// tenant comes from `current_user`; the executor statements below retain their
/// host-injected `app.tenant` input because those statements key their own queue
/// predicates from it.
///
/// A MINTED GENERATION, NOT THE BARE ACL ROLE (`wamn-0h0g.22.31`). The stable
/// role is `NOLOGIN` and nothing in production ever authenticates as it — the
/// executor's platform pool dials a generation that INHERITS it, and
/// `pool::credential_exactness_hook` asserts exactly that membership on every
/// physical connection. Driving these legs as the stable role would prove a
/// principal that cannot exist at runtime, and would take its privileges
/// directly rather than through the inheritance edge that is the real path.
const EXECUTOR_LOGIN: &str = "wamn_transitions_executor_login";

fn executor_preamble() -> String {
    format!(
        "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL search_path TO wamn_run; \
         SET LOCAL app.tenant = 't1';"
    )
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn run_state_live() {
    let url = std::env::var("WAMN_RUN_STORE_PG_URL")
        .expect("set WAMN_RUN_STORE_PG_URL to the throwaway superuser database");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");

    // The class-grant leg below proves the DDL's guest ACL through a prepared
    // App generation. Its production builder also converges the stable
    // `wamn_app` carrier to NOLOGIN/NOINHERIT/passwordless, so a drifted cluster
    // role cannot make the leg pass through ambient authority.
    //
    // THE TWO WRITER ROLES BELOW ARE HAND-ROLLED, AND THAT IS A RULED DIVERGENCE
    // (wamn-0h0g.20.15: accept the divergence, comment it, close at P3). Every other
    // live bootstrap now mints them from the production builder; this one structurally
    // cannot. `wamn-run-state` ships INSIDE the guest components
    // (components/Cargo.toml:25), and `components` is a SEPARATE cargo workspace that
    // has never heard of `wamn-control-provision`. On top of that,
    // tests/conformance/tests/workspace_tiers.rs computes each tier's path-dependency
    // closure KIND-BLIND — it filters on `dependency.path` alone and never on kind — so
    // a DEV-dependency counts exactly like a real one, and adding the builder here reds
    // `workspace_tier_membership_matches_live_classification` with "selected package
    // wamn-control-provision missing from cargo metadata".
    //
    // THAT LAST CLAUSE IS REFUTED AS OF `wamn-0h0g.22.31`, and the writer-role divergence
    // survives it anyway. `wamn-control-provision` is ALREADY a dev-dependency of this
    // crate — `tests/admission_live.rs` has imported it since `wamn-0h0g.22.9` — and this
    // file now imports it too, for the executor generation below;
    // `workspace_tier_membership_matches_live_classification` was measured GREEN with both.
    // So the barrier to minting the two writer roles from their builder is not the
    // dependency edge. Whether to mint them is still `wamn-0h0g.20.15`'s call, untouched
    // here. Teaching that closure to skip
    // dev-dependencies was rejected: it weakens a conformance guard to make one test
    // prettier.
    //
    // THE DRIFT CONTRACT. The `wamn_scenario_author` block below mirrors
    // `ensure_scenario_author_role_sql` in crates/schema/control/src/run_plane.rs, while
    // the `wamn_effect_writer` and `wamn_run_projection_writer` blocks mirror
    // `ensure_effect_writer_acl_role_sql` in crates/control/provision/src/sql.rs, and the
    // `wamn_executor_platform` block mirrors `ensure_workload_acl_role_sql` in that same
    // file. All four carry the production attributes exactly: NOLOGIN NOSUPERUSER NOCREATEDB
    // NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS. ANY CHANGE TO EITHER
    // PRODUCTION ATTRIBUTE SET UPDATES THIS BLOCK IN THE SAME COMMIT. The mirrored
    // surface is the attribute list and nothing else: the writer role NAMES are already
    // shared, since `EFFECT_WRITER_ROLE` and `RUN_PROJECTION_WRITER_ROLE` are this
    // crate's own constants (src/effect_writer_credential.rs) and the production builder
    // imports them FROM here. The `pg_roles` assertion after this bootstrap is what holds
    // the mirror to its word.
    //
    // REOPEN TRIGGER: a SECOND consumer needing this attribute set. Minting a shared
    // cross-workspace home crate for one reader was rejected as infrastructure built for
    // a single caller; a second reader is when that crate earns its existence.
    //
    // THE EXECUTOR'S RUN-PLANE GRANTS ARE THE PRODUCTION BUILDER'S NOW (`wamn-0h0g.22.31`).
    // The union grant this block carried was test-only because the cutover was unbuilt:
    // deploy/sql/run-state.sql withdrew `wamn_app`'s write surface at `5a8645d3` and
    // deferred the replacement to "their owning cutovers". `wamn-0h0g.22.37` built it, so
    // the standing instruction here — WHEN THE EXECUTOR CUTOVER LANDS, THIS BLOCK IS
    // REPLACED BY THAT BUILDER — is discharged: every fenced transition below now runs on
    // exactly `sql::grant_executor_platform_surface_sql`, and a leg that reaches its
    // semantics reaches them on what provisioning emits. What is NOT builder-owned is the
    // membership: `runs_platform` is the only policy an executor session matches, and it
    // is `TO wamn_platform`.
    //
    // `wamn_authority.current_tenant_key()` WENT WITH THE UNION AND IS NOT RESTORED. The
    // surface grants `tenant_key(text)` — load bearing, because `runs_tkey` is an
    // expression index over it and every status write maintains that index — and withholds
    // `current_tenant_key()`, because the floor policy that calls it is narrowed
    // `TO wamn_app` and this family only ever matches the permissive `TO wamn_platform`
    // arm. If a leg below ever needs it, that is a real widening to argue, not a fixture
    // gap to patch.
    //
    // BOTH GENERATIONS are dropped before they are minted. Roles are
    // CLUSTER-wide, so a login left behind by an earlier run could let a
    // mutated builder pass on stale state. `DROP OWNED BY` first, because a role
    // holding any grant cannot be dropped.
    let database = success(&url, "SELECT current_database();")
        .trim()
        .to_string();
    let app_login = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: "t1",
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("derive the transitions App generation");
    let app_generation = provision_sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_login,
        "transitions-app-proof-password",
        "2099-01-01T00:00:00Z",
    );
    let executor_generation = provision_sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::ExecutorPlatform,
        &database,
        EXECUTOR_LOGIN,
        "transitions-proof-password",
        "2099-01-01T00:00:00Z",
    );
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{app_login}') THEN \
                 EXECUTE format('DROP OWNED BY %I', '{app_login}'); \
                 EXECUTE format('DROP ROLE %I', '{app_login}'); \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS \
                 (SELECT FROM pg_roles WHERE rolname = 'wamn_run_projection_writer') THEN \
                 CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS \
                 (SELECT FROM pg_roles WHERE rolname = 'wamn_executor_platform') THEN \
                 CREATE ROLE wamn_executor_platform NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{EXECUTOR_LOGIN}') THEN \
                 EXECUTE format('DROP OWNED BY %I', '{EXECUTOR_LOGIN}'); \
                 EXECUTE format('DROP ROLE %I', '{EXECUTOR_LOGIN}'); \
               END IF; \
             END $$; \
             {app_generation} \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             {catalog} {run_state} {run_queue} \
             INSERT INTO catalog.packages \
               (tenant_id,package_id,package_version,manifest_sha256) \
             VALUES ('t1','cat','1.0.0', \
               'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'); \
             INSERT INTO catalog.effective_releases \
               (tenant_id,effective_release_id,environment,verified_publisher_principal) \
             VALUES ('t1',1,'prod','test-publisher'); \
             INSERT INTO catalog.effective_release_packages \
               (tenant_id,effective_release_id,package_id,package_version) \
             VALUES ('t1',1,'cat','1.0.0'); \
             GRANT wamn_platform TO wamn_executor_platform \
               WITH INHERIT TRUE, SET FALSE, ADMIN FALSE; \
             {executor_generation}"
        ),
    );

    // The mirrored role blocks above are create-only — unlike builder-owned `wamn_app`
    // they have no `ELSE ALTER` arms — and roles are CLUSTER-WIDE, so a role left behind by an
    // earlier database is silently kept with whatever attributes it already carries.
    // This is the leg that makes the drift contract checkable rather than aspirational:
    // it reds both when a block above stops matching the production attribute set and
    // when the cluster this suite was pointed at is not the throwaway it is documented
    // to be.
    success(
        &url,
        "DO $$ DECLARE mirrored text; BEGIN \
           SELECT string_agg(rolname, ',' ORDER BY rolname) INTO mirrored FROM pg_roles \
            WHERE rolname IN \
              ('wamn_effect_writer','wamn_executor_platform', \
               'wamn_run_projection_writer','wamn_scenario_author') \
              AND NOT rolsuper AND NOT rolbypassrls AND NOT rolcanlogin \
              AND NOT rolinherit AND NOT rolcreatedb AND NOT rolcreaterole \
              AND NOT rolreplication; \
           ASSERT mirrored = \
                    'wamn_effect_writer,wamn_executor_platform,\
                     wamn_run_projection_writer,wamn_scenario_author', \
                  'the mirrored roles must carry exactly the attributes \
                   ensure_scenario_author_role_sql and ensure_effect_writer_acl_role_sql mint \
                   (NOLOGIN NOSUPERUSER \
                   NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS); \
                   conforming roles were: ' || coalesce(mirrored, '<none>'); \
         END $$;",
    );

    // THE PRINCIPAL EVERY LEG BELOW RUNS AS, ASSERTED BEFORE ANY OF THEM RUN
    // (`wamn-0h0g.22.31`). A superuser or a `BYPASSRLS` login would satisfy every
    // transition below while proving nothing about the tenant floor, and would do it
    // SILENTLY — there is no leg here whose failure would name it. The membership edge is
    // asserted with the same force: these legs must reach their privileges by INHERITING
    // the stable role, which is what the runtime's connection probe checks, not by holding
    // them directly.
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = 'wamn_app' AND NOT rolcanlogin \
                    AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole \
                    AND NOT rolinherit AND NOT rolreplication AND NOT rolbypassrls \
                    AND rolpassword IS NULL), \
                 'the stable App role must be a passwordless NOLOGIN ACL carrier'; \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = '{app_login}' \
                    AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NOT NULL \
                    AND rolvaliduntil IS NOT NULL), \
                 'the App principal must be a minted, non-bypassing generation'; \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_auth_members AS membership \
                 JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                 JOIN pg_catalog.pg_roles AS child ON child.oid = membership.member \
                  WHERE parent.rolname = 'wamn_app' \
                    AND child.rolname = '{app_login}' \
                    AND NOT membership.admin_option \
                    AND membership.inherit_option \
                    AND NOT membership.set_option), \
                 'the App generation must INHERIT the stable ACL role'; \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = '{EXECUTOR_LOGIN}' \
                    AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NOT NULL \
                    AND rolvaliduntil IS NOT NULL), \
                 'the transitions principal must be a minted, non-bypassing generation'; \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_auth_members AS membership \
                 JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                 JOIN pg_catalog.pg_roles AS child ON child.oid = membership.member \
                  WHERE parent.rolname = 'wamn_executor_platform' \
                    AND child.rolname = '{EXECUTOR_LOGIN}' \
                    AND NOT membership.admin_option \
                    AND membership.inherit_option \
                    AND NOT membership.set_option), \
                 'the generation must INHERIT the stable role, not hold its grants'; \
               ASSERT pg_catalog.pg_has_role('{EXECUTOR_LOGIN}', 'wamn_platform', 'USAGE'); \
               ASSERT NOT pg_catalog.pg_has_role('{EXECUTOR_LOGIN}', 'wamn_app', 'USAGE'); \
             END $$;"
        ),
    );

    let release = release_caller_sql();
    let terminalize = terminalize_sql();

    // Positive caller release, duplicate replay, then terminalization. A
    // transition after terminal state returns its typed refusal.
    success(
        &url,
        "INSERT INTO wamn_run.environment_policies \
           (tenant_id,expected_environment,durability_class) \
         VALUES ('t1','prod','standard'); \
         INSERT INTO wamn_run.runs \
           (tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, environment, \
            wiring_id, wiring_version, attachment_id, status) \
         VALUES ('t1', 'release-1', 'f', 1, 'cat', 1, 'prod', \
           'fixture-wiring', 1, 'http-a', 'running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, lease_owner, lease_expires_at, lease_generation) \
         VALUES ('t1', 'release-1', 'worker-a', now() + interval '1 minute', 1);",
    );
    let release_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE released AS \
           EXECUTE release_stmt('release-1','release-1','worker-a',1, \
                                'responded','{{\"ok\":true}}',200,'respond','sha256:one'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM released) = 'released', 'caller released'; \
           ASSERT (SELECT caller_outcome_kind FROM runs WHERE run_id='release-1') = 'responded', \
                  'caller outcome persisted'; \
         END $$; COMMIT;",
        executor_preamble(),
        release
    );
    success(&url, &release_script);

    let replay_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE replayed AS \
           EXECUTE release_stmt('release-1','release-1','worker-a',1, \
                                'responded','{{\"ok\":true}}',200,'respond','sha256:one'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM replayed) = 'already-released', 'duplicate is replay'; \
           ASSERT (SELECT outcome_kind FROM replayed) = 'responded', 'stored kind returned'; \
         END $$; COMMIT;",
        executor_preamble(),
        release
    );
    success(&url, &replay_script);

    let terminal_script = format!(
        "{} PREPARE terminal_stmt \
           (text,text,text,bigint,text,text,text,text) AS {}; \
         CREATE TEMP TABLE terminal AS \
           EXECUTE terminal_stmt('release-1','release-1','worker-a',1, \
                                 'completed','frontier-exhausted','{{\"done\":true}}',NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM terminal) = 'terminalized', 'run terminalized'; \
           ASSERT (SELECT status FROM runs WHERE run_id='release-1') = 'completed', \
                  'terminal status persisted'; \
           ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='release-1'), \
                  'queue row removed atomically'; \
         END $$; COMMIT;",
        executor_preamble(),
        terminalize
    );
    success(&url, &terminal_script);

    // An attachment identifies admission provenance, not necessarily a waiting
    // caller. Cron and event runs terminalize naturally; request sources must
    // release their caller first.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,attachment_id,status,trigger_source) VALUES \
           ('t1','terminal-cron','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'cron-a','running','cron'), \
           ('t1','terminal-event','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'event-a','running','event'), \
           ('t1','terminal-http-open','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'http-open','running','http'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,attachment_id,status,trigger_source, \
            caller_outcome_kind,caller_outcome_json,caller_http_status,caller_release_node_id, \
            caller_outcome_hash,caller_released_at) VALUES \
           ('t1','terminal-http-released','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'http-released','running','http', \
            'responded','{}',200,'respond','sha256:released',now()); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
           ('t1','terminal-cron','worker-source',now()+interval '1 minute',1), \
           ('t1','terminal-event','worker-source',now()+interval '1 minute',1), \
           ('t1','terminal-http-open','worker-source',now()+interval '1 minute',1), \
           ('t1','terminal-http-released','worker-source',now()+interval '1 minute',1);",
    );
    let source_terminal_script = format!(
        "{} PREPARE terminal_stmt \
           (text,text,text,bigint,text,text,text,text) AS {}; \
         CREATE TEMP TABLE cron_terminal AS \
           EXECUTE terminal_stmt('terminal-cron','terminal-cron','worker-source',1, \
                                 'completed','frontier-exhausted','{{}}',NULL); \
         CREATE TEMP TABLE event_terminal AS \
           EXECUTE terminal_stmt('terminal-event','terminal-event','worker-source',1, \
                                 'completed','frontier-exhausted','{{}}',NULL); \
         CREATE TEMP TABLE http_open_terminal AS \
           EXECUTE terminal_stmt('terminal-http-open','terminal-http-open','worker-source',1, \
                                 'completed','frontier-exhausted','{{}}',NULL); \
         CREATE TEMP TABLE http_released_terminal AS \
           EXECUTE terminal_stmt('terminal-http-released','terminal-http-released', \
                                 'worker-source',1,'completed','frontier-exhausted','{{}}',NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM cron_terminal) = 'terminalized', \
                  'attached cron has no caller to release'; \
           ASSERT (SELECT result_code FROM event_terminal) = 'terminalized', \
                  'attached event has no caller to release'; \
           ASSERT (SELECT result_code FROM http_open_terminal) = 'caller-unreleased', \
                  'HTTP request must release its caller'; \
           ASSERT (SELECT status FROM runs WHERE run_id='terminal-http-open') = 'running', \
                  'caller refusal leaves the request running'; \
           ASSERT EXISTS (SELECT FROM run_queue WHERE run_id='terminal-http-open'), \
                  'caller refusal leaves the request queued'; \
           ASSERT (SELECT result_code FROM http_released_terminal) = 'terminalized', \
                  'released HTTP request terminalizes'; \
         END $$; COMMIT;",
        executor_preamble(),
        terminalize
    );
    success(&url, &source_terminal_script);

    // `release-1` is terminal AND already caller-released, so the typed answer is
    // the stored CAS winner, not the bare terminal refusal: `classified` reads
    // `run-terminal AND caller_released_at IS NOT NULL` as `already-released`, the
    // arm `terminal_caller_replay_still_returns_the_stored_cas_winner` pins in
    // src/transitions.rs. The bare `run-terminal` code is what a terminal run with
    // no caller to release returns, and no leg here observes it.
    let post_terminal_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE refused AS \
           EXECUTE release_stmt('release-1','release-1','worker-a',1, \
                                'failed','{{\"error\":{{}}}}',500,NULL,'sha256:two'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused) = 'already-released', \
                  'post-terminal transition is typed'; \
         END $$; COMMIT;",
        executor_preamble(),
        release
    );
    success(&url, &post_terminal_script);

    // Actual lock race: the new claimant increments generation and holds the
    // queue row while the stale worker enters release_caller. The stale statement
    // resumes after commit and must return fence-lost without caller mutation.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,attachment_id,status) \
         VALUES ('t1','race-1','f',1,'cat',1,'prod', \
           'fixture-wiring',1,'http-race','running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','race-1','stale-worker',now()+interval '1 minute',7);",
    );
    let race_url = url.clone();
    let winner = thread::spawn(move || {
        success(
            &race_url,
            &format!(
                "{} \
                 UPDATE run_queue SET lease_owner='winner', lease_generation=lease_generation+1, \
                        lease_expires_at=now()+interval '1 minute' \
                  WHERE run_id='race-1'; \
                 SELECT pg_sleep(1); COMMIT;",
                executor_preamble(),
            ),
        )
    });
    thread::sleep(Duration::from_millis(200));
    let stale_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE stale AS \
           EXECUTE release_stmt('race-1','race-1','stale-worker',7, \
                                'responded','{{\"bad\":true}}',200,'respond','sha256:stale'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM stale) = 'fence-lost', 'stale generation loses'; \
           ASSERT (SELECT caller_released_at FROM runs WHERE run_id='race-1') IS NULL, \
                  'FenceLost writes no caller state'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='race-1') = 'winner', \
                  'FenceLost writes no queue state'; \
         END $$; COMMIT;",
        executor_preamble(),
        release
    );
    success(&url, &stale_script);
    winner.join().expect("winner thread");

    // Named fault: caller release + terminal run + queue deletion all execute,
    // then the transaction aborts. None may survive.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,attachment_id,status) \
         VALUES ('t1','fault-1','f',1,'cat',1,'prod', \
           'fixture-wiring',1,'http-fault','running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','fault-1','worker-f',now()+interval '1 minute',9);",
    );
    let fault_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         PREPARE terminal_stmt \
           (text,text,text,bigint,text,text,text,text) AS {}; \
         EXECUTE release_stmt('fault-1','fault-1','worker-f',9, \
                              'failed','{{\"error\":{{\"code\":\"boom\"}}}}',500,NULL,'sha256:fault'); \
         EXECUTE terminal_stmt('fault-1','fault-1','worker-f',9, \
                               'failed','node-failed','null','terminal'); \
         SELECT 1/0; COMMIT;",
        executor_preamble(),
        release,
        terminalize
    );
    let fault = psql(&url, &fault_script);
    assert!(
        !fault.status.success(),
        "injected transaction fault must fail"
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT status FROM wamn_run.runs WHERE run_id='fault-1') = 'running', \
                  'fault rolled back run terminal state'; \
           ASSERT (SELECT caller_released_at FROM wamn_run.runs WHERE run_id='fault-1') IS NULL, \
                  'fault rolled back caller state'; \
           ASSERT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id='fault-1' \
                          AND lease_owner='worker-f' AND lease_generation=9), \
                  'fault rolled back queue deletion'; \
         END $$;",
    );

    // The claim-time release record (wamn-0h0g.15.23). The effective release is
    // already an immutable admission pin; the claiming pod proves that identity
    // and records only its verified manifest digest on the EXISTING claim write.
    // The digest is not blanket write-once: NULL -> value is the claim, value ->
    // NULL is how a runnable, effect-free run reopens its claimability (the queue
    // park, wamn-0h0g.15.82), and value -> value' is refused on every path.
    // Covered here against the INSTALLED DDL, which is the guard the composed
    // statements actually meet.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,status,durability_class) VALUES \
           ('t1','record-claim','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'dispatched','standard'), \
           ('t1','record-invalid-digest','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'running','standard'); \
         UPDATE wamn_run.environment_policies SET durability_class='durable' \
          WHERE tenant_id='t1'; \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,status,durability_class) VALUES \
           ('t1','record-effect','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'dispatched','durable'); \
         UPDATE wamn_run.environment_policies SET durability_class='standard' \
          WHERE tenant_id='t1'; \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id) VALUES \
           ('t1','record-claim'),('t1','record-effect');",
    );

    let claim = grant_production_claim_sql();
    let record_script = format!(
        "{} PREPARE claim_stmt (text,text,bigint,int,text) AS {}; \
         EXECUTE claim_stmt('record-claim','worker-record',30000,1, \
           'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'); \
         EXECUTE claim_stmt('record-effect','worker-effect',30000,1, \
           'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'); \
         DO $$ BEGIN \
           ASSERT (SELECT effective_release_id FROM runs WHERE run_id='record-claim') = 1, \
                  'the claim preserves the admitted effective release'; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-claim') \
                  = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
                  'the claim records the claiming manifest digest'; \
           ASSERT (SELECT status FROM runs WHERE run_id='record-claim') = 'running', \
                  'the record rides the claim write, not a second statement'; \
           ASSERT (SELECT lease_generation FROM run_queue WHERE run_id='record-claim') = 1, \
                  'the recording claim is the one that took the lease'; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-effect') \
                  = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
                  'each claim records its own manifest digest'; \
         END $$; COMMIT;",
        executor_preamble(),
        claim
    );
    success(&url, &record_script);

    // The admission trigger protects the effective-release pin independently of
    // the executor's column grant, so exercise it as the throwaway superuser.
    success(
        &url,
        "DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE wamn_run.runs SET effective_release_id = 2 \
              WHERE run_id = 'record-claim'; \
             ASSERT false, 'the admitted effective release was rewritten in place'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-admission-pin-immutable', refusal; \
           END; \
           ASSERT (SELECT effective_release_id FROM wamn_run.runs \
                    WHERE run_id='record-claim') = 1, \
                  'the refused rewrite left the admitted release intact'; \
         END $$;",
    );

    // The executor may write the per-claim digest, but cannot rewrite one already recorded.
    let refusal_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET manifest_digest = \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a' \
              WHERE run_id = 'record-claim'; \
             ASSERT false, 'a recorded manifest digest was rewritten in place'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-release-record-immutable', refusal; \
           END; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-claim') \
                  = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
                  'the refused rewrites left the recorded digest exactly as claimed'; \
         END $$; COMMIT;",
        executor_preamble()
    );
    success(&url, &refusal_script);

    // Erasure is the park/wake arm, and it is conditional: a runnable, effect-free
    // run may reopen its claimability, but a terminal run keeps the audit link to
    // the release closure it executed.
    let erasure_script = format!(
        "{} \
         UPDATE runs SET manifest_digest = NULL \
          WHERE run_id = 'record-claim'; \
         DO $$ BEGIN \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-claim') IS NULL, \
                  'a runnable, effect-free run may reopen its claimability'; \
         END $$; \
         UPDATE runs SET manifest_digest = \
                  'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a' \
          WHERE run_id = 'record-claim'; \
         UPDATE runs SET status = 'completed' WHERE run_id = 'record-claim'; \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET manifest_digest = NULL \
              WHERE run_id = 'record-claim'; \
             ASSERT false, 'a terminal run erased the release it executed under'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-release-record-immutable', refusal; \
           END; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-claim') \
                  = 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
                  'the re-recorded digest survives the refused erasure'; \
         END $$; COMMIT;",
        executor_preamble()
    );
    success(&url, &erasure_script);

    // The other erasure precondition: an attributed effect names the release that
    // fired it, and that link is never rewritten out from under it. `record-effect`
    // is still `running`, so only the effect evidence can refuse here.
    //
    // THIS LEG IS A PREMIUM-TIER PROOF (wamn-0h0g.20.2). The guard's
    // effect-attempt arm is class-gated, so `record-effect` is admitted
    // `durable` above; on the default `standard` class the same run erases its
    // record freely, which is what the leg below asserts and what keeps the
    // queue park from ever aborting on the guard. The split into
    // surviving-spine and shelved-floor suites is wamn-0h0g.20.4's.
    success(
        &url,
        "INSERT INTO wamn_run.effect_attempts \
           (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id,local_node_id, \
            source_artifact_hash,requirement_name,occurrence,seq,generation_fact_kind, \
            attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','record-effect', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',0, \
           'effect-node', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'manager',0,1,'not-required','2099-01-01T00:00:00Z','record-effect-input');",
    );
    let effect_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET manifest_digest = NULL \
              WHERE run_id = 'record-effect'; \
             ASSERT false, 'an attributed effect lost the release that fired it'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-release-record-immutable', refusal; \
           END; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-effect') \
                  = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
                  'the manifest the effect fired under is intact'; \
         END $$; COMMIT;",
        executor_preamble()
    );
    success(&url, &effect_script);

    // THE COMPLEMENT, AND THE HALF THE CLASS GATE MAKES LOAD-BEARING
    // (wamn-0h0g.20.2). The identical run on the DEFAULT class erases its
    // record freely even while carrying an attributed effect. If this leg ever
    // reds, `park_sql` — which carries the same class predicate on the same
    // `EXISTS` — aborts on this guard for every standard run that ever reached
    // the effect ledger, and the run plane loses the arm that reopens
    // claimability (wamn-0h0g.15.82).
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,status,durability_class) VALUES \
           ('t1','record-standard-effect','f',1,'cat',1,'prod', \
            'fixture-wiring',1,'running','standard'); \
         UPDATE wamn_run.runs SET manifest_digest = \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a' \
          WHERE run_id = 'record-standard-effect'; \
         INSERT INTO wamn_run.effect_attempts \
           (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id,local_node_id, \
            source_artifact_hash,requirement_name,occurrence,seq,generation_fact_kind, \
            attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','record-standard-effect', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',0, \
           'effect-node', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'manager',0,1,'not-required','2099-01-01T00:00:00Z','standard-effect-input');",
    );
    let standard_class_script = format!(
        "{} \
         UPDATE runs SET manifest_digest = NULL \
          WHERE run_id = 'record-standard-effect'; \
         DO $$ BEGIN \
           ASSERT (SELECT manifest_digest FROM runs \
                    WHERE run_id='record-standard-effect') IS NULL, \
                  'the default class could not clear a record the park must clear'; \
         END $$; COMMIT;",
        executor_preamble()
    );
    success(&url, &standard_class_script);

    // The class is defended TWICE, and the two guards are independent.
    //
    // First, by the column grant: `wamn_app` holds neither INSERT nor UPDATE on
    // `durability_class`, so the guest-visible role cannot buy the premium tier
    // at all — the refusal is `insufficient_privilege`, before any trigger runs.
    let class_grant_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET durability_class = 'durable' \
              WHERE run_id = 'record-standard-effect'; \
             ASSERT false, 'the app role holds write authority over the class'; \
           EXCEPTION WHEN insufficient_privilege THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'permission denied for table runs', refusal; \
           END; \
         END $$; COMMIT;",
        app_preamble(&app_login)
    );
    success(&url, &class_grant_script);

    // Second, by the column-scoped trigger, which is what defends the class
    // against a role the grant does not stop. RIDER 1 of wamn-0h0g.20.1: a
    // column the trigger does not NAME never fires its transition arm, so this
    // leg is the only thing that can tell a named column from an unnamed one.
    success(
        &url,
        "DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE wamn_run.runs SET durability_class = 'durable' \
              WHERE run_id = 'record-standard-effect'; \
             ASSERT false, 'an admitted run changed its durability class'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-admission-pin-immutable', refusal; \
           END; \
           ASSERT (SELECT durability_class FROM wamn_run.runs \
                    WHERE run_id='record-standard-effect') = 'standard', \
                  'the refused class change leaked through'; \
         END $$;",
    );

    // The ruled literal set against the INSTALLED DDL: the converged standard
    // policy selected the cheap tier, and nothing outside the pair is storable.
    // The pin trigger normally overwrites every supplied class; the
    // superuser-only replica session disables it for this one transaction so
    // this remains a narrow proof of the independent stored-row CHECK.
    success(
        &url,
        "BEGIN; SET LOCAL session_replication_role = replica; DO $$ BEGIN \
           ASSERT (SELECT durability_class FROM wamn_run.runs \
                    WHERE run_id='record-claim') = 'standard', \
                  'the standard policy did not select the cheap tier'; \
           BEGIN \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
                environment,wiring_id,wiring_version,status,durability_class) \
             VALUES ('t1','record-unruled-class','f',1,'cat',1,'prod', \
               'fixture-wiring',1,'running','premium'); \
             ASSERT false, 'the class CHECK admitted an unruled literal'; \
           EXCEPTION WHEN check_violation THEN NULL; \
           END; \
         END $$; ROLLBACK;",
    );

    // ---- RECLAIM AND REAP, DRIVEN UNDER THE CREDENTIAL (wamn-0h0g.22.42) ----
    //
    // `wamn-0h0g.22.31` closed with `renew_production_lease_sql`, the reclaim
    // pair (`advance_claim_attempts_sql`, `clear_pre_effect_state_sql`) and the
    // janitor pair (`select_exhausted_production_sql`,
    // `terminalize_exhausted_production_sql`) asserted only by admission_live's
    // schema-wide TOTALS. Their columns were in the granted set and NO
    // STATEMENT EVER EXECUTED under the credential. The legs below drive each
    // of them as the minted generation asserted at the top of this file, on
    // exactly `grant_executor_platform_surface_sql` — so a missing column grant
    // fails here with the statement NAMED, not as a silent zero-row read.
    //
    // THREE OF THE FIVE RUN THROUGH `EXECUTE ... USING` rather than this file's
    // PREPARE / `CREATE TEMP TABLE AS EXECUTE` shape. That shape refuses them:
    // `CREATE TABLE AS EXECUTE` accepts only a prepared SELECT/TABLE/VALUES
    // ("prepared statement is not a SELECT", measured), and those three are
    // top-level `UPDATE ... RETURNING`. The production string is still executed
    // VERBATIM with bound parameters, which is what the host does with it.
    //
    // EVERY REFUSAL ARM IS PAIRED WITH THE SAME STATEMENT SUCCEEDING IN THE
    // SAME SESSION. Under FORCE RLS a principal matching no policy reads zero
    // rows in silence, so a lone zero proves nothing; a zero standing beside a
    // one on the same statement and the same connection does. Every arm also
    // asserts the stored row AFTER the statement, never the statement's own
    // exit status.
    //
    // `t2` is a second tenant in the same database. `run_queue`'s only policy
    // is `run_queue_tenant`, which carries NO `TO` clause and keys on
    // `app.tenant` — so the queue fence applies to this family too, unlike
    // `runs`, whose `runs_platform` arm is `USING (true)`.
    success(
        &url,
        "INSERT INTO catalog.packages \
           (tenant_id,package_id,package_version,manifest_sha256) \
         VALUES ('t2','cat','1.0.0', \
           'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'); \
         INSERT INTO catalog.effective_releases \
           (tenant_id,effective_release_id,environment,verified_publisher_principal) \
         VALUES ('t2',1,'prod','test-publisher'); \
         INSERT INTO catalog.effective_release_packages \
           (tenant_id,effective_release_id,package_id,package_version) \
         VALUES ('t2',1,'cat','1.0.0'); \
         INSERT INTO wamn_run.environment_policies \
           (tenant_id,expected_environment,durability_class) \
         VALUES ('t2','prod','standard'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,attachment_id,status,input_json) VALUES \
           ('t1','renew-live','f',1,'cat',1,'prod','fixture-wiring',1,'http-renew', \
            'running','{}'), \
           ('t1','renew-expired','f',1,'cat',1,'prod','fixture-wiring',1,'http-renew-dead', \
            'running','{}'), \
           ('t2','renew-other-tenant','f',1,'cat',1,'prod','fixture-wiring',1,'http-renew-t2', \
            'running','{}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
           ('t1','renew-live','renew-worker',now()+interval '30 seconds',3), \
           ('t1','renew-expired','renew-worker',now()-interval '30 seconds',3), \
           ('t2','renew-other-tenant','renew-worker',now()+interval '30 seconds',3);",
    );
    let renew = renew_production_lease_sql();
    let renew_script = format!(
        "{} \
         DO $probe$ \
         DECLARE granted timestamptz; refused timestamptz; touched int; \
                 live_before timestamptz; dead_before timestamptz; \
         BEGIN \
           SELECT q.lease_expires_at INTO live_before \
             FROM run_queue AS q WHERE q.run_id='renew-live'; \
           SELECT q.lease_expires_at INTO dead_before \
             FROM run_queue AS q WHERE q.run_id='renew-expired'; \
           EXECUTE $renew${}$renew$ INTO refused \
             USING 'renew-live'::text,'other-worker'::text,3::bigint,600000::bigint; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 0 AND refused IS NULL, 'a foreign owner renewed a live lease'; \
           EXECUTE $renew${}$renew$ INTO refused \
             USING 'renew-live'::text,'renew-worker'::text,2::bigint,600000::bigint; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 0 AND refused IS NULL, 'a stale generation renewed a live lease'; \
           EXECUTE $renew${}$renew$ INTO refused \
             USING 'renew-expired'::text,'renew-worker'::text,3::bigint,600000::bigint; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 0 AND refused IS NULL, 'an already expired lease was extended'; \
           EXECUTE $renew${}$renew$ INTO refused \
             USING 'renew-other-tenant'::text,'renew-worker'::text,3::bigint,600000::bigint; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 0 AND refused IS NULL, 'the renew reached another tenant queue row'; \
           ASSERT (SELECT q.lease_expires_at FROM run_queue AS q WHERE q.run_id='renew-live') \
                  = live_before, 'a refused renew moved the live deadline'; \
           ASSERT (SELECT q.lease_expires_at FROM run_queue AS q WHERE q.run_id='renew-expired') \
                  = dead_before, 'a refused renew moved the expired deadline'; \
           EXECUTE $renew${}$renew$ INTO granted \
             USING 'renew-live'::text,'renew-worker'::text,3::bigint,600000::bigint; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 1, 'the exact owner and generation could not renew'; \
           ASSERT granted \
                  = (SELECT q.lease_expires_at FROM run_queue AS q WHERE q.run_id='renew-live'), \
                  'the returned deadline is not the stored one'; \
           ASSERT granted > live_before + interval '5 minutes', \
                  'the renewed lease did not move out to the requested TTL'; \
         END $probe$; COMMIT;",
        executor_preamble(),
        renew,
        renew,
        renew,
        renew,
        renew,
    );
    success(&url, &renew_script);
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT q.lease_expires_at FROM wamn_run.run_queue AS q \
                    WHERE q.tenant_id='t2' AND q.run_id='renew-other-tenant') \
                  < now() + interval '2 minutes', \
                  'the executor extended another tenant lease'; \
         END $$;",
    );

    // The pre-effect reclaim. `advance_claim_attempts_sql` is deliberately its
    // own statement OUTSIDE the grant's abort scope (wamn-0h0g.15.69), and
    // `clear_pre_effect_state_sql` reopens claimability by erasing exactly the
    // dead attempt's projection — `state_json` and the manifest digest — and
    // nothing else. `max_attempts` defaults to 20 here, so neither reclaim row
    // is a janitor candidate and the reap leg below cannot select one.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,attachment_id,status,input_json,state_json, \
            manifest_digest) VALUES \
           ('t1','reclaim-dead','f',1,'cat',1,'prod','fixture-wiring',1,'http-reclaim', \
            'running','{\"input\":7}','{\"cursor\":9}', \
            'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'), \
           ('t1','reclaim-fresh','f',1,'cat',1,'prod','fixture-wiring',1,'http-reclaim-new', \
            'dispatched','{}',NULL,NULL), \
           ('t2','reclaim-other-tenant','f',1,'cat',1,'prod','fixture-wiring',1, \
            'http-reclaim-t2','running','{}','{\"cursor\":9}', \
            'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation,attempts) VALUES \
           ('t1','reclaim-dead','dead-worker',now()-interval '1 minute',5,1), \
           ('t1','reclaim-fresh',NULL,NULL,0,0), \
           ('t2','reclaim-other-tenant','dead-worker',now()-interval '1 minute',5,1);",
    );
    let advance = advance_claim_attempts_sql();
    let clear = clear_pre_effect_state_sql();
    let reclaim_script = format!(
        "{} \
         DO $probe$ DECLARE spent int; reopened text; touched int; BEGIN \
           EXECUTE $advance${}$advance$ INTO spent USING 'reclaim-dead'::text; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 1 AND spent = 2, \
                  'replacing a prior lease is crash evidence, got ' \
                  || coalesce(spent::text, '<none>'); \
           EXECUTE $advance${}$advance$ INTO spent USING 'reclaim-fresh'::text; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 1 AND spent = 0, 'a first claim spent crash budget'; \
           EXECUTE $advance${}$advance$ INTO spent USING 'reclaim-other-tenant'::text; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 0, 'the advance reached another tenant queue row'; \
           EXECUTE $clear${}$clear$ INTO reopened USING 'reclaim-dead'::text; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 1 AND reopened = 'reclaim-dead', 'the reclaim reopened no run'; \
           EXECUTE $clear${}$clear$ INTO reopened USING 'reclaim-other-tenant'::text; \
           GET DIAGNOSTICS touched = ROW_COUNT; \
           ASSERT touched = 0, 'the reclaim reached another tenant run'; \
           ASSERT (SELECT q.attempts FROM run_queue AS q WHERE q.run_id='reclaim-dead') = 2, \
                  'the advanced crash evidence did not persist'; \
           ASSERT (SELECT r.state_json FROM runs AS r WHERE r.run_id='reclaim-dead') IS NULL, \
                  'the reclaim kept the dead attempt state'; \
           ASSERT (SELECT r.manifest_digest FROM runs AS r WHERE r.run_id='reclaim-dead') \
                  IS NULL, 'the reclaim kept the dead attempt manifest digest'; \
           ASSERT (SELECT r.input_json FROM runs AS r WHERE r.run_id='reclaim-dead') \
                  = '{{\"input\":7}}'::jsonb, 'the reclaim erased more than the dead attempt'; \
           ASSERT (SELECT r.status FROM runs AS r WHERE r.run_id='reclaim-dead') = 'running', \
                  'the reclaim moved the run status'; \
         END $probe$; COMMIT;",
        executor_preamble(),
        advance,
        advance,
        advance,
        clear,
        clear,
    );
    success(&url, &reclaim_script);
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT q.attempts FROM wamn_run.run_queue AS q \
                    WHERE q.tenant_id='t2' AND q.run_id='reclaim-other-tenant') = 1, \
                  'the executor spent another tenant crash budget'; \
           ASSERT (SELECT r.state_json FROM wamn_run.runs AS r \
                    WHERE r.tenant_id='t2' AND r.run_id='reclaim-other-tenant') \
                  = '{\"cursor\":9}'::jsonb, \
                  'the executor erased another tenant attempt state'; \
           ASSERT (SELECT r.manifest_digest FROM wamn_run.runs AS r \
                    WHERE r.tenant_id='t2' AND r.run_id='reclaim-other-tenant') \
                  = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
                  'the executor erased another tenant release record'; \
         END $$;",
    );

    // The janitor. `select_exhausted_production_sql` is a plain SELECT, so it
    // keeps this file's PREPARE shape; the scope arm beside it is the control
    // that reads zero legitimately while the same statement in the same
    // transaction reads one.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,attachment_id,status,trigger_source,input_json) VALUES \
           ('t1','reap-exhausted','f',1,'cat',1,'prod','fixture-wiring',1,'http-reap', \
            'running','http','{}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
            lease_generation,attempts,max_attempts) \
         VALUES ('t1','reap-exhausted','2000-01-01',900,'dead-worker','2000-01-01',5,3,3);",
    );
    let select_exhausted = select_exhausted_production_sql();
    let terminalize_exhausted = terminalize_exhausted_production_sql();
    let reap_script = format!(
        "{} PREPARE reap_select (bigint,text,text) AS {}; \
         PREPARE reap_terminal (text,text,text) AS {}; \
         CREATE TEMP TABLE reap_candidate AS EXECUTE reap_select(0,'cat','prod'); \
         CREATE TEMP TABLE reap_other_scope AS EXECUTE reap_select(0,'cat','dev'); \
         CREATE TEMP TABLE reaped AS EXECUTE reap_terminal('reap-exhausted', \
           '{{\"error\":{{\"code\":\"infrastructure-failure\"}}}}','sha256:reaped'); \
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM reap_candidate) = 1, \
                  'the janitor locked no crash-budget-exhausted candidate'; \
           ASSERT (SELECT run_id FROM reap_candidate) = 'reap-exhausted', \
                  'the janitor locked the wrong candidate'; \
           ASSERT (SELECT status FROM reap_candidate) = 'running', \
                  'the janitor projection lost the run status'; \
           ASSERT (SELECT durability_class FROM reap_candidate) = 'standard', \
                  'the janitor projection lost the durability class'; \
           ASSERT (SELECT wiring_id FROM reap_candidate) = 'fixture-wiring', \
                  'the janitor projection lost the frozen wiring identity'; \
           ASSERT (SELECT count(*) FROM reap_other_scope) = 0, \
                  'the janitor left its catalog and environment scope'; \
           ASSERT (SELECT status FROM reaped) = 'infrastructure-failure', \
                  'the reap returned no terminal status'; \
           ASSERT (SELECT status FROM runs WHERE run_id='reap-exhausted') \
                  = 'infrastructure-failure', 'the reap did not persist terminal status'; \
           ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='reap-exhausted'), \
                  'the reap did not dequeue atomically'; \
           ASSERT (SELECT caller_outcome_kind FROM runs WHERE run_id='reap-exhausted') \
                  = 'failed', 'the reap left an attached caller unreleased'; \
           ASSERT (SELECT caller_http_status FROM runs WHERE run_id='reap-exhausted') = 500, \
                  'the reap stored the wrong caller status'; \
           ASSERT (SELECT caller_outcome_hash FROM runs WHERE run_id='reap-exhausted') \
                  = 'sha256:reaped', 'the reap stored the wrong caller body hash'; \
           ASSERT (SELECT caller_released_at FROM runs WHERE run_id='reap-exhausted') \
                  IS NOT NULL, 'the reap released no caller'; \
           ASSERT (SELECT result_json FROM runs WHERE run_id='reap-exhausted') IS NULL, \
                  'a flow-grain run stored a manufactured result'; \
         END $$; COMMIT;",
        executor_preamble(),
        select_exhausted,
        terminalize_exhausted,
    );
    success(&url, &reap_script);

    // A malformed digest reaches the named CHECK while the old value is NULL;
    // the immutable-record guard owns only rewrites of a digest already recorded.
    // This leg is last so every behavioral arm above proves out before a malformed
    // release record can abort the suite.
    let digest_shape_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET manifest_digest = 'sha256:not-a-digest' \
              WHERE run_id = 'record-invalid-digest'; \
             ASSERT false, 'runs_release_record_check admitted a malformed digest'; \
           EXCEPTION WHEN check_violation THEN \
             GET STACKED DIAGNOSTICS refusal = CONSTRAINT_NAME; \
             ASSERT refusal = 'runs_release_record_check', refusal; \
           END; \
           ASSERT (SELECT manifest_digest FROM runs \
                    WHERE run_id='record-invalid-digest') IS NULL, \
                  'the unclaimed run carries no release record'; \
         END $$; COMMIT;",
        executor_preamble()
    );
    success(&url, &digest_shape_script);
}
