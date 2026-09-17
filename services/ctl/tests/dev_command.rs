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

/// `up` is a subcommand of `dev`, and it names its required inputs.
#[test]
fn dev_up_names_its_required_inputs() {
    let output = output(&["dev", "up", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    for input in [
        "--system-database-url",
        "--root",
        "--package",
        "--flow-http-component",
    ] {
        assert!(help.contains(input), "dev up help omitted {input}");
    }
}

/// Adding `up` must not have made the loop's own required input optional.
#[test]
fn the_bare_development_command_still_requires_its_configuration() {
    let output = output(&["dev", "--overlay-root", "apps/client_acme_receiving"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("--config"),
        "the missing-configuration refusal does not name --config: {stderr}"
    );
}

/// The Gate is a real child process, and readiness watches its port rather than
/// a route on it (wamn-10yt.10.32). Spawn something that is not a Gate and dies
/// at once: the refusal says so and NAMES the port readiness was watching.
#[test]
fn a_gate_that_dies_before_listening_is_reported_against_its_port() {
    let credentials = wamn_control::dev::environment::JourneyCredentials {
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
        .block_on(
            wamn_control::dev::environment::spawn_journey_management_gate(
                std::path::Path::new("/bin/false"),
                &credentials,
                "postgresql://unused.invalid/unused",
                "127.0.0.1:18099",
            ),
        )
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
        .args(["--overlay-root", "apps/client_acme_receiving"])
        .output()
        .expect("run wamn dev with a missing config");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("read development config"));
    assert!(stderr.contains(&missing.display().to_string()));
}
