//! Operator read and replay of capped per-registration JetStream dead letters.

use std::time::Duration;

use anyhow::{Context as _, bail};
use async_nats::HeaderMap;
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use clap::{Args, Subcommand};
use futures_util::StreamExt as _;
use serde::Serialize;
use wamn_event_wire::{DEAD_LETTER_STREAM, DeadLetter, dead_letter_subject};

#[derive(Debug, Args)]
pub struct DeadLettersArgs {
    /// Data-plane NATS carrying the WAMN_DLQ stream.
    #[arg(
        long,
        env = "WAMN_EVT_NATS_URL",
        default_value = "nats://evt-nats.wamn-system:4222"
    )]
    pub nats_url: String,

    /// Exact release tenant id.
    #[arg(long)]
    pub tenant: String,

    /// Exact release environment.
    #[arg(long)]
    pub environment: String,

    /// Exact release catalog id.
    #[arg(long)]
    pub catalog: String,

    /// Exact release registration id.
    #[arg(long)]
    pub registration: String,

    /// Maximum records to read or replay in this invocation.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,

    #[command(subcommand)]
    pub action: DeadLetterAction,
}

#[derive(Clone, Copy, Debug, Subcommand)]
pub enum DeadLetterAction {
    /// Print retained records as JSON lines without consuming them.
    Read,
    /// Republish original events, then delete only server-acknowledged records.
    Replay,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct OperatorRecord<'a> {
    dlq_stream_sequence: u64,
    dlq_subject: &'a str,
    record: &'a DeadLetter,
}

pub async fn run(args: DeadLettersArgs) -> anyhow::Result<()> {
    if args.limit == 0 {
        bail!("--limit must be non-zero");
    }
    let subject = dead_letter_subject(
        &args.tenant,
        &args.environment,
        &args.catalog,
        &args.registration,
    );
    let client = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect data-plane NATS at {}", args.nats_url))?;
    let jetstream = async_nats::jetstream::new(client);
    let stream = jetstream
        .get_stream(DEAD_LETTER_STREAM)
        .await
        .with_context(|| format!("get dead-letter stream {DEAD_LETTER_STREAM}"))?;
    let consumer = stream
        .create_consumer(PullConfig {
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::None,
            filter_subject: subject.clone(),
            memory_storage: true,
            num_replicas: 1,
            ..Default::default()
        })
        .await
        .context("create ephemeral dead-letter reader")?;
    let mut messages = consumer
        .fetch()
        .max_messages(args.limit)
        .expires(Duration::from_secs(1))
        .messages()
        .await
        .context("fetch dead letters")?;

    while let Some(message) = messages.next().await {
        let message =
            message.map_err(|error| anyhow::anyhow!("read dead-letter message: {error}"))?;
        let stream_sequence = message
            .info()
            .map_err(|error| anyhow::anyhow!("read dead-letter source metadata: {error}"))?
            .stream_sequence;
        let record = DeadLetter::from_slice(&message.payload)
            .with_context(|| format!("parse dead-letter stream sequence {stream_sequence}"))?;
        match args.action {
            DeadLetterAction::Read => {
                println!(
                    "{}",
                    serde_json::to_string(&OperatorRecord {
                        dlq_stream_sequence: stream_sequence,
                        dlq_subject: &subject,
                        record: &record,
                    })?
                );
            }
            DeadLetterAction::Replay => {
                replay(&jetstream, &stream, stream_sequence, &record).await?;
                println!(
                    "replayed dlq_stream_sequence={stream_sequence} source_stream_sequence={} registration={}",
                    record.source_stream_sequence, args.registration
                );
            }
        }
    }
    Ok(())
}

async fn replay(
    jetstream: &async_nats::jetstream::Context,
    dead_letters: &async_nats::jetstream::stream::Stream,
    dead_letter_sequence: u64,
    record: &DeadLetter,
) -> anyhow::Result<()> {
    let mut headers = HeaderMap::new();
    for header in &record.headers {
        headers.append(header.name.as_str(), header.value.as_str());
    }
    // Reusing the source event's message id would make JetStream accept this
    // replay as a duplicate while that id remains inside the EVT stream's
    // dedupe window. The DLQ sequence instead gives retries of this operator
    // action one stable id without colliding with the original publication.
    let message_id = replay_message_id(dead_letter_sequence);
    headers.insert(NATS_MESSAGE_ID, message_id.as_str());
    let ack = jetstream
        .publish_with_headers(
            record.original_subject.clone(),
            headers,
            record.body.clone().into(),
        )
        .await
        .context("send replayed event")?
        .await
        .context("await replayed event storage ack")?;
    if ack.stream != record.source_stream {
        bail!(
            "replayed event was stored in unexpected stream {:?}; expected {:?}",
            ack.stream,
            record.source_stream
        );
    }
    let deleted = dead_letters
        .delete_message(dead_letter_sequence)
        .await
        .with_context(|| format!("delete replayed dead-letter {dead_letter_sequence}"))?;
    if !deleted {
        bail!("replayed dead-letter {dead_letter_sequence} was not present for deletion");
    }
    Ok(())
}

fn replay_message_id(dead_letter_sequence: u64) -> String {
    format!("wamn-dlq-replay:{dead_letter_sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_and_host_derive_the_same_exact_subject() {
        assert_eq!(
            dead_letter_subject("tenant-a", "prod", "orders", "on-shipped"),
            "dlq.tenant-a.prod.orders.on-shipped"
        );
        assert_eq!(replay_message_id(42), "wamn-dlq-replay:42");
    }
}
