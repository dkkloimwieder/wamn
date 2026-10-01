//! Live test of the org `control` credential (`wamn-a40n.2`).
//!
//! `provision-org` mints the credential through the shared generation
//! lifecycle. The login reads exactly what an org's control host reads: who
//! holds `project-admin` in the org, and whether a control session's password
//! login is live. It writes nothing and reads nothing else.
//!
//! The test runs on the PostgreSQL server of its test process. It creates
//! cluster-global roles and revokes PUBLIC CONNECT on every non-template
//! database.

use std::path::{Path, PathBuf};

use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, NoTls};
use wamn_control::provision_project_env::{
    OrgWorkloadActionRequest, WorkloadActionOutcome, WorkloadActionVerb, WorkloadGenerationAction,
    provisioning_transaction, run_org_workload_action,
};
use wamn_control_provision::{
    CONTROL_ROLE, CredentialGeneration, PLATFORM_GROUP_ROLE, WorkloadRoleFamily,
};
use wamn_platform_identity::control::{control_projects, control_session_is_active};
use wamn_session::token::{SessionAuthority, SessionClaims};

const ORG: &str = "ctlorg";
const ISSUER: &str = "https://identity.example.test";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable control database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

fn secret_path() -> PathBuf {
    std::env::temp_dir().join(format!("wamn-control-live-{}.json", std::process::id()))
}

fn prepare(admin_url: &str, secret: &Path) -> OrgWorkloadActionRequest {
    let url = url::Url::parse(admin_url).expect("parse the admin URL");
    OrgWorkloadActionRequest {
        org: ORG.to_owned(),
        system_database_url: admin_url.to_owned(),
        db_host: url.host_str().map(str::to_owned),
        db_port: url.port_or_known_default().unwrap_or(5432),
        namespace: "hosts".to_owned(),
        action: WorkloadGenerationAction {
            family: WorkloadRoleFamily::Control,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        },
        secret: Some(secret.to_path_buf()),
    }
}

async fn refused(client: &Client, statement: &str) {
    let error = client.batch_execute(statement).await.expect_err(statement);
    assert_eq!(
        error.code(),
        Some(&SqlState::INSUFFICIENT_PRIVILEGE),
        "{statement}: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_control_login_reads_the_control_authority_of_its_org_and_nothing_else() {
    let database = wamn_control_provision::test_database::system();
    let admin_url = database.url().to_owned();
    let mut admin = connect(&admin_url).await;
    // provision-system closes the PUBLIC TEMPORARY floor of the control
    // database, and the lifecycle refuses a database that has it open.
    admin
        .batch_execute(&format!(
            "REVOKE TEMPORARY ON DATABASE \"{}\" FROM PUBLIC",
            database.name()
        ))
        .await
        .expect("close the PUBLIC TEMPORARY floor");
    admin
        .batch_execute("SET ROLE wamn_system")
        .await
        .expect("assume the control owner");
    let (person, login) = {
        let transaction = provisioning_transaction(&mut admin)
            .await
            .expect("bind wamn:provisioning");
        let person = wamn_platform_identity::create_human(
            &transaction,
            "control-live",
            "control.live@example.test",
            "Control Live",
        )
        .await
        .expect("create the person");
        transaction
            .batch_execute(&format!(
                "INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
                   VALUES ('{ORG}', 'pooled', 'wamn-pg'); \
                 INSERT INTO registry.projects (org, id) VALUES ('{ORG}', 'billing');"
            ))
            .await
            .expect("record the org and its project");
        wamn_platform_identity::assign_project_role(
            &transaction,
            person.id(),
            ORG,
            "billing",
            "project-admin",
        )
        .await
        .expect("assign project-admin");
        let login: String = transaction
            .query_one(
                "INSERT INTO identity.password_logins \
                   (principal_id, issuer, audience, authenticated_at, expires_at, renewal_expires_at) \
                 VALUES ($1::text::uuid, $2, $3, now(), now() + interval '8 hours', \
                         now() + interval '1 hour') \
                 RETURNING id::text",
                &[
                    &person.id().as_str(),
                    &ISSUER,
                    &format!("urn:wamn:control:{ORG}"),
                ],
            )
            .await
            .expect("record a password login")
            .get(0);
        transaction.commit().await.expect("commit the fixture");
        (person, login)
    };
    admin.batch_execute("RESET ROLE").await.expect("reset role");

    let secret = secret_path();
    let outcome = run_org_workload_action(&prepare(&admin_url, &secret))
        .await
        .expect("prepare the control credential");
    assert!(matches!(outcome, WorkloadActionOutcome::Prepared { .. }));
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&secret).expect("read the Secret"))
            .expect("parse the Secret");
    std::fs::remove_file(&secret).expect("remove the Secret");
    assert_eq!(manifest["metadata"]["name"], format!("wamn-control-{ORG}"));
    assert_eq!(manifest["metadata"]["namespace"], "hosts");
    let control_url = manifest["stringData"]["url"]
        .as_str()
        .expect("the Secret carries one url")
        .to_owned();
    let control = connect(&control_url).await;

    let parents: Vec<String> = admin
        .query(
            "SELECT g.rolname FROM pg_auth_members m \
               JOIN pg_roles g ON g.oid = m.roleid JOIN pg_roles r ON r.oid = m.member \
              WHERE r.rolname = $1",
            &[&CONTROL_ROLE],
        )
        .await
        .expect("read the parents of the control role")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert!(
        !parents.iter().any(|parent| parent == PLATFORM_GROUP_ROLE),
        "the control role is not a {PLATFORM_GROUP_ROLE} member: {parents:?}"
    );

    assert_eq!(
        control_projects(&control, person.id(), ORG)
            .await
            .expect("read the control projects"),
        ["billing"]
    );
    assert!(
        control_projects(&control, person.id(), "otherorg")
            .await
            .expect("read another org")
            .is_empty()
    );
    let claims = |login: &str| SessionClaims {
        iss: ISSUER.to_owned(),
        sub: person.id().as_str().to_owned(),
        org: ORG.to_owned(),
        aud: format!("urn:wamn:control:{ORG}"),
        roles: Vec::new(),
        exp: 0,
        iat: 0,
        jti: "live".to_owned(),
        authority: SessionAuthority::Login(login.to_owned()),
        csrf: None,
    };
    assert!(
        control_session_is_active(&control, &claims(&login))
            .await
            .expect("check the live login")
    );
    assert!(
        !control_session_is_active(&control, &claims("00000000-0000-0000-0000-000000000000"))
            .await
            .expect("check an unknown login")
    );

    for statement in [
        "INSERT INTO identity.project_roles (principal_id, org, project, role) \
           SELECT principal_id, org, 'other', role FROM identity.project_roles",
        "UPDATE identity.password_logins SET revoked_at = now()",
        "DELETE FROM identity.principals",
        "SELECT 1 FROM identity.pats",
        "SELECT 1 FROM identity.password_credentials",
        "SELECT 1 FROM identity.session_keys",
        "SELECT 1 FROM registry.orgs",
        "SELECT 1 FROM registry.project_envs",
    ] {
        refused(&control, statement).await;
    }
}
