use super::{BTreeMap, Value, release_digest};

/// Release digest to the count of host pods with a deletion timestamp that
/// are not terminated: the pods that are draining.
pub(super) fn terminating(pods: &Value) -> BTreeMap<String, usize> {
    let mut terminating = BTreeMap::new();
    for pod in pods["items"].as_array().into_iter().flatten() {
        if pod["metadata"]["deletionTimestamp"].is_null()
            || matches!(
                pod["status"]["phase"].as_str(),
                Some("Succeeded" | "Failed")
            )
        {
            continue;
        }
        *terminating
            .entry(release_digest(pod).to_owned())
            .or_insert(0) += 1;
    }
    terminating
}
