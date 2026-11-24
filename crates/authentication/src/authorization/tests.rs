use std::sync::Arc;

use wamn_identity_protocol::AuthorityClass;
use wamn_registry::DEFAULT_PROJECT;

use super::Authorization;
use wamn_postgres::{CredentialProvider, ResolvedCredential, WamnPostgres};

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
    let pools = Arc::new(WamnPostgres::with_provider(Arc::clone(&provider) as _));
    let database = Arc::clone(&pools);
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

    authorization
        .user_operation_permissions(&DEFAULT_PROJECT, &project_env_key, &principal_id)
        .await
        .expect_err("a provider that names no credential resolves nothing");

    let asked = provider
        .asked
        .lock()
        .expect("recording provider lock poisoned");
    assert_eq!(asked.as_slice(), [AuthorityClass::Platform]);
}
