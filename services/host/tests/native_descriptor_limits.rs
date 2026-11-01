//! Test startup limits in disposable host processes.

use std::time::Duration;

use tokio::process::Command;

async fn startup_limits(soft: u64, hard: u64) {
    let missing_ca = std::env::temp_dir().join(format!(
        "wamn-descriptor-missing-ca-{}-{hard}.pem",
        std::process::id()
    ));
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new("python3")
            .args([
                "-c",
                "import os, resource, sys; resource.setrlimit(resource.RLIMIT_NOFILE, (int(sys.argv[1]), int(sys.argv[2]))); os.execv(sys.argv[3], sys.argv[3:])",
                &soft.to_string(),
                &hard.to_string(),
                env!("CARGO_BIN_EXE_wamn-host"),
                "host",
                "--drain-bound-seconds=300",
                "--oci-ca-path",
            ])
            .arg(missing_ca)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("host startup exceeded 15 seconds")
    .expect("start host with isolated descriptor limits");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1), "{log}");
    let events: Vec<_> = log
        .lines()
        .filter(|line| line.contains("native descriptor limits initialized"))
        .collect();
    assert_eq!(events.len(), 1, "{log}");
    for (field, value) in [
        ("descriptor_soft_limit", format!("Some({hard})")),
        (
            "default_max_connections",
            (hard / 2).clamp(64, 32768).to_string(),
        ),
        (
            "default_max_http_ingress_connections",
            (hard / 4).clamp(256, 32768).to_string(),
        ),
    ] {
        assert!(
            events[0]
                .split_whitespace()
                .any(|item| item == format!("{field}={value}")),
            "{field} differs from {value}: {log}"
        );
    }
    let refusal = log
        .find("trust the configured OCI CA bundles")
        .expect("host reached the deliberate CA refusal");
    assert!(log.find(events[0]).unwrap() < refusal, "{log}");
}

#[tokio::test]
async fn startup_raises_the_soft_descriptor_limit() {
    startup_limits(256, 4096).await;
}

#[tokio::test]
async fn startup_respects_the_hard_descriptor_limit() {
    startup_limits(256, 256).await;
}
