use crate::{OperationDatabase, OperationHost, PgError};
use std::sync::Arc;
use wamn_authentication::authorization::{Authorization, AuthorizationStore as _};
use wamn_identity_protocol::AuthorityClass;
use wamn_postgres::{
    CredentialProvider, DEFAULT_PROJECT, ResolvedCredential, StaticCredentialProvider, WamnPostgres,
};

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
    let application = wamn_application::application_config::ApplicationConfig {
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

#[tokio::test]
async fn workflow_caller_retains_typed_connection_unavailable() -> anyhow::Result<()> {
    let pools = WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(
        std::collections::HashMap::new(),
        None,
    )));
    let pools = Arc::new(pools);
    let postgres = OperationHost::new(Arc::clone(&pools), pools, None);
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
