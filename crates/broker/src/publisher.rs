//! JetStream publishing for the change reader.

use anyhow::Context as _;
use async_nats::header::{HeaderMap, NATS_EXPECTED_STREAM, NATS_MESSAGE_ID};
use async_nats::jetstream;
use async_nats::jetstream::context::{PublishError, PublishErrorKind};
use bytes::Bytes;
use wamn_event::reader::EventPublisher;

/// Publish captured rows to an expected JetStream stream.
#[derive(Debug)]
pub struct JetStreamPublisher {
    js: jetstream::Context,
    stream: String,
    max_payload: usize,
}

impl JetStreamPublisher {
    pub fn new(client: async_nats::Client, stream: String) -> Self {
        let max_payload = client.max_payload();
        Self {
            js: jetstream::new(client),
            stream,
            max_payload,
        }
    }
}

impl EventPublisher for JetStreamPublisher {
    type Ack = jetstream::context::PublishAckFuture;

    fn payload_limit(&self, id: &str) -> usize {
        // HPUB counts the protocol header block as part of max_payload.
        let headers = b"NATS/1.0\r\n\r\n".len()
            + AsRef::<str>::as_ref(&NATS_MESSAGE_ID).len()
            + AsRef::<str>::as_ref(&NATS_EXPECTED_STREAM).len()
            + 2 * b": \r\n".len()
            + id.len()
            + self.stream.len();
        self.max_payload.saturating_sub(headers)
    }

    fn is_payload_error(&self, error: &anyhow::Error) -> bool {
        error.chain().any(|source| {
            source
                .downcast_ref::<PublishError>()
                .is_some_and(|error| error.kind() == PublishErrorKind::MaxPayloadExceeded)
                || source
                    .downcast_ref::<jetstream::Error>()
                    .is_some_and(|error| {
                        error.error_code() == jetstream::ErrorCode::STREAM_MESSAGE_EXCEEDS_MAXIMUM
                    })
        })
    }

    async fn send(&self, subject: &str, id: &str, payload: &Bytes) -> anyhow::Result<Self::Ack> {
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, id);
        headers.insert(NATS_EXPECTED_STREAM, self.stream.as_str());
        self.js
            .publish_with_headers(subject.to_owned(), headers, payload.clone())
            .await
            .context("publish")
    }

    async fn settle(&self, ack: Self::Ack) -> anyhow::Result<bool> {
        let ack = ack.await.context("ack")?;
        Ok(ack.duplicate)
    }
}
