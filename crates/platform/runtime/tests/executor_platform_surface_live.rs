//! Ignored live gate for the executor-platform credential's PLAN SUPPLY and
//! its exact-credential probe (`wamn-0h0g.22.42`).
//!
//! Two things `wamn-0h0g.22.31` could not discharge live meet here because they
//! need the same fixture: ONE provisioned executor-platform generation against
//! the REAL `deploy/sql` schemas.
//!
//! PLAN SUPPLY. `wiring_resolution`'s three statements had NO LIVE GATE AT ALL,
//! and the ELEVEN catalog relations they read were covered only by
//! admission_live's schema-wide `has_table_privilege` totals. A grant asserted
//! by a total with nothing running through it is the weakest proof this branch
//! accepts, so each statement is executed here, verbatim, with bound
//! parameters, as the minted generation. The union of the relations the three
//! touch IS [`sql::EXECUTOR_PLATFORM_CATALOG_RELATIONS`]: activation, effective
//! release heads and membership, wirings and tombstones plus the library
//! (active); the v3 snapshot and release components (release); and the four
//! connection relations (candidate).
//!
//! THE RAW STATEMENTS RATHER THAN `resolve_active_wiring` AND FRIENDS. Those
//! wrappers decode and LOWER the result through `wiring_lowering`, which is a
//! different owner with its own refusals — a lowering failure would arrive
//! looking exactly like a grant failure, and a lowering that got stricter would
//! red this gate for a reason that has nothing to do with the credential. What
//! is unproven is whether these statements EXECUTE and RETURN under this
//! credential, so that is what runs.
//!
//! CREDENTIAL EXACTNESS. `credential_exactness_probe`'s wrong-database,
//! wrong-tenant-binding and wrong-membership arms were unit-covered against a
//! `FakeQueries` double; `PostgresQueryExecutor` — the half that actually asks
//! PostgreSQL — had never run against a server at all. Each arm runs here on a
//! real connection.
//!
//! *** UNDER FORCE RLS A PRINCIPAL THAT MATCHES NO POLICY READS ZERO ROWS WITH
//! NO ERROR. *** Every refusal arm below therefore stands beside the same
//! statement SUCCEEDING on the same connection, and asserts observed values
//! rather than an exit status. The principal is asserted NOT `rolsuper` and NOT
//! `rolbypassrls` before any of it runs: a superuser answers TRUE to every
//! `pg_has_role` and reads every row, so it would satisfy the whole file while
//! proving nothing.

use anyhow::Context as _;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_runtime::plugins::wamn_postgres::{
    ACTIVE_WIRING_SQL, AclExpectation, AclTarget, AmbientCredentialState, CANDIDATE_WIRING_SQL,
    CredentialConnectionKind, CredentialProbeErrorKind, CredentialProbePredicate,
    ExpectedCredentialIdentity, MembershipExpectation, MembershipMode, RELEASE_WIRING_SQL,
    credential_exactness_probe, explicit_credential_source,
};

/// The generation login every leg runs as.
///
/// Named with `ExecutorPlatform`'s frozen SHORT generation prefix: the family's
/// stable role name does not fit the 63-byte identifier cap once a scope digest
/// and an A/B suffix are appended, so a login that reads like a real generation
/// has to start here.
const GENERATION: &str = "wamn_exec_platform_surface_live_a";
const GENERATION_PASSWORD: &str = "executor-surface-proof-password";

/// The second database the wrong-`current_database` arm connects to.
const ELSEWHERE: &str = "w68_execplat_elsewhere";

const TENANT: &str = "t1";
const PACKAGE_ID: &str = "cat";
const PACKAGE_VERSION: &str = "1.0.0";
const ENVIRONMENT: &str = "prod";
const EFFECTIVE_RELEASE_ID: i32 = 1;
const WIRING_ID: &str = "wiring-a";
const WIRING_VERSION: i32 = 1;
/// The retired pointer: enabled, well formed, and tombstoned.
const DEAD_WIRING_ID: &str = "wiring-dead";

fn digest(fill: &str) -> String {
    format!("sha256:{}", fill.repeat(64))
}

fn graph(wiring_id: &str) -> String {
    format!(
        "{{\"format-version\":\"0.1\",\"wiring-id\":\"{wiring_id}\",\"version\":1,\
          \"entry\":\"node\",\"nodes\":{{\"node\":{{\"component\":\"entity\",\
          \"interface-version\":\"0.1\",\"operation\":\"create\"}}}}}}"
    )
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("executor-platform surface live connection failed: {error}");
        }
    });
    Ok(client)
}

/// Rewrite an admin url onto the generation's credential, optionally moving it
/// to another database.
fn generation_url(admin: &str, database: Option<&str>) -> anyhow::Result<String> {
    let mut url = Url::parse(admin)?;
    url.set_username(GENERATION)
        .map_err(|()| anyhow::anyhow!("set the generation username"))?;
    url.set_password(Some(GENERATION_PASSWORD))
        .map_err(|()| anyhow::anyhow!("set the generation password"))?;
    if let Some(database) = database {
        url.set_path(&format!("/{database}"));
    }
    Ok(url.into())
}

/// Open one transaction shaped like `begin_with_claims` does for this class.
///
/// The three plan-supply statements take the tenant as `$1` and read no GUC, so
/// the claim is not what fences them; it is set because it IS what the host
/// injects, and because the `run_queue` leg below depends on it.
async fn begin_claimed(client: &Client) -> anyhow::Result<()> {
    client.batch_execute("BEGIN").await?;
    client
        .execute("SELECT set_config('app.tenant', $1, true)", &[&TENANT])
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a disposable PostgreSQL 18 URL in WAMN_EXEC_PLATFORM_PG_URL"]
async fn executor_platform_surface_live() -> anyhow::Result<()> {
    let admin_url = std::env::var("WAMN_EXEC_PLATFORM_PG_URL")
        .context("set WAMN_EXEC_PLATFORM_PG_URL to a disposable superuser PostgreSQL url")?;
    let admin = connect(&admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);

    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog_ddl = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .context("read catalog DDL")?;
    let run_state_ddl = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .context("read run-state DDL")?;
    let run_queue_ddl = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .context("read run-queue DDL")?;

    // THE SECOND DATABASE GOES FIRST, AND THAT ORDER IS LOAD BEARING.
    // `DROP OWNED BY` reaches only the current database, so a generation left
    // behind by an earlier run still holds `CONNECT` on `ELSEWHERE` and
    // `DROP ROLE` refuses with a DETAIL naming it. Dropping the database first
    // removes that ACL entry with it. `CREATE`/`DROP DATABASE` cannot run
    // inside a transaction block, so each is its own `batch_execute` — a
    // multi-statement simple query is wrapped in an implicit transaction.
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {ELSEWHERE}"))
        .await
        .context("drop the elsewhere database")?;

    // THE GENERATION IS DROPPED BEFORE IT IS MINTED, INSIDE AN EXISTENCE CHECK.
    // Roles are CLUSTER-wide, so a login left behind by an earlier run of this
    // suite satisfies every `IF NOT EXISTS` in the builder and would let a
    // MUTATED builder pass on the previous run's role.
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[ \
                 'wamn_app','wamn_scenario_author','wamn_control_author','wamn_effect_writer', \
                 'wamn_executor_platform','wamn_http_admitter','{GENERATION}' \
               ] LOOP \
                 IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname=role_name) THEN \
                   EXECUTE format('DROP OWNED BY %I', role_name); \
                   EXECUTE format('DROP ROLE %I', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_control_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             BEGIN; {catalog_ddl} {run_state_ddl} {run_queue_ddl} COMMIT;"
        ))
        .await
        .context("install the run and catalog planes")?;
    admin
        .batch_execute(&format!("CREATE DATABASE {ELSEWHERE}"))
        .await
        .context("create the elsewhere database")?;

    // ONE BUILDER CALL AND NOTHING ELSE. `prepare_workload_generation_sql`
    // carries `grant_executor_platform_surface_sql` as its stable surface, the
    // `wamn_platform` group edge, the generation role, its password and its
    // `CONNECT`. Every plan-supply leg below therefore stands on exactly what
    // provisioning emits; no grant is manufactured here.
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::ExecutorPlatform,
            &database,
            GENERATION,
            GENERATION_PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await
        .context("provision the executor-platform generation")?;
    // The wrong-membership arm names a role that must EXIST: `pg_has_role`
    // raises on an unknown role, which the probe reports as an UNAVAILABLE
    // probe rather than a mismatch, and the arm would then pass for the wrong
    // reason. This is the family's own builder, not a hand-rolled role.
    admin
        .batch_execute(&sql::ensure_workload_acl_role_sql(
            WorkloadRoleFamily::HttpAdmitter,
        ))
        .await
        .context("ensure the http-admitter acl role")?;
    // `CONNECT` on the second database is FIXTURE PLUMBING for the
    // wrong-`current_database` arm — the ability to open a socket, not a
    // run-plane authority. `ELSEWHERE` carries no schema, no relation and no
    // grant that any statement in this file reads.
    admin
        .batch_execute(&format!(
            "GRANT CONNECT ON DATABASE {ELSEWHERE} TO \"{GENERATION}\";"
        ))
        .await
        .context("let the generation reach the elsewhere database")?;

    // THE PRINCIPAL, ASSERTED BEFORE ANY LEG RUNS. A superuser or a BYPASSRLS
    // login satisfies every read below and every `pg_has_role` above while
    // proving nothing about either, and it would do it SILENTLY.
    admin
        .batch_execute(&format!(
            "DO $$ BEGIN \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = '{GENERATION}' \
                    AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NOT NULL \
                    AND rolvaliduntil IS NOT NULL), \
                 'the surface principal must be a minted, non-bypassing generation'; \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_auth_members AS membership \
                 JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                 JOIN pg_catalog.pg_roles AS child ON child.oid = membership.member \
                  WHERE parent.rolname = 'wamn_executor_platform' \
                    AND child.rolname = '{GENERATION}' \
                    AND NOT membership.admin_option AND membership.inherit_option \
                    AND NOT membership.set_option), \
                 'the generation must INHERIT the stable role, not hold its grants'; \
               ASSERT pg_catalog.pg_has_role('{GENERATION}', 'wamn_platform', 'USAGE'), \
                 'the generation must match the permissive platform floor arm'; \
               ASSERT NOT pg_catalog.pg_has_role('{GENERATION}', 'wamn_app', 'USAGE'), \
                 'the generation must not reach the guest role'; \
               ASSERT NOT pg_catalog.pg_has_role('{GENERATION}', 'wamn_http_admitter', 'USAGE'), \
                 'the generation must not reach the callable-http role'; \
             END $$;"
        ))
        .await
        .context("assert the generation identity")?;

    let component_digest = digest("a");
    let projection_hash = digest("6");
    let imports_fingerprint = digest("b");
    let wiring_hash = digest("c");
    let dead_wiring_hash = digest("d");
    let manifest_sha256 = digest("f");
    let requirement_hash = digest("1");
    let definition_hash = digest("2");
    let validation_hash = digest("3");
    let manifest_body = concat!(
        "{\"attachments\":{},\"components\":[],\"format-version\":3,",
        "\"registrations\":{},\"release\":{\"effective-release-id\":1,",
        "\"environment\":\"prod\",\"packages\":[{\"package-id\":\"cat\",",
        "\"package-version\":\"1.0.0\",\"tenant-id\":\"t1\"}],",
        "\"tenant-id\":\"t1\"},\"wirings\":[]}"
    );
    admin
        .batch_execute(&format!(
            "INSERT INTO catalog.packages \
               (tenant_id,package_id,package_version,manifest_sha256) \
             VALUES ('{TENANT}','{PACKAGE_ID}','{PACKAGE_VERSION}','{manifest_sha256}'); \
             INSERT INTO catalog.effective_releases \
               (tenant_id,effective_release_id,environment,verified_publisher_principal) \
             VALUES ('{TENANT}',{EFFECTIVE_RELEASE_ID},'{ENVIRONMENT}','test-publisher'); \
             INSERT INTO catalog.effective_release_packages \
               (tenant_id,effective_release_id,package_id,package_version) \
             VALUES ('{TENANT}',{EFFECTIVE_RELEASE_ID},'{PACKAGE_ID}','{PACKAGE_VERSION}'); \
             INSERT INTO catalog.effective_release_heads \
               (tenant_id,environment,effective_release_id) \
             VALUES ('{TENANT}','{ENVIRONMENT}',{EFFECTIVE_RELEASE_ID}); \
             INSERT INTO catalog.component_library \
               (tenant_id,package_id,package_version,component,interface_version,operation, \
                component_digest,projection_hash,imports,imports_fingerprint,effects,input_ports, \
                output_ports,parameters) \
             VALUES ('{TENANT}','{PACKAGE_ID}','{PACKAGE_VERSION}','entity','0.1','create', \
                     '{component_digest}','{projection_hash}','[]','{imports_fingerprint}', \
                     '[]','[]','[]','[]'); \
             INSERT INTO catalog.wirings \
               (tenant_id,package_id,package_version,wiring_id,version, \
                graph_json,wiring_hash) VALUES \
               ('{TENANT}','{PACKAGE_ID}','{PACKAGE_VERSION}','{WIRING_ID}',{WIRING_VERSION}, \
                '{live_graph}','{wiring_hash}'), \
               ('{TENANT}','{PACKAGE_ID}','{PACKAGE_VERSION}','{DEAD_WIRING_ID}',{WIRING_VERSION}, \
                '{dead_graph}','{dead_wiring_hash}'); \
             INSERT INTO catalog.wiring_activation \
               (tenant_id,package_id,environment,wiring_id,confirmed_definition_hash,enabled) \
             VALUES \
               ('{TENANT}','{PACKAGE_ID}','{ENVIRONMENT}','{WIRING_ID}','{wiring_hash}',true), \
               ('{TENANT}','{PACKAGE_ID}','{ENVIRONMENT}','{DEAD_WIRING_ID}', \
                '{dead_wiring_hash}',true); \
             INSERT INTO catalog.wiring_tombstones \
               (tenant_id,package_id,environment,wiring_id,reason) \
             VALUES ('{TENANT}','{PACKAGE_ID}','{ENVIRONMENT}','{DEAD_WIRING_ID}', \
                     'surface-proof'); \
             INSERT INTO catalog.release_components \
               (tenant_id,effective_release_id,wiring_package_id,wiring_package_version, \
                wiring_id,wiring_version,node_id,package_id,package_version,component_digest) \
             VALUES ('{TENANT}',{EFFECTIVE_RELEASE_ID},'{PACKAGE_ID}','{PACKAGE_VERSION}', \
                     '{WIRING_ID}',{WIRING_VERSION},'node','{PACKAGE_ID}','{PACKAGE_VERSION}', \
                     '{component_digest}'); \
             INSERT INTO catalog.release_manifest_v3_snapshots \
               (tenant_id,effective_release_id,manifest_digest,canonical_bytes) \
             SELECT '{TENANT}',{EFFECTIVE_RELEASE_ID}, \
                    'sha256:' || encode(sha256(bytes), 'hex'), bytes \
               FROM (SELECT convert_to('{manifest_body}', 'UTF8') AS bytes) AS frozen; \
             INSERT INTO catalog.connection_requirements \
               (tenant_id,component_digest,store_alias,requirement_json,requirement_hash) \
             VALUES ('{TENANT}','{component_digest}','a-store', \
                     '{{\"requirement-type\":\"http\"}}','{requirement_hash}'); \
             INSERT INTO catalog.connection_instances \
               (tenant_id,environment,instance_id,requirement_type,contract) \
             VALUES ('{TENANT}','{ENVIRONMENT}','instance-a','http','wamn:http/0.1'); \
             INSERT INTO catalog.connection_generations \
               (tenant_id,environment,instance_id,generation,definition_json, \
                definition_hash,credential_set_handle) \
             VALUES ('{TENANT}','{ENVIRONMENT}','instance-a',1, \
                     '{{\"base-url\":\"https://a.invalid\"}}','{definition_hash}', \
                     'credential-a-1'); \
             UPDATE catalog.connection_instances \
                SET active_generation=1,revision=revision+1, \
                    updated_at=clock_timestamp()+interval '1 second' \
              WHERE tenant_id='{TENANT}' AND environment='{ENVIRONMENT}'; \
             INSERT INTO catalog.connection_bindings \
               (tenant_id,effective_release_id,component_digest,store_alias, \
                environment,instance_id,binding_status,validation_status,validation_hash) \
             VALUES ('{TENANT}',{EFFECTIVE_RELEASE_ID},'{component_digest}','a-store', \
                     '{ENVIRONMENT}','instance-a','active','valid','{validation_hash}'); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id,expected_environment,durability_class) \
             VALUES ('{TENANT}','{ENVIRONMENT}','standard'); \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
                wiring_id,wiring_version,status,input_json) \
             VALUES ('{TENANT}','run-1','f',1,'{PACKAGE_ID}',{EFFECTIVE_RELEASE_ID},'{ENVIRONMENT}', \
                     '{WIRING_ID}',{WIRING_VERSION},'dispatched','{{}}'); \
             INSERT INTO wamn_run.run_queue (tenant_id,run_id) VALUES ('{TENANT}','run-1');",
            live_graph = graph(WIRING_ID),
            dead_graph = graph(DEAD_WIRING_ID),
        ))
        .await
        .context("seed the plan-supply fixture")?;

    // ---- PLAN SUPPLY, LIVE UNDER THE CREDENTIAL ---------------------------
    let platform_url = generation_url(&admin_url, None)?;
    let generation = connect(&platform_url).await?;
    begin_claimed(&generation).await?;

    let active = generation
        .query(
            ACTIVE_WIRING_SQL,
            &[
                &TENANT,
                &PACKAGE_ID,
                &ENVIRONMENT,
                &WIRING_ID,
                &WIRING_VERSION,
            ],
        )
        .await
        .context("the active-wiring snapshot must execute under the credential")?;
    assert_eq!(
        active.len(),
        1,
        "the enabled pointer resolved to no row under the executor credential"
    );
    assert_eq!(active[0].get::<_, i32>(0), WIRING_VERSION);
    assert_eq!(active[0].get::<_, i32>(1), EFFECTIVE_RELEASE_ID);
    assert_eq!(active[0].get::<_, String>(2), PACKAGE_VERSION);
    assert_eq!(active[0].get::<_, String>(4), wiring_hash);
    let components: Value = serde_json::from_str(&active[0].get::<_, String>(5))?;
    assert_eq!(
        components,
        serde_json::json!([{
            "scope": {
                "tenant-id": TENANT, "package-id": PACKAGE_ID,
                "package-version": PACKAGE_VERSION,
            },
            "component": "entity", "interface-version": "0.1", "operation": "create",
            "registered-operation": null,
            "component-digest": component_digest, "imports": [],
            "imports-fingerprint": imports_fingerprint, "effects": [],
            "input-ports": [], "output-ports": [], "parameters": [],
        }]),
        "the component library join produced no admitted fact"
    );

    // THE CONTROL THAT LEGITIMATELY READS ZERO. `wiring-dead` is a complete,
    // ENABLED pointer at a well-formed definition whose hash matches — the only
    // thing that withholds it is `catalog.wiring_tombstones`. A session that
    // matched no policy would read zero here AND zero above; this one reads
    // zero here and one above, on the same statement and the same connection.
    let tombstoned = generation
        .query(
            ACTIVE_WIRING_SQL,
            &[
                &TENANT,
                &PACKAGE_ID,
                &ENVIRONMENT,
                &DEAD_WIRING_ID,
                &WIRING_VERSION,
            ],
        )
        .await?;
    assert!(
        tombstoned.is_empty(),
        "a retired wiring id resolved through its own tombstone"
    );

    let manifest_digest: String = admin
        .query_one(
            "SELECT manifest_digest FROM catalog.release_manifest_v3_snapshots \
              WHERE tenant_id=$1 AND effective_release_id=$2",
            &[&TENANT, &EFFECTIVE_RELEASE_ID],
        )
        .await?
        .get(0);
    let release = generation
        .query(
            RELEASE_WIRING_SQL,
            &[
                &TENANT,
                &PACKAGE_ID,
                &ENVIRONMENT,
                &WIRING_ID,
                &WIRING_VERSION,
                &EFFECTIVE_RELEASE_ID,
                &manifest_digest,
            ],
        )
        .await
        .context("the release-wiring snapshot must execute under the credential")?;
    assert_eq!(
        release.len(),
        1,
        "the frozen release version resolved to no row under the executor credential"
    );
    assert_eq!(release[0].get::<_, String>(2), PACKAGE_VERSION);
    assert_eq!(release[0].get::<_, String>(4), wiring_hash);
    let release_components: Value = serde_json::from_str(&release[0].get::<_, String>(5))?;
    assert_eq!(
        release_components.as_array().map(Vec::len),
        Some(1),
        "release membership produced no component closure"
    );
    assert_eq!(release_components[0]["node-id"], "node");
    assert_eq!(
        release_components[0]["component"]["component-digest"],
        component_digest
    );

    // The release path's own legitimate zero: the same coordinates under a
    // manifest digest this release never sealed.
    let foreign_release = generation
        .query(
            RELEASE_WIRING_SQL,
            &[
                &TENANT,
                &PACKAGE_ID,
                &ENVIRONMENT,
                &WIRING_ID,
                &WIRING_VERSION,
                &EFFECTIVE_RELEASE_ID,
                &digest("e"),
            ],
        )
        .await?;
    assert!(
        foreign_release.is_empty(),
        "a foreign manifest digest resolved a serving wiring"
    );

    let candidate = generation
        .query(
            CANDIDATE_WIRING_SQL,
            &[
                &TENANT,
                &PACKAGE_ID,
                &ENVIRONMENT,
                &WIRING_ID,
                &WIRING_VERSION,
                &EFFECTIVE_RELEASE_ID,
                &wiring_hash,
            ],
        )
        .await
        .context("the candidate-wiring snapshot must execute under the credential")?;
    assert_eq!(
        candidate.len(),
        1,
        "the frozen candidate resolved to no row under the executor credential"
    );
    assert_eq!(
        (candidate[0].get::<_, i64>(6), candidate[0].get::<_, i64>(7)),
        (1, 0),
        "the candidate node summary is wrong"
    );
    assert_eq!(
        (candidate[0].get::<_, i64>(8), candidate[0].get::<_, i64>(9)),
        (1, 1),
        "the requirement did not resolve to one usable binding"
    );
    let binding_world: Value = serde_json::from_str(&candidate[0].get::<_, String>(10))?;
    assert_eq!(
        binding_world,
        serde_json::json!([{
            "component-digest": component_digest, "store-alias": "a-store",
            "requirement-hash": requirement_hash, "instance-id": "instance-a",
            "instance-revision": 1, "requirement-type": "http",
            "contract": "wamn:http/0.1", "validation-hash": validation_hash,
            "generation": 1, "definition-hash": definition_hash,
            "credential-set-handle": "credential-a-1",
        }]),
        "the binding world the four connection relations produce is wrong"
    );

    // The candidate path's own legitimate zero: the exact coordinates under a
    // content hash that names a different document.
    let foreign_candidate = generation
        .query(
            CANDIDATE_WIRING_SQL,
            &[
                &TENANT,
                &PACKAGE_ID,
                &ENVIRONMENT,
                &WIRING_ID,
                &WIRING_VERSION,
                &EFFECTIVE_RELEASE_ID,
                &dead_wiring_hash,
            ],
        )
        .await?;
    assert!(
        foreign_candidate.is_empty(),
        "a candidate resolved under another document's content hash"
    );

    // *** `has_table_privilege` IS BLIND TO RLS. *** The ACL says this
    // principal may SELECT `wamn_run.run_queue`; `run_queue_tenant` carries no
    // `TO` clause and keys on `app.tenant`, so the SAME connection reads one
    // row with the claim injected and ZERO without it. This is why every arm in
    // this file asserts rows rather than privileges.
    let claimed_rows: i64 = generation
        .query_one("SELECT count(*) FROM wamn_run.run_queue", &[])
        .await?
        .get(0);
    assert_eq!(claimed_rows, 1, "the claimed session read no queue row");
    generation.batch_execute("COMMIT").await?;
    let unclaimed_rows: i64 = generation
        .query_one("SELECT count(*) FROM wamn_run.run_queue", &[])
        .await?
        .get(0);
    assert_eq!(
        unclaimed_rows, 0,
        "an unclaimed session read the queue that the tenant claim is supposed to fence"
    );
    let acl_says_yes: bool = generation
        .query_one(
            "SELECT pg_catalog.has_table_privilege( \
               current_user, 'wamn_run.run_queue', 'SELECT')",
            &[],
        )
        .await?
        .get(0);
    assert!(
        acl_says_yes,
        "the ACL blindness leg needs the privilege it is blind about"
    );

    // ---- CREDENTIAL EXACTNESS, LIVE ---------------------------------------
    //
    // The probe checks `session_user`, `current_user` and `current_database()`
    // against the expectation, then asks the SERVER for each membership and
    // each ACL fact. Only the source arms refuse before a socket exists; the
    // rest are the server's own answers, and none of them had ever been asked.
    let expected_identity = |database: &str, memberships, acl| {
        ExpectedCredentialIdentity::new(GENERATION, GENERATION, database, TENANT, memberships, acl)
    };
    let member_of_own_role = || {
        vec![MembershipExpectation::new(
            "wamn_executor_platform",
            MembershipMode::Member,
            true,
        )]
    };

    // THE CONTROL ARM. Everything correct: the probe passes on a real
    // connection, so every refusal below is the named predicate and not a
    // probe that refuses whatever it is given.
    let exact = credential_exactness_probe(
        explicit_credential_source(&platform_url, TENANT, AmbientCredentialState::Absent)
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        expected_identity(
            &database,
            member_of_own_role(),
            vec![
                AclExpectation::new(AclTarget::Table("catalog.wirings".into()), "SELECT", true),
                AclExpectation::new(AclTarget::Table("wamn_run.runs".into()), "INSERT", false),
            ],
        ),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    exact
        .probe_pooled(&generation)
        .await
        .map_err(|error| anyhow::anyhow!("the exact credential was refused: {error}"))?;

    // WRONG `current_database`. Source and expectation must agree on the
    // database name or the probe refuses before the socket, so the mismatch has
    // to arrive the way it would in production: a connection that is not on the
    // database the credential named. The refusal is therefore the SERVER's
    // `current_database()`, and it carries a connection kind — proof it was
    // reached over the wire rather than decided from the url.
    let elsewhere_url = generation_url(&admin_url, Some(ELSEWHERE))?;
    let elsewhere = connect(&elsewhere_url).await?;
    let observed_elsewhere: String = elsewhere
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    assert_eq!(observed_elsewhere, ELSEWHERE);
    let wrong_database = exact
        .probe_pooled(&elsewhere)
        .await
        .expect_err("a connection on another database must be refused");
    assert_eq!(
        wrong_database.kind(),
        CredentialProbeErrorKind::PredicateMismatch
    );
    assert_eq!(
        wrong_database.predicate(),
        CredentialProbePredicate::Database
    );
    assert_eq!(
        wrong_database.connection_kind(),
        Some(CredentialConnectionKind::Pooled),
        "a database refusal decided without a connection proves nothing about the server"
    );

    // WRONG MEMBERSHIP. Identity and database are exact, so the only thing left
    // to refuse is the server's own `pg_has_role`. `wamn_app` is the guest ACL
    // role this generation must never hold — the exact confusion that made the
    // platform pool dial the guest url before `wamn-0h0g.22.31`.
    let wrong_membership = credential_exactness_probe(
        explicit_credential_source(&platform_url, TENANT, AmbientCredentialState::Absent)
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        expected_identity(
            &database,
            vec![MembershipExpectation::new(
                "wamn_app",
                MembershipMode::Member,
                true,
            )],
            Vec::new(),
        ),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?
    .probe_pooled(&generation)
    .await
    .expect_err("a membership the generation does not hold must be refused");
    assert_eq!(
        wrong_membership.kind(),
        CredentialProbeErrorKind::PredicateMismatch
    );
    assert_eq!(
        wrong_membership.predicate(),
        CredentialProbePredicate::Membership
    );
    assert_eq!(
        wrong_membership.connection_kind(),
        Some(CredentialConnectionKind::Pooled)
    );

    // The other direction, and it is the arm a superuser would swallow: a
    // FORBIDDEN membership the generation genuinely lacks must be reported as
    // absent. `pg_has_role` answers TRUE for every role when asked about a
    // superuser, so this passes only because the principal asserted above is
    // not one.
    credential_exactness_probe(
        explicit_credential_source(&platform_url, TENANT, AmbientCredentialState::Absent)
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        expected_identity(
            &database,
            vec![MembershipExpectation::new(
                "wamn_http_admitter",
                MembershipMode::Member,
                false,
            )],
            Vec::new(),
        ),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?
    .probe_pooled(&generation)
    .await
    .map_err(|error| {
        anyhow::anyhow!("a genuinely absent membership was reported present: {error}")
    })?;

    // WRONG ACL. `wamn_run.runs` is SELECT plus a COLUMN-grain UPDATE for this
    // family and INSERT for nothing, so an expectation that claims INSERT is
    // refused by the server's `has_table_privilege`, not by this test.
    let wrong_acl = credential_exactness_probe(
        explicit_credential_source(&platform_url, TENANT, AmbientCredentialState::Absent)
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        expected_identity(
            &database,
            member_of_own_role(),
            vec![AclExpectation::new(
                AclTarget::Table("wamn_run.runs".into()),
                "INSERT",
                true,
            )],
        ),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?
    .probe_pooled(&generation)
    .await
    .expect_err("an ACL fact the family does not hold must be refused");
    assert_eq!(
        wrong_acl.kind(),
        CredentialProbeErrorKind::PredicateMismatch
    );
    assert_eq!(wrong_acl.predicate(), CredentialProbePredicate::Acl);
    assert_eq!(
        wrong_acl.connection_kind(),
        Some(CredentialConnectionKind::Pooled)
    );

    // WRONG TENANT BINDING. This arm refuses BEFORE a socket exists, and that
    // is the whole point: the binding is lifecycle metadata minted beside the
    // credential, so a selector pairing this credential with another tenant's
    // expectation never gets to open a connection at all. `connection_kind()`
    // is `None` — the evidence that no server was consulted — while the
    // control arm above, differing only in this value, connected and read rows.
    let wrong_binding = credential_exactness_probe(
        explicit_credential_source(&platform_url, TENANT, AmbientCredentialState::Absent)
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        ExpectedCredentialIdentity::new(
            GENERATION,
            GENERATION,
            database.as_str(),
            "t2",
            member_of_own_role(),
            Vec::new(),
        ),
    )
    .expect_err("a credential paired with another tenant's expectation must be refused");
    assert_eq!(
        wrong_binding.kind(),
        CredentialProbeErrorKind::PredicateMismatch
    );
    assert_eq!(
        wrong_binding.predicate(),
        CredentialProbePredicate::TenantBinding
    );
    assert_eq!(
        wrong_binding.connection_kind(),
        None,
        "the tenant binding must refuse before a connection is used"
    );

    Ok(())
}
