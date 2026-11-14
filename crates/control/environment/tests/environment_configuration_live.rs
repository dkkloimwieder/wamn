//! Restored web selections persist without replacing another application's configuration.

use wamn_environment::{
    environment::document::{Client, EnvironmentDocument},
    provision_project_env,
};
use wamn_registry::{Triple, configuration};

#[tokio::test]
async fn restored_clients_keep_other_applications_and_survive_configuration_reads()
-> anyhow::Result<()> {
    let database = wamn_control_provision::test_database::system();
    database.execute(&[
        &wamn_control_provision::test_database::seed_org_project_env_sql("acme", "apps", "dev"),
    ])?;
    let triple = Triple::new("acme", "dev", "apps");
    let document = EnvironmentDocument::from_json(br#"{
        "org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,
        "applications":{"first":{"hostname":"first.example","revocation_seconds":5},
        "second":{"hostname":"second.example","revocation_seconds":5}}
    }"#)?;
    let mut connection = wamn_postgres::connect(database.url()).await?;
    let mut transaction = provision_project_env::provisioning_transaction(&mut connection).await?;
    assert!(configuration::write(&mut transaction, &document.configuration()).await?);
    transaction.commit().await?;
    let first = Client {
        package: "first".into(),
        web: wamn_digest::bytes_sha256(b"restored web"),
    };
    let second = Client {
        package: "second".into(),
        web: wamn_digest::bytes_sha256(b"other web"),
    };
    for (application, client) in [("first", &first), ("second", &second)] {
        let mut transaction =
            provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(
            configuration::write_client(&mut transaction, &triple, application, Some(client))
                .await?
        );
        transaction.commit().await?;
    }
    let read = configuration::read(&mut connection, &triple)
        .await?
        .unwrap();
    assert_eq!(read.applications["first"].client.as_ref(), Some(&first));
    assert_eq!(read.applications["second"].client.as_ref(), Some(&second));
    let mut transaction = provision_project_env::provisioning_transaction(&mut connection).await?;
    assert!(configuration::write_client(&mut transaction, &triple, "first", None).await?);
    transaction.commit().await?;
    let read = configuration::read(&mut connection, &triple)
        .await?
        .unwrap();
    assert!(read.applications["first"].client.is_none());
    assert_eq!(read.applications["second"].client.as_ref(), Some(&second));
    Ok(())
}
