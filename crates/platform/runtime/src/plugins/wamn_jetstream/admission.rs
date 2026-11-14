//! Pull and acknowledge declared application consumers.
use super::EventBatchAdmission;
use super::{
    CONSUMER_CONFIG_DRIFT, NATS_MESSAGE_ID, PullConfig, WamnJetstream, consumer_config_matches,
};
use anyhow::Context as _;
use futures_util::StreamExt as _;
use std::time::Duration;

impl WamnJetstream {
    /// Attach only to provisioned consumers. The workflow owner selects and admits events.
    pub async fn admit_application_events(
        &self,
        postgres: &crate::plugins::wamn_postgres::WamnPostgres,
        project: &str,
        release: &wamn_engine::release_manifest::LoadedRelease,
        sources: &[(&str, &str, &str)],
        admit: impl AsyncFn(
            &crate::plugins::wamn_postgres::WamnPostgres,
            &str,
            &wamn_engine::release_manifest::LoadedRelease,
            &EventBatchAdmission<'_>,
        ) -> Result<(), wamn_run_state::run_store::ProductionLeaseError>,
    ) -> anyhow::Result<()> {
        let stream = self
            .stream
            .as_ref()
            .ok_or(anyhow::anyhow!("event connection unavailable"))?;
        let target = wamn_registry::Triple::new(
            self.event_coordinates.org.as_ref(),
            self.event_coordinates.environment.as_ref(),
            self.event_coordinates.project.as_ref(),
        );
        for &(application, package, entity) in sources {
            let config =
                wamn_broker::events::application_consumer(&target, application, package, entity)?;
            let consumer = stream
                .get_consumer::<PullConfig>(
                    config.durable_name.as_deref().expect("declared durable"),
                )
                .await
                .map_err(anyhow::Error::from_boxed)
                .context("attach application events")?;
            if !consumer_config_matches(&config, &consumer.cached_info().config) {
                return Err(anyhow::anyhow!(CONSUMER_CONFIG_DRIFT));
            }
            let mut messages = consumer
                .fetch()
                .max_messages(64)
                .max_bytes(
                    usize::try_from(wamn_broker::events::EVENT_MAX_PULL_BYTES)
                        .expect("bounded pull size fits usize"),
                )
                .expires(Duration::from_millis(100))
                .messages()
                .await
                .context("fetch application events")?;
            while let Some(message) = messages.next().await {
                let message = message
                    .map_err(anyhow::Error::from_boxed)
                    .context("read application event")?;
                let sequence = message
                    .info()
                    .map_err(anyhow::Error::from_boxed)
                    .context("read event delivery metadata")?
                    .stream_sequence;
                let source = wamn_event_wire::SourceEvent::decode(&message.payload);
                let id = message.headers.as_ref().and_then(|headers| {
                    let mut ids = headers.get_all(NATS_MESSAGE_ID);
                    ids.next().filter(|_| ids.next().is_none())
                });
                let verified = source.as_ref().ok().zip(id).is_some_and(|(source, id)| {
                    source
                        .identity(
                            &wamn_registry::project_env_key(
                                &target.org,
                                &target.project,
                                target.env.as_str(),
                            ),
                            &target.project,
                            target.env.as_str(),
                            &[id.as_str()],
                        )
                        .is_some()
                });
                drop(source);
                if !verified {
                    tracing::warn!(sequence, "refuse event with invalid source identity");
                    message
                        .ack_with(async_nats::jetstream::AckKind::Term)
                        .await
                        .map_err(anyhow::Error::from_boxed)
                        .context("terminate invalid event")?;
                    continue;
                }
                let envelope =
                    std::str::from_utf8(&message.payload).context("read event payload")?;
                let id = id.expect("verified source has one identity header");
                admit(
                    postgres,
                    project,
                    release,
                    &EventBatchAdmission {
                        source_package: package,
                        entity,
                        envelope,
                        sequence,
                        source_event_id: id.as_str(),
                    },
                )
                .await
                .context("admit application event")?;
                message
                    .double_ack()
                    .await
                    .map_err(anyhow::Error::from_boxed)
                    .context("acknowledge admitted event")?;
            }
        }
        Ok(())
    }
}
