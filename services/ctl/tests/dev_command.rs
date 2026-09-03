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
