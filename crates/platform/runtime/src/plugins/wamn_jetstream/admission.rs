//! Environment routing and application-owned event admission.

use std::collections::BTreeSet;

use futures_util::StreamExt as _;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_materializer::workflow::SourceEvent;

use super::{
    CONSUMER_CONFIG_DRIFT, EventCoordinates, JsError, NATS_MESSAGE_ID, PullConfig,
    UNREGISTERED_SOURCE, WamnJetstream, consumer_config_matches, map_get_stream_err,
    materializer_consumer_config, stream_name, subject_token,
};
use crate::plugins::wamn_postgres::{EventBatchAdmission, WamnPostgres};
use std::time::Duration;

fn application_consumer(
    coordinates: &EventCoordinates,
    application: &str,
    package: &str,
    entity: &str,
) -> PullConfig {
    let durable = format!(
        "application_{}_{}_{}",
        application.len(),
        subject_token(application),
        wamn_materializer::workflow::entity_durable(package, entity),
    );
    let filter = wamn_registry::name::event_subject(
        &coordinates.org,
        &coordinates.project,
        &coordinates.environment,
        &format!("{}.>", subject_token(entity)),
    );
    let mut config = materializer_consumer_config(&durable, &filter, Duration::from_secs(30), 5);
    // A temporarily incompatible host must not exhaust an application's
    // deliveries during a rollout. Concurrent hosts share this durable.
    config.max_deliver = -1;
    config.max_waiting = 64;
    config
}

impl WamnJetstream {
    /// Route a source delivery to durable application consumers without selecting a release.
    pub async fn route_event(
        &self,
        postgres: &WamnPostgres,
        admission: &EventBatchAdmission<'_>,
    ) -> Result<(), JsError> {
        let project_env_key = self.materializer_project_env_key()?;
        let coordinates = &self.event_coordinates;
        let source = SourceEvent::decode(admission.envelope.as_bytes())
            .map_err(|error| JsError::Other(error.to_string()))?;
        if admission.sequence == 0
            || source
                .identity(
                    &project_env_key,
                    &coordinates.project,
                    &coordinates.environment,
                    &[admission.source_event_id],
                )
                .is_none()
        {
            return Err(JsError::Other(
                "invalid event sequence or source identity".into(),
            ));
        }
        if source.source() != (admission.source_package, admission.entity) {
            return Ok(());
        }
        let triggers = postgres
            .event_triggers(
                &coordinates.project,
                &project_env_key,
                &coordinates.environment,
            )
            .await
            .map_err(|error| JsError::Other(error.to_string()))?;
        let applications: BTreeSet<_> = triggers
            .iter()
            .filter(|trigger| {
                trigger.source_package == admission.source_package
                    && trigger.entity == admission.entity
            })
            .map(|trigger| trigger.application.as_str())
            .collect();
        let context = self.ensure_ctx().await?;
        let name = stream_name(
            &coordinates.org,
            &coordinates.project,
            &coordinates.environment,
        );
        let stream = context
            .get_stream(&name)
            .await
            .map_err(|error| map_get_stream_err(&name, &error))?;
        for application in applications {
            let config = application_consumer(
                coordinates,
                application,
                admission.source_package,
                admission.entity,
            );
            match stream
                .consumer_info(config.durable_name.as_deref().expect("application durable"))
                .await
            {
                Ok(info) => {
                    if !consumer_config_matches(&config, &info.config) {
                        return Err(JsError::Other(CONSUMER_CONFIG_DRIFT.into()));
                    }
                }
                Err(error)
                    if error.kind()
                        == async_nats::jetstream::context::ConsumerInfoErrorKind::NotFound =>
                {
                    stream
                        .create_consumer_strict(config)
                        .await
                        .map_err(|error| {
                            JsError::Other(format!("route application event: {error}"))
                        })?;
                }
                Err(error) => {
                    return Err(JsError::Other(format!(
                        "read application consumer: {error}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Admit and acknowledge one bounded batch per application source using the host's release.
    pub async fn admit_application_events(
        &self,
        postgres: &WamnPostgres,
        release: &LoadedRelease,
        admit: impl AsyncFn(
            &EventBatchAdmission<'_>,
        ) -> Result<(), wamn_run_state::run_store::ProductionLeaseError>,
    ) -> Result<(), JsError> {
        let project_env_key = self.materializer_project_env_key()?;
        let coordinates = &self.event_coordinates;
        if release.target().applications.len() != 1
            || release.target().project_env_key != project_env_key
            || release.target().environment != coordinates.environment.as_ref()
        {
            return Err(JsError::Other(UNREGISTERED_SOURCE.into()));
        }
        let triggers = postgres
            .event_triggers(
                &coordinates.project,
                &project_env_key,
                &coordinates.environment,
            )
            .await
            .map_err(|error| JsError::Other(error.to_string()))?;
        let sources: BTreeSet<_> = triggers
            .iter()
            .filter(|trigger| {
                release
                    .target()
                    .applications
                    .contains_key(&trigger.application)
            })
            .map(|trigger| {
                (
                    trigger.application.as_str(),
                    trigger.source_package.as_str(),
                    trigger.entity.as_str(),
                )
            })
            .collect();
        let context = self.ensure_ctx().await?;
        let name = stream_name(
            &coordinates.org,
            &coordinates.project,
            &coordinates.environment,
        );
        let stream = context
            .get_stream(&name)
            .await
            .map_err(|error| map_get_stream_err(&name, &error))?;
        for (application, package, entity) in sources {
            let config = application_consumer(coordinates, application, package, entity);
            let consumer = match stream
                .get_consumer::<PullConfig>(
                    config.durable_name.as_deref().expect("application durable"),
                )
                .await
            {
                Ok(consumer) => consumer,
                Err(error)
                    if error
                        .downcast_ref::<async_nats::jetstream::context::ConsumerInfoError>()
                        .is_some_and(|error| {
                            matches!(
                                error.kind(),
                                async_nats::jetstream::context::ConsumerInfoErrorKind::NotFound
                            )
                        }) =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(JsError::Other(format!(
                        "attach application events: {error}"
                    )));
                }
            };
            if !consumer_config_matches(&config, &consumer.cached_info().config) {
                return Err(JsError::Other(CONSUMER_CONFIG_DRIFT.into()));
            }
            let mut messages = consumer
                .fetch()
                .max_messages(64)
                .max_bytes(
                    usize::try_from(wamn_broker::events::MATERIALIZER_MAX_PULL_BYTES)
                        .expect("bounded pull size fits usize"),
                )
                .expires(Duration::from_millis(100))
                .messages()
                .await
                .map_err(|error| JsError::Other(format!("fetch application events: {error}")))?;
            while let Some(message) = messages.next().await {
                let message = message.map_err(|error| JsError::Other(error.to_string()))?;
                let sequence = message
                    .info()
                    .map_err(|error| JsError::Other(error.to_string()))?
                    .stream_sequence;
                let envelope = std::str::from_utf8(&message.payload)
                    .map_err(|error| JsError::Other(error.to_string()))?;
                let headers = message
                    .headers
                    .as_ref()
                    .ok_or_else(|| JsError::Other("event source identity is absent".into()))?;
                let mut ids = headers.get_all(NATS_MESSAGE_ID);
                let id = ids.next().filter(|_| ids.next().is_none()).ok_or_else(|| {
                    JsError::Other("event source identity must have one header".into())
                })?;
                admit(&EventBatchAdmission {
                    source_package: package,
                    entity,
                    envelope,
                    sequence,
                    source_event_id: id.as_str(),
                })
                .await
                .map_err(|error| JsError::Other(error.to_string()))?;
                message
                    .double_ack()
                    .await
                    .map_err(|error| JsError::Other(error.to_string()))?;
            }
        }
        Ok(())
    }
}
