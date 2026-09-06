use std::process::Command;

fn output(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wamn"))
        .args(arguments)
        .output()
        .expect("run wamn product command")
}

#[test]
fn product_binary_exposes_only_the_development_command() {
    let output = output(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(help.contains("dev"));
    assert!(!help.contains("provision-project"));
    assert!(!help.contains("publish-release"));
}

#[test]
fn development_command_requires_its_two_explicit_inputs_and_offers_watch() {
    let output = output(&["dev", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    for input in ["--config <FILE>", "--overlay-root <DIRECTORY>", "--watch"] {
        assert!(help.contains(input), "development help omitted {input}");
    }
}

/// `up` is a subcommand of `dev`, and it names the two inputs the spawned Gate
/// added: the binary to spawn and the fixed port to spawn it on
/// (wamn-10yt.10.32).
#[test]
fn dev_up_names_the_scenario_worker_binary_and_its_fixed_port() {
    let output = output(&["dev", "up", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    for input in [
        "--scenario-worker-binary",
        "--gate-bind",
        "--system-database-url",
        "--root",
        "--package",
    ] {
        assert!(help.contains(input), "dev up help omitted {input}");
    }
}

/// Adding `up` must not have made the loop's own required input optional.
#[test]
fn the_bare_development_command_still_requires_its_configuration() {
    let output = output(&["dev", "--overlay-root", "packages/client_acme_receiving"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("--config"),
        "the missing-configuration refusal does not name --config: {stderr}"
    );
}

/// The Gate is a spawned child now, so it cannot report back an ephemeral port
/// the way the in-process launch did. Port 0 is refused, the refusal NAMES the
/// address, and it lands before any credential is minted.
#[test]
fn dev_up_refuses_an_ephemeral_gate_port_and_names_it() {
    let root = std::env::temp_dir().join(format!("wamn-dev-up-ephemeral-{}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_wamn"))
        .args([
            "dev",
            "up",
            "--system-database-url",
            "postgresql://postgres@127.0.0.1:1/postgres",
        ])
        .arg("--root")
        .arg(&root)
        .args([
            "--scenario-worker-binary",
            "/nonexistent/wamn-scenario-worker",
            "--gate-bind",
            "127.0.0.1:0",
            "--nats-url",
            "nats://127.0.0.1:1",
            "--tempo-query-url",
            "http://127.0.0.1:1",
            "--otel-exporter-otlp-endpoint",
            "http://127.0.0.1:1",
            "--component-artifact-base",
            "127.0.0.1:1/wamn/components",
            "--release-artifact-base",
            "127.0.0.1:1/wamn/releases",
            "--registry-auth-file",
            "/nonexistent/.dockerconfigjson",
            "--route-host",
            "receiving.localhost",
            "--flow-http-workload-image",
            "127.0.0.1:1/wamn/flow-http:dev",
            "--host-binary",
            "/nonexistent/wamn-host",
            "--package",
            "packages/receiving",
        ])
        .output()
        .expect("run wamn dev up with an ephemeral gate port");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("127.0.0.1:0"),
        "the ephemeral-port refusal does not name the address: {stderr}"
    );
    // The refusal precedes standup, so nothing was created to clean up.
    assert!(
        !root.exists(),
        "wamn dev up created {} before settling its Gate address",
        root.display()
    );
}

/// The Gate is a real child process, and readiness watches its port rather than
/// a route on it (wamn-10yt.10.32). Spawn something that is not a Gate and dies
/// at once: the refusal says so and NAMES the port readiness was watching.
#[test]
fn a_gate_that_dies_before_listening_is_reported_against_its_port() {
    let credentials = wamn_ctl::dev::environment::JourneyCredentials {
        guest_sql: String::new(),
        executor_platform: String::new(),
        event_materializer: String::new(),
        http_admitter: String::new(),
        identity_reader: "postgresql://unused.invalid/unused".to_owned(),
        control_author: "postgresql://unused.invalid/unused".to_owned(),
        management_admitter: String::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a runtime for the spawned Gate");
    let error = runtime
        .block_on(wamn_ctl::dev::environment::spawn_journey_management_gate(
            std::path::Path::new("/bin/false"),
            &credentials,
            "postgresql://unused.invalid/unused",
            "127.0.0.1:18099",
        ))
        .expect_err("/bin/false is not a management Gate");
    let rendered = error.to_string();
    assert!(
        rendered.contains("127.0.0.1:18099"),
        "the readiness refusal does not name the port: {rendered}"
    );
    assert!(
        rendered.contains("stopped before listening"),
        "the readiness refusal does not say the Gate died: {rendered}"
    );
}

#[test]
fn unreadable_configuration_refuses_before_any_stage_runs() {
    let missing =
        std::env::temp_dir().join(format!("wamn-dev-missing-config-{}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_wamn"))
        .args(["dev", "--config"])
        .arg(&missing)
        .args(["--overlay-root", "packages/client_acme_receiving"])
        .output()
        .expect("run wamn dev with a missing config");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("read development config"));
    assert!(stderr.contains(&missing.display().to_string()));
}
