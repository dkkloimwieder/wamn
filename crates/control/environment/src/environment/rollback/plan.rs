use super::Value;

/// The revisions whose status is `deployed` or `superseded`, newest first.
pub fn successful_newest_first(history: &[Value]) -> Vec<u64> {
    let mut revisions: Vec<u64> = history
        .iter()
        .filter(|entry| matches!(entry["status"].as_str(), Some("deployed" | "superseded")))
        .filter_map(|entry| entry["revision"].as_u64())
        .collect();
    revisions.sort_unstable_by(|a, b| b.cmp(a));
    revisions
}
