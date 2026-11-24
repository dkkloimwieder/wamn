//! Deploy-time NATS stream and capture reads.

use anyhow::Context as _;
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::stream::RawMessageErrorKind;
use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::Duration;
use wamn_event::deploy_broker::{CA_FILE, DeploymentBroker, EventBroker, EventObserver};
use wamn_event::{SourceEvent, verified_source_event_id};
use wamn_registry::Triple;

use crate::nats::{InitialConnect, connect_platform};

#[derive(Debug)]
pub struct NatsDeployment;

#[async_trait::async_trait]
impl DeploymentBroker for NatsDeployment {
    fn credentials_inbox(&self, credentials: &str) -> anyhow::Result<String> {
        crate::nats::credentials_inbox(credentials)
    }

    async fn provision(
        &self,
        url: &str,
        credentials: &str,
        triple: &Triple,
        policy: &EventBroker,
    ) -> anyhow::Result<()> {
        let broker = admin(url, credentials).await?;
        crate::streams::provision(
            &broker,
            triple,
            policy.stream_replicas,
            Duration::from_secs(policy.dup_window_secs),
            &[],
        )
        .await
    }

    async fn remove(&self, url: &str, credentials: &str, stream: &str) -> anyhow::Result<bool> {
        crate::streams::delete(&admin(url, credentials).await?, stream).await
    }

    async fn last_capture_event(
        &self,
        observer: &EventObserver,
        stream: &str,
        triple: &Triple,
    ) -> anyhow::Result<Option<(u64, DateTime<Utc>)>> {
        let client = connect_platform(&observer.nats_url, &observer.nats_tls_ca, &observer.creds)
            .await
            .context("connect the event observer credential")?;
        let broker = async_nats::jetstream::new(client);
        last_cdc_event(&broker, stream, &triple.project, &triple.env).await
    }
}

async fn admin(url: &str, credentials: &str) -> anyhow::Result<async_nats::jetstream::Context> {
    let options = crate::nats::user_options(async_nats::ConnectOptions::new(), credentials)?;
    let options = crate::nats::tls_options(options, Some(Path::new(CA_FILE)));
    let client = crate::nats::connect(options, url, InitialConnect::Once).await?;
    Ok(async_nats::jetstream::new(client))
}

/// The LSN and `commit_ts` of the newest CDC event on the source stream. `None`
/// when the stream holds no CDC event.
async fn last_cdc_event(
    broker: &async_nats::jetstream::Context,
    stream: &str,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> anyhow::Result<Option<(u64, DateTime<Utc>)>> {
    let source = broker
        .get_stream(stream)
        .await
        .with_context(|| format!("read the source stream {stream}"))?;
    let state = &source.cached_info().state;
    if state.messages == 0 {
        return Ok(None);
    }
    let mut sequence = state.last_sequence;
    while sequence >= state.first_sequence.max(1) {
        match source.get_raw_message(sequence).await {
            Ok(message) => {
                let ids = message
                    .headers
                    .get_all(NATS_MESSAGE_ID)
                    .map(async_nats::HeaderValue::as_str)
                    .collect::<Vec<_>>();
                // Derived records and old coordinates do not establish a CDC position.
                if let Ok(SourceEvent::Cdc(envelope)) = SourceEvent::decode(&message.payload)
                    && verified_source_event_id(project, env, &envelope, &ids).is_some()
                {
                    return Ok(Some((envelope.identity.commit_lsn, envelope.commit_ts)));
                }
            }
            Err(error) if error.kind() == RawMessageErrorKind::NoMessageFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read the source stream at sequence {sequence}"));
            }
        }
        sequence -= 1;
    }
    Ok(None)
}
