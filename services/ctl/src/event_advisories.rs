//! Read retained broker advisories and report whether their source payloads exist.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::Duration;

use anyhow::{Context as _, bail};
#[cfg(test)]
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::stream::RawMessageErrorKind;
use clap::Args;
#[cfg(test)]
use futures_util::StreamExt as _;
use serde::Serialize;
use wamn_event_wire::{DeliveryAdvisory, delivery_advisory_stream};

#[derive(Debug, Args)]
pub struct EventAdvisoriesArgs {
    /// Event-plane NATS with the retained advisory stream.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Event-broker username; requires its password file.
    #[arg(long, env = "WAMN_EVT_NATS_USERNAME", requires = "nats_password_file")]
    pub nats_username: Option<String>,

    /// File containing the event-broker password; requires its username.
    #[arg(long, env = "WAMN_EVT_NATS_PASSWORD_FILE", requires = "nats_username")]
    pub nats_password_file: Option<PathBuf>,

    /// Exact source stream from the broker advisory.
    #[arg(long)]
    pub stream: String,

    /// Exact durable consumer from the broker advisory.
    #[arg(long)]
    pub consumer: String,

    /// Maximum advisory records to print.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum SourcePayload {
    Available { subject: String, body: Vec<u8> },
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct OperatorRecord {
    advisory_sequence: u64,
    advisory: DeliveryAdvisory,
    source: SourcePayload,
}

fn broker_token(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || ".*>".contains(character)
        })
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

fn event_nats_options(
    username: Option<&str>,
    password_file: Option<&Path>,
) -> anyhow::Result<async_nats::ConnectOptions> {
    match (username, password_file) {
        (None, None) => Ok(async_nats::ConnectOptions::new()),
        (Some(username), Some(path)) => {
            if username.is_empty()
                || !username
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                bail!("event-broker username must be one nonempty broker token");
            }
            let password =
                std::fs::read_to_string(path).context("read the event-broker password file")?;
            if password.is_empty() {
                bail!("event-broker password file is empty");
            }
            Ok(async_nats::ConnectOptions::new()
                .user_and_password(username.to_owned(), password)
                .custom_inbox_prefix(format!("_INBOX_{username}")))
        }
        _ => bail!("event broker requires both username and password file"),
    }
}

pub async fn run(args: EventAdvisoriesArgs) -> anyhow::Result<()> {
    if args.limit == 0 || !broker_token(&args.stream) || !broker_token(&args.consumer) {
        bail!("--limit must be positive, and --stream and --consumer must be exact broker names");
    }
    let client = event_nats_options(
        args.nats_username.as_deref(),
        args.nats_password_file.as_deref(),
    )?
    .connect(&args.nats_url)
    .await
    .context("connect to event-plane NATS")?;
    let jetstream = async_nats::jetstream::new(client);
    let stream = jetstream
        .get_stream(delivery_advisory_stream(&args.stream))
        .await
        .context("open retained delivery advisories")?;
    for record in retained_advisories(
        &jetstream,
        &stream,
        &args.stream,
        &args.consumer,
        args.limit,
    )
    .await?
    {
        println!("{}", serde_json::to_string(&record)?);
    }
    Ok(())
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
    let subjects = ["MAX_DELIVERIES", "MSG_TERMINATED"]
        .map(|kind| format!("$JS.EVENT.ADVISORY.CONSUMER.{kind}.{source_stream}.{consumer}"));
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
    use anyhow::ensure;
    use async_nats::jetstream::consumer::Consumer;
    use async_nats::jetstream::{AckKind, Message};
    use wamn_control_provision::events::{materializer_consumer_config, source_stream_config};
    use wamn_control_registry::Triple;
    use wamn_event_wire::{DeliveryAdvisoryKind, stream_name};

    use super::*;

    #[test]
    fn event_broker_credentials_refuse_partial_invalid_and_unreadable_inputs() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "wamn-advisories-nats-{}-{nonce}",
            std::process::id()
        ));
        assert!(event_nats_options(None, None).is_ok());
        for (username, password_file) in [
            (Some("publisher_dev"), None),
            (None, Some(path.as_path())),
            (Some("publisher_dev"), Some(path.as_path())),
            (Some(""), Some(path.as_path())),
            (Some("publisher.*"), Some(path.as_path())),
            (Some("publisher>"), Some(path.as_path())),
            (Some("publisher dev"), Some(path.as_path())),
        ] {
            assert!(event_nats_options(username, password_file).is_err());
        }
        std::fs::write(&path, []).expect("write empty password file");
        assert!(event_nats_options(Some("publisher_dev"), Some(&path)).is_err());
        std::fs::write(&path, format!("{nonce}\n")).expect("write private password file");
        assert!(event_nats_options(Some("publisher_dev"), Some(&path)).is_ok());
        std::fs::remove_file(path).expect("remove private password file");
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
    #[ignore = "requires this proof's disposable event broker via WAMN_NATIVE_C_NATS_URL"]
    async fn retained_broker_advisories_report_missing_source_payloads() -> anyhow::Result<()> {
        let url = std::env::var("WAMN_NATIVE_C_NATS_URL")
            .context("set WAMN_NATIVE_C_NATS_URL to this proof's disposable event broker")?;
        let jetstream = async_nats::jetstream::new(async_nats::connect(url).await?);
        let source_name = stream_name("c-advisory", "app", "dev");
        let advisory_name = delivery_advisory_stream(&source_name);
        let scope = Triple::new("c-advisory", "app", "dev");
        let duplicate_window = Duration::from_secs(120);
        let consumers = [
            materializer_consumer_config(
                "exhausted",
                "evt.c-advisory.app.dev.exhausted",
                Duration::from_millis(100),
                2,
            ),
            materializer_consumer_config(
                "terminated",
                "evt.c-advisory.app.dev.terminated",
                Duration::from_secs(30),
                2,
            ),
        ];
        crate::event_streams::provision(&jetstream, &scope, 1, duplicate_window, &consumers)
            .await?;
        let source = jetstream.get_stream(&source_name).await?;
        let mut advisories = jetstream.get_stream(&advisory_name).await?;
        let result: anyhow::Result<()> = async {
            crate::event_streams::provision(&jetstream, &scope, 1, duplicate_window, &consumers).await?;
            let declared_source = source_stream_config(&scope, 1, duplicate_window);
            jetstream.update_stream(async_nats::jetstream::stream::Config {
                max_messages: 1,
                ..declared_source.clone()
            }).await?;
            ensure!(crate::event_streams::provision(&jetstream, &scope, 1, duplicate_window, &consumers).await.is_err(), "changed stream configuration was accepted");
            ensure!(jetstream.get_stream(&source_name).await?.cached_info().config.max_messages == 1, "activation changed the stored stream configuration");
            jetstream.update_stream(declared_source).await?;
            source.update_consumer(PullConfig { max_deliver: 3, ..consumers[0].clone() }).await?;
            ensure!(crate::event_streams::provision(&jetstream, &scope, 1, duplicate_window, &consumers).await.is_err(), "changed consumer configuration was accepted");
            ensure!(source.consumer_info("exhausted").await?.config.max_deliver == 3, "activation changed the stored consumer configuration");
            source.update_consumer(consumers[0].clone()).await?;
            let exhausted = source.get_consumer::<PullConfig>("exhausted").await.map_err(anyhow::Error::from_boxed)?;
            let exhausted_sequence = jetstream
                .publish("evt.c-advisory.app.dev.exhausted", "exhausted payload".into())
                .await?
                .await?
                .sequence;
            for attempt in 1..=2 {
                let message = fetch_one(&exhausted)
                    .await?
                    .context("broker omitted the expected retry")?;
                ensure!(
                    message.info().map_err(anyhow::Error::from_boxed)?.delivered == attempt,
                    "broker attempt differs from the actual delivery count"
                );
                message
                    .ack_with(AckKind::Nak(None))
                    .await
                    .map_err(anyhow::Error::from_boxed)?;
            }
            ensure!(
                fetch_one(&exhausted).await?.is_none(),
                "broker delivered past max_deliver"
            );

            let valid_sequence = jetstream
                .publish("evt.c-advisory.app.dev.exhausted", "later valid payload".into())
                .await?.await?.sequence;
            let valid = fetch_one(&exhausted).await?.context("exhausted poison blocked the later valid message")?;
            ensure!(valid.info().map_err(anyhow::Error::from_boxed)?.stream_sequence == valid_sequence,
                "consumer redelivered poison instead of the later valid message");
            valid.double_ack().await.map_err(anyhow::Error::from_boxed)?;

            let terminated = source.get_consumer::<PullConfig>("terminated").await.map_err(anyhow::Error::from_boxed)?;
            let terminated_sequence = jetstream
                .publish("evt.c-advisory.app.dev.terminated", "terminated payload".into())
                .await?
                .await?
                .sequence;
            let message = fetch_one(&terminated)
                .await?
                .context("broker omitted the terminal delivery")?;
            message
                .ack_with(AckKind::Term)
                .await
                .map_err(anyhow::Error::from_boxed)?;

            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while advisories.info().await?.state.messages < 2 {
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "broker did not retain both delivery advisories"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let state = advisories.cached_info().state.clone();
            ensure!(state.messages == 2, "unexpected retained advisory count");
            for (consumer, kind) in [("exhausted", DeliveryAdvisoryKind::MaxDeliver), ("terminated", DeliveryAdvisoryKind::Terminated)] {
                let records = retained_advisories(&jetstream, &advisories, &source_name, consumer, 1).await?;
                ensure!(records.len() == 1, "selected consumer advisory is missing");
                ensure!(records[0].advisory.kind == kind, "selected consumer has another advisory kind");
                ensure!(matches!(&records[0].source, SourcePayload::Available { .. }), "retained source payload is missing");
            }
            ensure!(advisories.info().await?.state.consumer_count == 0, "advisory reads created a consumer");
            let mut exhausted_seen = false;
            let mut terminated_seen = false;
            for sequence in state.first_sequence..=state.last_sequence {
                let message = advisories.get_raw_message(sequence).await?;
                let advisory = DeliveryAdvisory::from_slice(&message.payload)?;
                ensure!(
                    advisory.stream == source_name,
                    "advisory names another source"
                );
                let expected = match advisory.kind {
                    DeliveryAdvisoryKind::MaxDeliver => {
                        ensure!(
                            advisory.consumer == "exhausted"
                                && advisory.stream_seq == exhausted_sequence
                                && advisory.deliveries == 2,
                            "exhaustion advisory does not identify the real attempts"
                        );
                        exhausted_seen = true;
                        b"exhausted payload".as_slice()
                    }
                    DeliveryAdvisoryKind::Terminated => {
                        ensure!(
                            advisory.consumer == "terminated"
                                && advisory.stream_seq == terminated_sequence
                                && advisory.deliveries == 1,
                            "termination advisory does not identify the settled message"
                        );
                        terminated_seen = true;
                        b"terminated payload".as_slice()
                    }
                };
                ensure!(
                    matches!(source_payload(&jetstream, &advisory).await?,
                    SourcePayload::Available { body, .. } if body == expected),
                    "retained source payload differs"
                );
                source.delete_message(advisory.stream_seq).await?;
                ensure!(
                    matches!(
                        source_payload(&jetstream, &advisory).await?,
                        SourcePayload::Unavailable
                    ),
                    "deleted source payload was reported as available"
                );
                println!(
                    "NATIVE_C_BROKER_ADVISORY {}",
                    serde_json::to_string(&advisory)?
                );
            }
            ensure!(
                exhausted_seen && terminated_seen,
                "one advisory disposition was missing"
            );
            ensure!(
                advisories.info().await?.state.messages == 2,
                "source deletion removed retained broker advisories"
            );
            for consumer in ["exhausted", "terminated"] {
                let records = retained_advisories(&jetstream, &advisories, &source_name, consumer, 100).await?;
                ensure!(records.len() == 1, "retained advisory was lost after source deletion");
                ensure!(matches!(&records[0].source, SourcePayload::Unavailable), "deleted source payload was reported as available");
            }
            println!(
                "NATIVE_C_ADVISORY_READER_PASS exhaustion=1 termination=1 available=2 unavailable=2 later_valid=1"
            );
            Ok(())
        }
        .await;
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
        let client = crate::event_streams::connection_options(
            &credentials.username,
            &credentials.password_file,
        )?
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
        let first_client = event_broker::connect(credentials, server).await?;
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while first_client.connection_state() != async_nats::connection::State::Disconnected {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "the interrupted materializer connection did not close"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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
        println!(
            "NATIVE_C_INTERRUPTED_ACK_PASS first_delivery=1 repeated_delivery=2 same_sequence=true later_progress=1 connection_closed=true"
        );
        Ok(())
    }

    async fn large_messages_obey_pull_and_ack_limits(
        publisher: &async_nats::jetstream::Context,
        materializer: &async_nats::jetstream::Context,
        source_name: &str,
        consumer: &PullConfig,
    ) -> anyhow::Result<()> {
        use wamn_control_provision::events::MATERIALIZER_MAX_PULL_BYTES;

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
                && declared.config.max_bytes == MATERIALIZER_MAX_PULL_BYTES,
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
        let mut pending = Vec::new();
        let mut largest_pull = 0;
        let mut pull_bytes = Vec::new();
        let mut observed_pending = Vec::new();
        let mut byte_limited_pulls = 0;
        while pending.len() < 64 {
            let mut messages = attached
                .fetch()
                .max_messages(64 - pending.len())
                .max_bytes(MATERIALIZER_MAX_PULL_BYTES as usize)
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
                                && bytes <= MATERIALIZER_MAX_PULL_BYTES as usize
                                && MATERIALIZER_MAX_PULL_BYTES as usize - bytes < PAYLOAD_BYTES,
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
                pending.len() > before && bytes <= MATERIALIZER_MAX_PULL_BYTES as usize,
                "the native pull exceeded its byte bound or made no progress"
            );
            largest_pull = largest_pull.max(bytes);
            pull_bytes.push(bytes);
            let observed = stream.consumer_info(name).await?.num_ack_pending;
            observed_pending.push(observed);
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
        let total_received = pull_bytes.iter().sum::<usize>() + last.payload.len();
        println!(
            "NATIVE_C_LARGE_MESSAGES_PASS messages={MESSAGE_COUNT} payload_bytes={PAYLOAD_BYTES} byte_limited_pulls={byte_limited_pulls} pull_payload_bytes={pull_bytes:?} observed_ack_pending={observed_pending:?} largest_pull_payload_bytes={largest_pull} total_received_payload_bytes={total_received} pull_limit_bytes={MATERIALIZER_MAX_PULL_BYTES} blocked_ack_pending={} blocked_undelivered={} final_ack_pending={} final_undelivered={} later_progress=1",
            full.num_ack_pending, full.num_pending, settled.num_ack_pending, settled.num_pending,
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
            initial.advisory.kind == DeliveryAdvisoryKind::Terminated
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
                    && records[0].advisory.kind == DeliveryAdvisoryKind::Terminated,
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
        println!(
            "NATIVE_C_SOURCE_EXPIRY_PASS source_max_age_ms=2000 source_duplicate_window_ms=1000 available=1 expired=1 retained_advisory=1 message_delete_calls=0"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires an owned nats-server executable via WAMN_NATIVE_C_NATS_BIN"]
    async fn scoped_credentials_confine_management_delivery_and_monitoring() -> anyhow::Result<()> {
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        use std::process::Stdio;
        use wamn_control_provision::events::advisory_stream_config;
        use wamn_runtime::plugins::wamn_jetstream::{WamnJetstream, WamnJetstreamConfig};
        use wamn_test_infrastructure::{event_broker, scratch::ScratchRoot};

        let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
            .map(PathBuf::from)
            .context("set WAMN_NATIVE_C_NATS_BIN to the owned nats-server executable")?;
        ensure!(binary.is_file(), "NATS executable is absent");
        let binary = binary
            .canonicalize()
            .context("resolve the owned NATS executable")?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = ScratchRoot(
            std::env::temp_dir().join(format!("native-c-scoped-{}-{nonce}", std::process::id())),
        );
        std::fs::DirBuilder::new().mode(0o700).create(root.path())?;
        let scopes = [
            Triple::new("acme", "receiving", "dev"),
            Triple::new("acme", "wms", "dev"),
            Triple::new("acme", "receiving", "prod"),
        ];
        let mut brokers = Vec::new();
        let mut declarations = Vec::new();
        let mut users = Vec::new();
        for (index, scope) in scopes.iter().enumerate() {
            let directory = root.path().join(index.to_string());
            std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
            let source = source_stream_config(scope, 1, Duration::from_secs(120));
            let advisory = advisory_stream_config(scope, 1);
            let consumer = materializer_consumer_config(
                "registered",
                &format!(
                    "evt.{}.{}.{}.item.update",
                    scope.org, scope.project, scope.env
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
                    consumers.push(materializer_consumer_config(
                        name,
                        &format!(
                            "evt.{}.{}.{}.item.{name}",
                            scope.org, scope.project, scope.env
                        ),
                        ack_wait,
                        max_deliver,
                    ));
                }
            }
            let broker = event_broker::prepare(
                &directory,
                scope,
                "route-tenant",
                &source,
                &advisory,
                &consumers,
            )?;
            let mut configuration: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&broker.configuration)?)?;
            users.append(
                configuration["authorization"]["users"]
                    .as_array_mut()
                    .context("native broker configuration omitted its users")?,
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
        let mut stage = "broker readiness";
        let result = tokio::time::timeout(Duration::from_secs(90), async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                ensure!(child.try_wait()?.is_none(), "owned NATS process exited before readiness");
                if tokio::net::TcpStream::connect(address).await.is_ok() { break; }
                ensure!(tokio::time::Instant::now() < deadline, "owned NATS process did not listen");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            stage = "provision declared streams and consumers";
            let mut managers = Vec::new();
            for (scope, broker) in scopes.iter().zip(&brokers) {
                let manager = async_nats::jetstream::new(event_broker::connect(&broker.provisioning, &server).await?);
                let (_, _, _, consumers) = &declarations[managers.len()];
                crate::event_streams::provision(&manager, scope, 1, Duration::from_secs(120), consumers).await?;
                crate::event_streams::provision(&manager, scope, 1, Duration::from_secs(120), consumers).await?;
                managers.push(manager);
            }
            let broker = &brokers[0];
            let (source, advisory, consumer, consumers) = &declarations[0];
            let active = || WamnJetstream::new(WamnJetstreamConfig {
                nats_url: Some(server.clone()),
                nats_username: Some(broker.runtime.username.clone()),
                nats_password_file: Some(broker.runtime.password_file.clone()),
                event_scope: Some(scopes[0].clone()),
                stream_replicas: Some(1),
                dup_window_secs: Some(120),
            });
            stage = "activate the runtime";
            active().activate_events().await
                .map_err(|error| anyhow::anyhow!("declared runtime activation failed: {error:?}"))?;
            let runtime = async_nats::jetstream::new(event_broker::connect(&broker.runtime, &server).await?);
            let materializer = async_nats::jetstream::new(event_broker::connect(&broker.materializer, &server).await?);
            stage = "attach the materializer";
            let attached = materializer.get_stream(&source.name).await?
                .get_consumer::<PullConfig>("registered").await.map_err(anyhow::Error::from_boxed)?;
            stage = "publish the runtime payload";
            let sequence = runtime.publish(consumer.filter_subject.clone(), "runtime payload".into()).await?.await?.sequence;
            stage = "read the runtime payload";
            let message = fetch_one(&attached).await?.context("materializer did not read runtime publication")?;
            ensure!(message.payload.as_ref() == b"runtime payload", "runtime payload changed");
            stage = "terminate the runtime payload";
            message.ack_with(AckKind::Term).await.map_err(anyhow::Error::from_boxed)?;
            let publisher = async_nats::jetstream::new(event_broker::connect(&broker.publisher, &server).await?);
            stage = "publish the CDC payload";
            publisher.publish(consumer.filter_subject.clone(), "publisher payload".into()).await?.await?;
            stage = "read the CDC payload";
            let message = fetch_one(&attached).await?.context("materializer did not read publisher publication")?;
            ensure!(message.payload.as_ref() == b"publisher payload", "publisher payload changed");
            stage = "acknowledge the CDC payload";
            message.double_ack().await.map_err(anyhow::Error::from_boxed)?;
            stage = "read retained advisories and source data";
            let (observer_client, mut observer_errors) = monitored_broker_client(&broker.observer, &server).await?;
            let mut observer = async_nats::jetstream::new(observer_client);
            observer.set_timeout(Duration::from_millis(500));
            let mut retained = observer.get_stream(&advisory.name).await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while retained.info().await?.state.messages == 0 {
                ensure!(tokio::time::Instant::now() < deadline, "termination metadata did not arrive");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let records = retained_advisories(&observer, &retained, &source.name, "registered", 2).await?;
            ensure!(records.len() == 1 && records[0].advisory.stream_seq == sequence, "observer read another delivery");
            ensure!(matches!(&records[0].source, SourcePayload::Available { body, .. } if body == b"runtime payload"), "observer lost its permitted source payload");

            stage = "refuse runtime management";
            for credentials in [&broker.runtime, &broker.publisher, &broker.materializer] {
                let (client, mut errors) = monitored_broker_client(credentials, &server).await?;
                let mut restricted = async_nats::jetstream::new(client);
                restricted.set_timeout(Duration::from_millis(500));
                stage = "refuse runtime stream creation";
                ensure!(restricted.create_stream(source.clone()).await.is_err(), "runtime created a stream");
                require_permission_denial(&mut errors, &format!("$JS.API.STREAM.CREATE.{}", source.name)).await?;
                stage = "refuse runtime stream update";
                ensure!(restricted.update_stream(source.clone()).await.is_err(), "runtime updated a stream");
                require_permission_denial(&mut errors, &format!("$JS.API.STREAM.UPDATE.{}", source.name)).await?;
                stage = "refuse runtime stream deletion";
                ensure!(restricted.delete_stream(&source.name).await.is_err(), "runtime deleted a stream");
                require_permission_denial(&mut errors, &format!("$JS.API.STREAM.DELETE.{}", source.name)).await?;
                let stream = restricted.get_stream(&source.name).await?;
                stage = "refuse runtime consumer creation";
                ensure!(stream.create_consumer_strict(consumer.clone()).await.is_err(), "runtime created a consumer");
                require_permission_denial(&mut errors, &format!("$JS.API.CONSUMER.CREATE.{}.registered", source.name)).await?;
                stage = "refuse runtime consumer update";
                ensure!(stream.update_consumer(consumer.clone()).await.is_err(), "runtime updated a consumer");
                require_permission_denial(&mut errors, &format!("$JS.API.CONSUMER.CREATE.{}.registered", source.name)).await?;
                stage = "refuse runtime consumer deletion";
                ensure!(stream.delete_consumer("registered").await.is_err(), "runtime deleted a consumer");
                require_permission_denial(&mut errors, &format!("$JS.API.CONSUMER.DELETE.{}.registered", source.name)).await?;
            }
            let (runtime_client, mut runtime_errors) = monitored_broker_client(&broker.runtime, &server).await?;
            let mut restricted_runtime = async_nats::jetstream::new(runtime_client.clone());
            restricted_runtime.set_timeout(Duration::from_millis(500));
            stage = "refuse access to other environments";
            for (foreign_source, foreign_advisory, foreign_consumer, _) in &declarations[1..] {
                let published = restricted_runtime.publish(foreign_consumer.filter_subject.clone(), "foreign payload".into()).await;
                let refused = match published {
                    Ok(acknowledgement) => acknowledgement.await.is_err(),
                    Err(_) => true,
                };
                ensure!(refused, "runtime published into another environment");
                require_permission_denial(&mut runtime_errors, &foreign_consumer.filter_subject).await?;
                let subscription = runtime_client.subscribe(foreign_source.subjects[0].clone()).await?;
                runtime_client.flush().await?;
                require_permission_denial(&mut runtime_errors, &foreign_source.subjects[0]).await?;
                drop(subscription);
                let foreign = restricted_runtime.get_stream_no_info(&foreign_source.name).await?;
                ensure!(foreign.get_consumer::<PullConfig>("registered").await.is_err(), "runtime attached to a foreign consumer");
                require_permission_denial(&mut runtime_errors, &format!("$JS.API.CONSUMER.INFO.{}.registered", foreign_source.name)).await?;
                for name in [&foreign_source.name, &foreign_advisory.name] {
                    ensure!(observer.get_stream(name).await.is_err(), "observer read foreign stream metadata");
                    require_permission_denial(&mut observer_errors, &format!("$JS.API.STREAM.INFO.{name}")).await?;
                    let foreign = observer.get_stream_no_info(name).await?;
                    ensure!(foreign.get_raw_message(1).await.is_err(), "observer read foreign retained data");
                    require_permission_denial(&mut observer_errors, &format!("$JS.API.STREAM.MSG.GET.{name}")).await?;
                }
            }
            stage = "refuse changed stream and consumer declarations";
            let manager = &managers[0];
            manager.update_stream(async_nats::jetstream::stream::Config { max_messages: 2, ..source.clone() }).await?;
            ensure!(active().activate_events().await.is_err(), "runtime accepted changed stream configuration");
            ensure!(manager.get_stream(&source.name).await?.cached_info().config.max_messages == 2, "runtime reconfigured the stream");
            manager.update_stream(source.clone()).await?;
            let stream = manager.get_stream(&source.name).await?;
            stream.update_consumer(PullConfig { max_deliver: 3, ..consumer.clone() }).await?;
            ensure!(crate::event_streams::provision(manager, &scopes[0], 1, Duration::from_secs(120), &[consumer.clone()]).await.is_err(), "provisioning accepted changed consumer configuration");
            ensure!(stream.consumer_info("registered").await?.config.max_deliver == 3, "provisioning reconfigured the consumer");
            stream.update_consumer(consumer.clone()).await?;
            active().activate_events().await.map_err(|error| anyhow::anyhow!("restored activation failed: {error:?}"))?;
            stage = "redeliver after an interrupted acknowledgement";
            interrupted_delivery_redelivers(&server, &broker.materializer, &publisher, &source.name, &consumers[1]).await?;
            stage = "bound near-one-MiB messages and outstanding acknowledgements";
            large_messages_obey_pull_and_ack_limits(&publisher, &materializer, &source.name, &consumers[2]).await?;
            stage = "retain advisory metadata after actual source time expiry";
            source_time_expiry_preserves_advisory(manager, &publisher, &materializer, &observer, source, advisory, &consumers[3]).await?;
            println!("NATIVE_C_SCOPED_PASS environments=3 provisioning=3 runtime_management_refusals=18 foreign_metadata_and_data_refusals=8 foreign_runtime_refusals=6");
            Ok::<(), anyhow::Error>(())
        }).await;
        let kill = child.start_kill();
        let reaped = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        result.with_context(|| {
            format!("scoped native event test exceeded 90 seconds during {stage}")
        })??;
        kill.context("stop the owned NATS process")?;
        reaped
            .context("owned NATS process did not exit within five seconds")?
            .context("reap the owned NATS process")?;
        Ok(())
    }
}
