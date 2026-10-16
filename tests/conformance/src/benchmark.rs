//! Compare throughput from the two newest platform benchmarks.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Benchmark {
    concurrency: u32,
    get: Measurement,
    update: Measurement,
}

#[derive(Debug, Deserialize)]
struct Measurement {
    throughput_per_sec: f64,
}

/// The numeric parts of a platform version, which order 0.1.9 before 0.1.10.
fn version(text: &str) -> anyhow::Result<Vec<u64>> {
    text.split('.')
        .map(|part| {
            part.parse()
                .with_context(|| format!("platform version {text}"))
        })
        .collect()
}

fn compare(previous: &Benchmark, current: &Benchmark) -> anyhow::Result<()> {
    ensure!(
        previous.concurrency == 16 && current.concurrency == 16,
        "benchmark comparison requires c16"
    );
    for (call, previous, current) in [
        ("get", &previous.get, &current.get),
        ("update", &previous.update, &current.update),
    ] {
        let before = previous.throughput_per_sec;
        let after = current.throughput_per_sec;
        ensure!(
            before.is_finite() && before > 0.0 && after.is_finite() && after >= 0.0,
            "{call}: invalid throughput"
        );
        // A drop of exactly 20% passes.
        ensure!(
            after >= before * 0.8,
            "{call} throughput at c16 dropped more than 20%: {before} -> {after} requests/s"
        );
    }
    Ok(())
}

#[test]
fn platform_benchmark_gate() -> anyhow::Result<()> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(version) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("platform-"))
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        files.push((self::version(version)?, path));
    }
    files.sort();
    let [.., (_, previous), (_, current)] = files.as_slice() else {
        println!(
            "benchmark gate passes: fewer than two platform results ({})",
            files.len()
        );
        return Ok(());
    };
    let read = |path: &Path| -> anyhow::Result<Benchmark> {
        serde_json::from_slice(&std::fs::read(path)?)
            .with_context(|| format!("read {}", path.display()))
    };
    compare(&read(previous)?, &read(current)?)
        .with_context(|| format!("{} -> {}", previous.display(), current.display()))
}

fn measured(get: f64, update: f64) -> Benchmark {
    Benchmark {
        concurrency: 16,
        get: Measurement {
            throughput_per_sec: get,
        },
        update: Measurement {
            throughput_per_sec: update,
        },
    }
}

#[test]
fn benchmark_drop_threshold() {
    let baseline = measured(100.0, 100.0);
    assert!(compare(&baseline, &measured(79.0, 100.0)).is_err());
    assert!(compare(&baseline, &measured(100.0, 79.0)).is_err());
    assert!(compare(&baseline, &measured(81.0, 81.0)).is_ok());
    assert!(compare(&baseline, &measured(80.0, 80.0)).is_ok());
}

#[test]
fn benchmark_version_order() {
    let mut versions = ["0.1.10", "0.1.9"].map(|text| version(text).unwrap());
    versions.sort();
    assert_eq!(versions, [vec![0, 1, 9], vec![0, 1, 10]]);
}
