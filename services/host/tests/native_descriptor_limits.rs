//! Test startup limits through disposable host processes.

use std::time::Duration;
use tokio::process::Command;

async fn startup_limits(soft: u64, hard: u64) {
    use wamn_test_infrastructure::process_input;
    let files = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let input = files.path().join("settings.json");
    process_input::create(&input).unwrap();
    let mut child = Command::new("python3")
        .args(["-c", "import os,resource,sys; resource.setrlimit(resource.RLIMIT_NOFILE,tuple(map(int,sys.argv[1:3]))); os.execv(sys.argv[3],sys.argv[3:])"])
        .args([soft.to_string(), hard.to_string()])
        .args([env!("CARGO_BIN_EXE_wamn-host"), "host", "--scheduler-nats-url=nats://fixture:4222", "--data-nats-url=nats://fixture:4222", "--settings"])
        .arg(&input)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("TOKIO_WORKER_THREADS", "2")
        .env("TMPDIR", files.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn().unwrap();
    let writer = process_input::reader(&input, &mut child)
        .await
        .unwrap()
        .expect("host reads its settings after descriptor initialization");
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", child.id().unwrap())).unwrap();
    let fields = limits
        .lines()
        .find_map(|line| line.strip_prefix("Max open files"))
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(fields[0].parse::<u64>().unwrap(), hard);
    assert_eq!(fields[1].parse::<u64>().unwrap(), hard);
    drop(writer);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[tokio::test]
async fn startup_raises_the_soft_descriptor_limit() {
    startup_limits(256, 4096).await;
}

#[tokio::test]
async fn startup_respects_the_hard_descriptor_limit() {
    startup_limits(256, 256).await;
}
