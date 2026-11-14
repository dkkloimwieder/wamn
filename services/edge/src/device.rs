//! The device loop: each frame of the device is one call of one attachment's
//! operation, one claim and one sample.
//!
//! The loop calls as the fixed local principal that the configuration names,
//! with the permissions of its role in the release grants (spec 4.4). Each
//! frame gets a new sample key, which is the request id and so the caller key
//! of its one request element. The input is `[{"request_id", "value": {"frame",
//! "captured_at"}}]`. Calls run one at a time. A completed request element stores its
//! `value` as the sample, in the transaction that completes its claim.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use chrono::{SecondsFormat, Utc};
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType};

use crate::config::DeviceConfig;
use crate::operation::EdgeOperation;
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

    /// The counter of [`Self::dropped_frames`], for the status.
    pub(crate) fn dropped_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped)
    }

    /// Wait for the loop to end. A call in flight finishes first.
    pub async fn join(self) {
        if let Err(error) = self.task.await {
            tracing::warn!(%error, "the device loop failed");
        }
    }
}

/// Make sure of the device of `config` against the release, open its port, and run
/// the loop until `stopped` turns true.
///
/// Refuses an attachment that the role cannot call, a wiring target, a route
/// that changes no records, and a route that names an idempotency key, since
/// the loop keys each frame by its request id.
pub fn start(
    config: &DeviceConfig,
    release: &EdgeRelease,
    delivery: Arc<EdgeOperation>,
    samples: SampleStore,
    stopped: watch::Receiver<bool>,
) -> anyhow::Result<DeviceLoop> {
    let caller = AuthenticatedCaller::new(
        config.attachment.as_str(),
        config.principal.as_str(),
        CredentialType::QueuedService,
        false,
        release
            .grants()
            .permissions(std::slice::from_ref(&config.role))
            .iter()
            .map(|permission| wamn_catalog::sealed_operation_reference(permission).to_owned())
            .collect(),
        Some(config.role.as_str().into()),
    );
    let manifest = release.release().manifest();
    let attachment = manifest
        .attachment(&config.attachment)
        .context("device attachment is missing")?;
    let wamn_catalog::AttachmentTarget::Route {
        component,
        operation,
    } = attachment.target();
    let route = manifest
        .route(attachment.package_id(), &component, &operation)
        .context("device route is missing")?;
    let fact = release
        .components()
        .iter()
        .find_map(|component| component.operations.get(&operation))
        .context("device operation is missing")?;
    wamn_engine::native_routes::authorize_released_operation(Some(&caller), fact)?;
    anyhow::ensure!(
        !route.type_.is_read(),
        "{} is a {:?} route; the device loop calls a route that changes records",
        config.attachment,
        route.type_
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
    delivery: Arc<EdgeOperation>,
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
        let captured_at = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
        let input = json!([{
            "request_id": sample_key,
            "value": {"frame": frame, "captured_at": captured_at},
        }]);
        match delivery
            .sample(caller.clone(), &sample_key, &captured_at, &input)
            .await
        {
            Ok(()) => {
                samples.notify_stored();
                tracing::debug!(%sample_key, "the device call stored its sample");
            }
            Err(error) => tracing::warn!(%sample_key, %error, "device call failed"),
        }
    }
}
