//! Live-apply gate for the dispatcher principal's PROVISIONING (wamn-0h0g.12.122,
//! cut over to credential generations by wamn-0h0g.22.24).
//!
//! `wamn-0h0g.12.66` landed the builders and re-pointed
//! `deploy/platform/dispatcher-projects.example.yaml` at `wamn_dispatch_reader`,
//! but nothing called them: the shipped manifest named a role production
//! provisioning did not create. `wamn-0h0g.12.122` made provisioning create it.
//! `wamn-0h0g.22.24` then took its LOGIN away, because a cluster-global role
//! with a per-database `GRANT CONNECT` is reach across every database on the
//! cluster the moment the family gains generations that inherit it — the defect
//! `wamn-0h0g.12.179` measured live for the guest.
//!
//! This gate applies THE SAME text the subcommand emits — [`role_sql`] and
//! [`privilege_sql`], not a transcription.
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** URL (path `/postgres`) of a throwaway
//! Postgres — the variable `run_plane_live` uses, so one container serves both.
//! Skipped cleanly when unset.
//!
//! The legs run sequentially under one test entry: PostgreSQL roles are
//! CLUSTER-wide, so two entries mutating `wamn_dispatch_reader` in parallel
//! would race each other and every other gate pointed at the same container.
//!
//! Four proofs:
//!
//! 1. **the stable role lands connection-free**, from the real emitted batches —
//!    NOLOGIN, no password, and NO database `CONNECT` entry of its own;
//! 2. **replay is a no-op, and a pre-cutover environment CONVERGES**, proven by
//!    applying each batch twice and diffing every `aclitem` on the database plus
//!    every attribute of the role, then by re-introducing the retired `GRANT`
//!    and watching the shipped batch take it away;
//! 3. **the revoke sits on the surviving side of the owner statement** —
//!    measured, not assumed, and the measurement is ASYMMETRIC (see
//!    `owner_statement_asymmetry_leg`);
//! 4. **the cross-database reach is CLOSED** — a dispatch-reader generation
//!    minted for one database opens a session on that database and is REFUSED on
//!    a neighbouring one. Both directions, because an arm that only shows the
//!    refusal cannot tell a closed reach from a broken credential.

mod support;

use tokio_postgres::{Client, NoTls};

use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, DB_OWNER_ROLE, DISPATCH_READER_ROLE, WorkloadRoleFamily,
    WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_ctl::provision_project_env::{privilege_sql, role_posture_sql, role_sql};

/// The legacy app-password input remains until `wamn-0h0g.12.185`, but
/// `ensure_app_role_sql` deliberately emits none of it. Keeping a conspicuous
/// fixture value here proves the role batch cannot leak that input back into
/// the retired shared LOGIN.
const APP_PASSWORD: &str = "wamn_app";
const RETIRED_APP_PASSWORD: &str = "retired-app-session-probe";
const GENERATION_PASSWORD: &str = "reader-generation-probe";
const DATABASE: &str = "wamn-db-probe--dispatch--dev";
const ORDERING_DATABASE: &str = "wamn-db-probe--ordering--dev";
/// The NEIGHBOUR the cross-database arm proves is unreachable. It is a real
/// provisioned environment, not an empty database: the reach this closes was
/// measured between two environments on one cluster.
const NEIGHBOUR_DATABASE: &str = "wamn-db-probe--neighbour--dev";

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

fn role_config(url: &str, database: &str, role: &str, password: &str) -> tokio_postgres::Config {
    let mut config: tokio_postgres::Config = url.parse().expect("parse Postgres URL");
    config.dbname(database).user(role).password(password);
    config
}

async fn connect_to(url: &str, database: &str, role: &str, password: &str) -> Client {
    let (client, conn) = role_config(url, database, role, password)
        .connect(NoTls)
        .await
        .expect("dial as role");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

/// The FALLIBLE dial. A refusal arm that used `connect_to` would panic on the
/// outcome it is trying to assert.
async fn try_connect_to(
    url: &str,
    database: &str,
    role: &str,
    password: &str,
) -> Result<(), String> {
    match role_config(url, database, role, password)
        .connect(NoTls)
        .await
    {
        Ok((client, conn)) => {
            tokio::spawn(async move {
                let _ = conn.await;
            });
            drop(client);
            Ok(())
        }
        Err(error) => Err(format!("{error:?}")),
    }
}

/// `CREATE`/`DROP DATABASE` are forbidden inside a transaction block, and a
/// multi-statement simple query IS one. Every statement here goes alone.
async fn run_alone(client: &Client, statement: &str) {
    client
        .batch_execute(statement)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

/// Apply the emitted role artifact with the same commit boundary psql gives
/// its complete statements: harden first, then drain after NOLOGIN is visible.
async fn apply_role_artifact(client: &Client) {
    run_alone(client, &role_posture_sql(APP_PASSWORD)).await;
    run_alone(client, &sql::drain_app_role_sessions_sql()).await;
}

/// PostgreSQL roles are CLUSTER-wide, so a preamble that only drops databases is
/// not hermetic. `DROP OWNED BY` is what makes the role droppable: it removes the
/// role's privileges on objects in the current database *and* on shared objects,
/// which is where a previous run's database `CONNECT` lives.
///
/// The GENERATIONS are dropped first and BY PATTERN, not by name: a leftover
/// healthy generation from an earlier run sits happily inside
/// `prepare_workload_generation_sql`'s `IF NOT EXISTS` and would mask a mutated
/// builder — and it is the generation, not the stable role, that carries
/// `CONNECT` now.
///
/// Only the reader family is dropped. `wamn_app` and `wamn_db_owner` are shared
/// with every other gate against this container and are left to their own
/// idempotent create-or-harden builders inside [`role_sql`].
async fn drop_reader_role(su: &Client) {
    run_alone(
        su,
        &format!(
            "DO $generations$ DECLARE generation record; BEGIN \
               FOR generation IN SELECT rolname FROM pg_catalog.pg_roles \
                                  WHERE rolname ~ '^{DISPATCH_READER_ROLE}_[0-9a-f]{{40}}_[ab]$' \
               LOOP \
                 EXECUTE format('DROP OWNED BY %I', generation.rolname); \
                 EXECUTE format('DROP ROLE %I', generation.rolname); \
               END LOOP; \
             END $generations$;"
        ),
    )
    .await;
    run_alone(
        su,
        &format!(
            "DO $preamble$ BEGIN \
               IF EXISTS (SELECT FROM pg_catalog.pg_roles \
                          WHERE rolname = '{DISPATCH_READER_ROLE}') THEN \
                 EXECUTE 'DROP OWNED BY {DISPATCH_READER_ROLE}'; \
                 EXECUTE 'DROP ROLE {DISPATCH_READER_ROLE}'; \
               END IF; \
             END $preamble$;"
        ),
    )
    .await;
}

/// The dispatch-reader A generation for one database, DERIVED from the same
/// builder `provision-project-env` uses rather than spelled.
fn reader_generation(database: &str) -> String {
    workload_generation_role(
        WorkloadRoleFamily::DispatchReader,
        WorkloadRoleScope::ProjectEnvironment {
            org: "probe",
            project: "dispatch",
            environment: "dev",
            database,
        },
        CredentialGeneration::A,
    )
    .expect("the dispatch reader takes a project-environment scope")
}

/// Every `aclitem` on the database, rendered and sorted under the `C` collation
/// so the comparison is byte order rather than the server's locale.
async fn database_acl(su: &Client, database: &str) -> Vec<String> {
    su.query_one(
        "SELECT COALESCE( \
           (SELECT array_agg(entry::text ORDER BY entry::text COLLATE \"C\") \
              FROM pg_catalog.pg_database AS db, unnest(db.datacl) AS entry \
             WHERE db.datname = $1), \
           ARRAY[]::text[])",
        &[&database],
    )
    .await
    .expect("read database ACL")
    .get(0)
}

/// Every attribute the role builder claims to converge, in one row.
///
/// `pg_authid`, NOT `pg_roles`, for the password: the view substitutes the
/// literal `'********'` for every row's `rolpassword`, so `IS NOT NULL` reads
/// TRUE against `pg_roles` for a role that has no password at all — and "carries
/// no credential" is the whole assertion this bead added.
async fn role_attributes(su: &Client, role: &str) -> Option<Vec<bool>> {
    su.query_opt(
        "SELECT ARRAY[rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, \
                      rolinherit, rolreplication, rolbypassrls, \
                      rolpassword IS NOT NULL] \
           FROM pg_catalog.pg_authid WHERE rolname = $1",
        &[&role],
    )
    .await
    .expect("read role attributes")
    .map(|row| row.get(0))
}

async fn role_xmin(su: &Client, role: &str) -> String {
    su.query_one(
        "SELECT xmin::text FROM pg_catalog.pg_authid WHERE rolname = $1",
        &[&role],
    )
    .await
    .expect("read role row version")
    .get(0)
}

async fn can_connect(su: &Client, role: &str, database: &str) -> bool {
    su.query_one(
        "SELECT pg_catalog.has_database_privilege($1, $2, 'CONNECT')",
        &[&role, &database],
    )
    .await
    .expect("probe CONNECT")
    .get(0)
}

#[tokio::test]
async fn dispatch_reader_provisioning_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-0h0g.12.122 provisioning gate");
        return;
    };
    let su = connect(&url).await;
    provisioned_reader_is_idempotent_and_connection_free_leg(&su, &url).await;
    owner_statement_asymmetry_leg(&su).await;
    cross_database_reach_is_closed_leg(&su, &url).await;
}

/// The whole provisioning path, applied twice and diffed.
async fn provisioned_reader_is_idempotent_and_connection_free_leg(su: &Client, url: &str) {
    run_alone(
        su,
        &format!("DROP DATABASE IF EXISTS \"{DATABASE}\" WITH (FORCE)"),
    )
    .await;
    drop_reader_role(su).await;

    // Step 1 of the runbook: the role batch, to the target cluster's superuser.
    let roles = role_posture_sql(APP_PASSWORD);
    let artifact = role_sql(APP_PASSWORD);
    assert!(
        !artifact.contains(&format!("PASSWORD '{APP_PASSWORD}'")),
        "the legacy app-password input reached role SQL: {artifact}"
    );
    apply_role_artifact(su).await;
    run_alone(
        su,
        &format!("ALTER ROLE \"{APP_ROLE}\" LOGIN PASSWORD '{RETIRED_APP_PASSWORD}' INHERIT"),
    )
    .await;
    let retired_app = connect_to(url, "postgres", APP_ROLE, RETIRED_APP_PASSWORD).await;
    let retired_pid: i32 = retired_app
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .expect("observe the retired shared-login session")
        .get(0);
    assert_eq!(
        su.query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE pid = $1 AND usename = $2",
            &[&retired_pid, &APP_ROLE],
        )
        .await
        .expect("observe the active retired shared-login session")
        .get::<_, i64>(0),
        1,
        "the session-drain proof did not establish an old shared session"
    );
    run_alone(su, &roles).await;
    run_alone(su, &sql::drain_app_role_sessions_sql()).await;
    assert_eq!(
        su.query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE usename = $1",
            &[&APP_ROLE],
        )
        .await
        .expect("read shared-login session residue after convergence")
        .get::<_, i64>(0),
        0,
        "the bounded native drain left an authenticated wamn_app session"
    );
    assert!(
        retired_app.simple_query("SELECT 1").await.is_err(),
        "the retired shared-login client survived server-side termination"
    );
    assert!(
        try_connect_to(url, "postgres", APP_ROLE, RETIRED_APP_PASSWORD)
            .await
            .is_err(),
        "the retired shared credential opened a new session after NOLOGIN"
    );
    let app_hardened = role_attributes(su, APP_ROLE)
        .await
        .expect("the role batch created the app ACL role");
    assert_eq!(
        app_hardened,
        vec![false, false, false, false, false, false, false, false],
        "wamn_app is not the stable passwordless NOLOGIN NOINHERIT ACL role \
         (order: login, super, createdb, createrole, inherit, replication, \
         bypassrls, password set): {app_hardened:?}"
    );
    let hardened = role_attributes(su, DISPATCH_READER_ROLE)
        .await
        .expect("the role batch created the dispatch reader");
    assert_eq!(
        hardened,
        vec![false, false, false, false, false, false, false, false],
        "NOLOGIN and NOTHING else — no password, no attribute (order: login, \
         super, createdb, createrole, inherit, replication, bypassrls, password \
         set): {hardened:?}"
    );
    let app_xmin = role_xmin(su, APP_ROLE).await;

    // Replay leg: the create-or-harden block must be an actual no-op. Comparing
    // xmin catches an unnecessary ALTER that post-state equality would hide.
    apply_role_artifact(su).await;
    assert_eq!(
        role_attributes(su, APP_ROLE).await.as_ref(),
        Some(&app_hardened),
        "replaying the role batch moved the app ACL role"
    );
    assert_eq!(
        role_xmin(su, APP_ROLE).await,
        app_xmin,
        "the exact second convergence rewrote the stable app role"
    );
    assert_eq!(
        su.query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE usename = $1",
            &[&APP_ROLE],
        )
        .await
        .expect("read shared-login session residue after exact replay")
        .get::<_, i64>(0),
        0,
        "the exact second convergence left a retired shared-login session"
    );
    assert_eq!(
        role_attributes(su, DISPATCH_READER_ROLE).await.as_ref(),
        Some(&hardened),
        "the role batch is not idempotent"
    );

    // A drifted attribute is re-hardened rather than reported as success — the
    // arm that separates create-or-HARDEN from CREATE IF NOT EXISTS. The drift
    // seeded here is EXACTLY the retired shape: a LOGIN role with a password,
    // which is what a pre-wamn-0h0g.22.24 cluster carries.
    run_alone(
        su,
        &format!(
            "ALTER ROLE \"{DISPATCH_READER_ROLE}\" LOGIN PASSWORD 'legacy' BYPASSRLS CREATEDB"
        ),
    )
    .await;
    run_alone(su, &roles).await;
    assert_eq!(
        role_attributes(su, DISPATCH_READER_ROLE).await.as_ref(),
        Some(&hardened),
        "a pre-cutover LOGIN reader was not re-hardened to a connection-free \
         NOLOGIN carrier"
    );

    // Step 2 stand-in for production's CNPG `Database` CR. An EXISTING
    // environment is the interesting starting state, so the database begins
    // owned by the guest-reachable role wamn-0h0g.12.108 took title away from.
    run_alone(
        su,
        &format!("CREATE DATABASE \"{DATABASE}\" OWNER \"{APP_ROLE}\""),
    )
    .await;

    // Step 3: the privilege batch, applied twice, diffed aclitem by aclitem.
    let privileges = privilege_sql(DATABASE);
    run_alone(su, &privileges).await;
    let first = database_acl(su, DATABASE).await;
    run_alone(su, &privileges).await;
    let second = database_acl(su, DATABASE).await;
    assert_eq!(
        first, second,
        "replaying the privilege batch moved the database ACL"
    );

    // The exact converged ACL. Pinned whole, so a widened grant (`CTc`), a lost
    // `PUBLIC` revoke (an `=Tc/…` entry), or a re-appearing stable-role entry
    // all fail here rather than passing a "can it connect" smoke.
    //
    // BOTH stable ACL roles are ABSENT. Each is cluster-global and each has
    // generations that INHERIT it, so a `CONNECT` entry here reaches every
    // project-env database on the cluster — measured for `wamn_app` at
    // wamn-0h0g.12.179 and closed for `wamn_dispatch_reader` at
    // wamn-0h0g.22.24.
    assert_eq!(
        first,
        vec![format!("{DB_OWNER_ROLE}=CTc/{DB_OWNER_ROLE}")],
        "converged database ACL"
    );
    for stable in [APP_ROLE, DISPATCH_READER_ROLE] {
        assert!(
            !can_connect(su, stable, DATABASE).await,
            "the emitted privilege batch must leave the stable {stable} ACL role \
             connection free: {first:?}"
        );
    }

    // The CONVERGENCE direction, which "stopped granting" would not prove: seed
    // exactly what a pre-cutover environment carries and watch the shipped batch
    // take it away.
    run_alone(
        su,
        &format!("GRANT CONNECT ON DATABASE \"{DATABASE}\" TO \"{DISPATCH_READER_ROLE}\""),
    )
    .await;
    assert!(can_connect(su, DISPATCH_READER_ROLE, DATABASE).await);
    run_alone(su, &privileges).await;
    assert!(
        !can_connect(su, DISPATCH_READER_ROLE, DATABASE).await,
        "the shipped batch must REVOKE a pre-cutover reader CONNECT, not merely \
         stop granting one"
    );

    // And the GENERATION is the thing that dials, with no manual SQL between
    // provisioning and the dial.
    let generation = reader_generation(DATABASE);
    run_alone(
        su,
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::DispatchReader,
            DATABASE,
            &generation,
            GENERATION_PASSWORD,
            "2100-01-01T00:00:00Z",
        ),
    )
    .await;
    let reader = connect_to(url, DATABASE, &generation, GENERATION_PASSWORD).await;
    let who: String = reader
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("the reader session works")
        .get(0);
    assert_eq!(who, generation);
    drop(reader);

    run_alone(su, &format!("DROP DATABASE \"{DATABASE}\" WITH (FORCE)")).await;
    drop_reader_role(su).await;
}

/// **A measured premise correction, kept as a test so it cannot rot.**
///
/// `provision_project_env::privilege_sql` emits its statements after
/// `ALTER DATABASE … OWNER TO`. Measured on PostgreSQL the effect of that
/// statement is NOT symmetric: it rewrites only the OUTGOING OWNER's ACL entry,
/// so
///
/// * `wamn_app`, which owned the database, loses the `CONNECT` that had merged
///   into `wamn_app=CTc/wamn_app` — the hazard measured at `47b404cf`; while
/// * `wamn_dispatch_reader`, which never owns it, keeps `c` and only has its
///   GRANTOR rewritten (`reader=c/wamn_app` → `reader=c/wamn_db_owner`).
///
/// That asymmetry is exactly why the REVOKE must follow the owner statement
/// too, and why a reordering regression is one-sided and easy to miss: the
/// re-ownership would clear one role's entry by accident and leave the other's
/// standing, so a batch that revoked before the owner change would look correct
/// for `wamn_app` and leave the dispatcher reachable.
async fn owner_statement_asymmetry_leg(su: &Client) {
    run_alone(
        su,
        &format!("DROP DATABASE IF EXISTS \"{ORDERING_DATABASE}\" WITH (FORCE)"),
    )
    .await;
    drop_reader_role(su).await;
    apply_role_artifact(su).await;
    run_alone(
        su,
        &format!("CREATE DATABASE \"{ORDERING_DATABASE}\" OWNER \"{APP_ROLE}\""),
    )
    .await;

    // The RETIRED shape, re-created by hand because the shipped builders no
    // longer produce it: both stable roles granted CONNECT before the owner
    // statement.
    run_alone(su, &sql::grant_connect_on_database_sql(ORDERING_DATABASE)).await;
    run_alone(
        su,
        &format!("GRANT CONNECT ON DATABASE \"{ORDERING_DATABASE}\" TO \"{DISPATCH_READER_ROLE}\""),
    )
    .await;
    assert!(can_connect(su, APP_ROLE, ORDERING_DATABASE).await);
    assert!(can_connect(su, DISPATCH_READER_ROLE, ORDERING_DATABASE).await);

    run_alone(
        su,
        &format!("{};", sql::set_database_owner_sql(ORDERING_DATABASE)),
    )
    .await;
    let acl = database_acl(su, ORDERING_DATABASE).await;
    assert!(
        !can_connect(su, APP_ROLE, ORDERING_DATABASE).await,
        "the outgoing owner kept CONNECT across re-ownership — the 47b404cf \
         hazard the statement order exists for is gone, and the ordering \
         comment in provision_project_env::privilege_sql is now wrong: {acl:?}"
    );
    assert!(
        can_connect(su, DISPATCH_READER_ROLE, ORDERING_DATABASE).await,
        "a non-owner grantee lost CONNECT across re-ownership: {acl:?}"
    );

    // The shipped order then takes the SURVIVING entry away. That is the whole
    // reason the reader's revoke has to follow the owner statement: before it,
    // re-ownership would carry the revoke's effect off again.
    run_alone(su, &privilege_sql(ORDERING_DATABASE)).await;
    for stable in [APP_ROLE, DISPATCH_READER_ROLE] {
        assert!(
            !can_connect(su, stable, ORDERING_DATABASE).await,
            "the shipped batch left {stable} connectable on a re-owned database"
        );
    }
    assert_eq!(
        database_acl(su, ORDERING_DATABASE).await,
        vec![format!("{DB_OWNER_ROLE}=CTc/{DB_OWNER_ROLE}")],
        "the converged ACL on a re-owned database names the title role alone"
    );

    run_alone(
        su,
        &format!("DROP DATABASE \"{ORDERING_DATABASE}\" WITH (FORCE)"),
    )
    .await;
    drop_reader_role(su).await;
}

/// **THE RULED PROOF (`wamn-0h0g.22.24` step 5): a dispatch-reader generation
/// minted for ONE database cannot open a session on another.**
///
/// This is the reach `wamn-0h0g.12.179` measured live for the guest and the
/// reason the stable role's `CONNECT` had to go: `wamn_dispatch_reader` is
/// cluster-global, its generations are members `WITH INHERIT TRUE`, and
/// `has_database_privilege` resolves through membership — so a single
/// `GRANT CONNECT` per environment on the stable role was a session on every
/// environment.
///
/// BOTH DIRECTIONS ARE ASSERTED. A refusal on its own cannot distinguish a
/// closed reach from a credential that never worked, so the positive control
/// runs first, against the same credential, in the same leg.
async fn cross_database_reach_is_closed_leg(su: &Client, url: &str) {
    for database in [DATABASE, NEIGHBOUR_DATABASE] {
        run_alone(
            su,
            &format!("DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"),
        )
        .await;
    }
    drop_reader_role(su).await;
    apply_role_artifact(su).await;

    // TWO provisioned environments on one cluster, both through the shipped
    // batches — the exact topology the reach crossed.
    for database in [DATABASE, NEIGHBOUR_DATABASE] {
        run_alone(su, &format!("CREATE DATABASE \"{database}\"")).await;
        run_alone(su, &privilege_sql(database)).await;
    }

    // One generation, minted for DATABASE only.
    let generation = reader_generation(DATABASE);
    run_alone(
        su,
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::DispatchReader,
            DATABASE,
            &generation,
            GENERATION_PASSWORD,
            "2100-01-01T00:00:00Z",
        ),
    )
    .await;

    // THE ACL POST-STATE, from the server, before a single dial: the generation
    // holds CONNECT on its own database and on nothing else, and the stable role
    // it inherits holds none at all. `has_database_privilege` resolves through
    // membership, so this is the transitive answer, not the direct grant.
    assert!(
        can_connect(su, &generation, DATABASE).await,
        "the minted generation cannot reach the database it was minted for"
    );
    assert!(
        !can_connect(su, &generation, NEIGHBOUR_DATABASE).await,
        "the generation reaches a NEIGHBOURING project-env database — the \
         cluster-global stable role is handing out CONNECT again"
    );
    assert!(
        !can_connect(su, DISPATCH_READER_ROLE, DATABASE).await
            && !can_connect(su, DISPATCH_READER_ROLE, NEIGHBOUR_DATABASE).await,
        "the stable dispatch-reader ACL role holds CONNECT somewhere; every \
         generation inherits it"
    );

    // AND THE REAL SESSIONS, because an ACL read is a claim about what the
    // server would do and a dial is what it does.
    try_connect_to(url, DATABASE, &generation, GENERATION_PASSWORD)
        .await
        .expect("the generation must open a session on its OWN database");
    let refusal = try_connect_to(url, NEIGHBOUR_DATABASE, &generation, GENERATION_PASSWORD)
        .await
        .expect_err("the generation opened a session on a NEIGHBOURING database");
    assert!(
        refusal.contains("42501") || refusal.to_lowercase().contains("permission denied"),
        "the neighbour refused for the wrong reason (a wrong password or a \
         missing database would prove nothing about reach): {refusal}"
    );

    for database in [DATABASE, NEIGHBOUR_DATABASE] {
        run_alone(su, &format!("DROP DATABASE \"{database}\" WITH (FORCE)")).await;
    }
    drop_reader_role(su).await;
}
