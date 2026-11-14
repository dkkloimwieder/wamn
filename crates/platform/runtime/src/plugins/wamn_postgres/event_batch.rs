//! Event trigger reads through authorized executor transport.
use super::{AuthorityClass, WamnPostgres};
use sqlx::Row as _;
use wamn_materializer::workflow::EventTrigger;
use wamn_run_state::admission;
use wamn_run_state::run_store::{ProductionLeaseError, ProductionLeaseErrorType};

/// Decode stored event trigger facts for workflow orchestration.
pub fn decode_event_trigger(
    row: &sqlx::postgres::PgRow,
) -> Result<EventTrigger, ProductionLeaseError> {
    let ops: String = row.get(4);
    let sequence: i64 = row.get(6);
    Ok(EventTrigger {
        application: row.get(0),
        workflow_id: row.get(1),
        source_package: row.get(2),
        entity: row.get(3),
        ops: serde_json::from_str(&ops)
            .map_err(|error| ProductionLeaseError::invalid("decode event operations", error))?,
        condition: row.get(5),
        enable_sequence: u64::try_from(sequence)
            .map_err(|error| ProductionLeaseError::invalid("decode enable sequence", error))?,
    })
}

impl WamnPostgres {
    pub(crate) fn require_event_materializer(
        &self,
        component: &str,
        project_env_key: &str,
    ) -> Result<(), ProductionLeaseError> {
        if self.workload_authority_for(component) != AuthorityClass::EventMaterializer
            || self.project_env_key_for(component).as_deref() != Some(project_env_key)
        {
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "authorize event materializer",
                "component is not the environment materializer",
            ));
        }
        Ok(())
    }

    /// Read enabled event triggers installed in this environment.
    pub async fn event_triggers(
        &self,
        project: &str,
        project_env_key: &str,
        environment: &str,
    ) -> Result<Vec<EventTrigger>, ProductionLeaseError> {
        self.executor_read(project, project_env_key, async |connection| {
            admission::event_workflows(connection, project_env_key, environment, None)
                .await?
                .iter()
                .map(|row| decode_event_trigger(row).map_err(anyhow::Error::from))
                .collect()
        })
        .await
        .map_err(|error| ProductionLeaseError::storage("read enabled event triggers", error))
    }
}

/// One message from one entity consumer. Target and release come from the host.
#[derive(Debug)]
pub struct EventBatchAdmission<'a> {
    pub source_package: &'a str,
    pub entity: &'a str,
    pub envelope: &'a str,
    pub sequence: u64,
    pub source_event_id: &'a str,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::plugins::wamn_postgres::StaticCredentialProvider;

    #[test]
    fn event_materialization_refuses_the_wrong_workload_authority() {
        let postgres = WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(
            HashMap::new(),
            None,
        )));
        let project_env_key = "acme--dev--apps";
        postgres
            .set_project_env_key("guest", project_env_key)
            .unwrap();
        let error = postgres
            .require_event_materializer("guest", project_env_key)
            .unwrap_err();
        assert_eq!(error.error_type(), ProductionLeaseErrorType::Identity);
        postgres
            .bind_workload_authority("guest", "event-materializer")
            .unwrap();
        postgres
            .require_event_materializer("guest", project_env_key)
            .unwrap();
        assert_eq!(
            postgres
                .require_event_materializer("guest", "another-environment")
                .unwrap_err()
                .error_type(),
            ProductionLeaseErrorType::Identity
        );
    }
}
