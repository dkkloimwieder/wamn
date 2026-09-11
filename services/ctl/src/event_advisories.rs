//! Read retained broker advisories and report whether their source payloads exist.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use async_nats::jetstream::stream::RawMessageErrorKind;
use clap::Args;
use futures_util::StreamExt as _;
use serde::Serialize;
use wamn_event_wire::{DELIVERY_ADVISORY_STREAM, DeliveryAdvisory};

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
        .get_stream(DELIVERY_ADVISORY_STREAM)
        .await
        .context("open retained delivery advisories")?;
    let consumer = stream
        .create_consumer(PullConfig {
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::None,
            filter_subjects: ["MAX_DELIVERIES", "MSG_TERMINATED"]
                .map(|kind| {
                    format!(
                        "$JS.EVENT.ADVISORY.CONSUMER.{kind}.{}.{}",
                        args.stream, args.consumer
                    )
                })
                .to_vec(),
            memory_storage: true,
            num_replicas: 1,
            inactive_threshold: Duration::from_secs(5),
            ..Default::default()
        })
        .await
        .context("create the ephemeral advisory reader")?;
    let consumer_name = consumer.cached_info().name.clone();
    let result = async {
        let mut messages = consumer
            .fetch()
            .max_messages(args.limit)
            .expires(Duration::from_secs(1))
            .messages()
            .await
            .context("fetch retained advisories")?;
        while let Some(message) = messages.next().await {
            let message = message
                .map_err(anyhow::Error::from_boxed)
                .context("read a delivery advisory")?;
            let advisory_sequence = message
                .info()
                .map_err(anyhow::Error::from_boxed)
                .context("read advisory sequence")?
                .stream_sequence;
            let advisory = DeliveryAdvisory::from_slice(&message.payload)
                .context("decode the broker delivery advisory")?;
            if advisory.stream != args.stream || advisory.consumer != args.consumer {
                bail!("broker advisory coordinates differ from the selected stream and consumer");
            }
            let source = source_payload(&jetstream, &advisory).await?;
            println!(
                "{}",
                serde_json::to_string(&OperatorRecord {
                    advisory_sequence,
                    advisory,
                    source
                })?
            );
        }
        Ok(())
    }
    .await;
    let cleanup = stream
        .delete_consumer(&consumer_name)
        .await
        .context("remove this invocation's ephemeral advisory reader");
    result?;
    cleanup?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use anyhow::ensure;
    use async_nats::jetstream::consumer::Consumer;
    use async_nats::jetstream::stream::{Config as StreamConfig, StorageType};
    use async_nats::jetstream::{AckKind, Message};
    use wamn_event_wire::{DELIVERY_ADVISORY_SUBJECTS, DeliveryAdvisoryKind};

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
        let source = jetstream
            .create_stream(StreamConfig {
                name: "C_ADVISORY_SOURCE".into(),
                subjects: vec!["c.advisory.*".into()],
                storage: StorageType::File,
                max_messages: 8,
                ..Default::default()
            })
            .await?;
        let mut advisories = jetstream
            .create_stream(StreamConfig {
                name: DELIVERY_ADVISORY_STREAM.into(),
                subjects: DELIVERY_ADVISORY_SUBJECTS.map(str::to_owned).to_vec(),
                storage: StorageType::File,
                max_messages: 8,
                ..Default::default()
            })
            .await?;
        let result: anyhow::Result<()> = async {
            let exhausted = source
                .create_consumer(PullConfig {
                    durable_name: Some("exhausted".into()),
                    filter_subject: "c.advisory.exhausted".into(),
                    ack_policy: AckPolicy::Explicit,
                    ack_wait: Duration::from_millis(100),
                    max_deliver: 2,
                    ..Default::default()
                })
                .await?;
            let exhausted_sequence = jetstream
                .publish("c.advisory.exhausted", "exhausted payload".into())
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
                .publish("c.advisory.exhausted", "later valid payload".into())
                .await?.await?.sequence;
            let valid = fetch_one(&exhausted).await?.context("exhausted poison blocked the later valid message")?;
            ensure!(valid.info().map_err(anyhow::Error::from_boxed)?.stream_sequence == valid_sequence,
                "consumer redelivered poison instead of the later valid message");
            valid.double_ack().await.map_err(anyhow::Error::from_boxed)?;

            let terminated = source
                .create_consumer(PullConfig {
                    durable_name: Some("terminated".into()),
                    filter_subject: "c.advisory.terminated".into(),
                    ack_policy: AckPolicy::Explicit,
                    max_deliver: 2,
                    ..Default::default()
                })
                .await?;
            let terminated_sequence = jetstream
                .publish("c.advisory.terminated", "terminated payload".into())
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
            let mut exhausted_seen = false;
            let mut terminated_seen = false;
            for sequence in state.first_sequence..=state.last_sequence {
                let message = advisories.get_raw_message(sequence).await?;
                let advisory = DeliveryAdvisory::from_slice(&message.payload)?;
                ensure!(
                    advisory.stream == "C_ADVISORY_SOURCE",
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
            println!(
                "NATIVE_C_ADVISORY_READER_PASS exhaustion=1 termination=1 available=2 unavailable=2 later_valid=1"
            );
            Ok(())
        }
        .await;
        let source_cleanup = jetstream.delete_stream("C_ADVISORY_SOURCE").await;
        let advisory_cleanup = jetstream.delete_stream(DELIVERY_ADVISORY_STREAM).await;
        result?;
        source_cleanup?;
        advisory_cleanup?;
        Ok(())
    }
}
