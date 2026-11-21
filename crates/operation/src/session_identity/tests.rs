use crate::invocation::{AuthorizedInvocation, InvocationRequest};
use crate::scope_registry;
use crate::transactions::validate_session_identity;
use crate::{OperationDatabase, OperationHost, PgError, SessionIdentity, SqlValue};
use std::sync::Arc;
use wamn_authentication::authorization::{
    Authorization, AuthorizationStore as _, UnprovisionedPrincipal, require_provisioned_principal,
};
use wamn_identity_protocol::AuthorityClass;
use wamn_postgres::{
    ClassCredentials, CredentialProvider, DEFAULT_PROJECT, ResolvedCredential,
    StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_test_infrastructure::containers::{Postgres, start_postgres};

#[test]
fn validate_identity_rejects_malformed_identities() {
    assert!(
        validate_session_identity(
            Some("public"),
            Some("owner-1"),
            Some("inspector"),
            Some(&"platform-fixture:widget/update@1.0.0".parse().unwrap())
        )
        .is_ok()
    );
    assert!(validate_session_identity(None, None, None, None).is_ok());
    assert!(validate_session_identity(Some("has-hyphen"), None, None, None).is_err());
    assert!(validate_session_identity(None, Some("bad;runner"), None, None).is_err());
    // `''` is the base denial policy, never an identity.
    assert!(validate_session_identity(None, None, Some(""), None).is_err());
}

// ------------------------------------------------------------------
// Live-PG tests. Each test starts its own PostgreSQL server.
// ------------------------------------------------------------------

/// The executing principal a live transactional statement binds.
pub(crate) const LIVE_PRINCIPAL: &str = "5c2b7e1a-9d3f-4a6b-8c1e-2f4d6a8b0c3e";

/// Ensure the stable guest ACL role exists for the generation logins.
const ENSURE_GUEST_ACL_ROLE_SQL: &str = "DO $acl$ BEGIN \
           BEGIN CREATE ROLE wamn_app NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS; \
           EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
         END $acl$;";

/// The platform principal id `wamn-identity-protocol` derives for `wamn:executor`.
const LIVE_PLATFORM_PRINCIPAL: &str = "d318d033-29ea-5cb0-ab56-24340413fbcc";

/// Install the product identity schema and seed the principals that own rows.
pub(crate) async fn ensure_live_user_rows(admin_url: &str, principals: &[&str]) {
    let mut admin = connect_raw(admin_url).await;
    for sql in [
        ENSURE_GUEST_ACL_ROLE_SQL,
        wamn_provision::sql::ensure_project_roles_sql(),
        wamn_provision::CATALOG_SCHEMA_SQL,
        wamn_provision::APP_SCHEMA_SQL,
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut admin)
            .await
            .unwrap();
    }
    sqlx::raw_sql("BEGIN").execute(&mut admin).await.unwrap();
    sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES ($1::text::uuid,'','fixture','admin:identity-fixture')")
        .bind(LIVE_PRINCIPAL).execute(&mut admin).await.unwrap();
    // The app schema seeds the platform principals, `wamn:executor` among them.
    for principal in principals.iter().filter(|principal| **principal != LIVE_PLATFORM_PRINCIPAL) {
        sqlx::query("INSERT INTO platform.\"user\" (id,issuer,sub,type,email) VALUES ($1::text::uuid,'https://fixture.example',$1,'user',$1 || '@example.invalid')")
            .bind(principal)
            .execute(&mut admin).await.unwrap();
    }
    sqlx::raw_sql("COMMIT").execute(&mut admin).await.unwrap();
}

/// Start a private server and return it with the administrator URL of its test database.
pub(crate) async fn live_server() -> (Postgres, String) {
    let server = start_postgres()
        .await
        .expect("start the test PostgreSQL server");
    let database = server
        .create_database("operation")
        .await
        .expect("create the test database");
    (server, database.into())
}

/// Admit one call's SQL identity the way the operation entry admits it.
/// Dropping the returned owner ends the call and revokes its scope.
pub(crate) fn enter(
    pg: &OperationHost,
    scope: &str,
    identity: SessionIdentity,
) -> Arc<AuthorizedInvocation> {
    AuthorizedInvocation::enter(
        &pg.scopes,
        InvocationRequest {
            scope: scope.to_owned(),
            identity,
            context: None,
            caller: None,
            registered_operation: None,
            causation: None,
            invocation: None,
            read: false,
        },
    )
    .expect("the operation entry admits the identity")
}

/// The guest and query logins of one project, each named for its own class as
/// the host names a project's mounted logins.
pub(crate) fn project_credentials(guest_url: &str) -> ClassCredentials {
    let mut query = url::Url::parse(guest_url).expect("parse the guest url");
    let login = query.username().replacen("wamn_app_", "wamn_query_", 1);
    query.set_username(&login).expect("set the query login");
    ClassCredentials::default()
        .with_class(AuthorityClass::GuestSql, guest_url)
        .with_class(AuthorityClass::DynamicSql, query)
}

fn live_database(admin_url: &str) -> String {
    url::Url::parse(admin_url)
        .expect("parse the live test url")
        .path()
        .trim_start_matches('/')
        .to_string()
}

/// Rewrite a disposable-database URL onto a PROPERLY NAMED guest generation,
/// creating the login if it is missing.
///
/// `wamn-0h0g.22.6.7` binds a guest credential to its project_env_key: resolution
/// verifies that the login carries `app_identity_hash(project_env_key, database)`, so a
/// URL naming an arbitrary user no longer resolves for the guest class.
/// That is the point — a shared login is exactly what item 2 retires — and
/// it means a live guest test has to authenticate as a real generation.
pub(crate) async fn live_guest_url(
    admin_url: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> String {
    let mut url = url::Url::parse(admin_url).expect("parse the live test url");
    let mut database = live_database(admin_url);
    let role = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(project_env_key, &mut database)
    );
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        format!(
            // The tests in this module run in PARALLEL against one cluster
            // and roles are cluster-wide, so IF NOT EXISTS races: two
            // sessions both see the role absent and both create it. The
            // exception guard is what makes the create idempotent under
            // concurrency, not the existence test.
            "{ENSURE_GUEST_ACL_ROLE_SQL} \
                 DO $$ BEGIN \
                   BEGIN \
                     CREATE ROLE \"{role}\" LOGIN PASSWORD 'live-guest'; \
                   EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; \
                   END; \
                 END $$; \
                 GRANT wamn_app TO \"{role}\";"
        ),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("ensure the live guest generation");
    sqlx::raw_sql(sqlx::AssertSqlSafe(query_fixture_sql(&role, "live-guest")))
        .execute(&mut admin)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../../../deploy/sql/record-history.sql"))
        .execute(&mut admin)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::sql::grant_dynamic_sql_surface_sql(),
    ))
    .execute(&mut admin)
    .await
    .unwrap();
    url.set_username(&role).expect("set the guest login");
    url.set_password(Some("live-guest"))
        .expect("set the password");
    url.to_string()
}

async fn connect_raw(url: &str) -> sqlx::PgConnection {
    let client = wamn_postgres::connect(url).await.unwrap();

    client
}

async fn transaction_rows(
    transaction: &crate::OperationTransaction,
    sql: &str,
) -> Result<Vec<Vec<SqlValue>>, PgError> {
    let rows = transaction
        .with_host_connection(|mut connection| async move {
            let result = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
                .fetch_all(connection.connection())
                .await;
            (connection, result)
        })
        .await?;
    rows.iter().map(wamn_postgres::types::decode_row).collect()
}

async fn run_operation(
    pg: &OperationHost,
    scope: &str,
    sql: &str,
) -> Result<Vec<Vec<SqlValue>>, PgError> {
    let transaction = pg.begin_operation_transaction(scope).await?;
    let result = transaction_rows(&transaction, sql).await;
    transaction.rollback().await?;
    result
}

/// Rows a component sees in its host operation transaction.
async fn visible_rows(pg: &OperationHost, component: &str, sql: &str) -> usize {
    match run_operation(pg, component, sql).await {
        Ok(rows) => rows.len(),
        Err(e) => panic!("operation SQL {sql:?} failed: {e:?}"),
    }
}

/// A provider that RECORDS which authority class each resolution was asked
/// for, and names no credential for any of them.
///
/// `Ok(None)` is what keeps the test hermetic: `ensure_pool` refuses on the
/// spot, so the class under test is observed with no pool built and no socket
/// opened.
#[derive(Default)]
struct RecordingProvider {
    asked: std::sync::Mutex<Vec<AuthorityClass>>,
}

impl CredentialProvider for RecordingProvider {
    fn resolve(
        &self,
        _project: &wamn_registry::ProjectId,
        class: AuthorityClass,
        _project_env_key: Option<&wamn_registry::ProjectEnvKey>,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        self.asked
            .lock()
            .expect("recording provider lock poisoned")
            .push(class);
        Ok(None)
    }
}

#[tokio::test]
async fn user_operation_permissions_reuse_only_the_callable_http_authority() {
    let provider = Arc::new(RecordingProvider::default());
    let pools = Arc::new(WamnPostgres::with_provider(
        Arc::clone(&provider) as Arc<dyn CredentialProvider>,
    ));
    let database = Arc::new(OperationDatabase::new(Arc::clone(&pools)));
    let authorization = Authorization::new(pools, database);
    let application = wamn_engine::application_config::ApplicationConfig {
        org: "org".parse().unwrap(),
        project: (*DEFAULT_PROJECT).clone(),
        environment: "a".parse().unwrap(),
        application: "app".parse().unwrap(),
        grant_cache_seconds: 1,
        components: Vec::new(),
    };
    authorization.configure_application(&application).unwrap();
    let project_env_key = wamn_registry::project_env_key(
        &application.org,
        &application.project,
        &application.environment,
    );
    let principal_id = "00000000-0000-0000-0000-000000000019"
        .parse()
        .expect("user UUID");

    authorization.user_operation_permissions(
        &DEFAULT_PROJECT,
        &project_env_key,
        &principal_id,
    )
    .await
    .expect_err("a provider that names no credential resolves nothing");

    let asked = provider
        .asked
        .lock()
        .expect("recording provider lock poisoned")
        .clone();
    assert_eq!(asked, vec![AuthorityClass::Platform]);
}

/// Project database rows used to verify refusal of another project's credential.
fn credential_refusal_fixture_sql(schema: &str, role_a: &str) -> String {
    let query = query_fixture_sql(role_a, "live-guest");
    let identity = include_str!("../../../../deploy/sql/record-history.sql");
    format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role_a}') THEN \
                 DROP OWNED BY {role_a}; DROP ROLE {role_a}; END IF; \
             END $reset$; \
             {ENSURE_GUEST_ACL_ROLE_SQL} {identity} \
             CREATE ROLE {role_a} LOGIN PASSWORD 'live-guest' NOSUPERUSER NOBYPASSRLS; \
             GRANT wamn_app TO {role_a}; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions (id int NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_identity ON {schema}.dispositions \
                 USING ((SELECT wamn_authority.current_role()) IS NOT NULL); \
             INSERT INTO {schema}.dispositions VALUES (1), (2); \
             GRANT USAGE ON SCHEMA {schema} TO {role_a}; \
             GRANT SELECT ON {schema}.dispositions TO {role_a}; {query} {dynamic}",
        dynamic = wamn_provision::sql::grant_dynamic_sql_surface_sql(),
    )
}

/// A host with one project's credential refuses another project before reading rows.
#[tokio::test]
#[ignore = "requires: docker"]
async fn live_a_second_project_env_key_is_refused_rather_than_served_the_first_project_env_keys_rows()
 {
    const PROJECT_ENV_KEY_A: &str = "test--dev--seam-live-a";
    const PROJECT_ENV_KEY_B: &str = "test--dev--seam-live-b";

    let (_server, test_database) = live_server().await;
    let admin_url = test_database.as_str();
    let suffix = std::process::id();
    let schema = format!("wamn_seam_{suffix}");
    let mut database = live_database(admin_url);
    // Named exactly as provisioning names a guest generation, so the
    // credential the host resolves is bound to PROJECT_ENV_KEY_A by its own digest.
    let role_a = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(&PROJECT_ENV_KEY_A.parse().unwrap(), &mut database)
    );
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        credential_refusal_fixture_sql(&schema, &role_a),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("seed the credential refusal fixture as the superuser owner");

    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL]).await;
    let mut url = url::Url::parse(admin_url).expect("parse the live test url");
    url.set_username(&role_a).expect("set A's login");
    url.set_password(Some("live-guest"))
        .expect("set A's password");
    let pools = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(project_credentials(url.as_str())),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .unwrap();
    let pg = OperationHost::new(Arc::new(pools), None);

    let scope_a = "warm-instance-0";
    let scope_b = "warm-instance-1";
    let identity = |project_env_key: &wamn_registry::ProjectEnvKey| SessionIdentity {
        project_env_key: Some(project_env_key.clone()),
        schema: Some(schema.clone()),
        user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
        ..SessionIdentity::default()
    };
    let invocation_a = enter(&pg, scope_a, identity(&PROJECT_ENV_KEY_A.parse().unwrap()));
    let _invocation_b = enter(&pg, scope_b, identity(&PROJECT_ENV_KEY_B.parse().unwrap()));

    // CONTROL: the table is reachable on this path at all, so the refusal
    // below is a refusal and not a broken fixture or search_path.
    assert_eq!(
        visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
        2,
        "A sees exactly its own two rows through its own login"
    );

    // B's acquisition is legitimate; the HOST simply holds no credential for
    // it. That must refuse. Serving A's credential would hand B two rows
    // belonging to another project_env_key, which is the failure this design exists
    // to make impossible.
    let refused = run_operation(&pg, scope_b, "SELECT id FROM dispositions")
        .await
        .err()
        .expect("an project_env_key this host holds no credential for cannot query");
    assert!(
        matches!(&refused, PgError::ConnectionUnavailable),
        "a second project_env_key must be REFUSED at credential resolution, not served \
             the first project_env_key's connection: {refused:?}"
    );

    assert_eq!(
        visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
        2,
        "A's rows are unchanged by B's refused acquisition"
    );

    // Ending A's checkout revokes A's identity and nothing else. Matched on
    // the `WAMN0` code specifically: a revoke that cleared only the
    // search_path would also fail this query, for an unrelated reason.
    drop(invocation_a);
    assert_eq!(scope_registry::session_identity(&pg.scopes, scope_a), None);
    let unbound = run_operation(&pg, scope_a, "SELECT id FROM dispositions")
        .await
        .err()
        .expect("an unbound identity scope cannot query at all");
    assert!(
        matches!(&unbound, PgError::QueryError((code, _)) if code == "WAMN0"),
        "an instance whose checkout ended must resolve NO project_env_key, not merely \
             fail for some other reason: {unbound:?}"
    );
}

// ------------------------------------------------------------------
// wamn-0h0g.17.33 — the pipelined identity flight, and its test obligation.
// ------------------------------------------------------------------

/// The fixture a cold-parse test needs: a schema the session's own
/// `search_path` does NOT contain, holding the only relation the statement
/// names, readable by the guest generation.
fn cold_parse_fixture_sql(schema: &str, role: &str) -> String {
    format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.cold_parse (id int NOT NULL); \
             INSERT INTO {schema}.cold_parse VALUES (1), (2); \
             GRANT USAGE ON SCHEMA {schema} TO \"{role}\"; \
             GRANT SELECT ON {schema}.cold_parse TO \"{role}\";"
    )
}

/// The statement a cold-parse test runs: it names `cold_parse`
/// UNQUALIFIED, so Parse resolves it only under the bound `search_path`.
fn cold_parse_statement() -> &'static str {
    "SELECT id FROM cold_parse ORDER BY id"
}

/// *** A COLD CONNECTION STILL PARSES INSIDE THE IDENTITY TRANSACTION. ***
///
/// The regression guard for `wamn-0h0g.15.137.15` and the correctness half
/// of `wamn-0h0g.17.33`. The pool is brand new, so nothing on the physical
/// connection has been parsed; the statement names an unqualified relation
/// that exists ONLY in a schema outside the session's `search_path`. Parse
/// is where a relation name resolves, so this can succeed only if the
/// server ran the Parse after the transaction-LOCAL `search_path` the
/// identity install — that is, inside the identity transaction.
///
/// IT IS NOT RACY. On the pre-fix shape `begin_with_identity` opens by
/// awaiting its OWN `prepare_cached`, which on a cold connection is a
/// guaranteed round trip, and the statement half — polled during that await
/// — always sends its Parse first. Reverting to that shape, or swapping the
/// two branches of the `join!`, fails this every run.
#[tokio::test]
#[ignore = "requires: docker"]
async fn live_a_cold_connection_parses_inside_the_identity_transaction() {
    const PROJECT_ENV_KEY: &str = "test--dev--coldparse";
    let (_server, database) = live_server().await;
    let admin_url = database.as_str();
    let schema = format!("wamn_coldparse_{}", std::process::id());
    let role = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(
            &PROJECT_ENV_KEY.parse().unwrap(),
            &live_database(admin_url)
        )
    );
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL]).await;
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        cold_parse_fixture_sql(&schema, &role),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("seed the cold-parse fixture as the superuser owner");

    let pools = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(project_credentials(&guest_url)),
        // ONE connection, and a pool built in this test, so the checkout
        // below is guaranteed to be a NEW physical connection with an empty
        // statement cache.
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let pg = OperationHost::new(Arc::new(pools), None);
    let scope = "coldparse-instance-0";
    let _invocation = enter(
        &pg,
        scope,
        SessionIdentity {
            project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
            schema: Some(schema.clone()),
            user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
            ..SessionIdentity::default()
        },
    );

    let statement = cold_parse_statement();
    let rows = run_operation(&pg, scope, statement).await.expect(
        "a COLD connection must parse the statement inside the identity transaction: a \
                 `relation \"cold_parse\" does not exist` here is the statement reaching the \
                 server ahead of the identity",
    );
    assert_eq!(rows.len(), 2, "the bound search_path resolved the relation");

    // And the same connection, now WARM, still runs it: the flight that
    // saves the round trip is the one this second call takes.
    let again = run_operation(&pg, scope, statement)
        .await
        .expect("the warm connection runs the pipelined flight");
    assert_eq!(again.len(), 2);
}

/// The principal check refuses a principal that owns no `platform."user"` row
/// in the project_env_key, and the refusal names the cause (`wamn-0h0g.9.19`).
///
/// The minimum is installed and the row is left out, so the refusal is a
/// refusal and not a missing table or an unreachable database. The control
/// half is the same check admitting a principal that owns a row, which proves
/// a guard that refused everything would not pass this test.
///
/// The platform principal is the third case: `wamn:executor` carries its own
/// row, which provisioning writes, so it passes like any other principal.
#[tokio::test]
#[ignore = "requires: docker"]
async fn live_a_principal_with_no_user_row_is_refused() {
    const PROJECT_ENV_KEY: &str = "test--dev--unprovisioned";
    const ABSENT_PRINCIPAL: &str = "11111111-2222-4333-8444-555555555555";
    let (_server, database) = live_server().await;
    let admin_url = database.as_str();
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL, LIVE_PLATFORM_PRINCIPAL]).await;
    let pools = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(project_credentials(&guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the pools build from the guest generation's url");
    let project_env_key: wamn_registry::ProjectEnvKey = PROJECT_ENV_KEY.parse().unwrap();
    let check = async |principal: &str| {
        require_provisioned_principal(
            &pools,
            &project_env_key,
            &DEFAULT_PROJECT,
            &principal.parse().unwrap(),
        )
        .await
    };

    let refused = check(ABSENT_PRINCIPAL)
        .await
        .expect_err("a principal with no user row is refused");
    let denial = refused
        .downcast_ref::<UnprovisionedPrincipal>()
        .expect("the refusal names the missing user row");
    assert_eq!(
        denial.principal_id(),
        &ABSENT_PRINCIPAL
            .parse::<wamn_identity_protocol::PrincipalId>()
            .unwrap()
    );
    assert_eq!(denial.project_env_key(), &project_env_key);
    assert_eq!(denial.project(), &*DEFAULT_PROJECT);

    // CONTROL: the provisioned principal passes.
    check(LIVE_PRINCIPAL)
        .await
        .expect("a provisioned principal passes");
    // And a platform principal is a principal like any other.
    check(LIVE_PLATFORM_PRINCIPAL)
        .await
        .expect("the platform principal's own row passes");
}

/// A statement that reports the transaction operation.
fn operation_statement() -> &'static str {
    "SELECT COALESCE((SELECT (wamn_authority.current_identity()).operation), '')"
}

/// Reads and writes record their operation. A later transaction inherits none.
#[tokio::test]
#[ignore = "requires: docker"]
async fn live_a_transaction_binds_the_executing_operation_and_a_pooled_connection_keeps_none() {
    const PROJECT_ENV_KEY: &str = "test--dev--operation";
    const OPERATION: &str = "platform-fixture:widget/update@1.0.0";
    let (_server, database) = live_server().await;
    let admin_url = database.as_str();
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL]).await;
    let pools = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(project_credentials(&guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let pg = OperationHost::new(Arc::new(pools), None);
    let scope = "operation-instance-1";
    let identity = SessionIdentity {
        project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
        user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
        operation: Some(OPERATION.parse().unwrap()),
        ..SessionIdentity::default()
    };
    let invocation = enter(&pg, scope, identity.clone());

    let bound = run_operation(&pg, scope, operation_statement())
        .await
        .expect("the transactional statement runs");
    assert!(
        matches!(&bound[0][0], SqlValue::Text(value) if value == OPERATION),
        "the transaction carries the executing operation: {:?}",
        bound
    );
    let read = run_operation(&pg, scope, operation_statement())
        .await
        .expect("the read runs");
    assert!(
        matches!(&read[0][0], SqlValue::Text(value) if value == OPERATION),
        "the read carries the executing operation: {:?}",
        read
    );

    drop(invocation);
    let _next = enter(
        &pg,
        scope,
        SessionIdentity {
            operation: None,
            ..identity
        },
    );
    let next = run_operation(&pg, scope, operation_statement())
        .await
        .expect("the next transactional statement runs");
    assert!(
        matches!(&next[0][0], SqlValue::Text(value) if value.is_empty()),
        "the next transaction carries no previous operation: {:?}",
        next
    );
    assert_eq!(
        pg.pools.destroyed_connections().load(std::sync::atomic::Ordering::Relaxed),
        0,
        "every statement reused the one pooled connection"
    );
}

/// A statement that reports the transaction application.
fn application_statement() -> &'static str {
    "SELECT COALESCE((SELECT wamn_authority.current_application()), '')"
}

/// The identity half of the cross-application refusal
/// (docs/architecture/data-access.md#rendering).
///
/// Two applications share the one pooled connection. Each transaction carries
/// the application its host bound and never the other's: not the previous
/// transaction's, not a session-level value left on the connection, and not a
/// value that guest SQL names, which the identity guard refuses.
#[tokio::test]
#[ignore = "requires: docker"]
async fn live_a_transaction_carries_only_the_application_its_host_bound() {
    const PROJECT_ENV_KEY: &str = "test--dev--application";
    const APPLICATION_A: &str = "platform_fixture";
    const APPLICATION_B: &str = "platform_fixture_overlay";
    let (_server, database) = live_server().await;
    let admin_url = database.as_str();
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL]).await;
    let pools = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(project_credentials(&guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let pg = OperationHost::new(Arc::new(pools), None);
    let identity = |application: &wamn_registry::ApplicationName| SessionIdentity {
        project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
        user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
        application: Some(application.clone()),
        ..SessionIdentity::default()
    };
    let (scope_a, scope_b) = ("application-instance-a", "application-instance-b");
    let invocation_a = enter(&pg, scope_a, identity(&APPLICATION_A.parse().unwrap()));
    let _invocation_b = enter(&pg, scope_b, identity(&APPLICATION_B.parse().unwrap()));
    let carried = async |scope: &str| {
        let result = run_operation(&pg, scope, application_statement())
            .await
            .expect("the transactional statement runs");
        match &result[0][0] {
            SqlValue::Text(value) => value.clone(),
            other => panic!("the application reads as text: {other:?}"),
        }
    };

    assert_eq!(carried(scope_a).await, APPLICATION_A);
    assert_eq!(carried(scope_b).await, APPLICATION_B);
    assert_eq!(
        carried(scope_a).await,
        APPLICATION_A,
        "B's transaction on the shared connection does not reach A's"
    );

    drop(invocation_a);
    let _next = enter(
        &pg,
        scope_a,
        SessionIdentity {
            application: None,
            ..identity(&APPLICATION_A.parse().unwrap())
        },
    );
    assert_eq!(
        carried(scope_a).await,
        "",
        "an acquisition with no application carries none"
    );
    assert_eq!(
        pg.pools.destroyed_connections().load(std::sync::atomic::Ordering::Relaxed),
        0,
        "every statement reused the one pooled connection"
    );
}

#[tokio::test]
async fn workflow_caller_retains_typed_connection_unavailable() -> anyhow::Result<()> {
    let pools = WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(
        std::collections::HashMap::new(),
        None,
    )));
    let postgres = OperationHost::new(Arc::new(pools), None);
    let error = postgres
        .workflow_node_caller(
            &"default".parse().unwrap(),
            &"org--dev--a".parse().unwrap(),
            &"00000000-0000-4000-8000-000000000081"
                .parse::<wamn_identity_protocol::PrincipalId>()
                .unwrap(),
            "reader",
        )
        .await
        .expect_err("an unavailable credential refuses the caller lookup");
    assert!(matches!(
        error.downcast_ref::<PgError>(),
        Some(PgError::ConnectionUnavailable)
    ));
    Ok(())
}

/// Seed the SELECT-only role for raw SQL interface tests.
fn query_fixture_sql(guest: &str, password: &str) -> String {
    use postgres_protocol::escape::{escape_identifier, escape_literal};
    let query = guest.replacen("wamn_app_", "wamn_query_", 1);
    format!(
        "DO $fixture$ BEGIN BEGIN CREATE ROLE wamn_query NOLOGIN NOINHERIT; EXCEPTION WHEN duplicate_object THEN NULL; END; BEGIN CREATE ROLE {role} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD {password}; EXCEPTION WHEN duplicate_object THEN NULL; END; END $fixture$; GRANT wamn_query TO {role}; ALTER DEFAULT PRIVILEGES GRANT SELECT ON TABLES TO wamn_query; ALTER DEFAULT PRIVILEGES GRANT USAGE ON SCHEMAS TO wamn_query; DO $read$ DECLARE schema_name text; BEGIN FOR schema_name IN SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' AND nspname <> 'information_schema' LOOP EXECUTE format('GRANT USAGE ON SCHEMA %I TO wamn_query',schema_name); EXECUTE format('GRANT SELECT ON ALL TABLES IN SCHEMA %I TO wamn_query',schema_name); END LOOP; END $read$;",
        role = escape_identifier(&query),
        password = escape_literal(password)
    )
}
