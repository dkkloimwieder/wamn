//! The CLI sends target-only requests and keeps route and Job failures fatal.

use std::collections::BTreeSet;
use std::io::{BufRead as _, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_catalog::{HostAttachment, HostHandler, HostRouteSet};

struct Request {
    method: String,
    path: String,
    value: Value,
}

fn exchange(listener: &TcpListener, status: u16, answer: Value) -> anyhow::Result<Request> {
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
    let value = if method == "GET" {
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
    Ok(Request {
        method,
        path,
        value,
    })
}

fn field_name(field: &Value, read: bool) -> Option<&str> {
    let path = field["path"].as_str().unwrap();
    if read {
        Some(path)
    } else {
        path.strip_prefix("value.")
    }
}

fn contract(attachment: &HostAttachment, request: &Request) -> anyhow::Result<()> {
    let contract = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/catalog/model/src/host_route/control/contracts")
        .join(format!(
            "{}.input.json",
            attachment.route.operation.replace('.', "/")
        ));
    let contract: Value = serde_json::from_slice(&std::fs::read(contract)?)?;
    let fields = contract["fields"].as_array().unwrap();
    let read = attachment.route.type_.is_read();
    let allowed: BTreeSet<_> = fields
        .iter()
        .filter_map(|field| field_name(field, read))
        .collect();
    let required: BTreeSet<_> = fields
        .iter()
        .filter(|field| field["nullable"] == false)
        .filter_map(|field| field_name(field, read))
        .collect();
    let actual: BTreeSet<_> = request
        .value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        actual.is_subset(&allowed),
        "{}: {actual:?}",
        attachment.route.operation
    );
    assert!(required.is_subset(&actual));
    assert_eq!(request.value["project"], "apps");
    assert_eq!(request.value["env"], "dev");
    assert_eq!(request.method, if read { "GET" } else { "POST" });
    assert_eq!(
        request.path.split('?').next().unwrap(),
        attachment.definition["route"]["path"].as_str().unwrap()
    );
    Ok(())
}

fn command(root: &Path, url: &str, arguments: &[&str]) -> anyhow::Result<Command> {
    // A declared application stays a published selection even when its name is a directory.
    std::fs::create_dir_all(root.join("a"))?;
    let file = root.join("environment.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({
            "org":"acme", "project":"apps", "env":"dev", "target":"cloud", "policy":"dev","revocation_seconds":60,
            "applications":{"a":{"current":"none", "revocation_seconds":5,"hostname":"a.invalid", "packages":["a"]}}
        }))?,
    )?;
    let internal = arguments.first() == Some(&"env");
    let mut command = Command::new(if internal {
        env!("CARGO_BIN_EXE_wamn-ctl")
    } else {
        env!("CARGO_BIN_EXE_wamn")
    });
    command
        .current_dir(root)
        .env_clear()
        .env("WAMN_CONTROL_URL", url)
        .env("WAMN_TOKEN", "caller-session")
        .args(arguments);
    if !internal {
        command.arg("--env");
    }
    command.arg(file);
    Ok(command)
}

fn run(
    root: &Path,
    arguments: &[&str],
    responses: Vec<(u16, Value)>,
) -> anyhow::Result<(Output, Vec<Request>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    let mut command = command(root, &url, arguments)?;
    let failure = responses
        .iter()
        .any(|(_, answer)| answer.get("error").is_some() || answer["value"]["state"] == "failed");
    let server = std::thread::spawn(move || -> anyhow::Result<Vec<Request>> {
        responses
            .into_iter()
            .map(|(status, answer)| exchange(&listener, status, answer))
            .collect()
    });
    let direct = root.join("direct");
    std::fs::create_dir_all(&direct)?;
    for name in ["kubectl", "helm"] {
        let file = direct.join(name);
        std::fs::write(&file, "#!/bin/sh\n: > \"${0%/*}/called\"\nexit 98\n")?;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700))?;
    }
    command.env("PATH", &direct);
    // Denials stay fatal even when the caller also supplies trusted inputs.
    if failure {
        command.env("KUBECONFIG", root.join("kubeconfig")).env(
            "WAMN_APPLY_DATABASE_URL",
            "postgres://unused.invalid/system",
        );
    }
    let output = command.output()?;
    assert!(
        !direct.join("called").exists(),
        "remote command called a direct deployment tool"
    );
    // A failed child must not leave the server waiting for an expected request.
    if server.is_finished() {
        return Ok((output, server.join().unwrap()?));
    }
    // Wake accept if the CLI stopped before sending all requests.
    let _ = TcpStream::connect(url.trim_start_matches("http://"));
    Ok((output, server.join().unwrap()?))
}

#[test]
fn remote_commands_send_only_contract_targets_without_direct_credentials() -> anyhow::Result<()> {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let digest = format!("sha256:{}", "a".repeat(64));
    for (_, attachment) in HostRouteSet::Control
        .attachments()
        .filter(|(_, attachment)| {
            attachment.route.handler == HostHandler::OperationJob
                && attachment.route.operation != "operation.status"
        })
    {
        let arguments: Vec<&str> = match attachment.route.operation {
            "application.deploy" => vec!["deploy", "--release", &digest],
            "application.rollback" => vec!["rollback", "a", "--release", &digest],
            "schema.diff" => vec!["schema", "diff", "a", "--release", &digest],
            "schema.apply" => vec![
                "schema",
                "apply",
                "a",
                "--release",
                &digest,
                "--resolve",
                "a:migrations/0001.sql=retry",
            ],
            "schema.reset" => vec!["schema", "reset"],
            "schema.adopt" => vec!["schema", "adopt"],
            operation => panic!("add CLI coverage for {operation}"),
        };
        let mut responses = vec![(200, json!({"value":{"job":"op-fixture"}}))];
        if attachment.route.operation == "application.deploy" {
            for state in ["pending", "pending", "running", "running"] {
                responses.push((
                    200,
                    json!({"value":{"job":"op-fixture", "state":state, "log":"starting\nrepeated log tail"}}),
                ));
            }
        }
        responses.push((200, json!({"value":{"job":"op-fixture", "state":"succeeded", "log":"completed SQL or deployment"}})));
        let mut states: Vec<_> = responses[1..]
            .iter()
            .map(|(_, answer)| answer["value"]["state"].clone())
            .collect();
        states.dedup();
        let log = responses.last().unwrap().1["value"]["log"]
            .as_str()
            .unwrap()
            .to_owned();
        let (output, requests) = run(scratch.path(), &arguments, responses)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        contract(attachment, &requests[0])?;
        for pair in arguments.windows(2) {
            if let Some(field) = pair[0].strip_prefix("--") {
                assert_eq!(requests[0].value[field], pair[1]);
            }
        }
        if attachment.route.operation != "application.deploy" && arguments.contains(&"a") {
            assert_eq!(requests[0].value["application"], "a");
        }
        for request in &requests[1..] {
            contract(
                wamn_ctl::component_verbs::route("operation.status")?,
                request,
            )?;
            assert_eq!(request.value["job"], "op-fixture");
        }
        let output = String::from_utf8(output.stdout)?;
        let mut lines = output.splitn(states.len() + 1, '\n');
        for expected in states {
            let transition: Value = serde_json::from_str(lines.next().unwrap())?;
            assert_eq!(transition["state"], expected);
            assert!(transition.get("log").is_none());
        }
        assert_eq!(lines.next(), Some(format!("{log}\n").as_str()));
    }
    Ok(())
}

#[test]
fn route_denials_and_failed_jobs_exit_nonzero_without_direct_fallback() -> anyhow::Result<()> {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let digest = format!("sha256:{}", "a".repeat(64));
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
        let failure = responses.last().unwrap().1.clone();
        let (output, _) = run(scratch.path(), &["deploy", "--release", &digest], responses)?;
        assert!(!output.status.success());
        if let Some(code) = failure["error"]["code"].as_str() {
            assert!(String::from_utf8_lossy(&output.stderr).contains(code));
        } else {
            let output = String::from_utf8(output.stdout)?;
            let (state, log) = output.split_once('\n').context("state before final log")?;
            let terminal: Value = serde_json::from_str(state)?;
            assert_eq!(terminal["state"], failure["value"]["state"]);
            assert!(terminal.get("log").is_none());
            assert_eq!(log.strip_suffix('\n'), failure["value"]["log"].as_str());
        }
    }
    Ok(())
}

#[test]
fn env_rollback_uses_the_control_operation() -> anyhow::Result<()> {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let (output, requests) = run(
        scratch.path(),
        &["env", "rollback", "--application", "a"],
        vec![
            (200, json!({"value":{"job":"op-fixture"}})),
            (
                200,
                json!({"value":{"job":"op-fixture", "state":"succeeded", "log":"rolled back"}}),
            ),
        ],
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    contract(
        wamn_ctl::component_verbs::route("application.rollback")?,
        &requests[0],
    )?;
    assert_eq!(requests[0].value["application"], "a");
    assert!(requests[0].value["release"].is_null());
    Ok(())
}
