use super::{BTreeMap, Duration, Stranded, Value};
use std::fmt::Write as _;

/// Whether the live set is exactly the declared release, or empty for none.
pub fn settled(live: &BTreeMap<String, usize>, target: Option<&str>) -> bool {
    match target {
        None => live.is_empty(),
        Some(digest) => live.len() == 1 && live.contains_key(digest),
    }
}

/// The largest `terminationGracePeriodSeconds` of the pods that are not of
/// the declared release.
pub fn largest_grace(pods: &Value, target: Option<&str>) -> Duration {
    pods["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pod| pod["metadata"]["annotations"]["wamn.release-digest"].as_str() != target)
        .filter_map(|pod| pod["spec"]["terminationGracePeriodSeconds"].as_u64())
        .max()
        .map_or(Duration::ZERO, Duration::from_secs)
}

/// "stranded runs: <run> (<digest>), ...".
pub fn describe(stranded: &[Stranded]) -> String {
    let mut text = String::from("stranded runs, pinned to a release with no live pod:");
    for run in stranded {
        write!(text, " {} ({})", run.run_id, run.manifest_digest)
            .expect("writing to a String succeeds");
    }
    text
}
