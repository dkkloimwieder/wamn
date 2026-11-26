//! Read the single platform NATS server before publishing a lower account limit.

use serde::Deserialize;

use super::{Error, JSZ_SUBJECT, Limits, STATZ_SUBJECT};

#[derive(Deserialize)]
struct Reply<T> {
    data: T,
}

#[derive(Deserialize)]
struct AccountStats {
    account_statz: Vec<CoreUsage>,
}

#[derive(Deserialize)]
struct CoreUsage {
    acc: String,
    total_conns: i64,
    num_subscriptions: i64,
}

#[derive(Deserialize)]
struct JetStreamUsage {
    id: String,
    storage: i64,
    #[serde(default)]
    stream_detail: Vec<Stream>,
}

#[derive(Deserialize)]
struct Stream {
    config: StreamConfig,
    state: StreamState,
}

#[derive(Deserialize)]
struct StreamConfig {
    max_bytes: i64,
}

#[derive(Deserialize)]
struct StreamState {
    consumer_count: i64,
}

pub(crate) async fn check_shrink<B, E, F>(
    subject: &str,
    current: Limits,
    desired: Limits,
    request: &(impl Fn(String, &'static [u8]) -> F + Sync),
) -> Result<(), Error>
where
    B: AsRef<[u8]>,
    F: Future<Output = Result<B, E>> + Send,
    E: std::error::Error + Send + Sync + 'static,
{
    let shrink_payload = current.payload > desired.payload;
    if current.conn > desired.conn || current.subs > desired.subs || shrink_payload {
        let stats: AccountStats = read(
            subject,
            STATZ_SUBJECT,
            br#"{"include_unused":true}"#,
            request,
        )
        .await?;
        let usage = stats
            .account_statz
            .into_iter()
            .find(|usage| usage.acc == subject)
            .ok_or_else(missing_usage)?;
        // Account-wide subscriptions bound the number on any one connection.
        if usage.total_conns > desired.conn
            || (current.subs > desired.subs && usage.num_subscriptions > desired.subs)
            || (shrink_payload && usage.total_conns > 0)
        {
            return Err(Error::limit_refused(
                "NATS client limits conflict with current use",
            ));
        }
    }
    if current.consumer > desired.consumer
        || current.streams > desired.streams
        || current.disk_storage > desired.disk_storage
        || shrink_payload
    {
        let usage: JetStreamUsage = read(
            subject,
            JSZ_SUBJECT,
            br#"{"streams":true,"config":true}"#,
            request,
        )
        .await?;
        if usage.id != subject {
            return Err(missing_usage());
        }
        let configured_bytes = usage.stream_detail.iter().fold(0_i64, |total, stream| {
            total.saturating_add(stream.config.max_bytes.max(0))
        });
        let stream_count = i64::try_from(usage.stream_detail.len()).unwrap_or(i64::MAX);
        if stream_count > desired.streams
            || usage
                .stream_detail
                .iter()
                .any(|stream| stream.state.consumer_count > desired.consumer)
            || desired.disk_storage < configured_bytes.max(usage.storage)
            || (shrink_payload && usage.storage > 0)
        {
            return Err(Error::limit_refused(
                "NATS JetStream limits conflict with current use",
            ));
        }
    }
    Ok(())
}

async fn read<T, B, E, F>(
    account: &str,
    subject: &str,
    payload: &'static [u8],
    request: &(impl Fn(String, &'static [u8]) -> F + Sync),
) -> Result<T, Error>
where
    T: serde::de::DeserializeOwned,
    B: AsRef<[u8]>,
    F: Future<Output = Result<B, E>> + Send,
    E: std::error::Error + Send + Sync + 'static,
{
    let response = request(subject.replacen('*', account, 1), payload)
        .await
        .map_err(|source| Error::new("read NATS environment usage", source))?;
    serde_json::from_slice::<Reply<T>>(response.as_ref())
        .map(|reply| reply.data)
        .map_err(|source| Error::new("decode NATS environment usage", source))
}

fn missing_usage() -> Error {
    Error::new(
        "read NATS environment usage",
        std::io::Error::other("the NATS server did not report usage for the account"),
    )
}
