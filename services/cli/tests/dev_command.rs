use std::process::Command;

/// Adding `up` must not have made the loop's own required input optional.
#[test]
fn the_bare_development_command_still_requires_its_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_wamn"))
        .args(["dev", "--overlay-root", "apps/platform_fixture_overlay"])
        .output()
        .expect("run wamn without its configuration");
    assert!(!output.status.success());
}

#[test]
fn unreadable_configuration_refuses_before_any_stage_runs() {
    let missing =
        std::env::temp_dir().join(format!("wamn-dev-missing-config-{}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_wamn"))
        .args(["dev", "--config"])
        .arg(&missing)
        .args(["--overlay-root", "apps/platform_fixture_overlay"])
        .output()
        .expect("run wamn dev with a missing config");
    assert!(!output.status.success());
}
