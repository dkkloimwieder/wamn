//! Read retained broker advisories and report whether their source payloads exist.

mod plan;
use plan::broker_token;

#[cfg(test)]
use std::time::Duration;

use anyhow::{Context as _, bail};
#[cfg(test)]
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::stream::RawMessageErrorKind;
#[cfg(test)]
use futures_util::StreamExt as _;
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

async fn retained_advisories(
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

#[cfg(test)]
mod tests {
    use crate::events::{event_consumer_config, source_stream_config};
    use anyhow::ensure;
    use async_nats::jetstream::consumer::Consumer;
    use async_nats::jetstream::{AckKind, Message};
    use wamn_event_wire::stream_name;
    use wamn_registry::Triple;

    use super::*;

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

    #[test]
    fn broker_permissions_confine_each_role_to_its_environment() -> anyhow::Result<()> {
        use crate::events::advisory_stream_config;
        use wamn_test_infrastructure::{event_broker, scratch::ScratchRoot};

        let root = ScratchRoot::create()?;
        let target = Triple::new("example", "dev", "fixture");
        let source = source_stream_config(&target, 1, Duration::from_secs(120));
        let advisory = advisory_stream_config(&target, 1);
        let consumer = event_consumer_config(
            "registered",
            "evt.example.dev.fixture.item.update",
            Duration::from_secs(30),
            2,
        );
        let broker = event_broker::prepare(
            root.path(),
            &target,
            "project",
            &source,
            &advisory,
            &[consumer],
        )?;
        let configuration: serde_json::Value =
            serde_json::from_slice(&std::fs::read(broker.configuration)?)?;
        let users = configuration["authorization"]["users"].as_array().unwrap();
        let allows = |user: &serde_json::Value, operation: &str, subject: &str| {
            user["permissions"][operation]["allow"]
                .as_array()
                .unwrap()
                .iter()
                .any(|pattern| {
                    let mut tokens = subject.split('.');
                    for token in pattern.as_str().unwrap().split('.') {
                        if token == ">" {
                            return tokens.next().is_some();
                        }
                        let Some(actual) = tokens.next() else {
                            return false;
                        };
                        if token != "*" && token != actual {
                            return false;
                        }
                    }
                    tokens.next().is_none()
                })
        };
        for user in users
            .iter()
            .filter(|user| user.get("permissions").is_some())
        {
            let role = user["user"].as_str().unwrap().split('_').next().unwrap();
            for (subject, permitted) in [
                (
                    "evt.example.dev.fixture.item.update".to_owned(),
                    vec!["runtime", "publisher"],
                ),
                (
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "INFO",
                        (source.name).as_ref(),
                    ]),
                    vec![
                        "provisioning",
                        "runtime",
                        "publisher",
                        "materializer",
                        "observer",
                    ],
                ),
                (
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "MSG",
                        "GET",
                        (source.name).as_ref(),
                    ]),
                    vec!["observer"],
                ),
                (
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "INFO",
                        (source.name).as_ref(),
                        "registered",
                    ]),
                    vec!["provisioning", "runtime", "materializer", "observer"],
                ),
                (
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "MSG",
                        "NEXT",
                        (source.name).as_ref(),
                        "registered",
                    ]),
                    vec!["runtime", "materializer"],
                ),
                (
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "ACK",
                        (source.name).as_ref(),
                        "registered",
                        "1",
                        "1",
                        "1",
                        "1",
                        "0",
                    ]),
                    vec!["runtime", "materializer"],
                ),
            ] {
                assert_eq!(
                    allows(user, "publish", &subject),
                    permitted.contains(&role),
                    "{role}: {subject}"
                );
            }
            for name in [&source.name, &advisory.name] {
                for operation in ["CREATE", "UPDATE", "DELETE"] {
                    let subject = wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        (operation).as_ref(),
                        (name).as_ref(),
                    ]);
                    assert_eq!(
                        allows(user, "publish", &subject),
                        role == "provisioning",
                        "{role}: {subject}"
                    );
                }
            }
            for operation in ["CREATE", "DELETE"] {
                let subject = wamn_registry::name::nats_subject(&[
                    "$JS",
                    "API",
                    "CONSUMER",
                    (operation).as_ref(),
                    (source.name).as_ref(),
                    "registered",
                ]);
                assert_eq!(
                    allows(user, "publish", &subject),
                    role == "provisioning",
                    "{role}: {subject}"
                );
            }
            for foreign in [
                Triple::new("example", "dev", "other"),
                Triple::new("example", "prod", "fixture"),
            ] {
                let stream = source_stream_config(&foreign, 1, Duration::from_secs(120));
                let advisory = advisory_stream_config(&foreign, 1);
                for subject in [
                    stream.subjects[0].replace('>', "item.update"),
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "INFO",
                        (stream.name).as_ref(),
                    ]),
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "MSG",
                        "GET",
                        (stream.name).as_ref(),
                    ]),
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "INFO",
                        (advisory.name).as_ref(),
                    ]),
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "MSG",
                        "GET",
                        (advisory.name).as_ref(),
                    ]),
                    wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "INFO",
                        (stream.name).as_ref(),
                        "registered",
                    ]),
                ] {
                    assert!(!allows(user, "publish", &subject), "{role}: {subject}");
                    assert!(!allows(user, "subscribe", &subject), "{role}: {subject}");
                }
            }
            assert!(allows(
                user,
                "subscribe",
                &wamn_registry::name::nats_subject(&[
                    &wamn_registry::name::nats_token(
                        "_INBOX_",
                        (user["user"].as_str().unwrap()).as_ref(),
                        ""
                    ),
                    "reply"
                ])
            ));
            assert!(!allows(
                user,
                "subscribe",
                "evt.example.dev.fixture.item.update"
            ));
        }
        Ok(())
    }

    async fn fetch_one(consumer: &Consumer<PullConfig>) -> anyhow::Result<Option<Message>> {
        let mut batch = consumer
            .fetch()
            .max_messages(1)
            .expires(Duration::from_millis(200))
            .messages()
            .await?;
        batch
            .next()
            .await
            .transpose()
            .map_err(anyhow::Error::from_boxed)
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_URL"]
    async fn exhausted_delivery_retains_advisory_and_allows_later_messages() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_URL");
        retained_broker_advisory(DeliveryAdvisoryType::MaxDeliver).await
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_URL"]
    async fn terminated_delivery_retains_advisory_after_source_deletion() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_URL");
        retained_broker_advisory(DeliveryAdvisoryType::Terminated).await
    }

    async fn retained_broker_advisory(type_: DeliveryAdvisoryType) -> anyhow::Result<()> {
        wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_URL"]);
        let url = std::env::var("WAMN_NATIVE_C_NATS_URL")?;
        let jetstream = async_nats::jetstream::new(async_nats::connect(url).await?);
        let name = match type_ {
            DeliveryAdvisoryType::MaxDeliver => "exhausted",
            DeliveryAdvisoryType::Terminated => "terminated",
        };
        let target = Triple::new(format!("c-{name}"), "dev", "app");
        let source_name = stream_name(&target.org, &target.project, target.env.as_str());
        let advisory_name = delivery_advisory_stream(&source_name);
        let subject = wamn_registry::name::event_subject(
            &target.org,
            &target.project,
            target.env.as_str(),
            "item.update",
        );
        let consumer = event_consumer_config(name, &subject, Duration::from_millis(100), 2);
        crate::streams::provision(
            &jetstream,
            &target,
            1,
            Duration::from_secs(120),
            &[consumer],
        )
        .await?;
        let result: anyhow::Result<()> = async {
            let source = jetstream.get_stream(&source_name).await?;
            let consumer = source.get_consumer::<PullConfig>(name).await.map_err(anyhow::Error::from_boxed)?;
            let sequence = jetstream.publish(subject.clone(), "source payload".into()).await?.await?.sequence;
            match type_ {
                DeliveryAdvisoryType::MaxDeliver => {
                    for attempt in 1..=2 {
                        let message = fetch_one(&consumer).await?.context("broker omitted the expected retry")?;
                        ensure!(message.info().map_err(anyhow::Error::from_boxed)?.delivered == attempt,
                            "broker attempt differs from the actual delivery count");
                        message.ack_with(AckKind::Nak(None)).await.map_err(anyhow::Error::from_boxed)?;
                    }
                    ensure!(fetch_one(&consumer).await?.is_none(), "broker delivered past max_deliver");
                    let later_sequence = jetstream.publish(subject, "later valid payload".into()).await?.await?.sequence;
                    let later = fetch_one(&consumer).await?.context("exhausted poison blocked the later valid message")?;
                    ensure!(later.info().map_err(anyhow::Error::from_boxed)?.stream_sequence == later_sequence,
                        "consumer redelivered poison instead of the later valid message");
                    later.double_ack().await.map_err(anyhow::Error::from_boxed)?;
                }
                DeliveryAdvisoryType::Terminated => {
                    fetch_one(&consumer).await?.context("broker omitted the terminal delivery")?
                        .ack_with(AckKind::Term).await.map_err(anyhow::Error::from_boxed)?;
                }
            }
            let mut advisories = jetstream.get_stream(&advisory_name).await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while advisories.info().await?.state.messages == 0 {
                ensure!(tokio::time::Instant::now() < deadline, "broker did not retain the delivery advisory");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let records = retained_advisories(&jetstream, &advisories, &source_name, name, 1).await?;
            ensure!(records.len() == 1, "selected consumer advisory is missing");
            let record = &records[0];
            ensure!(record.advisory.type_ == type_ && record.advisory.stream_seq == sequence,
                "advisory does not identify the settled source message");
            let deliveries = match type_ {
                DeliveryAdvisoryType::MaxDeliver => 2,
                DeliveryAdvisoryType::Terminated => 1,
            };
            ensure!(record.advisory.deliveries == deliveries, "advisory lost its actual delivery count");
            ensure!(matches!(&record.source, SourcePayload::Available { body, .. } if body == b"source payload"),
                "retained source payload differs");
            source.delete_message(sequence).await?;
            let records = retained_advisories(&jetstream, &advisories, &source_name, name, 100).await?;
            ensure!(records.len() == 1, "retained advisory was lost after source deletion");
            ensure!(matches!(&records[0].source, SourcePayload::Unavailable),
                "deleted source payload was reported as available");
            ensure!(advisories.info().await?.state.consumer_count == 0, "advisory reads created a consumer");
            Ok(())
        }.await;
        let source_cleanup = jetstream.delete_stream(&source_name).await;
        let advisory_cleanup = jetstream.delete_stream(&advisory_name).await;
        result?;
        source_cleanup?;
        advisory_cleanup?;
        Ok(())
    }

    async fn monitored_broker_client(
        credentials: &wamn_test_infrastructure::event_broker::Credentials,
        server: &str,
    ) -> anyhow::Result<(
        async_nats::Client,
        tokio::sync::mpsc::UnboundedReceiver<async_nats::ServerError>,
    )> {
        let (errors, received) = tokio::sync::mpsc::unbounded_channel();
        let client = credentials
            .options()
            .request_timeout(Some(Duration::from_millis(500)))
            .event_callback(move |event| {
                let errors = errors.clone();
                async move {
                    if let async_nats::Event::ServerError(error) = event {
                        let _ = errors.send(error);
                    }
                }
            })
            .connect(server)
            .await?;
        Ok((client, received))
    }

    async fn require_permission_denial(
        errors: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::ServerError>,
        subject: &str,
    ) -> anyhow::Result<()> {
        let error = tokio::time::timeout(Duration::from_secs(2), errors.recv())
            .await?
            .context("broker omitted its permission refusal")?;
        let message = error.to_string();
        ensure!(
            message
                .to_ascii_lowercase()
                .contains("permissions violation")
                && message.contains(subject),
            "broker did not refuse the selected subject {subject}: {message}"
        );
        Ok(())
    }

    async fn interrupted_delivery_redelivers(
        server: &str,
        credentials: &wamn_test_infrastructure::event_broker::Credentials,
        publisher: &async_nats::jetstream::Context,
        source_name: &str,
        consumer: &PullConfig,
    ) -> anyhow::Result<()> {
        use wamn_test_infrastructure::event_broker;

        let name = consumer
            .durable_name
            .as_deref()
            .context("interrupted consumer has a name")?;
        let sequence = publisher
            .publish(
                consumer.filter_subject.clone(),
                "interrupted payload".into(),
            )
            .await?
            .await?
            .sequence;
        let (closed, mut closure) = tokio::sync::mpsc::unbounded_channel();
        let first_client = credentials
            .options()
            .event_callback(move |event| {
                let closed = closed.clone();
                async move {
                    if matches!(event, async_nats::Event::Closed) {
                        let _ = closed.send(());
                    }
                }
            })
            .connect(server)
            .await?;
        let first_context = async_nats::jetstream::new(first_client.clone());
        let first_stream = first_context.get_stream(source_name).await?;
        let first_consumer = first_stream
            .get_consumer::<PullConfig>(name)
            .await
            .map_err(anyhow::Error::from_boxed)?;
        let first = fetch_one(&first_consumer)
            .await?
            .context("the first delivery did not arrive")?;
        let info = first.info().map_err(anyhow::Error::from_boxed)?;
        ensure!(
            info.stream_sequence == sequence
                && info.delivered == 1
                && first.payload.as_ref() == b"interrupted payload",
            "the first interrupted delivery differs from the published message"
        );
        let pending = first_stream.consumer_info(name).await?;
        ensure!(
            pending.num_ack_pending == 1 && pending.ack_floor.stream_sequence < sequence,
            "the broker did not retain the outstanding acknowledgement"
        );
        drop(first);
        first_client.drain().await?;
        tokio::time::timeout(Duration::from_secs(5), closure.recv())
            .await
            .context("the interrupted materializer connection did not close")?
            .context("the interrupted materializer omitted its native closure event")?;
        ensure!(
            first_client.flush().await.is_err(),
            "the closed materializer client still accepts commands"
        );
        drop(first_consumer);
        drop(first_stream);
        drop(first_context);
        drop(first_client);

        let replacement =
            async_nats::jetstream::new(event_broker::connect(credentials, server).await?);
        let stream = replacement.get_stream(source_name).await?;
        let attached = stream
            .get_consumer::<PullConfig>(name)
            .await
            .map_err(anyhow::Error::from_boxed)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let repeated = loop {
            if let Some(message) = fetch_one(&attached).await? {
                break message;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "the broker did not redeliver the unacknowledged message"
            );
        };
        let info = repeated.info().map_err(anyhow::Error::from_boxed)?;
        ensure!(
            info.stream_sequence == sequence
                && info.delivered == 2
                && repeated.payload.as_ref() == b"interrupted payload",
            "the replacement consumer did not receive the same second delivery"
        );
        repeated
            .double_ack()
            .await
            .map_err(anyhow::Error::from_boxed)?;
        let settled = stream.consumer_info(name).await?;
        ensure!(
            settled.num_ack_pending == 0 && settled.ack_floor.stream_sequence == sequence,
            "the confirmed acknowledgement did not settle the repeated delivery"
        );
        let later_sequence = publisher
            .publish(
                consumer.filter_subject.clone(),
                "later interrupted payload".into(),
            )
            .await?
            .await?
            .sequence;
        let later = fetch_one(&attached)
            .await?
            .context("interrupted delivery blocked later progress")?;
        ensure!(
            later
                .info()
                .map_err(anyhow::Error::from_boxed)?
                .stream_sequence
                == later_sequence
                && later.payload.as_ref() == b"later interrupted payload",
            "the replacement consumer did not reach the next message"
        );
        later
            .double_ack()
            .await
            .map_err(anyhow::Error::from_boxed)?;
        Ok(())
    }

    async fn large_messages_obey_pull_and_ack_limits(
        publisher: &async_nats::jetstream::Context,
        materializer: &async_nats::jetstream::Context,
        source_name: &str,
        consumer: &PullConfig,
    ) -> anyhow::Result<()> {
        use crate::events::EVENT_MAX_PULL_BYTES;

        const PAYLOAD_BYTES: usize = 1024 * 1024 - 1024;
        const MESSAGE_COUNT: usize = 65;
        let name = consumer
            .durable_name
            .as_deref()
            .context("pressure consumer has a name")?;
        let stream = materializer.get_stream(source_name).await?;
        let attached = stream
            .get_consumer::<PullConfig>(name)
            .await
            .map_err(anyhow::Error::from_boxed)?;
        let declared = stream.consumer_info(name).await?;
        ensure!(
            declared.config.max_ack_pending == 64
                && declared.config.max_batch == 64
                && declared.config.max_bytes == EVENT_MAX_PULL_BYTES,
            "the pressure consumer must retain the declared materializer bounds"
        );
        let mut sequences = Vec::new();
        for index in 0..MESSAGE_COUNT {
            let mut payload = vec![0xa5; PAYLOAD_BYTES];
            payload[..8].copy_from_slice(&(index as u64).to_be_bytes());
            sequences.push(
                publisher
                    .publish(consumer.filter_subject.clone(), payload.into())
                    .await?
                    .await?
                    .sequence,
            );
        }
        let max_pull_bytes = usize::try_from(EVENT_MAX_PULL_BYTES)?;
        let mut pending = Vec::new();
        let mut byte_limited_pulls = 0;
        while pending.len() < 64 {
            let mut messages = attached
                .fetch()
                .max_messages(64 - pending.len())
                .max_bytes(max_pull_bytes)
                .expires(Duration::from_millis(200))
                .messages()
                .await?;
            let mut bytes = 0;
            let before = pending.len();
            while let Some(message) = messages.next().await {
                let message = match message {
                    Ok(message) => message,
                    // The pinned client exposes the broker's byte boundary as an I/O error.
                    Err(error)
                        if error.to_string()
                            == r#"error while processing messages from the stream: 409, Some("Message Size Exceeds MaxBytes")"# =>
                    {
                        ensure!(
                            bytes > 0
                                && bytes <= max_pull_bytes
                                && max_pull_bytes - bytes < PAYLOAD_BYTES,
                            "the broker stopped before filling the declared byte limit"
                        );
                        byte_limited_pulls += 1;
                        break;
                    }
                    Err(error) => return Err(anyhow::Error::from_boxed(error)),
                };
                let index = pending.len();
                ensure!(
                    index < 64,
                    "the native pull exceeded the pending acknowledgement bound"
                );
                let info = message.info().map_err(anyhow::Error::from_boxed)?;
                ensure!(
                    info.stream_sequence == sequences[index]
                        && info.delivered == 1
                        && message.payload.len() == PAYLOAD_BYTES
                        && message.payload[..8] == (index as u64).to_be_bytes()
                        && message.payload[8..].iter().all(|byte| *byte == 0xa5),
                    "a large payload or its delivery identity changed"
                );
                bytes += message.payload.len();
                pending.push(message);
            }
            ensure!(
                pending.len() > before && bytes <= max_pull_bytes,
                "the native pull exceeded its byte bound or made no progress"
            );
            let observed = stream.consumer_info(name).await?.num_ack_pending;
            ensure!(
                observed <= 64,
                "the broker exceeded its acknowledgement limit"
            );
        }
        ensure!(
            byte_limited_pulls > 0,
            "the pressure case did not reach the native pull byte boundary"
        );
        let full = stream.consumer_info(name).await?;
        ensure!(
            full.num_ack_pending == 64 && full.num_pending == 1,
            "the broker must hold exactly 64 acknowledgements and one undelivered message"
        );
        ensure!(
            fetch_one(&attached).await?.is_none(),
            "the broker delivered past max_ack_pending"
        );
        for message in pending {
            message
                .double_ack()
                .await
                .map_err(anyhow::Error::from_boxed)?;
        }
        let last = fetch_one(&attached)
            .await?
            .context("acknowledgements did not release later delivery")?;
        ensure!(
            last.info()
                .map_err(anyhow::Error::from_boxed)?
                .stream_sequence
                == sequences[64]
                && last.payload.len() == PAYLOAD_BYTES
                && last.payload[..8] == 64u64.to_be_bytes()
                && last.payload[8..].iter().all(|byte| *byte == 0xa5),
            "the final large message changed"
        );
        last.double_ack().await.map_err(anyhow::Error::from_boxed)?;
        let settled = stream.consumer_info(name).await?;
        ensure!(
            settled.num_ack_pending == 0
                && settled.num_pending == 0
                && settled.ack_floor.stream_sequence == sequences[64],
            "pressure did not settle after later progress"
        );
        Ok(())
    }

    async fn source_time_expiry_preserves_advisory(
        manager: &async_nats::jetstream::Context,
        publisher: &async_nats::jetstream::Context,
        materializer: &async_nats::jetstream::Context,
        observer: &async_nats::jetstream::Context,
        source: &async_nats::jetstream::stream::Config,
        advisory: &async_nats::jetstream::stream::Config,
        consumer: &PullConfig,
    ) -> anyhow::Result<()> {
        let name = consumer
            .durable_name
            .as_deref()
            .context("expiry consumer has a name")?;
        let declared = async_nats::jetstream::stream::Config {
            max_age: Duration::from_secs(2),
            duplicate_window: Duration::from_secs(1),
            ..source.clone()
        };
        let retained_source = manager.update_stream(declared.clone()).await?;
        ensure!(
            retained_source.config == declared,
            "the owned source expiry policy differs from its declaration"
        );
        let attached = materializer
            .get_stream(&source.name)
            .await?
            .get_consumer::<PullConfig>(name)
            .await
            .map_err(anyhow::Error::from_boxed)?;
        let sequence = publisher
            .publish(
                consumer.filter_subject.clone(),
                "expiring source payload".into(),
            )
            .await?
            .await?
            .sequence;
        let message = fetch_one(&attached)
            .await?
            .context("the expiring source delivery did not arrive")?;
        ensure!(
            message
                .info()
                .map_err(anyhow::Error::from_boxed)?
                .stream_sequence
                == sequence,
            "the expiry case received another source message"
        );
        message
            .ack_with(AckKind::Term)
            .await
            .map_err(anyhow::Error::from_boxed)?;
        let retained = observer.get_stream(&advisory.name).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let initial = loop {
            let records = retained_advisories(observer, &retained, &source.name, name, 2).await?;
            if let Some(record) = records.into_iter().next() {
                break record;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "the expiry termination advisory did not arrive"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        ensure!(
            initial.advisory.type_ == DeliveryAdvisoryType::Terminated
                && initial.advisory.stream_seq == sequence
                && initial.advisory.deliveries == 1
                && matches!(&initial.source, SourcePayload::Available { body, .. } if body == b"expiring source payload"),
            "the initial expiry advisory must still identify its actual available source payload"
        );
        loop {
            let records = retained_advisories(observer, &retained, &source.name, name, 2).await?;
            ensure!(
                records.len() == 1
                    && records[0].advisory_sequence == initial.advisory_sequence
                    && records[0].advisory.stream_seq == sequence
                    && records[0].advisory.type_ == DeliveryAdvisoryType::Terminated,
                "source expiry changed or removed the retained termination advisory"
            );
            if matches!(records[0].source, SourcePayload::Unavailable) {
                break;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "the source message did not expire under the declared age limit"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let source_state = observer.get_stream(&source.name).await?;
        ensure!(
            source_state.cached_info().config.max_age == Duration::from_secs(2)
                && source_state.cached_info().state.messages == 0,
            "the broker did not expire the owned source messages"
        );
        let advisory_state = observer.get_stream(&advisory.name).await?;
        ensure!(
            advisory_state.cached_info().config == *advisory
                && advisory_state.cached_info().state.consumer_count == 0,
            "expiry changed advisory retention or monitoring created a consumer"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn targeted_credentials_allow_delivery_and_monitoring() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        targeted_broker_behavior(async |fixture| {
            let server = &fixture.server;
            let broker = &fixture.brokers[0];
            let (source, advisory, consumer, consumers) = &fixture.declarations[0];
            let publisher = async_nats::jetstream::new(wamn_test_infrastructure::event_broker::connect(&broker.publisher, server).await?);
            let materializer = async_nats::jetstream::new(wamn_test_infrastructure::event_broker::connect(&broker.materializer, server).await?);
            let (observer_client, _) = monitored_broker_client(&broker.observer, server).await?;
            let mut observer = async_nats::jetstream::new(observer_client);
            observer.set_timeout(Duration::from_millis(500));
            fixture.runtime().await?;
            let runtime = async_nats::jetstream::new(wamn_test_infrastructure::event_broker::connect(&broker.runtime, server).await?);
            let attached = runtime.get_stream(&source.name).await?.get_consumer::<PullConfig>("registered").await.map_err(anyhow::Error::from_boxed)?;
            let sequence = runtime.publish(consumer.filter_subject.clone(), "runtime payload".into()).await?.await?.sequence;
            let message = fetch_one(&attached).await?.context("materializer did not read runtime publication")?;
            ensure!(message.payload.as_ref() == b"runtime payload", "runtime payload changed");
            message.ack_with(AckKind::Term).await.map_err(anyhow::Error::from_boxed)?;
            publisher.publish(consumer.filter_subject.clone(), "publisher payload".into()).await?.await?;
            let message = fetch_one(&attached).await?.context("materializer did not read publisher publication")?;
            ensure!(message.payload.as_ref() == b"publisher payload", "publisher payload changed");
            message.double_ack().await.map_err(anyhow::Error::from_boxed)?;
            let other = materializer.get_stream(&source.name).await?
                .get_consumer::<PullConfig>("interrupted").await.map_err(anyhow::Error::from_boxed)?;
            publisher.publish(consumers[1].filter_subject.clone(), "other consumer payload".into()).await?.await?;
            fetch_one(&other).await?.context("materializer did not read the other consumer payload")?
                .ack_with(AckKind::Term).await.map_err(anyhow::Error::from_boxed)?;
            let mut retained = observer.get_stream(&advisory.name).await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while retained.info().await?.state.messages < 2 {
                ensure!(tokio::time::Instant::now() < deadline, "both consumers must retain their termination metadata");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let records = retained_advisories(&observer, &retained, &source.name, "registered", 2).await?;
            ensure!(records.len() == 1 && records[0].advisory.stream_seq == sequence, "observer read another delivery");
            ensure!(matches!(&records[0].source, SourcePayload::Available { body, .. } if body == b"runtime payload"), "observer lost its permitted source payload");
            Ok(())
        })
        .await
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn runtime_credentials_refuse_broker_management() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        targeted_broker_behavior(async |fixture| {
            let server = &fixture.server;
            let broker = &fixture.brokers[0];
            let (source, _, consumer, _) = &fixture.declarations[0];
            for credentials in [&broker.runtime, &broker.publisher, &broker.materializer] {
                let (client, mut errors) = monitored_broker_client(credentials, server).await?;
                let mut restricted = async_nats::jetstream::new(client);
                restricted.set_timeout(Duration::from_millis(500));
                ensure!(
                    restricted.create_stream(source.clone()).await.is_err(),
                    "runtime created a stream"
                );
                require_permission_denial(
                    &mut errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "CREATE",
                        (source.name).as_ref(),
                    ]),
                )
                .await?;
                ensure!(
                    restricted.update_stream(source.clone()).await.is_err(),
                    "runtime updated a stream"
                );
                require_permission_denial(
                    &mut errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "UPDATE",
                        (source.name).as_ref(),
                    ]),
                )
                .await?;
                ensure!(
                    restricted.delete_stream(&source.name).await.is_err(),
                    "runtime deleted a stream"
                );
                require_permission_denial(
                    &mut errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        "DELETE",
                        (source.name).as_ref(),
                    ]),
                )
                .await?;
                let stream = restricted.get_stream(&source.name).await?;
                ensure!(
                    stream
                        .create_consumer_strict(consumer.clone())
                        .await
                        .is_err(),
                    "runtime created a consumer"
                );
                require_permission_denial(
                    &mut errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "CREATE",
                        (source.name).as_ref(),
                        "registered",
                    ]),
                )
                .await?;
                ensure!(
                    stream.update_consumer(consumer.clone()).await.is_err(),
                    "runtime updated a consumer"
                );
                require_permission_denial(
                    &mut errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "CREATE",
                        (source.name).as_ref(),
                        "registered",
                    ]),
                )
                .await?;
                ensure!(
                    stream.delete_consumer("registered").await.is_err(),
                    "runtime deleted a consumer"
                );
                require_permission_denial(
                    &mut errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "DELETE",
                        (source.name).as_ref(),
                        "registered",
                    ]),
                )
                .await?;
            }
            Ok(())
        })
        .await
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn targeted_credentials_refuse_foreign_environments() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        targeted_broker_behavior(async |fixture| {
            let server = &fixture.server;
            let broker = &fixture.brokers[0];
            let (observer_client, mut observer_errors) =
                monitored_broker_client(&broker.observer, server).await?;
            let mut observer = async_nats::jetstream::new(observer_client);
            observer.set_timeout(Duration::from_millis(500));
            let declarations = &fixture.declarations;
            let (runtime_client, mut runtime_errors) =
                monitored_broker_client(&broker.runtime, server).await?;
            let mut restricted_runtime = async_nats::jetstream::new(runtime_client.clone());
            restricted_runtime.set_timeout(Duration::from_millis(500));
            let (materializer_client, mut materializer_errors) =
                monitored_broker_client(&broker.materializer, server).await?;
            let mut restricted_materializer = async_nats::jetstream::new(materializer_client);
            restricted_materializer.set_timeout(Duration::from_millis(500));
            for (foreign_source, foreign_advisory, foreign_consumer, _) in &declarations[1..] {
                let publication = restricted_runtime
                    .publish(
                        foreign_consumer.filter_subject.clone(),
                        "foreign payload".into(),
                    )
                    .await;
                let refused = match publication {
                    Ok(acknowledgement) => acknowledgement.await.is_err(),
                    Err(_) => true,
                };
                ensure!(refused, "runtime published into another environment");
                require_permission_denial(&mut runtime_errors, &foreign_consumer.filter_subject)
                    .await?;
                let subscription = runtime_client
                    .subscribe(foreign_source.subjects[0].clone())
                    .await?;
                runtime_client.flush().await?;
                require_permission_denial(&mut runtime_errors, &foreign_source.subjects[0]).await?;
                drop(subscription);
                let foreign = restricted_runtime
                    .get_stream_no_info(&foreign_source.name)
                    .await?;
                ensure!(
                    foreign
                        .get_consumer::<PullConfig>("registered")
                        .await
                        .is_err(),
                    "runtime attached to a foreign consumer"
                );
                require_permission_denial(
                    &mut runtime_errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "INFO",
                        (foreign_source.name).as_ref(),
                        "registered",
                    ]),
                )
                .await?;
                let foreign = restricted_materializer
                    .get_stream_no_info(&foreign_source.name)
                    .await?;
                ensure!(
                    foreign
                        .get_consumer::<PullConfig>("registered")
                        .await
                        .is_err(),
                    "materializer attached to a foreign consumer"
                );
                require_permission_denial(
                    &mut materializer_errors,
                    &wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "CONSUMER",
                        "INFO",
                        (foreign_source.name).as_ref(),
                        "registered",
                    ]),
                )
                .await?;
                for name in [&foreign_source.name, &foreign_advisory.name] {
                    ensure!(
                        observer.get_stream(name).await.is_err(),
                        "observer read foreign stream metadata"
                    );
                    require_permission_denial(
                        &mut observer_errors,
                        &wamn_registry::name::nats_subject(&[
                            "$JS",
                            "API",
                            "STREAM",
                            "INFO",
                            (name).as_ref(),
                        ]),
                    )
                    .await?;
                    let foreign = observer.get_stream_no_info(name).await?;
                    ensure!(
                        foreign.get_raw_message(1).await.is_err(),
                        "observer read foreign retained data"
                    );
                    require_permission_denial(
                        &mut observer_errors,
                        &wamn_registry::name::nats_subject(&[
                            "$JS",
                            "API",
                            "STREAM",
                            "MSG",
                            "GET",
                            (name).as_ref(),
                        ]),
                    )
                    .await?;
                }
            }
            Ok(())
        })
        .await
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn activation_and_provisioning_refuse_changed_broker_configuration() -> anyhow::Result<()>
    {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        targeted_broker_behavior(async |fixture| {
            let (source, _, consumer, _) = &fixture.declarations[0];
            let managers = &fixture.managers;
            let targets = &fixture.targets;
            let manager = &managers[0];
            manager
                .update_stream(async_nats::jetstream::stream::Config {
                    max_messages: 2,
                    ..source.clone()
                })
                .await?;
            ensure!(
                fixture.runtime().await.is_err(),
                "runtime accepted changed stream configuration"
            );
            ensure!(
                crate::streams::provision(
                    manager,
                    &targets[0],
                    1,
                    Duration::from_secs(120),
                    std::slice::from_ref(consumer)
                )
                .await
                .is_err(),
                "provisioning accepted changed stream configuration"
            );
            ensure!(
                manager
                    .get_stream(&source.name)
                    .await?
                    .cached_info()
                    .config
                    .max_messages
                    == 2,
                "runtime reconfigured the stream"
            );
            manager.update_stream(source.clone()).await?;
            let stream = manager.get_stream(&source.name).await?;
            stream
                .update_consumer(PullConfig {
                    max_deliver: 3,
                    ..consumer.clone()
                })
                .await?;
            ensure!(
                crate::streams::provision(
                    manager,
                    &targets[0],
                    1,
                    Duration::from_secs(120),
                    std::slice::from_ref(consumer)
                )
                .await
                .is_err(),
                "provisioning accepted changed consumer configuration"
            );
            ensure!(
                stream.consumer_info("registered").await?.config.max_deliver == 3,
                "provisioning reconfigured the consumer"
            );
            stream.update_consumer(consumer.clone()).await?;
            fixture
                .runtime()
                .await
                .map_err(|error| anyhow::anyhow!("restored activation failed: {error:?}"))?;
            Ok(())
        })
        .await
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn interrupted_acknowledgement_redelivers_and_allows_later_messages() -> anyhow::Result<()>
    {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        targeted_broker_behavior(async |fixture| {
            let server = &fixture.server;
            let broker = &fixture.brokers[0];
            let (source, _, _, consumers) = &fixture.declarations[0];
            let publisher = async_nats::jetstream::new(
                wamn_test_infrastructure::event_broker::connect(&broker.publisher, server).await?,
            );
            interrupted_delivery_redelivers(
                server,
                &broker.materializer,
                &publisher,
                &source.name,
                &consumers[1],
            )
            .await?;
            Ok(())
        })
        .await
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn large_messages_obey_broker_pull_and_ack_limits() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        targeted_broker_behavior(async |fixture| {
            let server = &fixture.server;
            let broker = &fixture.brokers[0];
            let (source, _, _, consumers) = &fixture.declarations[0];
            let publisher = async_nats::jetstream::new(
                wamn_test_infrastructure::event_broker::connect(&broker.publisher, server).await?,
            );
            let materializer = async_nats::jetstream::new(
                wamn_test_infrastructure::event_broker::connect(&broker.materializer, server)
                    .await?,
            );
            large_messages_obey_pull_and_ack_limits(
                &publisher,
                &materializer,
                &source.name,
                &consumers[2],
            )
            .await?;
            Ok(())
        })
        .await
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn source_time_expiry_keeps_retained_advisories() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
        targeted_broker_behavior(async |fixture| {
            let server = &fixture.server;
            let broker = &fixture.brokers[0];
            let (source, advisory, _, consumers) = &fixture.declarations[0];
            let publisher = async_nats::jetstream::new(
                wamn_test_infrastructure::event_broker::connect(&broker.publisher, server).await?,
            );
            let materializer = async_nats::jetstream::new(
                wamn_test_infrastructure::event_broker::connect(&broker.materializer, server)
                    .await?,
            );
            let (observer_client, _) = monitored_broker_client(&broker.observer, server).await?;
            let mut observer = async_nats::jetstream::new(observer_client);
            observer.set_timeout(Duration::from_millis(500));
            let managers = &fixture.managers;
            source_time_expiry_preserves_advisory(
                &managers[0],
                &publisher,
                &materializer,
                &observer,
                source,
                advisory,
                &consumers[3],
            )
            .await?;
            Ok(())
        })
        .await
    }

    type BrokerDeclaration = (
        async_nats::jetstream::stream::Config,
        async_nats::jetstream::stream::Config,
        PullConfig,
        Vec<PullConfig>,
    );

    struct TargetedBroker {
        server: String,
        targets: [Triple; 3],
        brokers: Vec<wamn_test_infrastructure::event_broker::EventBroker>,
        declarations: Vec<BrokerDeclaration>,
        managers: Vec<async_nats::jetstream::Context>,
    }

    impl TargetedBroker {
        async fn runtime(
            &self,
        ) -> anyhow::Result<wamn_runtime::plugins::wamn_jetstream::WamnJetstream> {
            use wamn_runtime::plugins::wamn_jetstream::{WamnJetstream, WamnJetstreamConfig};
            let broker = &self.brokers[0];
            let (client, _) = monitored_broker_client(&broker.runtime, &self.server).await?;
            Ok(WamnJetstream::new(WamnJetstreamConfig {
                nats: Some(client),
                event_target: Some(self.targets[0].clone()),
                stream_replicas: Some(1),
                dup_window_secs: Some(120),
            })
            .await?)
        }
    }

    async fn targeted_broker_behavior(
        test: impl AsyncFnOnce(&TargetedBroker) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        use crate::events::advisory_stream_config;
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        use std::process::Stdio;
        use wamn_test_infrastructure::{event_broker, scratch::ScratchRoot};

        wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
        let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
            .map(std::path::PathBuf::from)
            .context("set WAMN_NATIVE_C_NATS_BIN to the owned nats-server executable")?;
        ensure!(binary.is_file(), "NATS executable is absent");
        let binary = binary
            .canonicalize()
            .context("resolve the owned NATS executable")?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = ScratchRoot(
            std::env::temp_dir().join(format!("native-c-targeted-{}-{nonce}", std::process::id())),
        );
        std::fs::DirBuilder::new().mode(0o700).create(root.path())?;
        let targets = [
            Triple::new("example", "dev", "fixture"),
            Triple::new("example", "dev", "other"),
            Triple::new("example", "prod", "fixture"),
        ];
        let mut brokers = Vec::new();
        let mut declarations = Vec::new();
        // One server, one CA, and one full-permission development user.
        let authority = wamn_test_infrastructure::nats_tls::Authority::new(root.path())?;
        let mut users = vec![wamn_test_infrastructure::nats_tls::full_permission_user(
            wamn_test_infrastructure::nats_tls::DEVELOPMENT_USER,
        )];
        for (index, target) in targets.iter().enumerate() {
            let directory = root.path().join(index.to_string());
            std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
            let source = source_stream_config(target, 1, Duration::from_secs(120));
            let advisory = advisory_stream_config(target, 1);
            let consumer = event_consumer_config(
                "registered",
                &wamn_registry::name::event_subject(
                    &(target.org),
                    &(target.project),
                    &(target.env),
                    &("item.update"),
                ),
                Duration::from_secs(30),
                2,
            );
            let mut consumers = vec![consumer.clone()];
            if index == 0 {
                for (name, ack_wait, max_deliver) in [
                    ("interrupted", Duration::from_millis(100), 3),
                    ("pressure", Duration::from_secs(60), 2),
                    ("expiry", Duration::from_secs(30), 2),
                ] {
                    consumers.push(event_consumer_config(
                        name,
                        &wamn_registry::name::event_subject(
                            &(target.org),
                            &(target.project),
                            &(target.env),
                            &(format!("item.{name}",)),
                        ),
                        ack_wait,
                        max_deliver,
                    ));
                }
            }
            let broker = event_broker::prepare_in(
                &authority,
                &directory,
                target,
                "route-project",
                &source,
                &advisory,
                &consumers,
            )?;
            let mut configuration: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&broker.configuration)?)?;
            users.extend(
                configuration["authorization"]["users"]
                    .as_array_mut()
                    .context("native broker configuration omitted its users")?
                    .drain(..)
                    .filter(|user| user.get("permissions").is_some()),
            );
            declarations.push((source, advisory, consumer, consumers));
            brokers.push(broker);
        }
        let configuration_path = root.path().join("nats.conf");
        let configuration = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&configuration_path)?;
        serde_json::to_writer(
            configuration,
            &serde_json::json!({
                "tls": authority.server_tls(),
                "authorization": { "users": users }
            }),
        )?;
        let reserved = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = reserved.local_addr()?;
        drop(reserved);
        let server = format!("nats://{address}");
        let log = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.path().join("nats.log"))?;
        let mut child = tokio::process::Command::new(binary)
            .current_dir(root.path())
            .args(["--jetstream", "--addr", "127.0.0.1", "--port"])
            .arg(address.port().to_string())
            .arg("--store_dir")
            .arg(root.path().join("data"))
            .arg("--config")
            .arg(&configuration_path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()?;
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            Box::pin(async {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    ensure!(
                        child.try_wait()?.is_none(),
                        "owned NATS process exited before readiness"
                    );
                    if tokio::net::TcpStream::connect(address).await.is_ok() {
                        break;
                    }
                    ensure!(
                        tokio::time::Instant::now() < deadline,
                        "owned NATS process did not listen"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let mut managers = Vec::new();
                for (target, broker) in targets.iter().zip(&brokers) {
                    let manager = async_nats::jetstream::new(
                        event_broker::connect(&broker.provisioning, &server).await?,
                    );
                    let (_, _, _, consumers) = &declarations[managers.len()];
                    crate::streams::provision(
                        &manager,
                        target,
                        1,
                        Duration::from_secs(120),
                        consumers,
                    )
                    .await?;
                    crate::streams::provision(
                        &manager,
                        target,
                        1,
                        Duration::from_secs(120),
                        consumers,
                    )
                    .await?;
                    managers.push(manager);
                }
                test(&TargetedBroker {
                    server,
                    targets,
                    brokers,
                    declarations,
                    managers,
                })
                .await
            }),
        )
        .await;
        let kill = child.start_kill();
        let reaped = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        result.context("targeted native event test exceeded 90 seconds")??;
        kill.context("stop the owned NATS process")?;
        reaped
            .context("owned NATS process did not exit within five seconds")?
            .context("reap the owned NATS process")?;
        Ok(())
    }
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
