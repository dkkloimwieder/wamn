//! Live test of `create-human` against a disposable PostgreSQL server.
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

use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, NoTls};
use wamn_control::create_human::{CreateHumanRequest, create_human_principal};
use wamn_control_provision::{
    PlatformComponent, SYSTEM_SCHEMA_SQL, bind_platform_principal_sql, sql,
};
use wamn_platform_identity::{IdentityError, IdentityErrorKind};
use wamn_test_infrastructure::locked_database;

const EMAIL: &str = "a.person@example.test";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable system database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
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
    let client = connect(url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$; \
             DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); \
             END $grant$;",
        )
        .await
        .expect("create the wamn_system role and give it the database");
    client
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .expect("create the wamn_db_owner role the record-history grants name");
    client
        .batch_execute(&format!(
            "SET ROLE wamn_system; {SYSTEM_SCHEMA_SQL} RESET ROLE;"
        ))
        .await
        .expect("apply the system schema of record as its owner");
    client
}

fn request(url: &str, subject: &str, email: &str) -> CreateHumanRequest {
    CreateHumanRequest {
        subject: subject.to_owned(),
        email: email.to_owned(),
        display_name: "A Person".to_owned(),
        system_database_url: url.to_owned(),
    }
}

#[tokio::test]
async fn create_human_stores_the_email_folded_to_lower_case() {
    let url = locked_database::database(wamn_test_postgres::database);
    let observer = system_database(&url).await;

    let principal = create_human_principal(request(&url, "A.Person@Example.Test", EMAIL))
        .await
        .expect("create the human principal");

    let id = principal.id().as_str().to_owned();
    let stored: String = observer
        .query_one(
            "SELECT email FROM identity.principals WHERE id = $1::text::uuid",
            &[&id],
        )
        .await
        .expect("read the stored email")
        .get(0);
    assert_eq!(stored, EMAIL);

    // The address the caller typed in mixed case names the same person.
    let mixed = create_human_principal(request(
        &url,
        "second.person@example.test",
        "Second.Person@Example.Test",
    ))
    .await
    .expect("create a second human principal from a mixed-case address");
    let mixed_id = mixed.id().as_str().to_owned();
    let folded: String = observer
        .query_one(
            "SELECT email FROM identity.principals WHERE id = $1::text::uuid",
            &[&mixed_id],
        )
        .await
        .expect("read the second stored email")
        .get(0);
    assert_eq!(folded, "second.person@example.test");
}

#[tokio::test]
async fn one_email_address_names_one_principal() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut observer = system_database(&url).await;

    create_human_principal(request(&url, "first.person@example.test", EMAIL))
        .await
        .expect("create the first human principal");

    // A different subject and a different spelling of the same address.
    let error = create_human_principal(request(
        &url,
        "second.person@example.test",
        "A.PERSON@Example.Test",
    ))
    .await
    .expect_err("the second principal with that address is refused");

    let identity = error
        .downcast_ref::<IdentityError>()
        .expect("the refusal carries the identity error");
    assert_eq!(identity.kind(), IdentityErrorKind::Conflict);

    let rows: i64 = observer
        .query_one(
            "SELECT count(*) FROM identity.principals WHERE email = $1",
            &[&EMAIL],
        )
        .await
        .expect("count the principals that carry the address")
        .get(0);
    assert_eq!(rows, 1);

    // The verb keeps the server's own words out of its error, so the naming of
    // the constraint comes from the server directly.
    observer
        .batch_execute("SET ROLE wamn_system")
        .await
        .expect("take the owner role of identity.principals");
    let transaction = observer
        .transaction()
        .await
        .expect("begin the refusal transaction");
    transaction
        .batch_execute(&bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await
        .expect("bind the actor the stamp trigger needs");
    let refused = transaction
        .execute(
            "INSERT INTO identity.principals (kind, subject, email, display_name) \
             VALUES ('human', 'third.person@example.test', $1, 'A Person')",
            &[&EMAIL],
        )
        .await
        .unwrap_err();
    let refusal = refused
        .as_db_error()
        .unwrap_or_else(|| panic!("the server refuses the repeated address: {refused}"));
    assert_eq!(refusal.code(), &SqlState::UNIQUE_VIOLATION);
    assert_eq!(refusal.constraint(), Some("principals_email_key"));
}

#[tokio::test]
async fn only_a_human_row_carries_an_email() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut client = system_database(&url).await;
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .expect("take the owner role of identity.principals");
    let platform = PlatformComponent::Provisioning.principal_id().to_string();

    let cases = [
        (
            "a service row with an email",
            "INSERT INTO identity.principals (kind, subject, email, display_name) \
             VALUES ('service', 'ingest', 'a.person@example.test', 'Ingest')"
                .to_owned(),
        ),
        (
            "a platform row with an email",
            format!(
                "INSERT INTO identity.principals (id, kind, subject, email, display_name) \
                 VALUES ('{platform}', 'platform', 'wamn:provisioning', \
                 'a.person@example.test', 'wamn:provisioning')"
            ),
        ),
        (
            "a human row without an email",
            "INSERT INTO identity.principals (kind, subject, display_name) \
             VALUES ('human', 'a.person@example.test', 'A Person')"
                .to_owned(),
        ),
    ];

    for (case, statement) in cases {
        let transaction = client
            .transaction()
            .await
            .expect("begin the refusal transaction");
        transaction
            .batch_execute(&bind_platform_principal_sql(
                PlatformComponent::Provisioning,
            ))
            .await
            .expect("bind the actor the stamp trigger needs");
        let error = transaction
            .execute(statement.as_str(), &[])
            .await
            .unwrap_err();
        let refusal = error
            .as_db_error()
            .unwrap_or_else(|| panic!("{case} is refused by the server: {error}"));
        assert_eq!(refusal.code(), &SqlState::CHECK_VIOLATION, "{case}");
        assert_eq!(
            refusal.constraint(),
            Some("principals_email_check"),
            "{case}"
        );
        drop(transaction);
    }
}
