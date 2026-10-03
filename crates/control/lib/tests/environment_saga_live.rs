//! Live test of the create-environment saga records (wamn-zua8.3): the
//! control family writes a saga and all of its steps, the worker records its
//! steps as `wamn_system`, and resume and abandon move only the statuses
//! they own. The test holds the process lock of its server, because the
//! installers create cluster-wide roles.

use serde_json::json;
use tokio_postgres::{Client, NoTls};
use wamn_control::bind_connection::RequirementType;
use wamn_control::environment_saga::{
    self, ConnectionRequest, EnvironmentRequest, PackageReference, STEPS,
};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_control_provision::sql::grant_control_surface_sql;
use wamn_test_infrastructure::locked_database;

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

fn request(env: &str) -> EnvironmentRequest {
    EnvironmentRequest {
        project: "receiving".to_owned(),
        env: env.to_owned(),
        tenant: "dev".to_owned(),
        route_host: "receiving.example.test".to_owned(),
        packages: vec![PackageReference {
            package_id: "wamn_receiving".to_owned(),
            version: "1.0.0".to_owned(),
        }],
        connections: vec![ConnectionRequest {
            instance_id: "labels".to_owned(),
            requirement_type: RequirementType::Blobstore,
            alias: "labels".to_owned(),
            definition: json!({"endpoint": "gcs", "container": "c", "prefix": "p"}),
        }],
    }
}

/// Write one saga as the control family, as the route does.
async fn create(client: &mut Client, org: &str, env: &str) -> String {
    let transaction = client.transaction().await.expect("open the transaction");
    transaction
        .batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .expect("enter the control family");
    let saga = environment_saga::create_environment_saga(&transaction, org, &request(env))
        .await
        .expect("the control family writes the saga");
    transaction.commit().await.expect("commit the saga");
    saga
}

async fn statuses(client: &Client, saga: &str) -> (String, Vec<String>) {
    let saga_status = client
        .query_one(
            "SELECT status FROM provisioning.sagas WHERE saga_id = $1",
            &[&saga],
        )
        .await
        .expect("read the saga")
        .get(0);
    let steps = client
        .query(
            "SELECT step || ' ' || name || ' ' || status || ' ' || coalesce(error, '-') \
               FROM provisioning.saga_steps WHERE saga_id = $1 ORDER BY step",
            &[&saga],
        )
        .await
        .expect("read the steps")
        .iter()
        .map(|row| row.get(0))
        .collect();
    (saga_status, steps)
}

#[tokio::test]
async fn the_saga_records_its_steps_resumes_and_abandons() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    provision_system(&ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    client
        .batch_execute(&grant_control_surface_sql())
        .await
        .expect("prepare the control surface");

    let first = create(&mut client, "acme", "dev").await;
    let second = create(&mut client, "acme", "test").await;
    let other = create(&mut client, "other", "dev").await;
    let (status, steps) = statuses(&client, &first).await;
    assert_eq!(status, "pending");
    let expected: Vec<String> = STEPS
        .iter()
        .enumerate()
        .map(|(index, name)| format!("{} {name} pending -", index + 1))
        .collect();
    assert_eq!(steps, expected, "the route writes every step at creation");
    let input: serde_json::Value = client
        .query_one(
            "SELECT input FROM provisioning.sagas WHERE saga_id = $1",
            &[&first],
        )
        .await
        .expect("read the input")
        .get(0);
    assert_eq!(
        serde_json::from_value::<EnvironmentRequest>(input).expect("decode the input"),
        request("dev")
    );

    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .expect("act as the worker");
    let open = environment_saga::next_open_saga(&client)
        .await
        .expect("read the next saga")
        .expect("a saga is open");
    assert_eq!(
        (open.saga_id.as_str(), open.org.as_str(), open.next_step),
        (first.as_str(), "acme", 1),
        "the oldest open saga runs first"
    );
    environment_saga::start_step(&client, &first, 1)
        .await
        .expect("start step 1");
    environment_saga::complete_step(&client, &first, 1)
        .await
        .expect("complete step 1");
    environment_saga::start_step(&client, &first, 2)
        .await
        .expect("start step 2");
    let open = environment_saga::next_open_saga(&client)
        .await
        .expect("read the next saga")
        .expect("a saga is open");
    assert_eq!(
        (open.saga_id.as_str(), open.next_step),
        (first.as_str(), 2),
        "the running saga of acme is the only open saga of acme, and runs before other"
    );
    let refused = environment_saga::saga_abandon(&mut client, &first)
        .await
        .expect_err("a running saga is not abandoned");
    assert_eq!(
        refused.to_string(),
        format!("saga {first} is running; only a failed or pending saga is abandoned")
    );

    environment_saga::fail_step(&client, &first, 2, "the run plane refused")
        .await
        .expect("fail step 2");
    let (status, steps) = statuses(&client, &first).await;
    assert_eq!(status, "failed");
    assert_eq!(steps[0], "1 provision-project-env completed -");
    assert_eq!(
        steps[1],
        "2 reconcile-run-plane failed the run plane refused"
    );
    let open = environment_saga::next_open_saga(&client)
        .await
        .expect("read the next saga")
        .expect("a saga is open");
    assert_eq!(open.saga_id, second, "a failed saga leaves its org free");

    environment_saga::saga_resume(&mut client, &first)
        .await
        .expect("resume the failed saga");
    let (status, steps) = statuses(&client, &first).await;
    assert_eq!(status, "pending");
    assert_eq!(steps[1], "2 reconcile-run-plane pending -");
    let refused = environment_saga::saga_resume(&mut client, &first)
        .await
        .expect_err("a pending saga does not resume");
    assert_eq!(
        refused.to_string(),
        format!("saga {first} is pending; only a failed saga resumes")
    );

    environment_saga::saga_abandon(&mut client, &second)
        .await
        .expect("abandon the pending saga");
    assert_eq!(statuses(&client, &second).await.0, "abandoned");

    let last = 14;
    let detail = json!({"commands": ["kubectl -n identity rollout restart deploy/identity"]});
    environment_saga::await_operator(&client, &other, last, &detail)
        .await
        .expect("record the operator commands");
    let (status, steps) = statuses(&client, &other).await;
    assert_eq!(status, "awaiting-operator");
    assert_eq!(steps[13], "14 awaiting-operator completed -");
    let refused = environment_saga::saga_abandon(&mut client, "missing")
        .await
        .expect_err("an unknown saga is refused");
    assert_eq!(
        refused.to_string(),
        "no create-environment saga has the id missing"
    );
}
