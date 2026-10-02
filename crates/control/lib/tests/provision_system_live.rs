//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! The test holds the process lock of its server, because the verb changes
//! cluster-wide roles and the PUBLIC CONNECT floor of every database.

use std::os::unix::fs::PermissionsExt as _;
use tokio_postgres::NoTls;

use wamn_control::provision_system::{
    EmitProvisionerRequest, ProvisionSystemRequest, emit_provisioner_credential, provision_system,
};
use wamn_test_infrastructure::locked_database;

#[tokio::test]
async fn provision_system_runs_once_and_records_the_platform_domain() {
    let url = locked_database::database(wamn_test_postgres::database);
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("connect to the disposable system database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    // CloudNativePG creates the owner role at bootstrap. The test does it here.
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    // CloudNativePG makes wamn_system the database owner, which can connect.
    client
        .batch_execute(
            "DO $$ BEGIN EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_system', \
                                        current_database()); END $$;",
        )
        .await
        .expect("let wamn_system connect as the owner can");
    let request = ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    };

    provision_system(&request)
        .await
        .expect("the first run installs the control store");
    let domain: Option<String> = client
        .query_one("SELECT platform_domain FROM registry.meta", &[])
        .await
        .expect("read the platform domain")
        .get(0);
    assert_eq!(domain.as_deref(), Some("wamn.example.test"));

    // The provisioning worker's login holds exactly the measured grants.
    let role = client
        .query_one(
            "SELECT rolcanlogin, rolsuper, rolcreaterole, rolcreatedb, rolinherit, \
                    rolreplication, rolbypassrls, rolconfig::text[], \
                    ARRAY(SELECT parent.rolname::text || ':' || m.inherit_option || ':' \
                                 || m.set_option || ':' || m.admin_option \
                            FROM pg_auth_members m \
                            JOIN pg_roles parent ON parent.oid = m.roleid \
                           WHERE m.member = r.oid ORDER BY 1) \
               FROM pg_roles r WHERE rolname = 'wamn_provisioner'",
            &[],
        )
        .await
        .expect("read wamn_provisioner");
    assert_eq!(
        (0..7)
            .map(|column| role.get::<_, bool>(column))
            .collect::<Vec<_>>(),
        [true, false, true, true, true, true, true],
        "LOGIN NOSUPERUSER CREATEROLE CREATEDB INHERIT REPLICATION BYPASSRLS"
    );
    assert_eq!(
        role.get::<_, Vec<String>>(7),
        ["createrole_self_grant=set, inherit"]
    );
    assert_eq!(
        role.get::<_, Vec<String>>(8),
        [
            "pg_signal_backend:true:true:false",
            "wamn_db_owner:true:true:false",
            "wamn_system:true:true:false",
        ]
    );

    let refusal = provision_system(&request)
        .await
        .expect_err("the second run refuses");
    assert!(
        format!("{refusal:#}").contains("already has the schema registry"),
        "{refusal:#}"
    );

    // The emit run on the installed database writes the Secret and the role
    // statement. The statement sets the password the Secret carries.
    let directory =
        std::env::temp_dir().join(format!("wamn-provision-system-live-{}", std::process::id()));
    std::fs::create_dir(&directory).expect("create the private output directory");
    let parsed = url::Url::parse(&url).expect("parse the test URL");
    let emit = EmitProvisionerRequest {
        system_database_url: url.to_string(),
        emit_secret: directory.join("secret.json"),
        emit_provisioner_sql: directory.join("provisioner.sql"),
        db_host: parsed
            .host_str()
            .expect("the test URL has a host")
            .to_owned(),
        db_port: parsed.port().unwrap_or(5432),
    };
    emit_provisioner_credential(&emit).expect("the emit run writes both outputs");
    let statement =
        std::fs::read_to_string(&emit.emit_provisioner_sql).expect("read the statement");
    let secret: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&emit.emit_secret).expect("read the Secret"))
            .expect("parse the Secret");
    let worker_url = secret["stringData"]["url"]
        .as_str()
        .expect("the Secret has a url");
    let password = url::Url::parse(worker_url)
        .expect("parse the worker URL")
        .password()
        .expect("the worker URL has a password")
        .to_owned();
    assert!(
        !statement.contains(&password),
        "the statement holds only the verifier"
    );
    assert_eq!(
        std::fs::metadata(&emit.emit_secret)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    client
        .batch_execute(&statement)
        .await
        .expect("the superuser applies the statement");
    let (worker, worker_connection) = tokio_postgres::connect(worker_url, NoTls)
        .await
        .expect("wamn_provisioner logs in with the Secret's password");
    tokio::spawn(async move {
        let _ = worker_connection.await;
    });
    let user: String = worker
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("read the current user")
        .get(0);
    assert_eq!(user, "wamn_provisioner");
    std::fs::remove_dir_all(&directory).expect("remove the output directory");
}
