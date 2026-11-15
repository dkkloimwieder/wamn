//! Read retained broker advisories and report whether their source payloads exist.

mod plan;
use plan::broker_token;

use anyhow::{Context as _, bail};
use async_nats::jetstream::stream::RawMessageErrorKind;
use serde::{Deserialize, Serialize};
use wamn_event_wire::delivery_advisory_stream;

/// Inputs of one read of the retained delivery advisories of one consumer.
#[derive(Debug)]
pub struct EventAdvisoriesRequest {
    /// The NATS of the retained advisory stream, with the client certificate.
    pub nats: crate::nats::PlatformNats,

    /// Exact source stream from the broker advisory.
    pub stream: String,

    /// Exact durable consumer from the broker advisory.
    pub consumer: String,

    /// Maximum advisory records to read.
    pub limit: usize,
}

/// Whether the message an advisory names is still in its source stream.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum SourcePayload {
    /// The source stream still holds the message, with this subject and body.
    Available {
        /// The subject the source message was published on.
        subject: String,
        /// The source message body, exactly as the broker retained it.
        body: Vec<u8>,
    },
    /// The source stream no longer holds the message.
    Unavailable,
}

/// One retained advisory and the source payload it names.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct OperatorRecord {
    /// Sequence of the advisory in the retained advisory stream.
    pub advisory_sequence: u64,
    /// The decoded broker delivery advisory.
    pub advisory: DeliveryAdvisory,
    /// The source payload the advisory names, if the stream still holds it.
    pub source: SourcePayload,
}

async fn source_payload(
    jetstream: &async_nats::jetstream::Context,
    advisory: &DeliveryAdvisory,
) -> anyhow::Result<SourcePayload> {
    let stream = jetstream
        .get_stream_no_info(&advisory.stream)
        .await
        .context("resolve the advisory source stream")?;
    match stream.get_raw_message(advisory.stream_seq).await {
        Ok(message) => Ok(SourcePayload::Available {
            subject: message.subject.to_string(),
            body: message.payload.to_vec(),
        }),
        Err(error) => match error.kind() {
            RawMessageErrorKind::NoMessageFound => Ok(SourcePayload::Unavailable),
            RawMessageErrorKind::JetStream(server) if server.code() == 404 => {
                Ok(SourcePayload::Unavailable)
            }
            _ => Err(error).context("read the advisory source payload"),
        },
    }
}

/// Read the retained delivery advisories of one consumer, newest sequence last,
/// each with the source payload it names.
pub async fn read_retained_advisories(
    request: EventAdvisoriesRequest,
) -> anyhow::Result<Vec<OperatorRecord>> {
    if request.limit == 0 || !broker_token(&request.stream) || !broker_token(&request.consumer) {
        bail!("the limit must be positive, and the stream and consumer must be exact broker names");
    }
    let client = request
        .nats
        .connect()
        .await
        .context("connect to event-plane NATS")?;
    let jetstream = async_nats::jetstream::new(client);
    let stream = jetstream
        .get_stream(delivery_advisory_stream(&request.stream))
        .await
        .context("open retained delivery advisories")?;
    retained_advisories(
        &jetstream,
        &stream,
        &request.stream,
        &request.consumer,
        request.limit,
    )
    .await
}

async fn next_advisory(
    stream: &async_nats::jetstream::stream::Stream,
    subject: &str,
    sequence: u64,
) -> anyhow::Result<Option<async_nats::jetstream::message::StreamMessage>> {
    match stream
        .get_first_raw_message_by_subject(subject, sequence)
        .await
    {
        Ok(message) => Ok(Some(message)),
        Err(error) => match error.kind() {
            RawMessageErrorKind::NoMessageFound => Ok(None),
            RawMessageErrorKind::JetStream(server) if server.code() == 404 => Ok(None),
            _ => Err(error).context("read the next retained delivery advisory"),
        },
    }
}

/// Read retained advisories through an existing broker connection.
pub async fn retained_advisories(
    jetstream: &async_nats::jetstream::Context,
    stream: &async_nats::jetstream::stream::Stream,
    source_stream: &str,
    consumer: &str,
    limit: usize,
) -> anyhow::Result<Vec<OperatorRecord>> {
    let subjects = ["MAX_DELIVERIES", "MSG_TERMINATED"].map(|type_| {
        wamn_registry::name::nats_subject(&[
            "$JS",
            "EVENT",
            "ADVISORY",
            "CONSUMER",
            type_,
            source_stream,
            consumer,
        ])
    });
    let mut next = [
        next_advisory(stream, &subjects[0], 1).await?,
        next_advisory(stream, &subjects[1], 1).await?,
    ];
    let mut records = Vec::new();
    while records.len() < limit {
        let Some((index, _)) = next
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                message.as_ref().map(|message| (index, message.sequence))
            })
            .min_by_key(|(_, sequence)| *sequence)
        else {
            break;
        };
        let message = next[index].take().expect("selected retained message");
        let advisory = DeliveryAdvisory::from_slice(&message.payload)
            .context("decode the broker delivery advisory")?;
        if advisory.stream != source_stream || advisory.consumer != consumer {
            bail!("broker advisory coordinates differ from the selected stream and consumer");
        }
        let source = source_payload(jetstream, &advisory).await?;
        records.push(OperatorRecord {
            advisory_sequence: message.sequence,
            advisory,
            source,
        });
        if records.len() < limit {
            next[index] = next_advisory(stream, &subjects[index], message.sequence + 1).await?;
        }
    }
    Ok(records)
}

/// Maximum retained advisories on one broker subject.
pub const DELIVERY_ADVISORY_MAX_MESSAGES_PER_SUBJECT: i64 = 1_000;

/// Advisory retention is seven days, independent of source payload retention.
pub const DELIVERY_ADVISORY_MAX_AGE_SECONDS: u64 = 7 * 24 * 60 * 60;

/// The broker's delivery disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryAdvisoryType {
    #[serde(rename = "io.nats.jetstream.advisory.v1.max_deliver")]
    MaxDeliver,
    #[serde(rename = "io.nats.jetstream.advisory.v1.terminated")]
    Terminated,
}

/// A retained broker advisory with no copied source payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryAdvisory {
    #[serde(rename = "type")]
    pub type_: DeliveryAdvisoryType,
    pub id: String,
    pub timestamp: String,
    pub stream: String,
    pub consumer: String,
    pub stream_seq: u64,
    pub deliveries: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl DeliveryAdvisory {
    /// Read the broker's advisory format.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use crate::events::{event_consumer_config, source_stream_config};
    use async_nats::jetstream::consumer::pull::Config as PullConfig;
    use std::time::Duration;
    use wamn_registry::Triple;
    #[test]
    fn changed_stream_and_consumer_declarations_refuse_without_a_broker() {
        use crate::events::{consumer_config_matches, stream_config_matches};
        use async_nats::jetstream::consumer::IntoConsumerConfig as _;

        let target = Triple::new("example", "dev", "fixture");
        let source = source_stream_config(&target, 1, Duration::from_secs(120));
        assert!(stream_config_matches(&source, &source));
        let changed = async_nats::jetstream::stream::Config {
            max_messages: 1,
            ..source.clone()
        };
        assert!(!stream_config_matches(&source, &changed));
        let consumer = event_consumer_config(
            "registered",
            "evt.example.dev.fixture.item.update",
            Duration::from_secs(30),
            2,
        );
        assert!(consumer_config_matches(
            &consumer,
            &consumer.clone().into_consumer_config()
        ));
        let changed = PullConfig {
            max_deliver: 3,
            ..consumer.clone()
        }
        .into_consumer_config();
        assert!(!consumer_config_matches(&consumer, &changed));
    }
}
