//! The CLI sends target-only requests and keeps route and Job failures fatal.

use std::io::{BufRead as _, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};

fn exchange(listener: &TcpListener, status: u16, answer: Value) -> anyhow::Result<()> {
    let (mut stream, _) = listener.accept()?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = std::io::BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().context("HTTP method")?.to_owned();
    let path = parts.next().context("HTTP path")?.to_owned();
    let mut length = 0;
    let mut authorized = false;
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').context("HTTP header")?;
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse()?;
        }
        if name.eq_ignore_ascii_case("authorization") {
            authorized = value.trim() == "Bearer caller-session";
        }
    }
    assert!(authorized);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let _value = if method == "GET" {
        assert!(body.is_empty());
        let query = path.split_once('?').context("read query")?.1;
        Value::Object(
            url::form_urlencoded::parse(query.as_bytes())
                .map(|(name, value)| {
                    serde_json::from_str(&value).map(|value| (name.into_owned(), value))
                })
                .collect::<Result<_, _>>()?,
        )
    } else {
        let [mut element]: [Value; 1] = serde_json::from_slice(&body)?;
        assert!(element["request_id"].is_string());
        element.as_object_mut().unwrap().remove("value").unwrap()
    };
    let body = serde_json::to_vec(&[answer])?;
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    Ok(())
}

fn command(root: &Path, url: &str, arguments: &[&str]) -> anyhow::Result<Command> {
    // A declared application stays a published selection even when its name is a directory.
    std::fs::create_dir_all(root.join("a"))?;
    let file = root.join("env.yaml");
    let document: wamn_env::env::document::EnvFile = serde_json::from_value(json!({
        "org":"acme", "env":"dev", "target":"cloud", "revocation_seconds":60,
        "database":{"instances":1,"storage":"2Gi","cpu":"200m","memory":"256Mi"},
        "projects":[{"name":"apps","apps":{"a":{}}}]
    }))?;
    std::fs::write(&file, document.to_yaml()?)?;
    let mut command = control_command(root, url);
    command.args(arguments).arg("--env").arg(file);
    Ok(command)
}

fn control_command(root: &Path, url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wamn"));
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .env_clear()
        .env("WAMN_CONTROL_URL", url)
        .env("WAMN_TOKEN", "caller-session");
    command
}

fn run(
    root: &Path,
    arguments: &[&str],
    responses: Vec<(u16, Value)>,
) -> anyhow::Result<(Output, Vec<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    let mut command = command(root, &url, arguments)?;
    let server = std::thread::spawn(move || -> anyhow::Result<Vec<()>> {
        responses
            .into_iter()
            .map(|(status, answer)| exchange(&listener, status, answer))
            .collect()
    });
    let output = command.output()?;
    // A failed child must not leave the server waiting for an expected request.
    if server.is_finished() {
        return Ok((output, server.join().unwrap()?));
    }
    // Wake accept if the CLI stopped before sending all requests.
    let _ = TcpStream::connect(url.trim_start_matches("http://"));
    Ok((output, server.join().unwrap()?))
}

#[test]
fn route_denials_and_failed_jobs_exit_nonzero() -> anyhow::Result<()> {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let digest = (format!("sha256:{}", "a".repeat(64)))
        .parse::<wamn_digest::Digest>()
        .unwrap();
    for responses in [
        vec![(403, json!({"error":{"code":"forbidden"}}))],
        vec![(200, json!({"error":{"code":"project_admin_required"}}))],
        vec![
            (200, json!({"value":{"job":"op-fixture"}})),
            (200, json!({"error":{"code":"not_found"}})),
        ],
        vec![
            (200, json!({"value":{"job":"op-fixture"}})),
            (
                200,
                json!({"value":{"job":"op-fixture", "state":"failed", "log":"SQL execution failed"}}),
            ),
        ],
    ] {
        let (output, _) = run(
            scratch.path(),
            &["deploy", "--app", "a", "--release", &digest.to_string()],
            responses,
        )?;
        assert!(!output.status.success());
    }
    Ok(())
}

const UNREACHABLE_CONTROL_URL: &str = "http://127.0.0.1:0";

fn assert_refuses(command: &mut Command, expected: &str) -> anyhow::Result<()> {
    let output = command.output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success(), "{command:?} succeeded");
    assert!(stderr.contains(expected), "expected {expected:?}: {stderr}");
    Ok(())
}

#[test]
fn env_data_deletion_confirmation_without_terminal() -> anyhow::Result<()> {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    for (confirmation, expected) in [
        (&[][..], "data deletion requires a terminal confirmation"),
        (
            &["--confirm", "apps"][..],
            "environment confirmation differs",
        ),
        (&["--confirm", " acme/dev/apps "][..], "call control route"),
    ] {
        assert_refuses(
            control_command(scratch.path(), UNREACHABLE_CONTROL_URL)
                .args(["env", "deactivate-project", "acme/dev/apps", "--data"])
                .args(confirmation),
            expected,
        )?;
    }
    let output = control_command(scratch.path(), UNREACHABLE_CONTROL_URL)
        .args([
            "env",
            "deactivate-project",
            "acme/dev/apps",
            "--confirm",
            "acme/dev/apps",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    Ok(())
}

#[test]
fn schema_reset_confirmation_without_terminal() -> anyhow::Result<()> {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    for (confirmation, expected) in [
        (&[][..], "schema reset requires a terminal confirmation"),
        (
            &["--confirm", "acme/dev/apps"][..],
            "project confirmation differs",
        ),
        (&["--confirm", " apps "][..], "call control route"),
    ] {
        let mut command = command(
            scratch.path(),
            UNREACHABLE_CONTROL_URL,
            &["schema", "reset", "--project", "apps"],
        )?;
        assert_refuses(command.args(confirmation), expected)?;
    }
    Ok(())
}
