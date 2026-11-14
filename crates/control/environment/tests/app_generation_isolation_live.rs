//! Live SESSION acceptance test for the App (guest) family's project isolation
//! (`wamn-dkzr`, from the `wamn-0h0g.12.185` acceptance).
//!
//! The App family already has a GRANT-level arm. `workload_generation_live`
//! reads `pg_database.datacl` through `aclexplode` and shows which databases
//! carry a direct `CONNECT` entry for a role. That is a read of what the server
//! was configured to hand out. It is not a refusal, and it runs on a superuser
//! connection, which passes every ACL test and so hides a role defect
//! completely.
//!
//! This acceptance test asks the other question. It opens a REAL session as the minted
//! generation login and records what the server answers at authentication time.
//!
//! The topology is the one the reach crossed. Two neighbouring project-env
//! databases sit on ONE disposable server, both provisioned with the text
//! `provision-project-env` emits rather than a transcription of it, and one App
//! generation is minted for the first of them. `wamn_app` is cluster-global and
//! every generation is a member of it `WITH INHERIT TRUE`, so one stray
//! `GRANT CONNECT` on the stable ACL role is a session on every environment of
//! the cluster. `wamn-0h0g.12.179` measured exactly that.
//!
//! BOTH DIRECTIONS ARE ASSERTED. A refusal on its own cannot tell a closed reach
//! from a credential that never worked, so the positive control runs first,
//! against the same credential, in the same test.
//!
//! The SQLSTATE is MEASURED and asserted as a code, never as a message string.
//! PostgreSQL 18 tests database `CONNECT` while it initializes the backend and
//! reports the failure as `42501`.
//!
//! The test runs on the PostgreSQL server of its own test process and holds the
//! process lock of that server, because it creates cluster-global roles.

use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_control_provision::{
    WorkloadRoleFamily, WorkloadRoleIdentity, project_env_database_name, workload_login,
};
use wamn_environment::provision_project_env::{privilege_sql, role_posture_sql};
use wamn_test_infrastructure::locked_database;

const ORG: &str = "dkzrisolation";
const PROJECT: &str = "inventory";
/// The environment the generation is minted for.
const HOME_ENVIRONMENT: &str = "dev";
/// The NEIGHBOUR. It is a real provisioned project-env database, not an empty
/// one, because the reach this closes was measured between two provisioned
/// environments on one cluster.
const NEIGHBOUR_ENVIRONMENT: &str = "staging";
const INSTANCE: &str = "q4w8z1n6";
/// ONE project across both environments. Same project, different database, so the
/// refusal cannot be explained away as a project mismatch.
const PROJECT_ENV_KEY: &str = "project-dkzr";
/// A throwaway probe string for a disposable server that dies with this test
/// process. It is never printed and it names no real credential.
const GENERATION_PROBE_PASSWORD: &str = "app-generation-probe";
const NEVER_EXPIRES: &str = "2100-01-01T00:00:00Z";

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect the disposable server as superuser");

    client
}

/// The FALLIBLE dial as the role under test.
///
/// It returns the server's own error, so the refusal arm reads a SQLSTATE
/// instead of matching text. A helper that panicked on failure cannot assert the
/// outcome this test exists for.
async fn dial(url: &str, database: &str, role: &str, password: &str) -> anyhow::Result<Client> {
    let mut config: sqlx::postgres::PgConnectOptions = url.parse().expect("parse the server URL");
    config = config.database(database).username(role).password(password);
    let client = wamn_postgres::connect(config.clone()).await?;

    Ok(client)
}

/// `CREATE DATABASE` is forbidden inside a transaction block, and a
/// multi-statement simple query is one. Every statement here goes alone.
async fn run_alone(superuser: &mut Client, statement: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe((statement).to_string()))
        .execute(&mut *superuser)
        .await
        .map(drop)
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

#[tokio::test]
async fn an_app_generation_cannot_open_a_session_on_a_neighbouring_project_env_database() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut superuser = connect(&url).await;

    let home = project_env_database_name(ORG, PROJECT, HOME_ENVIRONMENT, INSTANCE);
    let neighbour = project_env_database_name(ORG, PROJECT, NEIGHBOUR_ENVIRONMENT, INSTANCE);

    // Step 1 of the runbook, then step 2 and step 3 for each environment, with
    // the SAME text the verb emits.
    run_alone(&mut superuser, &role_posture_sql()).await;
    for database in [&home, &neighbour] {
        run_alone(&mut superuser, &format!("CREATE DATABASE \"{database}\"")).await;
        run_alone(&mut superuser, &privilege_sql(database)).await;
    }

    // One generation, minted for the HOME database only, through the builder
    // `--prepare-guest-generation` calls.
    let generation = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: PROJECT_ENV_KEY,
            database: &home,
        },
    )
    .expect("the App family takes a project target");
    run_alone(
        &mut superuser,
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::App,
            &home,
            &generation,
            GENERATION_PROBE_PASSWORD,
        ),
    )
    .await;

    // THE POSITIVE CONTROL. It runs first and it uses the same credential, so a
    // refusal below cannot be a broken password or a missing role.
    let mut session = dial(&url, &home, &generation, GENERATION_PROBE_PASSWORD)
        .await
        .expect("the minted generation must open a session on its OWN database");
    let who: String = sqlx::query("SELECT current_user::text")
        .fetch_one(&mut session)
        .await
        .expect("read the authenticated role")
        .get(0);
    assert_eq!(
        who, generation,
        "the positive control authenticated as another role"
    );
    drop(session);

    // THE RULED ASSERTION. The server refuses the session on the neighbour.
    let refusal = dial(&url, &neighbour, &generation, GENERATION_PROBE_PASSWORD)
        .await
        .expect_err(
            "the App generation opened a session on a NEIGHBOURING project-env \
             database, which is a live project-isolation defect",
        );
    assert_eq!(
        refusal
            .downcast_ref::<sqlx::Error>()
            .unwrap()
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501"),
        "the neighbour refused with the wrong SQLSTATE. A wrong password \
         (28P01) or an absent database (3D000) establishes nothing about \
         reach: {refusal:?}"
    );
}
