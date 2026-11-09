//! Command coverage with recorded cluster tools and real Helm rendering.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use wamn_environment_control::platform::artifact_plan::test_util::{Chart, prepared_chart};
use wamn_environment_control::platform::document::{ClusterDocument, UpgradeTarget};
use wamn_environment_control::platform::{artifact_plan, control, identity};

const TOOL: &str = r"#!/usr/bin/python3
import base64, json, pathlib, re, subprocess, sys, yaml
root = pathlib.Path(__file__).parent
tool = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
state = json.loads((root / 'state.json').read_text())
call = {'tool': tool, 'args': args}
if tool == 'helm' and 'upgrade' in args:
    values = args[args.index('-f') + 1]
    call['values'] = yaml.safe_load(pathlib.Path(values).read_text())
    upgrade = args.index('upgrade')
    name = args[upgrade + 2]
    chart = args[upgrade + 3]
    namespace = args[args.index('--namespace') + 1]
    rendered = subprocess.check_output([state['helm'], 'template', name, chart,
        '--namespace', namespace, '-f', values])
    call['rendered'] = list(yaml.safe_load_all(rendered))
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps(call) + '\n')
if tool == 'kind':
    if args[:2] == ['get', 'clusters']:
        print(state['cluster'])
        sys.exit(0)
    if args[:2] == ['export', 'kubeconfig']:
        sys.exit(0)
if tool == 'helm':
    if args[0] in ['show', 'template']:
        sys.exit(subprocess.run([state['helm']] + args, input=sys.stdin.buffer.read()).returncode)
    if args[0] == '--kubeconfig':
        args = args[4:]
    namespace = args[args.index('--namespace') + 1]
    if args[0] == 'list':
        pattern = args[args.index('--filter') + 1]
        print(json.dumps([{'name': key.split('/')[1], 'status': 'deployed'} for key in state['owners']
            if key.split('/')[0] == namespace and re.fullmatch(pattern, key.split('/')[1])]))
        sys.exit(0)
    if args[:2] == ['get', 'values']:
        print(json.dumps(state['owners'][namespace + '/' + args[2]]))
        sys.exit(0)
    if args[0] == 'upgrade':
        state['owners'][namespace + '/' + args[2]] = call['values']
        (root / 'state.json').write_text(json.dumps(state))
        sys.exit(0)
if tool == 'kubectl':
    namespace = args[args.index('-n') + 1]
    args = args[6:]
    if args[0] == 'get':
        resource = namespace + '/' + args[1]
        if resource == state.get('missing'):
            sys.exit(0)
        if args[1] == 'cluster.postgresql.cnpg.io/wamn-pg':
            print(json.dumps({'status': {'currentPrimary': 'wamn-pg-1'}}))
            sys.exit(0)
        if args[1].startswith(('secret/', 'configmap/')):
            data = {key: 'fixture' for key in ['url', 'ca.crt', 'tls.crt', 'tls.key',
                'key', 'api-key', '.dockerconfigjson', 'target.json']}
            if args[1].startswith('secret/'):
                data = {key: base64.b64encode(value.encode()).decode() for key, value in data.items()}
            print(json.dumps({'data': data}))
            sys.exit(0)
    if args[0] == 'exec':
        print(json.dumps({'active': state.get('active'), 'published': None}))
        sys.exit(0)
sys.exit(1)
";

struct Fixture {
    chart: Chart,
    previous: Chart,
    document: ClusterDocument,
    file: PathBuf,
    bin: PathBuf,
    path: std::ffi::OsString,
    helm: PathBuf,
}

impl Fixture {
    fn new() -> anyhow::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let file = repository.join("deploy/kind/cluster.k");
        let document = ClusterDocument::compile(&file)?;
        let chart = prepared_chart(&repository)?;
        let previous = prepared_chart(&repository)?;
        let values = chart.0.join("values.yaml");
        std::fs::write(
            &values,
            std::fs::read_to_string(&values)?
                .replace("identity.example/identity", "selected.example/identity")
                .replace(
                    "registry.example/components/http",
                    "selected.example/components/http",
                ),
        )?;
        let bin = chart.0.join("commands");
        std::fs::create_dir(&bin)?;
        let helm = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|path| path.join("helm"))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                anyhow::anyhow!("Helm is required for platform update render coverage")
            })?;
        for name in ["kind", "helm", "kubectl", "docker", "gcloud"] {
            let path = bin.join(name);
            std::fs::write(&path, TOOL)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
        let path = std::env::join_paths(std::iter::once(bin.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))?;
        Ok(Self {
            chart,
            previous,
            document,
            file,
            bin,
            path,
            helm,
        })
    }

    fn state(&self, selection: &UpgradeTarget) -> anyhow::Result<Value> {
        let chart = artifact_plan::select(&self.previous.0)?;
        let mut control = control::host_values(&self.document.org);
        control["runtime-operator"]["runtime"]["hostGroups"][0]["image"] = json!({
            "registry":"retained.example", "repository":"host", "tag":"retained", "pull_policy":"Never"
        });
        control["platform"] = json!({"chart":self.previous.0});
        let entry = identity::chart_input(&identity::ChartInput {
            chart: &self.previous.0.join("platform/identity"),
            name: "identity",
            namespace: "identity",
            image: chart.image("identity")?,
            pull_policy: "Never",
            issuer: "https://identity.identity.svc.cluster.local",
            database_secret: "retained-identity-db",
            tls_secret: "retained-identity-tls",
            operator_ca_secret: "retained-operator-ca",
            resend_secret: "",
            resend_from: "",
            invite_url: "",
            mode: identity::Mode::Session {
                targets: vec!["retained-target".into()],
                nats: None,
            },
        })?;
        let mut identity = entry["values"].clone();
        identity["platform"] = json!({"chart":self.previous.0});
        let mut owners = json!({"platform/wamn":{"platform":{"chart":self.previous.0}},
            "identity/identity":identity, "hosts/alpha":{"application":"retained"}});
        owners[format!("hosts/c-{}", self.document.org)] = control;
        if let UpgradeTarget::Host { release, group } = selection {
            owners[format!("hosts/{release}")] = json!({
                "runtime-operator":{"runtime":{"hostGroups":[{"name":group}]}}
            });
        }
        Ok(
            json!({"cluster":self.document.cluster_name, "owners":owners, "helm":self.helm,
            "active":{"kid":"retained-key", "x":"retained-public-key"}}),
        )
    }

    fn run(&self, selection: &str, state: &Value) -> anyhow::Result<(Output, Vec<Value>, Value)> {
        std::fs::write(self.bin.join("state.json"), serde_json::to_vec(state)?)?;
        std::fs::write(self.bin.join("calls.jsonl"), "")?;
        let output = Command::new(env!("CARGO_BIN_EXE_wamn"))
            .args(["platform", "deploy"])
            .arg(&self.file)
            .arg("--chart")
            .arg(&self.chart.0)
            .args(["--only", selection])
            .env("PATH", &self.path)
            .output()?;
        let calls = std::fs::read_to_string(self.bin.join("calls.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()?;
        let after = serde_json::from_slice(&std::fs::read(self.bin.join("state.json"))?)?;
        Ok((output, calls, after))
    }
}

fn assert_operations(calls: &[Value], owner: (&str, &str), allow_upgrade: bool) {
    let mut upgrades = Vec::new();
    for call in calls {
        let args = call["args"].as_array().unwrap();
        match call["tool"].as_str().unwrap() {
            "kind" => assert!(args[0] == "get" || args[0] == "export", "{call}"),
            "kubectl" => assert!(args[6] == "get" || args[6] == "exec", "{call}"),
            "helm" => {
                let args = if args[0] == "--kubeconfig" {
                    &args[4..]
                } else {
                    args.as_slice()
                };
                if args[0] == "show" || args[0] == "template" {
                    continue;
                }
                let namespace = args.iter().position(|arg| arg == "--namespace").unwrap();
                assert_eq!(args[namespace + 1], owner.1, "{call}");
                let name = match args[0].as_str().unwrap() {
                    "list" => {
                        let filter = args.iter().position(|arg| arg == "--filter").unwrap();
                        args[filter + 1].as_str().unwrap().trim_matches(['^', '$'])
                    }
                    "get" => {
                        assert_eq!(args[1], "values");
                        args[2].as_str().unwrap()
                    }
                    "upgrade" if allow_upgrade => {
                        upgrades.push(call);
                        args[2].as_str().unwrap()
                    }
                    _ => panic!("unexpected Helm operation: {call}"),
                };
                assert_eq!(name, owner.0, "{call}");
            }
            _ => panic!("unrelated tool: {call}"),
        }
    }
    assert_eq!(
        upgrades.len(),
        usize::from(allow_upgrade),
        "one selected release operation"
    );
}

#[test]
fn platform_service_updates_change_only_the_selected_release() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    for selection in ["control", "identity"] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        for recorded in [true, false] {
            let mut state = fixture.state(&target)?;
            let owner = format!("{namespace}/{name}");
            if !recorded {
                state["owners"][&owner]
                    .as_object_mut()
                    .unwrap()
                    .remove("platform");
            }
            // Explicit selection can replace an unavailable recorded chart.
            if recorded {
                state["owners"][&owner]["platform"]["chart"] = "/unavailable/old-chart".into();
            }
            let (output, calls, mut after) = fixture.run(selection, &state)?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_operations(&calls, (&name, namespace), true);
            let values = &after["owners"][&owner];
            assert_eq!(
                values["platform"]["chart"],
                fixture.chart.0.canonicalize()?.to_str().unwrap()
            );
            if selection == "control" {
                assert_eq!(
                    values["runtime-operator"]["runtime"]["hostGroups"],
                    state["owners"][&owner]["runtime-operator"]["runtime"]["hostGroups"]
                );
                let rendered = calls
                    .iter()
                    .find_map(|call| call.get("rendered"))
                    .unwrap()
                    .as_array()
                    .unwrap();
                let workload = rendered
                    .iter()
                    .find(|object| object["kind"] == "WorkloadDeployment")
                    .unwrap();
                assert_eq!(
                    workload["spec"]["template"]["spec"]["components"][0]["image"],
                    artifact_plan::select(&fixture.chart.0)?
                        .image("http")?
                        .split_once('/')
                        .map(|(_, path)| format!(
                            "wamn-registry.platform.svc.cluster.local:5000/{path}"
                        ))
                        .unwrap()
                );
            } else {
                assert_eq!(
                    format!(
                        "{}:{}",
                        values["image"]["repository"].as_str().unwrap(),
                        values["image"]["tag"].as_str().unwrap()
                    ),
                    artifact_plan::select(&fixture.chart.0)?.image("identity")?
                );
                assert_eq!(values["image"]["pullPolicy"], "IfNotPresent");
            }
            after["owners"][&owner] = state["owners"][&owner].clone();
            assert_eq!(
                after, state,
                "unrelated releases and signing state stay unchanged"
            );
        }
    }
    Ok(())
}

#[test]
fn platform_service_updates_refuse_missing_initialization_without_mutation() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    for (selection, missing) in [
        (
            "control",
            format!("hosts/secret/wamn-control-{}", fixture.document.org),
        ),
        ("identity", "identity/secret/retained-identity-db".into()),
    ] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        for signing_key in [true, false] {
            let mut state = fixture.state(&target)?;
            let expected = if signing_key {
                state["missing"] = missing.clone().into();
                {
                    let (namespace, resource) = missing.split_once('/').unwrap();
                    let (_, name) = resource.split_once('/').unwrap();
                    format!("secret {namespace}/{name}")
                }
            } else {
                state["active"] = Value::Null;
                "active identity signing key".into()
            };
            let (output, calls, after) = fixture.run(selection, &state)?;
            let error = String::from_utf8(output.stderr)?;
            assert!(!output.status.success());
            assert!(error.contains(&expected), "{error}");
            assert_operations(&calls, (&name, namespace), false);
            assert_eq!(after, state);
        }
    }
    Ok(())
}

#[test]
fn platform_updates_refuse_missing_owners_and_pending_hosts_without_mutation() -> anyhow::Result<()>
{
    let fixture = Fixture::new()?;
    for selection in ["control", "identity", "host:alpha/alpha"] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        let mut state = fixture.state(&target)?;
        state["owners"]
            .as_object_mut()
            .unwrap()
            .remove(&format!("{namespace}/{name}"));
        let (output, calls, after) = fixture.run(selection, &state)?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)?
                .contains(&format!("existing release {namespace}/{name}"))
        );
        assert_operations(&calls, (&name, namespace), false);
        assert_eq!(after, state);
        if matches!(target, UpgradeTarget::Host { .. }) {
            for groups_present in [true, false] {
                let mut state = fixture.state(&target)?;
                if !groups_present {
                    state["owners"][format!("{namespace}/{name}")] = json!({});
                }
                let (output, calls, after) = fixture.run(selection, &state)?;
                assert!(!output.status.success());
                let error = String::from_utf8(output.stderr)?;
                assert!(
                    error.contains(if groups_present {
                        "DEP4-04"
                    } else {
                        "no host groups"
                    }),
                    "{error}"
                );
                assert_operations(&calls, (&name, namespace), false);
                assert_eq!(after, state);
            }
        }
    }
    Ok(())
}
