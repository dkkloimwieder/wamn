use super::{DeserializeOwned, Serialize};
use anyhow::Context as _;

/// kubectl writes this prefix for every transport failure to the API server.
pub(super) fn unreachable(stderr: &str) -> bool {
    stderr.starts_with("Unable to connect to the server:")
}

pub(super) fn apply_request<T: Serialize>(
    object: &T,
) -> anyhow::Result<([&'static str; 4], Vec<u8>)> {
    Ok((
        ["apply", "--server-side", "-f", "-"],
        serde_json::to_vec(object).context("encode the kubectl object")?,
    ))
}

pub(super) fn get_arguments(kind: &str, name: &str) -> [String; 5] {
    [
        "get".into(),
        format!("{kind}/{name}"),
        "-o".into(),
        "json".into(),
        "--ignore-not-found".into(),
    ]
}

pub(super) fn decode_object<T: DeserializeOwned>(output: &str) -> anyhow::Result<Option<T>> {
    if output.trim().is_empty() {
        Ok(None)
    } else {
        serde_json::from_str(output)
            .map(Some)
            .context("decode the kubectl object")
    }
}

pub(super) fn wait_arguments(
    kind: &str,
    name: &str,
    condition: &str,
    duration: &str,
) -> [String; 4] {
    [
        "wait".into(),
        format!("{kind}/{name}"),
        format!("--for={condition}"),
        format!("--timeout={duration}"),
    ]
}
