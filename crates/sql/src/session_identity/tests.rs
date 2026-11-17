use super::super::SqlValue;
use super::super::pool::ResolvedCredential;
use super::*;
use std::collections::BTreeSet;

/// Guest-sql must never be usable as a platform authority, and the refusal
/// must survive `--release`, where a `debug_assert` would not exist.
#[tokio::test]
async fn a_platform_checkout_refuses_the_guest_authority() {
    let postgres = WamnPostgres::from_env(Some(ClassCredentials::every_class(
        "postgres://wamn_app_refusal@localhost/refusal-test",
    )))
    .expect("compose");
    let refused = postgres
        .checkout_platform(&DEFAULT_PROJECT, AuthorityClass::GuestSql)
        .await;
    assert!(
        refused.is_err(),
        "platform work under the guest authority must fail closed"
    );
}

/// REGRESSION GUARD (`wamn-0h0g.22.8.3`).
///
/// Removing the ambient read in split B silently cut the HOST's default
/// project, because `from_env` derived it from the config field that split
/// had just forced to `None`. Nothing in the sweep exercised that path, so
/// the only thing standing between that and a deployed host with no
/// database is this test. It asserts the composed credential actually
/// reaches resolution, and that composing without one resolves NOTHING
/// rather than falling back.
/// A guest credential url for `project_env_key` in `database`, named the way
/// provisioning names one: the login carries the project_env_key as its scope
/// digest (`wamn-0h0g.22.6.4`).
fn guest_url(project_env_key: &wamn_registry::ProjectEnvKey, database: &str) -> String {
    format!(
        "postgres://wamn_app_{}:pw@db/{database}",
        wamn_registry::app_identity_hash(project_env_key, database)
    )
}

/// *** THE REFUSAL THAT MAKES THE CREDENTIAL THE AUTHORITY. ***
///
/// After `wamn-0h0g.22.6` a guest's project_env_key is its LOGIN, so handing back a
/// credential minted for another project_env_key is not a mis-selection, it is a
/// cross-project_env_key read. Resolution refuses instead, and refuses again when the
/// caller names no project_env_key at all.
#[test]
fn a_guest_credential_resolves_for_its_own_project_env_key_and_no_other() {
    let host = WamnPostgres::from_env(Some(ClassCredentials::every_class(guest_url(
        &"test--dev--fixture".parse().unwrap(),
        "host-default",
    ))))
    .expect("compose with a credential");
    assert!(
        host.provider
            .resolve(
                &DEFAULT_PROJECT,
                AuthorityClass::GuestSql,
                Some(&"test--dev--fixture".parse().unwrap())
            )
            .expect("the credential's own project_env_key resolves")
            .is_some()
    );
    assert!(
        host.provider
            .resolve(
                &DEFAULT_PROJECT,
                AuthorityClass::GuestSql,
                Some(&"evil--dev--project".parse().unwrap())
            )
            .is_err(),
        "a credential minted for another project_env_key must be REFUSED, not borrowed"
    );
    assert!(
        host.provider
            .resolve(&DEFAULT_PROJECT, AuthorityClass::GuestSql, None)
            .is_err(),
        "guest resolution without an project_env_key has no authority to validate"
    );
    // Platform classes are project-environment scoped, so the project_env_key is not
    // part of their binding and its absence is not an error.
    assert!(
        host.provider
            .resolve(&DEFAULT_PROJECT, AuthorityClass::Platform, None)
            .expect("platform resolution needs no project_env_key")
            .is_some()
    );
}

#[test]
fn the_composed_credential_becomes_the_default_project() {
    let composed = WamnPostgres::from_env(Some(ClassCredentials::every_class(guest_url(
        &"test--dev--fixture".parse().unwrap(),
        "host-default",
    ))))
    .expect("compose with a credential");
    assert!(
        composed
            .provider
            .resolve(
                &DEFAULT_PROJECT,
                AuthorityClass::GuestSql,
                Some(&"test--dev--fixture".parse().unwrap())
            )
            .expect("resolve default")
            .is_some(),
        "a host composed WITH a credential must resolve the default project; \
             deploy/platform injects it via secretKeyRef and a host that cannot \
             resolve it has no database at all"
    );

    let bare = WamnPostgres::from_env(None).expect("compose without a credential");
    assert!(
        bare.provider
            .resolve(
                &DEFAULT_PROJECT,
                AuthorityClass::GuestSql,
                Some(&"test--dev--fixture".parse().unwrap())
            )
            .expect("resolve default")
            .is_none(),
        "composing without a credential must resolve nothing, not reach for \
             an ambient one"
    );
}

#[test]
fn a_serving_host_binds_its_credential_to_the_declared_project() {
    let executor_platform_url = "postgres://executor-platform@db/host-fixture";
    let composed = WamnPostgres::from_env_for_project(
        &"fixture".parse().unwrap(),
        Some(
            ClassCredentials::default().with_class(AuthorityClass::Platform, executor_platform_url),
        ),
    )
    .expect("compose the declared project credential");
    let resolved = composed
        .provider
        .resolve(&"fixture".parse().unwrap(), AuthorityClass::Platform, None)
        .expect("resolve the declared project")
        .expect("the declared project has an executor-platform credential");
    assert_eq!(
        resolved.database_url, executor_platform_url,
        "the host's exact credential must resolve under its trusted project"
    );
    assert!(
        composed
            .provider
            .resolve(&DEFAULT_PROJECT, AuthorityClass::Platform, None)
            .expect("resolve the unrelated default project")
            .is_none(),
        "binding a named project must not mint a default-project alias"
    );
}

// The identity-time record must match `runs_release_record_check` exactly, or a
// identity that would otherwise succeed dies on a CHECK inside the lease grant.
// The hand-rolled shape test this used to exercise is retired: the invariant
// now rides `wamn_digest::Digest`, so what is pinned here is that the TYPE admits
// exactly what the run-plane CHECK admits. Same coupling, one owner.
// wamn-cjv.2 — the in-band identity/role mutation guard.
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

#[test]
fn set_and_clear_current_run_is_per_component() {
    let pg = WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::default_only(None)));
    assert!(pg.current_run_for("c1").is_none());
    pg.set_current_run(
        "c1",
        Some(Causation {
            run: "r1".into(),
            root: "r1".into(),
            depth: 0,
        }),
    );
    assert_eq!(pg.current_run_for("c1").unwrap().run, "r1");
    // a second component is independent.
    assert!(pg.current_run_for("c2").is_none());
    // None clears it.
    pg.set_current_run("c1", None);
    assert!(pg.current_run_for("c1").is_none());
}

// ------------------------------------------------------------------
// Live-PG tests. Each test owns a database of the test PostgreSQL server
// and creates + drops its own objects.
// ------------------------------------------------------------------

/// The project_env_key every live guest checkout in this module authenticates for.
const LIVE_PROJECT_ENV_KEY: &str = "test--dev--fixture";

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
        wamn_catalog::CATALOG_SCHEMA_SQL,
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
    for principal in principals {
        sqlx::query("INSERT INTO platform.\"user\" (id,type,email,display_name) VALUES ($1::text::uuid,$2,$1 || '@example.invalid',$3)")
            .bind(principal)
            .bind(if *principal == LIVE_PLATFORM_PRINCIPAL { "platform" } else { "user" })
            .bind((*principal == LIVE_PLATFORM_PRINCIPAL).then_some("wamn:executor"))
            .execute(&mut admin).await.unwrap();
    }
    sqlx::raw_sql("COMMIT").execute(&mut admin).await.unwrap();
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
        (&format!(
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
        ))
            .to_string(),
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

fn database_url_for_role(database_url: &str, role: &str, password: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("database URL is an absolute URI");
    url.set_username(role)
        .expect("PostgreSQL URL accepts a username");
    url.set_password(Some(password))
        .expect("PostgreSQL URL accepts a password");
    url.to_string()
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
    rows.iter().map(super::super::types::decode_row).collect()
}

async fn run_operation(
    pg: &WamnPostgres,
    scope: &str,
    sql: &str,
) -> Result<Vec<Vec<SqlValue>>, PgError> {
    let transaction = pg.begin_operation_transaction(scope).await?;
    let result = transaction_rows(&transaction, sql).await;
    transaction.rollback().await?;
    result
}

/// Rows a component sees in its host operation transaction.
async fn visible_rows(pg: &WamnPostgres, component: &str, sql: &str) -> usize {
    match run_operation(pg, component, sql).await {
        Ok(rows) => rows.len(),
        Err(e) => panic!("operation SQL {sql:?} failed: {e:?}"),
    }
}

/// Protected caller identity and row ownership over a table the probe role
/// does not own, so RLS applies to it.
fn rls_fixture_sql(schema: &str, probe: &str, u1: &str, u2: &str) -> String {
    let query = query_fixture_sql(probe, probe);
    let identity = include_str!("../../../../deploy/sql/record-history.sql");
    format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DROP ROLE IF EXISTS {probe}; \
             {ENSURE_GUEST_ACL_ROLE_SQL} {identity} \
             CREATE ROLE {probe} LOGIN PASSWORD '{probe}' NOSUPERUSER NOBYPASSRLS; \
             GRANT wamn_app TO {probe}; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions ( \
                 id int NOT NULL, inspector_id uuid NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_identity ON {schema}.dispositions \
                 USING ((SELECT wamn_authority.current_role()) IS NOT NULL); \
             CREATE POLICY \"dispositions_owner_0\" ON {schema}.dispositions AS RESTRICTIVE \
                 FOR ALL \
                 USING (COALESCE((SELECT wamn_authority.current_role()), '') IN ('supervisor', 'admin') \
                        OR \"inspector_id\" = NULLIF((SELECT wamn_authority.current_principal_id())::text, '')::uuid); \
             INSERT INTO {schema}.dispositions VALUES (1, '{u1}'), (2, '{u2}'); \
             GRANT USAGE ON SCHEMA {schema} TO {probe}; \
             GRANT SELECT ON {schema}.dispositions TO {probe}; {query}"
    )
}

// Exercise the compiled ownership policy through the host transaction as a
// NOSUPERUSER NOBYPASSRLS login.
#[sqlx::test(migrations = false)]
async fn live_compiled_per_user_policy_permits_the_injected_caller(pool: sqlx::PgPool) {
    const PROJECT_ENV_KEY: &str = "test--dev--rls-identity-live";
    const COMPONENT: &str = "rls-identity-live-component";
    const U1: &str = "11111111-1111-4111-8111-111111111111";
    const U2: &str = "22222222-2222-4222-8222-222222222222";

    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let admin_url = database.as_str();
    let suffix = std::process::id();
    let schema = format!("wamn_rls_identity_{suffix}");
    // The probe login is NAMED AS A GUEST GENERATION for this test's project_env_key:
    // guest credential resolution verifies that the login carries
    // `app_identity_hash(project_env_key, database)` (`wamn-0h0g.22.6.7`), so a probe
    // with an arbitrary name would be refused before the policy under test
    // ever ran. The two project_env_keys differ per test, so the derived logins do
    // too and the tests stay parallel-safe.
    let probe = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(
            &PROJECT_ENV_KEY.parse().unwrap(),
            &live_database(admin_url)
        )
    );
    let _ = suffix;
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&rls_fixture_sql(&schema, &probe, U1, U2)).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("seed the per-user RLS fixture as the superuser owner");

    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(database_url_for_role(
            admin_url, &probe, &probe,
        ))),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .unwrap();
    pg.set_project_env_key(COMPONENT, &PROJECT_ENV_KEY.parse().unwrap());
    pg.set_schema(COMPONENT, &schema).unwrap();
    pg.set_role(COMPONENT, "inspector").unwrap();
    pg.set_user_id(COMPONENT, &U1.parse().unwrap()).unwrap();

    // CONTROL: the table is REACHABLE on this path — an aggregate returns its
    // one row however many rows RLS filtered away, and a wrong search_path or
    // a missing GRANT would raise instead. So a zero below is a policy
    // denying, not a broken fixture.
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT count(*) FROM dispositions").await,
        1,
        "control: the probe role must reach the fixture table"
    );

    // An inspector sees ONLY the row it owns.
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions WHERE id = 1").await,
        1,
        "the protected caller identity must permit the caller's OWN row"
    );
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions WHERE id = 2").await,
        0,
        "the ownership rule must still deny another user's row"
    );
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
        1
    );

    // …and an exempt role sees both, through the protected caller role.
    pg.set_role(COMPONENT, "admin").unwrap();
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
        2,
        "the protected caller role must satisfy the exempt-role condition"
    );

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!("DROP SCHEMA {schema} CASCADE; DROP OWNED BY {probe}; DROP ROLE {probe};"))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the per-user RLS fixture");
}

// Guest session settings cannot replace the protected transaction caller.
#[sqlx::test(migrations = false)]
async fn live_guest_cannot_override_the_injected_role_or_user_identity(pool: sqlx::PgPool) {
    const PROJECT_ENV_KEY: &str = "test--dev--rls-override-live";
    const COMPONENT: &str = "rls-override-live-component";
    const U1: &str = "11111111-1111-4111-8111-111111111111";
    const U2: &str = "22222222-2222-4222-8222-222222222222";

    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let admin_url = database.as_str();
    let suffix = std::process::id();
    let schema = format!("wamn_rls_override_{suffix}");
    // The probe login is NAMED AS A GUEST GENERATION for this test's project_env_key:
    // guest credential resolution verifies that the login carries
    // `app_identity_hash(project_env_key, database)` (`wamn-0h0g.22.6.7`), so a probe
    // with an arbitrary name would be refused before the policy under test
    // ever ran. The two project_env_keys differ per test, so the derived logins do
    // too and the tests stay parallel-safe.
    let probe = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(
            &PROJECT_ENV_KEY.parse().unwrap(),
            &live_database(admin_url)
        )
    );
    let _ = suffix;
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&rls_fixture_sql(&schema, &probe, U1, U2)).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("seed the per-user RLS fixture as the superuser owner");

    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(database_url_for_role(
            admin_url, &probe, &probe,
        ))),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .unwrap();
    pg.set_project_env_key(COMPONENT, &PROJECT_ENV_KEY.parse().unwrap());
    pg.set_schema(COMPONENT, &schema).unwrap();
    pg.set_role(COMPONENT, "inspector").unwrap();
    pg.set_user_id(COMPONENT, &U1.parse().unwrap()).unwrap();

    // The real transaction keeps its caller even when guest SQL sets a role.
    let (mut conn, _pp) = pg
        .checkout_guest(&DEFAULT_PROJECT, &PROJECT_ENV_KEY.parse().unwrap())
        .await
        .unwrap();
    pg.begin_with_identity(
        &mut conn,
        AuthorityClass::GuestSql,
        false,
        Some(&schema),
        None,
        Some("inspector"),
        Some(&U1.parse().unwrap()),
        None,
        None,
        None,
        5_000,
    )
    .await
    .unwrap();
    let owned: i64 = sqlx::query("SELECT count(*) FROM dispositions")
        .fetch_one(&mut *conn)
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        owned, 1,
        "control: the injected caller owns exactly one row"
    );
    sqlx::raw_sql("SET LOCAL app.role = 'admin'")
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .unwrap();
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(U2)
        .execute(&mut *conn)
        .await
        .unwrap();
    let escalated: i64 = sqlx::query("SELECT count(*) FROM dispositions")
        .fetch_one(&mut *conn)
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        escalated, 1,
        "a session setting cannot replace the transaction caller"
    );
    sqlx::raw_sql("ROLLBACK")
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .unwrap();

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!("DROP SCHEMA {schema} CASCADE; DROP OWNED BY {probe}; DROP ROLE {probe};"))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the per-user RLS fixture");
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
    let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);
    let principal_id = "00000000-0000-0000-0000-000000000019"
        .parse()
        .expect("user UUID");

    pg.user_operation_permissions(
        &DEFAULT_PROJECT,
        &"org--dev--a".parse().unwrap(),
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
#[sqlx::test(migrations = false)]
async fn live_a_second_project_env_key_is_refused_rather_than_served_the_first_project_env_keys_rows(
    pool: sqlx::PgPool,
) {
    const PROJECT_ENV_KEY_A: &str = "test--dev--seam-live-a";
    const PROJECT_ENV_KEY_B: &str = "test--dev--seam-live-b";

    let _lock = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
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
        (&credential_refusal_fixture_sql(&schema, &role_a)).to_string(),
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
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(url.to_string())),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .unwrap();

    let scope_a = "warm-instance-0";
    let scope_b = "warm-instance-1";
    let identity = |project_env_key: &wamn_registry::ProjectEnvKey| SessionIdentity {
        project_env_key: Some(project_env_key.clone()),
        schema: Some(schema.clone()),
        user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
        ..SessionIdentity::default()
    };
    pg.bind_session_identity(scope_a, &identity(&PROJECT_ENV_KEY_A.parse().unwrap()))
        .await
        .expect("project_env_key A's acquisition binds");
    let refused = pg
        .bind_session_identity(scope_b, &identity(&PROJECT_ENV_KEY_B.parse().unwrap()))
        .await
        .expect_err("B's principal read requires B's own credential");
    let refused = refused.downcast_ref::<SessionIdentityError>().unwrap();
    assert!(matches!(
        refused.type_,
        SessionIdentityErrorType::Infrastructure
    ));
    assert!(matches!(
        refused.source.downcast_ref::<PgError>(),
        Some(PgError::ConnectionUnavailable)
    ));

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
    pg.revoke_session_identity(scope_a);
    assert_eq!(pg.session_identity(scope_a), None);
    let unbound = run_operation(&pg, scope_a, "SELECT id FROM dispositions")
        .await
        .err()
        .expect("an unbound identity scope cannot query at all");
    assert!(
        matches!(&unbound, PgError::QueryError((code, _)) if code == "WAMN0"),
        "an instance whose checkout ended must resolve NO project_env_key, not merely \
             fail for some other reason: {unbound:?}"
    );

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP SCHEMA {schema} CASCADE; \
                 DROP OWNED BY {role_a}; DROP ROLE {role_a};"
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the fixture");
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
#[sqlx::test(migrations = false)]
async fn live_a_cold_connection_parses_inside_the_identity_transaction(pool: sqlx::PgPool) {
    const PROJECT_ENV_KEY: &str = "test--dev--coldparse";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
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
        (&cold_parse_fixture_sql(&schema, &role)).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("seed the cold-parse fixture as the superuser owner");

    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        // ONE connection, and a pool built in this test, so the checkout
        // below is guaranteed to be a NEW physical connection with an empty
        // statement cache.
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let scope = "coldparse-instance-0";
    pg.bind_session_identity(
        scope,
        &SessionIdentity {
            project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
            schema: Some(schema.clone()),
            user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
            ..SessionIdentity::default()
        },
    )
    .await
    .expect("the cold-parse scope binds");

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

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!("DROP SCHEMA {schema} CASCADE; DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the fixture");
}

/// A statement that reports the transaction caller.
fn principal_statement() -> &'static str {
    "SELECT COALESCE((SELECT wamn_authority.current_principal_id())::text, '')"
}

/// Reads and writes both record their executing principal.
#[sqlx::test(migrations = false)]
async fn live_reads_and_writes_bind_the_executing_principal(pool: sqlx::PgPool) {
    const PROJECT_ENV_KEY: &str = "test--dev--principal";
    const PRINCIPAL_B: &str = "00000000-0000-0000-0000-000000000020";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let admin_url = database.as_str();
    let role = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(
            &PROJECT_ENV_KEY.parse().unwrap(),
            &live_database(admin_url)
        )
    );
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL, PRINCIPAL_B]).await;
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let scope = "principal-instance-0";
    pg.bind_session_identity(
        scope,
        &SessionIdentity {
            project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
            user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
            ..SessionIdentity::default()
        },
    )
    .await
    .expect("the principal scope binds");

    let bound = run_operation(&pg, scope, principal_statement())
        .await
        .expect("the transactional statement runs");
    assert!(
        matches!(&bound[0][0], SqlValue::Text(value) if value == LIVE_PRINCIPAL),
        "the transaction carries the executing principal: {:?}",
        bound
    );
    // The pool holds one connection, so the read reuses the connection whose
    // transaction-local identity ended at COMMIT.
    let read = run_operation(&pg, scope, principal_statement())
        .await
        .expect("the read runs");
    assert!(
        matches!(&read[0][0], SqlValue::Text(value) if value == LIVE_PRINCIPAL),
        "the read carries the executing principal: {:?}",
        read
    );

    // B borrows the same physical connection after A's transaction and read.
    let before = pg
        .probe_checkout(&PROJECT_ENV_KEY.parse().unwrap())
        .await
        .unwrap()
        .backend_pid;
    pg.revoke_session_identity(scope);
    pg.bind_session_identity(
        scope,
        &SessionIdentity {
            project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
            user_id: Some(PRINCIPAL_B.parse().unwrap()),
            ..SessionIdentity::default()
        },
    )
    .await
    .unwrap();
    let bound = run_operation(&pg, scope, principal_statement())
        .await
        .unwrap();
    assert!(matches!(&bound[0][0], SqlValue::Text(value) if value == PRINCIPAL_B));
    let read = run_operation(&pg, scope, principal_statement())
        .await
        .unwrap();
    assert!(matches!(&read[0][0], SqlValue::Text(value) if value == PRINCIPAL_B));
    assert_eq!(
        pg.probe_checkout(&PROJECT_ENV_KEY.parse().unwrap())
            .await
            .unwrap()
            .backend_pid,
        before
    );

    drop(pg);
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
    )))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the fixture");
}

/// The bind refuses a principal that owns no `platform."user"` row in the
/// project_env_key, and the refusal names the cause (`wamn-0h0g.9.19`).
///
/// The minimum is installed and the row is left out, so the refusal is a
/// refusal and not a missing table or an unreachable database. The control
/// half is the same plugin binding a principal that owns a row, which proves
/// a guard that refused everything would not pass this test.
///
/// The platform principal is the third case: `wamn:executor` carries its own
/// row, which provisioning writes, so it binds like any other principal.
#[sqlx::test(migrations = false)]
async fn live_a_bind_refuses_a_principal_with_no_user_row(pool: sqlx::PgPool) {
    const PROJECT_ENV_KEY: &str = "test--dev--unprovisioned";
    const ABSENT_PRINCIPAL: &str = "11111111-2222-4333-8444-555555555555";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let admin_url = database.as_str();
    let role = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(
            &PROJECT_ENV_KEY.parse().unwrap(),
            &live_database(admin_url)
        )
    );
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL, LIVE_PLATFORM_PRINCIPAL]).await;
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let scope = "unprovisioned-instance-0";
    let identity = |principal: wamn_identity_protocol::PrincipalId| SessionIdentity {
        project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
        user_id: Some(principal),
        ..SessionIdentity::default()
    };

    let refused = pg
        .bind_session_identity(scope, &identity(ABSENT_PRINCIPAL.parse().unwrap()))
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
    assert_eq!(denial.project_env_key(), &PROJECT_ENV_KEY.parse().unwrap());
    assert_eq!(denial.project(), &*DEFAULT_PROJECT);

    // CONTROL: the provisioned principal binds.
    pg.bind_session_identity(scope, &identity(LIVE_PRINCIPAL.parse().unwrap()))
        .await
        .expect("a provisioned principal binds");
    assert_eq!(pg.user_id_for(scope), Some(LIVE_PRINCIPAL.parse().unwrap()));
    pg.revoke_session_identity(scope);

    // And a platform principal is a principal like any other.
    pg.bind_session_identity(scope, &identity(LIVE_PLATFORM_PRINCIPAL.parse().unwrap()))
        .await
        .expect("the platform principal's own row binds");
    assert_eq!(
        pg.user_id_for(scope),
        Some(LIVE_PLATFORM_PRINCIPAL.parse().unwrap())
    );

    drop(pg);
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
    )))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the fixture");
}

/// A statement that reports the transaction operation.
fn operation_statement() -> &'static str {
    "SELECT COALESCE((SELECT (wamn_authority.current_identity()).operation), '')"
}

/// An acquisition binds its operation, reads it back, and clears it. A later
/// acquisition with no operation does not inherit the previous one.
///
/// The identity name no executing principal, so the bind stays offline: the
/// provisioning read of `wamn-0h0g.9.19` runs only for a bind that names one,
/// and this test owns the operation registry rather than that test.
#[tokio::test]
async fn an_acquisition_binds_its_operation_and_the_next_one_does_not_inherit_it() {
    const OPERATION: &str = "platform-fixture:widget/update@1.0.0";
    let pg = WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(
        HashMap::new(),
        None,
    )));
    let scope = "operation-instance-0";
    let identity = SessionIdentity {
        project_env_key: Some(LIVE_PROJECT_ENV_KEY.parse().unwrap()),
        operation: Some(OPERATION.parse().unwrap()),
        ..SessionIdentity::default()
    };
    pg.bind_session_identity(scope, &identity)
        .await
        .expect("the operation binds");
    assert_eq!(pg.session_identity(scope), Some(identity.clone()));

    pg.bind_session_identity(
        scope,
        &SessionIdentity {
            operation: None,
            ..identity.clone()
        },
    )
    .await
    .expect("the next acquisition binds");
    assert_eq!(pg.operation_for(scope), None);

    pg.bind_session_identity(scope, &identity)
        .await
        .expect("the operation binds again");
    pg.revoke_session_identity(scope);
    assert_eq!(pg.operation_for(scope), None);
}

/// Reads and writes record their operation. A later transaction inherits none.
#[sqlx::test(migrations = false)]
async fn live_a_transaction_binds_the_executing_operation_and_a_pooled_connection_keeps_none(
    pool: sqlx::PgPool,
) {
    const PROJECT_ENV_KEY: &str = "test--dev--operation";
    const OPERATION: &str = "platform-fixture:widget/update@1.0.0";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let admin_url = database.as_str();
    let role = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(
            &PROJECT_ENV_KEY.parse().unwrap(),
            &live_database(admin_url)
        )
    );
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL]).await;
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let scope = "operation-instance-1";
    let identity = SessionIdentity {
        project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
        user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
        operation: Some(OPERATION.parse().unwrap()),
        ..SessionIdentity::default()
    };
    pg.bind_session_identity(scope, &identity)
        .await
        .expect("the operation scope binds");

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

    pg.bind_session_identity(
        scope,
        &SessionIdentity {
            operation: None,
            ..identity
        },
    )
    .await
    .expect("the next acquisition binds");
    let next = run_operation(&pg, scope, operation_statement())
        .await
        .expect("the next transactional statement runs");
    assert!(
        matches!(&next[0][0], SqlValue::Text(value) if value.is_empty()),
        "the next transaction carries no previous operation: {:?}",
        next
    );
    assert_eq!(
        pg.destroyed.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "every statement reused the one pooled connection"
    );

    drop(pg);
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
    )))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the fixture");
}

/// A statement that reports the transaction application.
fn application_statement() -> &'static str {
    "SELECT COALESCE((SELECT wamn_authority.current_application()), '')"
}

/// An acquisition binds its application, reads it back, and clears it. A
/// later acquisition with no application does not inherit the previous one.
/// The identity name no executing principal, so the bind stays offline.
#[tokio::test]
async fn an_acquisition_binds_its_application_and_the_next_one_does_not_inherit_it() {
    let pg = WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(
        HashMap::new(),
        None,
    )));
    let scope = "application-instance-0";
    let identity = SessionIdentity {
        project_env_key: Some(LIVE_PROJECT_ENV_KEY.parse().unwrap()),
        application: Some("platform_fixture".parse().unwrap()),
        ..SessionIdentity::default()
    };
    pg.bind_session_identity(scope, &identity)
        .await
        .expect("the application binds");
    assert_eq!(pg.session_identity(scope), Some(identity.clone()));

    pg.bind_session_identity(
        scope,
        &SessionIdentity {
            application: None,
            ..identity.clone()
        },
    )
    .await
    .expect("the next acquisition binds");
    assert_eq!(pg.application_for(scope), None);

    pg.bind_session_identity(scope, &identity)
        .await
        .expect("the application binds again");
    pg.revoke_session_identity(scope);
    assert_eq!(pg.application_for(scope), None);
}

/// The identity half of the cross-application refusal
/// (docs/architecture/data-access.md#rendering).
///
/// Two applications share the one pooled connection. Each transaction carries
/// the application its host bound and never the other's: not the previous
/// transaction's, not a session-level value left on the connection, and not a
/// value that guest SQL names, which the identity guard refuses.
#[sqlx::test(migrations = false)]
async fn live_a_transaction_carries_only_the_application_its_host_bound(pool: sqlx::PgPool) {
    const PROJECT_ENV_KEY: &str = "test--dev--application";
    const APPLICATION_A: &str = "platform_fixture";
    const APPLICATION_B: &str = "platform_fixture_overlay";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let admin_url = database.as_str();
    let role = format!(
        "wamn_app_{}",
        wamn_registry::app_identity_hash(
            &PROJECT_ENV_KEY.parse().unwrap(),
            &live_database(admin_url)
        )
    );
    let guest_url = live_guest_url(admin_url, &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(admin_url, &[LIVE_PRINCIPAL]).await;
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let identity = |application: &wamn_catalog::ApplicationName| SessionIdentity {
        project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
        user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
        application: Some(application.clone()),
        ..SessionIdentity::default()
    };
    let (scope_a, scope_b) = ("application-instance-a", "application-instance-b");
    pg.bind_session_identity(scope_a, &identity(&APPLICATION_A.parse().unwrap()))
        .await
        .expect("application A binds");
    pg.bind_session_identity(scope_b, &identity(&APPLICATION_B.parse().unwrap()))
        .await
        .expect("application B binds");
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

    pg.bind_session_identity(
        scope_a,
        &SessionIdentity {
            application: None,
            ..identity(&APPLICATION_A.parse().unwrap())
        },
    )
    .await
    .expect("an acquisition with no application binds");
    assert_eq!(
        carried(scope_a).await,
        "",
        "an acquisition with no application carries none"
    );
    assert_eq!(
        pg.destroyed.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "every statement reused the one pooled connection"
    );

    drop(pg);
    let mut admin = connect_raw(admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
    )))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the fixture");
}

/// A host transaction of a read invocation reads one snapshot: a write that
/// commits between its two selects, as `receipt_screen` makes them, is not
/// visible to the second (`wamn-ruakx`). The same transaction of a command
/// sees the write, which proves the read case is not vacuous.
#[sqlx::test(migrations = false)]
async fn live_a_read_invocation_transaction_reads_one_snapshot(pool: sqlx::PgPool) {
    const PROJECT_ENV_KEY: &str = "test--dev--read-snapshot";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let admin_url = database.as_str();
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
        ("CREATE TABLE IF NOT EXISTS public.read_snapshot_fixture \
               (id integer PRIMARY KEY, value integer NOT NULL); \
             GRANT SELECT ON public.read_snapshot_fixture TO wamn_app;")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("install the snapshot fixture");
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let scope = "read-snapshot-instance-0";
    pg.bind_session_identity(
        scope,
        &SessionIdentity {
            project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
            user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
            ..SessionIdentity::default()
        },
    )
    .await
    .expect("the scope binds");
    // A route read carries its run, but must not write a causation message.
    pg.set_current_run(
        scope,
        Some(Causation {
            run: "read-snapshot-run".into(),
            root: "read-snapshot-run".into(),
            depth: 0,
        }),
    );
    let sql = "SELECT value FROM public.read_snapshot_fixture WHERE id = 1";

    for (read, second_value, isolation) in
        [(true, 1, "repeatable read"), (false, 2, "read committed")]
    {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("INSERT INTO public.read_snapshot_fixture VALUES (1, 1) \
                 ON CONFLICT (id) DO UPDATE SET value = 1")
                .to_string(),
        ))
        .execute(&mut admin)
        .await
        .map(drop)
        .expect("reset the fixture row");
        pg.scopes
            .update::<crate::invocation::Authority, _>(scope, |record| record.read = read);
        let transaction = pg.begin_operation_transaction(scope).await.unwrap();
        let level = transaction
            .with_host_connection(|mut connection| async move {
                let result = sqlx::query_scalar::<_, String>("SHOW transaction_isolation")
                    .fetch_one(connection.connection())
                    .await;
                (connection, result)
            })
            .await
            .unwrap();
        assert_eq!(level, isolation, "read={read}");
        let first = transaction_rows(&transaction, sql).await.unwrap();
        assert!(matches!(first[0][0], SqlValue::Int32(1)), "{first:?}");
        sqlx::raw_sql("UPDATE public.read_snapshot_fixture SET value = 2 WHERE id = 1")
            .execute(&mut admin)
            .await
            .map(drop)
            .expect("commit a concurrent write");
        let second = transaction_rows(&transaction, sql).await.unwrap();
        assert!(
            matches!(second[0][0], SqlValue::Int32(value) if value == second_value),
            "read={read}: {second:?}"
        );
        transaction.rollback().await.unwrap();
    }

    drop(pg);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP TABLE public.read_snapshot_fixture; \
             DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("drop the fixture");
}

#[tokio::test]
async fn workflow_caller_retains_typed_connection_unavailable() -> anyhow::Result<()> {
    let postgres = WamnPostgres::with_provider(std::sync::Arc::new(StaticCredentialProvider::new(
        std::collections::HashMap::new(),
        None,
    )));
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

/// A host read operation carries identity and refuses writes after binding it.
#[sqlx::test(migrations = false)]
async fn live_operation_reads_keep_identity_and_refuse_writes(pool: sqlx::PgPool) {
    const PROJECT: &str = "test--dev--identity-read";
    const SCOPE: &str = "identity-read";
    let database = wamn_test_postgres::url(&pool);
    let guest = live_guest_url(database.as_str(), &PROJECT.parse().unwrap()).await;
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .unwrap();
    let mut admin = connect_raw(database.as_str()).await;
    sqlx::raw_sql("CREATE TABLE public.identity_read_probe(value integer); GRANT SELECT, INSERT ON public.identity_read_probe TO wamn_app, wamn_query")
        .execute(&mut admin).await.unwrap();
    pg.bind_request_element_session_identity(
        SCOPE,
        &SessionIdentity {
            project_env_key: Some(PROJECT.parse().unwrap()),
            user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
            operation: Some("admin:read-probe".parse().unwrap()),
            application: Some("fixture".parse().unwrap()),
            ..SessionIdentity::default()
        },
    )
    .unwrap();
    pg.scopes
        .update::<crate::invocation::Authority, _>(SCOPE, |record| record.read = true);
    let operation = run_operation(&pg, SCOPE, principal_statement())
        .await
        .unwrap();
    assert!(matches!(&operation[0][0], SqlValue::Text(value) if value == LIVE_PRINCIPAL));
    let error = run_operation(
        &pg,
        SCOPE,
        "INSERT INTO public.identity_read_probe VALUES(1)",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, PgError::QueryError((code, _)) if code == "25006"));
}
