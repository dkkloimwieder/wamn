//! The device loop: each frame of the device is one call of one attachment's
//! operation, one intent and one sample.
//!
//! The loop calls as the fixed local principal that the configuration names,
//! with the permissions of its role in the release grants (spec 4.4). Each
//! frame gets a new sample key, which is the request id and so the intent key
//! of its one item. The input is `[{"request_id", "value": {"frame",
//! "captured_at"}}]`. Calls run one at a time. A completed item stores its
//! `value` as the sample, in the transaction that finishes its intent.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use chrono::{SecondsFormat, Utc};
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialKind};
use wamn_engine::operation::logs_intent;
use wamn_engine::router_delivery::{DeliveryOutcome, SourceRef};

use crate::config::DeviceConfig;
use crate::delivery::EdgeDelivery;
use crate::release::EdgeRelease;
use crate::samples::SampleStore;
use crate::serial;

/// A running device loop.
#[derive(Debug)]
pub struct DeviceLoop {
    task: JoinHandle<()>,
    dropped: Arc<AtomicU64>,
}

impl DeviceLoop {
    /// The frames that the loop dropped since start: empty, not UTF-8, or
    /// longer than `max_frame`. A count above zero shows a misconfigured
    /// device.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Wait for the loop to end. A call in flight finishes first.
    pub async fn join(self) {
        if let Err(error) = self.task.await {
            tracing::warn!(%error, "the device loop failed");
        }
    }
}

/// Check the device of `config` against the release, open its port, and run
/// the loop until `stopped` turns true.
///
/// Refuses an attachment that the role cannot call, a wiring target, a route
/// that changes no records, and a route that names an idempotency key, since
/// the loop keys each frame by its request id.
pub fn start(
    config: &DeviceConfig,
    release: &EdgeRelease,
    delivery: Arc<EdgeDelivery>,
    samples: SampleStore,
    stopped: watch::Receiver<bool>,
) -> anyhow::Result<DeviceLoop> {
    let caller = AuthenticatedCaller::new(
        config.attachment.as_str(),
        config.principal.as_str(),
        CredentialKind::QueuedService,
        release
            .grants()
            .permissions(std::slice::from_ref(&config.role))
            .into_iter()
            .collect(),
    );
    let route = delivery.device_route(&config.attachment, &caller)?;
    anyhow::ensure!(
        logs_intent(route.kind),
        "{} is a {:?} route; the device loop calls a route that changes records",
        config.attachment,
        route.kind
    );
    if let Some(field) = &route.idempotency {
        anyhow::bail!(
            "{} names the idempotency key {field}; the device loop keys each frame by its \
             request_id",
            config.attachment
        );
    }
    let port = serial::open(&config.serial).context("open the device")?;
    let attachment = config.attachment.clone();
    let task = tokio::spawn(run(
        attachment,
        caller,
        delivery,
        samples,
        port.frames,
        stopped,
    ));
    Ok(DeviceLoop {
        task,
        dropped: port.dropped,
    })
}

async fn run(
    attachment: String,
    caller: AuthenticatedCaller,
    delivery: Arc<EdgeDelivery>,
    samples: SampleStore,
    mut frames: mpsc::Receiver<String>,
    mut stopped: watch::Receiver<bool>,
) {
    loop {
        let frame = tokio::select! {
            frame = frames.recv() => frame,
            _ = stopped.wait_for(|stopped| *stopped) => return,
        };
        let Some(frame) = frame else {
            tracing::warn!(%attachment, "the device port closed; the device loop ends");
            return;
        };
        let sample_key = uuid::Uuid::new_v4().to_string();
        let captured_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let input = json!([{
            "request_id": sample_key,
            "value": {"frame": frame, "captured_at": captured_at},
        }]);
        let outcome = delivery
            .call(
                SourceRef::Attachment(&attachment),
                Some(caller.clone()),
                sample_key.clone(),
                &input,
                (None, None),
                &samples.intents(&captured_at),
            )
            .await;
        match outcome {
            Ok(DeliveryOutcome::Respond(answer)) if stored(&answer) => {
                tracing::debug!(%sample_key, "the device call stored its sample");
            }
            Ok(outcome) => {
                tracing::warn!(%sample_key, ?outcome, "the device call stored no sample");
            }
            Err(error) => {
                tracing::warn!(%sample_key, ?error, "the device call failed");
            }
        }
    }
}

/// Whether the answer of one device call is an item without an error.
fn stored(answer: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(answer)
        .is_ok_and(|answer| answer[0].get("value").is_some() && answer[0].get("error").is_none())
}
