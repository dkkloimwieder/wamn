//! Compare throughput from the two newest benchmarks in each selected family.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Benchmark {
    concurrency: u32,
    #[serde(flatten)]
    calls: BTreeMap<String, Measurement>,
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
    for call in ["get", "update"] {
        ensure!(
            previous.calls.contains_key(call) && current.calls.contains_key(call),
            "{call}: benchmark has no measurement"
        );
    }
    for (call, previous) in &previous.calls {
        let current = current
            .calls
            .get(call)
            .with_context(|| format!("{call}: current benchmark has no measurement"))?;
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
    gate(&directory, "platform-")
}

fn gate(directory: &Path, prefix: &str) -> anyhow::Result<()> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(version) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(prefix))
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|name| name.starts_with(|character: char| character.is_ascii_digit()))
        else {
            continue;
        };
        files.push((self::version(version)?, path));
    }
    files.sort();
    let [.., (_, previous), (_, current)] = files.as_slice() else {
        println!(
            "benchmark gate passes: fewer than two {prefix} results ({})",
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

#[cfg(test)]
fn measured(get: f64, update: f64) -> Benchmark {
    Benchmark {
        concurrency: 16,
        calls: [("get", get), ("update", update)]
            .map(|(call, throughput_per_sec)| (call.to_owned(), Measurement { throughput_per_sec }))
            .into(),
    }
}

#[cfg(not(test))]
fn main() -> anyhow::Result<()> {
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Arguments {
        /// Compare local application and PostgreSQL results separately.
        #[arg(long)]
        local: bool,
    }

    let arguments = Arguments::parse();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench");
    if arguments.local {
        gate(&directory, "local-")?;
        gate(&directory, "local-pg-")
    } else {
        gate(&directory, "platform-")
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

#[test]
fn local_families_compare_independently() -> anyhow::Result<()> {
    let directory =
        std::env::temp_dir().join(format!("wamn-benchmark-families-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    for (family, throughput) in [("platform", 1000), ("local", 100), ("local-pg", 10000)] {
        for version in ["0.1.9", "0.1.10"] {
            let measurement = serde_json::json!({"throughput_per_sec": throughput});
            std::fs::write(
                directory.join(format!("{family}-{version}.json")),
                serde_json::to_vec(&serde_json::json!({
                    "concurrency": 16,
                    "get": measurement,
                    "update": measurement,
                    "query": measurement,
                    "record_receipt": measurement,
                    "mix": measurement,
                }))?,
            )?;
        }
    }
    let application = gate(&directory, "local-");
    let postgres = gate(&directory, "local-pg-");
    std::fs::remove_dir_all(&directory)?;
    application?;
    postgres
}

#[test]
fn every_baseline_call_keeps_the_same_threshold() {
    for call in [
        "query",
        "record_receipt",
        "mix",
        "select-only",
        "simple-update",
    ] {
        let mut baseline = measured(100.0, 100.0);
        baseline.calls.insert(
            call.into(),
            Measurement {
                throughput_per_sec: 100.0,
            },
        );
        let mut current = measured(100.0, 100.0);
        assert!(compare(&baseline, &current).is_err());
        current.calls.insert(
            call.into(),
            Measurement {
                throughput_per_sec: 79.0,
            },
        );
        assert!(compare(&baseline, &current).is_err());
        current.calls.insert(
            call.into(),
            Measurement {
                throughput_per_sec: 80.0,
            },
        );
        assert!(compare(&baseline, &current).is_ok());
    }
}
