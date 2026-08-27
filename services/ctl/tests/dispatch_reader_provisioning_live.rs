//! Live-apply gate for the dispatcher principal's PROVISIONING (wamn-0h0g.12.122).
//!
//! `wamn-0h0g.12.66` landed the three builders and re-pointed
//! `deploy/platform/dispatcher-projects.example.yaml` at `wamn_dispatch_reader`,
//! but nothing called them: the shipped manifest named a role production
//! provisioning did not create. This gate proves `provision-project-env`'s own
//! emitted batches now do, by applying THE SAME text the subcommand emits —
//! [`role_sql`] and [`privilege_sql`], not a transcription.
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
//! 1. **the role and the CONNECT grant land**, from the real emitted batches;
//! 2. **replay is a no-op**, proven by applying each batch TWICE and diffing
//!    every `aclitem` on the database plus every attribute of the role;
//! 3. **the CONNECT grant sits on the surviving side of the owner statement** —
//!    measured, not assumed, and the measurement is ASYMMETRIC (see
//!    `owner_statement_asymmetry_leg`);
//! 4. **the dispatcher can dial**, as the new principal, with no manual SQL.

mod support;

use tokio_postgres::{Client, NoTls};

use wamn_control_provision::{APP_ROLE, DB_OWNER_ROLE, DISPATCH_READER_ROLE, sql};
use wamn_ctl::provision_project_env::{privilege_sql, role_sql};

/// `ensure_app_role_sql` sets the password at CREATION ONLY, and `wamn_app` is
/// cluster-wide and shared with every other gate pointed at this container
/// (`run_plane_live::reset` dials it as `wamn_app`/`wamn_app`). Using any other
/// value here would silently break those gates on whichever ran second.
const APP_PASSWORD: &str = "wamn_app";
const READER_PASSWORD: &str = "reader-provisioning-probe";
const DATABASE: &str = "wamn-db-probe--dispatch--dev";
const ORDERING_DATABASE: &str = "wamn-db-probe--ordering--dev";

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

async fn connect_to(url: &str, database: &str, role: &str, password: &str) -> Client {
    let mut config: tokio_postgres::Config = url.parse().expect("parse Postgres URL");
    config.dbname(database).user(role).password(password);
    let (client, conn) = config.connect(NoTls).await.expect("dial as role");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

/// `CREATE`/`DROP DATABASE` are forbidden inside a transaction block, and a
/// multi-statement simple query IS one. Every statement here goes alone.
async fn run_alone(client: &Client, statement: &str) {
    client
        .batch_execute(statement)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

/// PostgreSQL roles are CLUSTER-wide, so a preamble that only drops databases is
/// not hermetic. `DROP OWNED BY` is what makes the role droppable: it removes the
/// role's privileges on objects in the current database *and* on shared objects,
/// which is where a previous run's database `CONNECT` lives.
///
/// Only the reader is dropped. `wamn_app` and `wamn_db_owner` are shared with
/// every other gate against this container and are left to their own idempotent
/// create-or-harden builders inside [`role_sql`].
async fn drop_reader_role(su: &Client) {
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
async fn role_attributes(su: &Client, role: &str) -> Option<Vec<bool>> {
    su.query_opt(
        "SELECT ARRAY[rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, \
                      rolinherit, rolreplication, rolbypassrls] \
           FROM pg_catalog.pg_roles WHERE rolname = $1",
        &[&role],
    )
    .await
    .expect("read role attributes")
    .map(|row| row.get(0))
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
    provisioned_reader_is_idempotent_and_dialable_leg(&su, &url).await;
    owner_statement_asymmetry_leg(&su).await;
}

/// The whole provisioning path, applied twice, diffed, and then dialed.
async fn provisioned_reader_is_idempotent_and_dialable_leg(su: &Client, url: &str) {
    run_alone(
        su,
        &format!("DROP DATABASE IF EXISTS \"{DATABASE}\" WITH (FORCE)"),
    )
    .await;
    drop_reader_role(su).await;

    // Step 1 of the runbook: the role batch, to the target cluster's superuser.
    let roles = role_sql(APP_PASSWORD, READER_PASSWORD);
    run_alone(su, &roles).await;
    let hardened = role_attributes(su, DISPATCH_READER_ROLE)
        .await
        .expect("the role batch created the dispatch reader");
    assert_eq!(
        hardened,
        vec![true, false, false, false, false, false, false],
        "LOGIN, and nothing else: {hardened:?}"
    );

    // Replay leg: the create-or-harden block must converge, not error.
    run_alone(su, &roles).await;
    assert_eq!(
        role_attributes(su, DISPATCH_READER_ROLE).await.as_ref(),
        Some(&hardened),
        "the role batch is not idempotent"
    );

    // A drifted attribute is re-hardened rather than reported as success — the
    // arm that separates create-or-HARDEN from CREATE IF NOT EXISTS.
    run_alone(
        su,
        &format!("ALTER ROLE \"{DISPATCH_READER_ROLE}\" BYPASSRLS CREATEDB"),
    )
    .await;
    run_alone(su, &roles).await;
    assert_eq!(
        role_attributes(su, DISPATCH_READER_ROLE).await.as_ref(),
        Some(&hardened),
        "a drifted reader was not re-hardened"
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
    // `PUBLIC` revoke (an `=Tc/…` entry), or a reader that never arrives all
    // fail here rather than passing a "can it connect" smoke.
    //
    // `wamn_app` is ABSENT (wamn-0h0g.12.179): it is the stable NOLOGIN guest
    // ACL role every per-tenant generation INHERITS, so a `CONNECT` entry here
    // reaches every project-env database on the cluster, and
    // `--prepare-guest-generation` refuses the result.
    assert_eq!(
        first,
        vec![
            format!("{DB_OWNER_ROLE}=CTc/{DB_OWNER_ROLE}"),
            format!("{DISPATCH_READER_ROLE}=c/{DB_OWNER_ROLE}"),
        ],
        "converged database ACL"
    );
    assert!(
        !can_connect(su, APP_ROLE, DATABASE).await,
        "the emitted privilege batch must leave the stable guest ACL role \
         connection free: {first:?}"
    );

    // Proof 4: the dispatcher dials the freshly provisioned environment as the
    // new principal, with no manual SQL between provisioning and the dial.
    let reader = connect_to(url, DATABASE, DISPATCH_READER_ROLE, READER_PASSWORD).await;
    let who: String = reader
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("the reader session works")
        .get(0);
    assert_eq!(who, DISPATCH_READER_ROLE);
    drop(reader);

    run_alone(su, &format!("DROP DATABASE \"{DATABASE}\" WITH (FORCE)")).await;
    drop_reader_role(su).await;
}

/// **A measured premise correction, kept as a test so it cannot rot.**
///
/// `provision_project_env::privilege_sql` emits its statements after
/// `ALTER DATABASE … OWNER TO`, and `grant_dispatch_reader_connect_sql`'s doc
/// says the order is "load-bearing exactly as it is for
/// `grant_connect_on_database_sql`". Measured on PostgreSQL, it is NOT
/// symmetric: `ALTER DATABASE … OWNER TO` rewrites only the OUTGOING OWNER's ACL
/// entry, so
///
/// * `wamn_app`, which owned the database, loses the `CONNECT` that had merged
///   into `wamn_app=CTc/wamn_app` — the hazard measured at `47b404cf`; while
/// * `wamn_dispatch_reader`, which never owns it, keeps `c` and only has its
///   GRANTOR rewritten (`reader=c/wamn_app` → `reader=c/wamn_db_owner`).
///
/// A reordering regression therefore breaks one side and leaves the dispatcher
/// working — a one-sided break a reader-only probe would miss. The asymmetry is
/// still measured on `wamn_app` because it is the only role that can be the
/// OUTGOING OWNER of a pre-`wamn-0h0g.12.108` environment; what changed at
/// wamn-0h0g.12.179 is the closing assertion, since the shipped batch now
/// REVOKES `wamn_app`'s `CONNECT` rather than putting it back.
async fn owner_statement_asymmetry_leg(su: &Client) {
    run_alone(
        su,
        &format!("DROP DATABASE IF EXISTS \"{ORDERING_DATABASE}\" WITH (FORCE)"),
    )
    .await;
    drop_reader_role(su).await;
    run_alone(su, &role_sql(APP_PASSWORD, READER_PASSWORD)).await;
    run_alone(
        su,
        &format!("CREATE DATABASE \"{ORDERING_DATABASE}\" OWNER \"{APP_ROLE}\""),
    )
    .await;

    // The MIS-ordered batch: both grants first, then the owner statement.
    run_alone(su, &sql::grant_connect_on_database_sql(ORDERING_DATABASE)).await;
    run_alone(
        su,
        &sql::grant_dispatch_reader_connect_sql(ORDERING_DATABASE),
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

    // And the shipped order restores the reader — and ONLY the reader. The
    // stable guest ACL role stays connection free through a batch applied to a
    // database it used to own, which is the pre-cutover environment
    // wamn-0h0g.12.179 has to converge.
    run_alone(su, &privilege_sql(ORDERING_DATABASE)).await;
    assert!(
        !can_connect(su, APP_ROLE, ORDERING_DATABASE).await,
        "the shipped batch re-granted the stable guest ACL role CONNECT"
    );
    assert!(can_connect(su, DISPATCH_READER_ROLE, ORDERING_DATABASE).await);

    // The other direction of the same contract: a pre-cutover environment where
    // the grant is ALREADY present must converge, not merely fail to re-add it.
    run_alone(su, &sql::grant_connect_on_database_sql(ORDERING_DATABASE)).await;
    assert!(can_connect(su, APP_ROLE, ORDERING_DATABASE).await);
    run_alone(su, &privilege_sql(ORDERING_DATABASE)).await;
    assert!(
        !can_connect(su, APP_ROLE, ORDERING_DATABASE).await,
        "the shipped batch must REVOKE a pre-cutover stable-role CONNECT, not \
         just stop granting one"
    );
    assert!(can_connect(su, DISPATCH_READER_ROLE, ORDERING_DATABASE).await);

    run_alone(
        su,
        &format!("DROP DATABASE \"{ORDERING_DATABASE}\" WITH (FORCE)"),
    )
    .await;
    drop_reader_role(su).await;
}
