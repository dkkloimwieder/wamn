//! Atomic admission of every matching event workflow for one delivery.

use wamn_engine::release_manifest::LoadedRelease;
use wamn_materializer::workflow::{EventTrigger, SourceEvent};
use wamn_project_state::{ADMIN_ROLE, PlatformComponent, workflow_user_id};
use wamn_run_state::queue::{insert_event_run_sql, insert_run_queue_sql, select_event_run_sql};

use super::production_claim::{
    ProductionClaimError, ProductionClaimErrorType, finish_queue_transaction,
    require_executor_authority,
};
use super::{AuthorityClass, WamnPostgres};

const ENABLED_EVENTS_SQL: &str = "SELECT application, workflow_id, source_package, entity, \
    array_to_json(ops)::text, condition, enable_sequence, version, graph_json::text, \
    wiring_hash, execution_role FROM catalog.workflows \
    WHERE env_key = $1 AND environment = $2 AND application = ANY($3) \
      AND enabled AND trigger = 'event' ORDER BY application, workflow_id";

fn contract(operation: &'static str, detail: impl Into<String>) -> ProductionClaimError {
    ProductionClaimError::new(ProductionClaimErrorType::Contract, operation, detail)
}

fn storage(
    operation: &'static str,
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ProductionClaimError {
    ProductionClaimError::with_source(ProductionClaimErrorType::Storage, operation, error)
}

fn invalid(
    operation: &'static str,
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ProductionClaimError {
    ProductionClaimError::with_source(ProductionClaimErrorType::Contract, operation, error)
}

fn trigger(row: &tokio_postgres::Row) -> Result<EventTrigger, ProductionClaimError> {
    let ops: String = row.get(4);
    let sequence: i64 = row.get(6);
    Ok(EventTrigger {
        application: row.get(0),
        workflow_id: row.get(1),
        source_package: row.get(2),
        entity: row.get(3),
        ops: serde_json::from_str(&ops)
            .map_err(|error| invalid("decode event operations", error))?,
        condition: row.get(5),
        enable_sequence: u64::try_from(sequence)
            .map_err(|error| invalid("decode enable sequence", error))?,
    })
}

impl WamnPostgres {
    pub(crate) fn require_event_materializer(
        &self,
        component: &str,
        release: &LoadedRelease,
    ) -> Result<(), ProductionClaimError> {
        if self.workload_authority_for(component) != AuthorityClass::EventMaterializer
            || self.env_key_for(component).as_deref() != Some(release.scope().env_key.as_str())
        {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorType::Identity,
                "authorize event materializer",
                "component is not the environment materializer",
            ));
        }
        Ok(())
    }

    /// Read the enabled triggers of the applications bound to this host.
    pub async fn event_triggers(
        &self,
        project: &str,
        release: &LoadedRelease,
    ) -> Result<Vec<EventTrigger>, ProductionClaimError> {
        let scope = release.scope();
        let names: Vec<_> = scope.applications.keys().cloned().collect();
        self.executor_read(project, &scope.env_key, async |connection| {
            connection
                .query(
                    ENABLED_EVENTS_SQL,
                    &[&scope.env_key, &scope.environment, &names],
                )
                .await?
                .iter()
                .map(|row| trigger(row).map_err(anyhow::Error::from))
                .collect()
        })
        .await
        .map_err(|error| storage("read enabled event triggers", error))
    }

    /// Re-read trigger rows, resolve their graphs and actor, and insert all runs
    /// and queue entries before committing. Any refusal rolls back the batch.
    pub async fn admit_event_batch(
        &self,
        project: &str,
        release: &LoadedRelease,
        admission: &EventBatchAdmission<'_>,
    ) -> Result<(), ProductionClaimError> {
        let scope = release.scope();
        let env_key = &scope.env_key;
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| storage("checkout event admission", error))?;
        let executor = PlatformComponent::Executor.principal_id().to_string();
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                false,
                env_key,
                Some("wamn_run"),
                None,
                None,
                Some(&executor),
                Some(PlatformComponent::Executor.principal_name()),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(storage("begin event admission", error));
        }
        let result = async {
            require_executor_authority(&connection).await?;
            let source = SourceEvent::decode(admission.envelope.as_bytes())
                .map_err(|error| invalid("decode event", error))?;
            if admission.sequence == 0 || source.identity(env_key, project, &scope.environment, &[admission.source_event_id]).is_none() {
                return Err(contract("identify event", "invalid event sequence or source identity"));
            }
            // Different packages can share an entity subject. Only its own
            // consumer admits the event; the others settle it without runs.
            if source.source() != (admission.source_package, admission.entity) {
                return Ok(());
            }
            let names: Vec<_> = scope.applications.keys().cloned().collect();
            let packages = scope.applications.values().flat_map(|application| application.packages.iter().cloned()).collect();
            let rows = connection.query(ENABLED_EVENTS_SQL, &[env_key, &scope.environment, &names]).await
                .map_err(|error| storage("read event workflows", error))?;
            let mut matches = Vec::new();
            for row in rows {
                let event = trigger(&row)?;
                if let Some(input) = event.matches(&source, admission.sequence, &packages, env_key)
                    .map_err(|error| invalid("filter event workflow", error))? {
                    matches.push((row, event, input));
                }
            }
            if matches.is_empty() { return Ok(()); }
            let actor = match &source {
                SourceEvent::Cdc(envelope) => history_actor(&connection, envelope).await?,
                SourceEvent::Derived(_) => None,
            };
            let durability: String = connection.query_opt(
                "SELECT durability_class FROM environment_policies WHERE env_key = $1 AND expected_environment = $2",
                &[env_key, &scope.environment],
            ).await.map_err(|error| storage("read environment policy", error))?
                .ok_or_else(|| contract("read environment policy", "environment policy is absent"))?.get(0);
            let parent = match &source {
                SourceEvent::Cdc(envelope) => envelope.causation.as_ref(),
                SourceEvent::Derived(event) => Some(&event.causation),
            };
            let source_run = parent.map(|parent| parent.run.as_str());
            let root_run = parent.map(|parent| parent.root.as_str());
            let depth = parent.map(|parent| i32::try_from(parent.depth + 1)).transpose()
                .map_err(|error| invalid("decode event depth", error))?;
            let principal = workflow_user_id().to_string();
            let delivery = format!("event:{}:{}", admission.sequence, admission.source_event_id);
            for (row, event, input) in matches {
                let version: i32 = row.get(7);
                let graph_json: String = row.get(8);
                let hash: String = row.get(9);
                let role = row.get::<_, Option<String>>(10).unwrap_or_else(|| ADMIN_ROLE.to_owned());
                let graph = super::workflow_resolution::parse_graph(&graph_json)
                    .map_err(|error| invalid("parse event workflow", error))?;
                let packages = &scope.applications[&event.application].packages;
                let gated = wamn_catalog::gate_workflow_document(env_key, packages, release.manifest(), &graph)
                    .map_err(|error| invalid("gate event workflow", error))?;
                if gated.as_str() != hash { return Err(contract("gate event workflow", "workflow hash differs")); }
                let components = wamn_catalog::resolve_workflow_components(env_key, packages, release.manifest(), &graph)
                    .map_err(|error| invalid("resolve event workflow", error))?;
                let digests = serde_json::Value::Object(components.into_iter().map(|(node, component)| (node, serde_json::Value::String(component.component_digest))).collect()).to_string();
                let key = serde_json::json!(["event", scope.environment, event.application, event.workflow_id, delivery]).to_string();
                let input = input.to_string();
                let digest = release.release().manifest_digest.as_str();
                let inserted = connection.query_opt(&insert_event_run_sql(), &[
                    env_key, &event.application, &digest, &scope.environment, &event.workflow_id,
                    &version, &hash, &principal, &role, &delivery, &actor, &digests, &key, &input, &durability, &source_run, &root_run, &depth,
                ]).await.map_err(|error| storage("insert event run", error))?;
                if let Some(row) = inserted {
                    let run_id: String = row.get(0);
                    connection.execute(insert_run_queue_sql(), &[env_key, &run_id]).await
                        .map_err(|error| storage("queue event run", error))?;
                } else if connection.query_opt(select_event_run_sql(), &[env_key, &key, &input]).await
                    .map_err(|error| storage("read existing event run", error))?.is_none() {
                    return Err(contract("read existing event run", "event idempotency conflict"));
                }
            }
            Ok(())
        }.await;
        finish_queue_transaction(self, connection, result, "commit event batch").await
    }
}

/// One message from one entity consumer. Scope and release come from the host.
#[derive(Debug)]
pub struct EventBatchAdmission<'a> {
    pub source_package: &'a str,
    pub entity: &'a str,
    pub envelope: &'a str,
    pub sequence: u64,
    pub source_event_id: &'a str,
}

async fn history_actor(
    connection: &tokio_postgres::Client,
    envelope: &wamn_event_wire::Envelope,
) -> Result<Option<String>, ProductionClaimError> {
    let schemas = connection.query("SELECT n.nspname::text FROM pg_namespace n JOIN pg_class c ON c.relnamespace = n.oid WHERE c.relname = 'wamn_entities' AND c.relkind = 'r' ORDER BY 1", &[]).await
        .map_err(|error| storage("read entity map schemas", error))?;
    for row in schemas {
        let schema: String = row.get(0);
        let quoted_schema = wamn_pg_core::quote_ident(&schema);
        let sql = format!(
            "SELECT c.oid, c.relname::text FROM {quoted_schema}.wamn_entities m JOIN pg_class c ON c.oid = m.relation_oid WHERE m.package_id = $1 AND m.entity_id = $2 AND c.relname = m.table_name AND c.relnamespace = $3::text::regnamespace"
        );
        let Some(model) = connection
            .query_opt(&sql, &[&envelope.package_id, &envelope.entity, &schema])
            .await
            .map_err(|error| storage("resolve event history model", error))?
        else {
            continue;
        };
        let oid: u32 = model.get(0);
        let table: String = model.get(1);
        let history = format!(
            "{quoted_schema}.{}",
            wamn_pg_core::quote_ident(&format!("{table}_history"))
        );
        let present: bool = connection
            .query_one("SELECT to_regclass($1) IS NOT NULL", &[&history])
            .await
            .map_err(|error| storage("locate event history", error))?
            .get(0);
        if !present {
            return Ok(None);
        }
        let image = match envelope.op {
            wamn_event_wire::Op::Delete => &envelope.old,
            _ => &envelope.new,
        }
        .as_ref()
        .ok_or_else(|| contract("read event row key", "event image is absent"))?;
        let image = serde_json::Value::Object(image.clone()).to_string();
        // History stores a full xid8. CDC carries its low 32 bits. Build the
        // primary key from the same columns and timestamp spelling as history.
        let table = format!("{quoted_schema}.{}", wamn_pg_core::quote_ident(&table));
        let sql = format!(
            "WITH image AS (SELECT to_jsonb(jsonb_populate_record(NULL::{table}, $3::text::jsonb)) AS value) SELECT h.changed_by::text FROM {history} h WHERE h.transaction_id % 4294967296 = $1 AND h.type = $2 AND h.row_key = (SELECT jsonb_object_agg(a.attname, CASE WHEN a.atttypid = 'timestamptz'::regtype AND isfinite((image.value ->> a.attname)::timestamptz) THEN to_jsonb(to_char((image.value ->> a.attname)::timestamptz AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) ELSE image.value -> a.attname END) FROM image, pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) WHERE i.indrelid = $4 AND i.indisprimary) ORDER BY h.position DESC LIMIT 1"
        );
        return connection
            .query_opt(
                &sql,
                &[
                    &i64::from(envelope.txid),
                    &envelope.op.as_str(),
                    &image,
                    &oid,
                ],
            )
            .await
            .map(|row| row.and_then(|row| row.get(0)))
            .map_err(|error| storage("read event history actor", error));
    }
    Ok(None)
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
        let release = LoadedRelease::control_root();
        postgres
            .set_env_key("guest", &release.scope().env_key)
            .unwrap();
        let error = postgres
            .require_event_materializer("guest", &release)
            .unwrap_err();
        assert_eq!(error.error_type(), ProductionClaimErrorType::Identity);
        postgres
            .bind_workload_authority("guest", "event-materializer")
            .unwrap();
        postgres
            .require_event_materializer("guest", &release)
            .unwrap();
    }
}
