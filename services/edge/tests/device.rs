//! The device loop reads frames from a pseudo-terminal, the virtual serial
//! port of spec 4.10, and stores one sample for each frame.
//!
//! The test needs the built guest, as the route tests do:
//! `WAMN_FLOW_HTTP_COMPONENT` names `http-route` built for `wasm32-wasip2`,
//! because `serve` starts the ingress (docs/operations/running-tests.md).

mod support;

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use serde_json::json;
use wamn_edge::config::{DeviceConfig, SerialConfig};
use wamn_edge::device::DeviceLoop;
use wamn_edge::samples::{Sample, SampleStore};
use wamn_run_state::IntentStore as _;
use wamn_run_state_sqlite::SqliteIntentStore;

use support::{ATTACHMENT, PRINCIPAL, bundle, config, ingress, key, start};

/// A pseudo-terminal pair: the controller that the test writes, and the path
/// of the device that the edge reads.
fn pseudo_terminal() -> (File, PathBuf) {
    let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)
        .expect("open a pseudo-terminal");
    grantpt(&controller).expect("grant the device");
    unlockpt(&controller).expect("unlock the device");
    let device = ptsname(&controller, Vec::new())
        .expect("name the device")
        .into_string()
        .expect("the device name is UTF-8");
    (File::from(controller), PathBuf::from(device))
}

/// The pending samples, once there are `count` of them.
async fn pending(samples: &SampleStore, count: usize) -> Vec<Sample> {
    for _ in 0..300 {
        let pending = samples.pending(10).await.expect("read the samples");
        if pending.len() >= count {
            return pending;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the device loop stored fewer than {count} samples in 30 seconds");
}

/// Two frames give two calls, two finished intents and two samples. A frame
/// longer than `max_frame` gives none, and a trailing carriage return is
/// dropped.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT"]
async fn each_frame_is_one_call_one_intent_and_one_sample() {
    let (_, public) = key("key-one", 1);
    let (directory, digest) = bundle("device", &ingress(), &public);
    let (mut controller, device) = pseudo_terminal();
    let mut config = config(&directory, digest);
    config.device = Some(DeviceConfig {
        attachment: ATTACHMENT.into(),
        principal: PRINCIPAL.into(),
        role: "device".into(),
        serial: SerialConfig {
            path: device,
            baud: 9600,
            max_frame: 16,
        },
    });
    let host = start(config).await;
    controller
        .write_all(b"0123456789ABCDEFGHIJ\n12.5 kg\r\n13.0 kg\n")
        .expect("send the frames");
    let stored = pending(host.samples(), 2).await;
    assert_eq!(
        host.device().map(DeviceLoop::dropped_frames),
        Some(1),
        "the long frame is counted"
    );
    host.stop().await.expect("the edge stops");

    assert_eq!(
        stored
            .iter()
            .map(|sample| sample.body["frame"].as_str())
            .collect::<Vec<_>>(),
        [Some("12.5 kg"), Some("13.0 kg")],
        "the long frame stores no sample"
    );
    for sample in &stored {
        assert_eq!(
            sample.body,
            json!({"frame": sample.body["frame"], "captured_at": sample.captured_at}),
            "the echo guest returns its input value as the sample"
        );
        assert_eq!(sample.attempts, 0);
    }

    let store =
        SqliteIntentStore::open(directory.join("edge.db")).expect("open with the edge stopped");
    assert!(
        store.uncertain(10).await.expect("list").is_empty(),
        "every device intent finished"
    );
    let samples = SampleStore::open(store).await.expect("open the samples");
    assert_eq!(
        samples.pending(10).await.expect("read the samples"),
        stored,
        "the samples are on disk"
    );
}
