//! JetStream publishing for the change reader.

use async_nats::header::{HeaderMap, NATS_EXPECTED_STREAM, NATS_MESSAGE_ID};
use async_nats::jetstream;
use bytes::Bytes;
use wamn_event::reader::EventPublisher;

/// Publish captured rows to an expected JetStream stream.
#[derive(Debug)]
pub struct JetStreamPublisher {
    js: jetstream::Context,
    stream: String,
}

impl JetStreamPublisher {
    pub fn new(client: async_nats::Client, stream: String) -> Self {
        Self {
            js: jetstream::new(client),
            stream,
        }
    }
}

impl EventPublisher for JetStreamPublisher {
    type Ack = jetstream::context::PublishAckFuture;

    async fn send(&self, subject: &str, id: &str, payload: &Bytes) -> anyhow::Result<Self::Ack> {
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, id);
        headers.insert(NATS_EXPECTED_STREAM, self.stream.as_str());
        self.js
            .publish_with_headers(subject.to_owned(), headers, payload.clone())
            .await
            .map_err(|error| anyhow::anyhow!("publish: {error}"))
    }

    async fn settle(&self, ack: Self::Ack) -> anyhow::Result<bool> {
        let ack = ack.await.map_err(|error| anyhow::anyhow!("ack: {error}"))?;
        Ok(ack.duplicate)
    }
}
