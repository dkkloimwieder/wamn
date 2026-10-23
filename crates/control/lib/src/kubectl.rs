//! Shared kubectl execution and typed object operations.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context as _, bail};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

/// kubectl could not reach the API server, so the request did not run.
///
/// A caller that polls can retry it until its own deadline (wamn-axz4u).
#[derive(Debug)]
pub struct Unreachable(String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// kubectl writes this prefix for every transport failure to the API server.
fn unreachable(stderr: &str) -> bool {
    stderr.starts_with("Unable to connect to the server:")
}

/// Run `kubectl` against the selected cluster and namespace.
///
/// Calls with input suppress standard error because the input can contain Secrets.
///
/// # Errors
///
/// Returns an error if the process fails or its output is not UTF-8.
pub async fn kubectl(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> anyhow::Result<String> {
    run(
        kubeconfig,
        context,
        namespace,
        arguments,
        input,
        input.is_none(),
    )
    .await
}

async fn run(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    arguments: &[&str],
    input: Option<&[u8]>,
    show_error: bool,
) -> anyhow::Result<String> {
    let mut child = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args(["--context", context, "-n", namespace])
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start kubectl")?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().context("open the kubectl input")?;
        stdin
            .write_all(input)
            .await
            .context("write the kubectl input")?;
        drop(stdin);
    }
    let output = child.wait_with_output().await.context("wait for kubectl")?;
    if !output.status.success() {
        if !show_error {
            bail!(
                "kubectl {} exited {}",
                arguments.first().copied().unwrap_or_default(),
                output.status
            );
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = format!(
            "kubectl {} exited {}: {}",
            arguments.join(" "),
            output.status,
            stderr.trim()
        );
        if unreachable(stderr.trim()) {
            return Err(Unreachable(message).into());
        }
        bail!(message);
    }
    Ok(String::from_utf8(output.stdout)
        .context("kubectl wrote text")?
        .trim()
        .to_owned())
}

/// Apply one typed object with server-side apply.
///
/// A failure names kubectl's standard error unless the object is a Secret.
///
/// # Errors
///
/// Returns an error if serialization or kubectl fails.
pub async fn apply<T: Serialize>(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    object: &T,
) -> anyhow::Result<()> {
    let (arguments, input) = apply_request(object)?;
    let secret = serde_json::to_value(object)
        .context("encode the kubectl object")?
        .get("kind")
        .is_none_or(|kind| kind == "Secret");
    run(
        kubeconfig,
        context,
        namespace,
        &arguments,
        Some(&input),
        !secret,
    )
    .await
    .map(drop)
}

fn apply_request<T: Serialize>(object: &T) -> anyhow::Result<([&'static str; 4], Vec<u8>)> {
    Ok((
        ["apply", "--server-side", "-f", "-"],
        serde_json::to_vec(object).context("encode the kubectl object")?,
    ))
}

/// Read one typed object, or return `None` if it is absent.
///
/// # Errors
///
/// Returns an error if kubectl fails or the object cannot be decoded.
pub async fn get<T: DeserializeOwned>(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    kind: &str,
    name: &str,
) -> anyhow::Result<Option<T>> {
    let arguments = get_arguments(kind, name);
    let arguments: Vec<_> = arguments.iter().map(String::as_str).collect();
    let output = kubectl(kubeconfig, context, namespace, &arguments, None).await?;
    decode_object(&output)
}

fn get_arguments(kind: &str, name: &str) -> [String; 5] {
    [
        "get".into(),
        format!("{kind}/{name}"),
        "-o".into(),
        "json".into(),
        "--ignore-not-found".into(),
    ]
}

fn decode_object<T: DeserializeOwned>(output: &str) -> anyhow::Result<Option<T>> {
    if output.trim().is_empty() {
        Ok(None)
    } else {
        serde_json::from_str(output)
            .map(Some)
            .context("decode the kubectl object")
    }
}

/// Wait for a condition, using kubectl's condition and duration syntax.
///
/// # Errors
///
/// Returns an error if kubectl fails, including when the timeout expires.
pub async fn wait(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    kind: &str,
    name: &str,
    condition: &str,
    duration: &str,
) -> anyhow::Result<()> {
    let arguments = wait_arguments(kind, name, condition, duration);
    let arguments: Vec<_> = arguments.iter().map(String::as_str).collect();
    kubectl(kubeconfig, context, namespace, &arguments, None)
        .await
        .map(drop)
}

fn wait_arguments(kind: &str, name: &str, condition: &str, duration: &str) -> [String; 4] {
    [
        "wait".into(),
        format!("{kind}/{name}"),
        format!("--for={condition}"),
        format!("--timeout={duration}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::{apply_request, decode_object, get_arguments, unreachable, wait_arguments};

    #[test]
    fn only_a_transport_failure_is_unreachable() {
        assert!(unreachable(
            "Unable to connect to the server: net/http: TLS handshake timeout"
        ));
        assert!(unreachable(
            "Unable to connect to the server: dial tcp 35.184.136.135:443: i/o timeout"
        ));
        assert!(!unreachable(
            "Error from server (Forbidden): jobs.batch \"x\" is forbidden"
        ));
    }
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Object {
        kind: String,
        metadata: Metadata,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Metadata {
        name: String,
    }

    #[test]
    fn apply_serializes_one_typed_object_to_standard_input() {
        let object = Object {
            kind: "Namespace".into(),
            metadata: Metadata {
                name: "platform".into(),
            },
        };
        let (arguments, input) = apply_request(&object).unwrap();
        assert_eq!(arguments, ["apply", "--server-side", "-f", "-"]);
        assert_eq!(
            input,
            br#"{"kind":"Namespace","metadata":{"name":"platform"}}"#
        );
        assert_eq!(
            decode_object::<Object>(std::str::from_utf8(&input).unwrap()).unwrap(),
            Some(object)
        );
    }

    #[test]
    fn get_uses_json_and_allows_an_absent_object() {
        assert_eq!(
            get_arguments("namespace", "platform"),
            [
                "get",
                "namespace/platform",
                "-o",
                "json",
                "--ignore-not-found"
            ]
        );
        assert_eq!(decode_object::<Object>(" \n").unwrap(), None);
        assert!(decode_object::<Object>("not json").is_err());
    }

    #[test]
    fn wait_passes_the_condition_and_duration() {
        assert_eq!(
            wait_arguments("job", "install", "condition=Complete", "120s"),
            [
                "wait",
                "job/install",
                "--for=condition=Complete",
                "--timeout=120s"
            ]
        );
    }
}
