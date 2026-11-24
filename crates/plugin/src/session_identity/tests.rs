use std::sync::Arc;

use wamn_authentication::authorization::Authorization;
use wamn_postgres::{StaticCredentialProvider, WamnPostgres};

use crate::{OperationHost, PgError};

#[tokio::test]
async fn workflow_caller_retains_typed_connection_unavailable() -> anyhow::Result<()> {
    let pools = WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(
        std::collections::HashMap::new(),
        None,
    )));
    let pools = Arc::new(pools);
    let authorization = Authorization::new(Arc::clone(&pools), Arc::clone(&pools) as _);
    let postgres = OperationHost::new(pools, Arc::new(authorization), None);
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
