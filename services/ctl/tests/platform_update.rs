//! Targeted platform commands inspect one existing owner and never enter installation.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use wamn_environment_control::platform::artifact_plan::test_util::prepared_chart;
use wamn_environment_control::platform::document::{ClusterDocument, UpgradeTarget};

const TOOL: &str = r"#!/usr/bin/python3
import json, pathlib, re, sys
root = pathlib.Path(__file__).parent
tool = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps({'tool': tool, 'args': args}) + '\n')
state = json.loads((root / 'state.json').read_text())
if tool == 'kind':
    if args[:2] == ['get', 'clusters']:
        print(state['cluster'])
        sys.exit(0)
    if args[:2] == ['export', 'kubeconfig']:
        sys.exit(0)
if tool == 'helm':
    args = args[4:] # Global kubeconfig and context arguments.
    namespace = args[args.index('--namespace') + 1]
    if args[0] == 'list':
        pattern = args[args.index('--filter') + 1]
        print(json.dumps([{'name': key.split('/')[1]} for key in state['owners']
            if key.split('/')[0] == namespace and re.fullmatch(pattern, key.split('/')[1])]))
        sys.exit(0)
    if args[:2] == ['get', 'values']:
        print(json.dumps(state['owners'][namespace + '/' + args[2]]))
        sys.exit(0)
sys.exit(1)
";

fn assert_read_only(calls: &Path, owner: (&str, &str)) -> anyhow::Result<()> {
    let mut inspected = false;
    for line in std::fs::read_to_string(calls)?.lines() {
        let call: Value = serde_json::from_str(line)?;
        let args = call["args"].as_array().unwrap();
        match call["tool"].as_str().unwrap() {
            "kind" => assert!(args[0] == "get" || args[0] == "export", "{call}"),
            "helm" => {
                let args = &args[4..];
                assert!(
                    args[0] == "list" || (args[0] == "get" && args[1] == "values"),
                    "{call}"
                );
                let namespace = args.iter().position(|arg| arg == "--namespace").unwrap();
                assert_eq!(args[namespace + 1], owner.1);
                if args[0] == "list" {
                    let filter = args.iter().position(|arg| arg == "--filter").unwrap();
                    assert_eq!(args[filter + 1], format!("^{}$", owner.0));
                } else {
                    assert_eq!(args[2], owner.0);
                }
                inspected = true;
            }
            _ => panic!("targeted update invoked an unrelated or mutating tool: {call}"),
        }
    }
    assert!(inspected, "targeted update never inspected its owner");
    Ok(())
}

#[test]
fn platform_updates_refuse_missing_owners_and_pending_owner_separation_without_mutation()
-> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let file = repository.join("deploy/kind/cluster.k");
    let document = ClusterDocument::compile(&file)?;
    let chart = prepared_chart(&repository)?;
    let bin = chart.0.join("commands");
    std::fs::create_dir(&bin)?;
    for name in ["kind", "helm", "kubectl", "docker", "gcloud"] {
        let path = bin.join(name);
        std::fs::write(&path, TOOL)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))?;
    let calls = bin.join("calls.jsonl");
    for (selection, issue) in [
        ("control", "DEP4-03"),
        ("identity", "DEP4-03"),
        ("host:alpha/alpha", "DEP4-04"),
    ] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&document);
        for installed in [
            None,
            Some(json!({})),
            Some(json!({
                "platform":{"chart":chart.0},
                "runtime-operator":{"runtime":{"hostGroups":[{"name":"alpha"}]}}
            })),
        ] {
            let mut owners = json!({});
            if let Some(values) = &installed {
                owners[format!("{namespace}/{name}")] = values.clone();
            }
            let state = json!({"cluster":document.cluster_name, "owners":owners});
            let state_file = bin.join("state.json");
            std::fs::write(&state_file, serde_json::to_vec(&state)?)?;
            std::fs::write(&calls, "")?;
            let output = Command::new(env!("CARGO_BIN_EXE_wamn"))
                .args(["platform", "deploy"])
                .arg(&file)
                .arg("--chart")
                .arg(&chart.0)
                .args(["--only", selection])
                .env("PATH", &path)
                .output()?;
            let error = String::from_utf8(output.stderr)?;
            assert!(!output.status.success());
            match &installed {
                None => assert!(
                    error.contains(&format!("existing release {namespace}/{name}")),
                    "{error}"
                ),
                Some(values) if values.as_object().unwrap().is_empty() => {
                    let expected = if matches!(target, UpgradeTarget::Host { .. }) {
                        "no host groups"
                    } else {
                        "initialized release"
                    };
                    assert!(error.contains(expected), "{error}");
                }
                Some(_) => assert!(error.contains(issue), "{error}"),
            }
            assert_read_only(&calls, (&name, namespace))?;
        }
    }
    Ok(())
}
