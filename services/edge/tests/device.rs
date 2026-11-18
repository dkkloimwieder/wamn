//! The device loop reads frames from a pseudo-terminal, the virtual serial
//! port of spec 4.10, and stores one sample for each frame.
//!
//! The support module builds the `apps/edge_device` bundle, as for the route
//! tests.

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
use wamn_edge::status;
use wamn_edge::store::EdgeStore;

use support::{ATTACHMENT, PRINCIPAL, ROLE, bundle, config, start};

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

/// Two frames give two calls, two completed claims and two samples, and the
/// device component trims each frame. A frame longer than `max_frame` gives
/// none, a trailing carriage return is dropped, and a blank frame, which the
/// component refuses, stores no sample. The status socket counts the dropped
/// frame and the pending samples while the edge runs, and is gone after it
/// stops.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: input:edge-application, env:WAMN_INGRESS_COMPONENT"]
async fn each_frame_is_one_call_one_intent_and_one_sample() {
    let (directory, digest) = bundle("device");
    let (mut controller, device) = pseudo_terminal();
    let mut config = config(&directory, digest);
    config.device = Some(DeviceConfig {
        attachment: ATTACHMENT.parse().unwrap(),
        principal: PRINCIPAL.parse().unwrap(),
        role: ROLE.into(),
        serial: SerialConfig {
            path: device,
            baud: 9600,
            max_frame: 16,
        },
    });
    let host = start(config).await;
    let db = directory.join("edge.db");
    let before = status::read(&db).await.expect("read the status");
    assert!(before["started_at"].is_string(), "{before}");
    {
        let observed = &(before["device"]);
        assert_eq!(observed["dropped_frames"], serde_json::json!(0));
    };
    assert_eq!(before["forward"], json!(null), "no forward is configured");
    {
        let observed = &(before["samples"]);
        assert_eq!(observed["pending"], serde_json::json!(0));
        assert!(observed["refused"].as_array().expect("array").is_empty());
    };
    assert_eq!(host.status_socket(), directory.join("status.sock"));
    controller
        .write_all(b"0123456789ABCDEFGHIJ\n12.5 kg\r\n   \n  13.0 kg\n")
        .expect("send the frames");
    let stored = pending(host.samples(), 2).await;
    assert_eq!(
        host.device().map(DeviceLoop::dropped_frames),
        Some(1),
        "the long frame is counted"
    );
    let after = status::read(&db).await.expect("read the status");
    {
        let observed = &(after["device"]);
        assert_eq!(observed["dropped_frames"], serde_json::json!(1));
    };
    {
        let observed = &(after["samples"]);
        assert_eq!(observed["pending"], serde_json::json!(2));
        assert!(observed["refused"].as_array().expect("array").is_empty());
    };
    assert_eq!(after["started_at"], before["started_at"]);
    host.stop().await.expect("the edge stops");
    assert!(
        status::read(&db).await.is_err(),
        "a stopped edge answers no status"
    );

    assert_eq!(
        stored
            .iter()
            .map(|sample| sample.body["frame"].as_str())
            .collect::<Vec<_>>(),
        [Some("12.5 kg"), Some("13.0 kg")],
        "the long frame stores no sample"
    );
    for sample in &stored {
        {
            let observed = &(sample.body);
            assert_eq!(observed["frame"], serde_json::json!(sample.body["frame"]));
            assert_eq!(
                observed["captured_at"],
                serde_json::json!(sample.captured_at)
            );
        };
        assert_eq!(sample.attempts, 0);
    }

    let store = EdgeStore::open(directory.join("edge.db"))
        .await
        .expect("open with the edge stopped");
    let samples = SampleStore::open(store).await.expect("open the samples");
    assert_eq!(
        samples.pending(10).await.expect("read the samples"),
        stored,
        "the samples are on disk"
    );
}
