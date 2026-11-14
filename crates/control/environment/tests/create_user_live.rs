//! Live test of `create-user` against a disposable PostgreSQL server.
//!
//! `wamn-t8co`. The verb had one caller and no test at all. The rules it has to
//! satisfy belong to the server, so this file states them against a real one.
//! `deploy/sql/system-schema.sql` holds every rule under test: the `email`
//! column of `identity.principals`, its `UNIQUE (email)` constraint, and
//! `principals_email_check`.
//!
//! Each test runs on the PostgreSQL server of its test process and holds the
//! process lock of that server, because the system schema needs the
//! cluster-wide `wamn_system` role.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};

use wamn_control_provision::{
    PlatformComponent, SYSTEM_SCHEMA_SQL, bind_platform_principal_sql, sql,
};
use wamn_environment::create_user::{CreateUserRequest, create_user_principal};
use wamn_platform_identity::{IdentityError, IdentityErrorType};

const EMAIL: &str = "a.person@example.test";

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to the disposable system database");

    client
}

/// Apply the system schema of record to a fresh database and return an
/// observer connection.
///
/// `deploy/sql/system-schema.sql` says the applier runs as the `wamn_system`
/// owner, so the whole composition is applied under that role. Applying it as
/// the superuser leaves `wamn_history.stamp_row` owned by the superuser, and
/// then `wamn_system` has no EXECUTE on the trigger function that every write
/// fires.
async fn system_database(url: &str) -> Client {
    let mut client = connect(url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$; \
             DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); \
             END $grant$;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("create the wamn_system role and give it the database");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("create the wamn_db_owner role the record-history grants name");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE wamn_system; {SYSTEM_SCHEMA_SQL} RESET ROLE;"
    )))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("apply the system schema of record as its owner");
    client
}

fn request(url: &str, subject: &str, email: &str) -> CreateUserRequest {
    CreateUserRequest {
        subject: subject.to_owned(),
        email: email.to_owned(),
        display_name: "A User".to_owned(),
        system_database_url: url.to_owned(),
    }
}

#[sqlx::test(migrations = false)]
async fn create_user_stores_the_email_folded_to_lower_case(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let url = wamn_test_postgres::url(&pool).to_string();
    let mut observer = system_database(&url).await;

    let principal = create_user_principal(
        request(&url, "A.Person@Example.Test", EMAIL),
        &wamn_broker::Publisher::default(),
    )
    .await
    .expect("create the user principal");

    let id = principal.id().to_string().as_str().to_owned();
    let stored: String =
        sqlx::query("SELECT email FROM identity.principals WHERE id = $1::text::uuid")
            .bind(&id)
            .fetch_one(&mut observer)
            .await
            .expect("read the stored email")
            .get(0);
    assert_eq!(stored, EMAIL);

    // The address the caller typed in mixed case names the same user.
    let mixed = create_user_principal(
        request(
            &url,
            "second.person@example.test",
            "Second.Person@Example.Test",
        ),
        &wamn_broker::Publisher::default(),
    )
    .await
    .expect("create a second user principal from a mixed-case address");
    let mixed_id = mixed.id().to_string().as_str().to_owned();
    let folded: String =
        sqlx::query("SELECT email FROM identity.principals WHERE id = $1::text::uuid")
            .bind(&mixed_id)
            .fetch_one(&mut observer)
            .await
            .expect("read the second stored email")
            .get(0);
    assert_eq!(folded, "second.person@example.test");
}

#[sqlx::test(migrations = false)]
async fn one_email_address_names_one_principal(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let url = wamn_test_postgres::url(&pool).to_string();
    let mut observer = system_database(&url).await;

    create_user_principal(
        request(&url, "first.person@example.test", EMAIL),
        &wamn_broker::Publisher::default(),
    )
    .await
    .expect("create the first user principal");

    // A different subject and a different spelling of the same address.
    let error = create_user_principal(
        request(&url, "second.person@example.test", "A.PERSON@Example.Test"),
        &wamn_broker::Publisher::default(),
    )
    .await
    .expect_err("the second principal with that address is refused");

    let identity = error
        .downcast_ref::<IdentityError>()
        .expect("the refusal carries the identity error");
    assert_eq!(identity.error_type(), IdentityErrorType::Conflict);

    let rows: i64 = sqlx::query("SELECT count(*) FROM identity.principals WHERE email = $1")
        .bind(&EMAIL)
        .fetch_one(&mut observer)
        .await
        .expect("count the principals that carry the address")
        .get(0);
    assert_eq!(rows, 1);

    // The verb keeps the server's own words out of its error, so the naming of
    // the constraint comes from the server directly.
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut observer)
        .await
        .map(drop)
        .expect("take the owner role of identity.principals");
    let mut transaction = observer
        .begin()
        .await
        .expect("begin the refusal transaction");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&bind_platform_principal_sql(PlatformComponent::Provisioning)).to_string(),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)
    .expect("bind the actor the stamp trigger needs");
    let refused = sqlx::query(
        "INSERT INTO identity.principals (type, subject, email, display_name) \
             VALUES ('user', 'third.person@example.test', $1, 'A User')",
    )
    .bind(&EMAIL)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .unwrap_err();
    let refusal = refused
        .as_database_error()
        .unwrap_or_else(|| panic!("the server refuses the repeated address: {refused}"));
    assert_eq!(refusal.code().as_deref(), Some("23505"));
    assert_eq!(refusal.constraint(), Some("principals_email_key"));
}

#[sqlx::test(migrations = false)]
async fn only_a_user_row_carries_an_email(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let url = wamn_test_postgres::url(&pool).to_string();
    let mut client = system_database(&url).await;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .expect("take the owner role of identity.principals");
    let platform = PlatformComponent::Provisioning.principal_id().to_string();

    let cases = [
        (
            "a service row with an email",
            "INSERT INTO identity.principals (type, subject, email, display_name) \
             VALUES ('service', 'ingest', 'a.person@example.test', 'Ingest')"
                .to_owned(),
        ),
        (
            "a platform row with an email",
            format!(
                "INSERT INTO identity.principals (id, type, subject, email, display_name) \
                 VALUES ('{platform}', 'platform', 'wamn:provisioning', \
                 'a.person@example.test', 'wamn:provisioning')"
            ),
        ),
        (
            "a user row without an email",
            "INSERT INTO identity.principals (type, subject, display_name) \
             VALUES ('user', 'a.person@example.test', 'A User')"
                .to_owned(),
        ),
    ];

    for (case, statement) in cases {
        let mut transaction = client.begin().await.expect("begin the refusal transaction");
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&bind_platform_principal_sql(PlatformComponent::Provisioning)).to_string(),
        ))
        .execute(&mut *transaction)
        .await
        .map(drop)
        .expect("bind the actor the stamp trigger needs");
        let error = sqlx::query(sqlx::AssertSqlSafe((statement.as_str()).to_string()))
            .execute(&mut *transaction)
            .await
            .map(|result| result.rows_affected())
            .unwrap_err();
        let refusal = error
            .as_database_error()
            .unwrap_or_else(|| panic!("{case} is refused by the server: {error}"));
        assert_eq!(refusal.code().as_deref(), Some("23514"), "{case}");
        assert_eq!(
            refusal.constraint(),
            Some("principals_email_check"),
            "{case}"
        );
        drop(transaction);
    }
}
