//! Shared help tests for service binaries.

use std::collections::BTreeSet;
use std::ffi::OsString;

use clap::CommandFactory as _;

fn environment_names(command: &clap::Command, names: &mut BTreeSet<OsString>) {
    for argument in command.get_arguments() {
        if let Some(name) = argument.get_env() {
            names.insert(name.to_owned());
        }
    }
    for subcommand in command.get_subcommands() {
        environment_names(subcommand, names);
    }
}

fn is_secret_environment(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    // These keys select files or an environment target, not credential contents.
    if name.ends_with("_FILE")
        || name.ends_with("_PATH")
        || name.ends_with("_PATHS")
        || matches!(
            name.as_ref(),
            "WAMN_NATS_TLS_KEY" | "WAMN_DEV_ENV_NATS_TLS_KEY" | "WAMN_MANAGEMENT_PROJECT_ENV_KEY"
        )
    {
        return false;
    }
    [
        "URL",
        "PASSWORD",
        "TOKEN",
        "SECRET",
        "KEY",
        "PAT",
        "CREDENTIAL",
    ]
    .iter()
    .any(|part| name.contains(part))
}

fn environment_marker(name: &std::ffi::OsStr) -> String {
    format!("__WAMN_HELP_SECRET_{}__", name.to_string_lossy())
}

fn find_leaks(
    command: &mut clap::Command,
    markers: &[(String, bool)],
    leaks: &mut Vec<String>,
) -> bool {
    let help = command.render_long_help().to_string();
    let mut visible_non_secret = false;
    for (marker, secret) in markers {
        if help.contains(marker) {
            if *secret {
                leaks.push(format!("{}: {marker}", command.get_name()));
            } else {
                visible_non_secret = true;
            }
        }
    }
    for subcommand in command.get_subcommands_mut() {
        visible_non_secret |= find_leaks(subcommand, markers, leaks);
    }
    visible_non_secret
}

#[test]
fn help_hides_environment_values() {
    const CHILD: &str = "_WAMN_HELP_ENV_TEST_CHILD";
    let mut command = crate::Cli::command();
    let mut names = BTreeSet::new();
    environment_names(&command, &mut names);
    assert!(
        !names.is_empty(),
        "the command must read environment inputs"
    );

    if std::env::var_os(CHILD).is_some() {
        let markers: Vec<_> = names
            .iter()
            .map(|name| (environment_marker(name), is_secret_environment(name)))
            .collect();
        let mut leaks = Vec::new();
        let visible_non_secret = find_leaks(&mut command, &markers, &mut leaks);
        assert!(
            leaks.is_empty(),
            "help exposed environment values: {leaks:?}"
        );
        assert!(
            visible_non_secret,
            "help must show at least one non-secret environment marker"
        );
        return;
    }

    // Only the child receives markers. Parallel tests keep their own environment,
    // and clap reads the real environment when the child constructs its command.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args([
        "--exact",
        "help_env_tests::help_hides_environment_values",
        "--nocapture",
        "--test-threads=1",
    ]);
    child.env(CHILD, "1");
    for name in names {
        child.env(&name, environment_marker(&name));
    }
    let output = child
        .output()
        .expect("run help test with environment markers");
    assert!(
        output.status.success(),
        "help subprocess failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
