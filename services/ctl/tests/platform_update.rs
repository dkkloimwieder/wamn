//! Command coverage with recorded cluster tools and real Helm rendering.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use wamn_environment_control::platform::artifact_plan::test_util::{Chart, prepared_chart};
use wamn_environment_control::platform::document::{ClusterDocument, UpgradeTarget};
use wamn_environment_control::platform::{artifact_plan, control, identity};

const TOOL: &str = r#"#!/usr/bin/python3
import base64, json, pathlib, re, subprocess, sys, yaml
root = pathlib.Path(__file__).parent
tool = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
state = json.loads((root / 'state.json').read_text())
call = {'tool': tool, 'args': args}
if tool == 'kubectl' and args[6] in ['apply', 'create']:
    call['object'] = json.load(sys.stdin)
if tool == 'helm' and 'upgrade' in args:
    values = args[args.index('-f') + 1]
    call['values'] = yaml.safe_load(pathlib.Path(values).read_text())
    upgrade = args.index('upgrade')
    name = args[upgrade + 2]
    chart = next(arg for arg in args[upgrade + 3:] if pathlib.Path(arg).is_dir() or arg.endswith('.tgz'))
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
    if args[:2] == ['get', 'nodes']:
        print('node')
        sys.exit(0)
    if args[:2] == ['export', 'kubeconfig']:
        sys.exit(0)
if tool == 'helm':
    if args[0] == '--kubeconfig':
        args = args[4:]
    if args[0] in ['show', 'template']:
        sys.exit(subprocess.run([state['helm']] + args, input=sys.stdin.buffer.read()).returncode)
    namespace = args[args.index('--namespace') + 1] if '--namespace' in args else args[args.index('-n') + 1]
    if args[0] == 'list':
        pattern = args[args.index('--filter') + 1]
        print(json.dumps([{'name': key.split('/')[1], 'status': 'deployed'} for key in state['owners']
            if key.split('/')[0] == namespace and re.fullmatch(pattern, key.split('/')[1])]))
        sys.exit(0)
    if args[:2] == ['get', 'values']:
        print(json.dumps(state['owners'][namespace + '/' + args[2]]))
        sys.exit(0)
    if args[0] == 'upgrade':
        if state.get('stop_release') == namespace + '/' + args[2]:
            sys.exit(1)
        state['owners'][namespace + '/' + args[2]] = call['values']
        (root / 'state.json').write_text(json.dumps(state))
        sys.exit(0)
if tool == 'docker':
    if args[0] == 'ps':
        print('registry')
    elif args[0] == 'inspect':
        print(json.dumps([{'Id':'registry', 'Name':'/registry', 'State':{'Running':True},
            'Config':{'Image':'registry:2'}, 'Mounts':[],
            'NetworkSettings':{'Ports':{'5000/tcp':[{'HostIp':'127.0.0.1','HostPort':'5001'}]},
                'Networks':{'kind':{'IPAddress':'172.18.0.2'}}}}]))
    elif args[:2] == ['network', 'ls']:
        print('kind')
    sys.exit(0)
if tool == 'kubectl':
    namespace = args[args.index('-n') + 1]
    args = args[6:]
    if args[0] in ['wait', 'rollout', 'apply']:
        sys.exit(0)
    if args[0] == 'create':
        obj = call['object']
        obj['metadata']['uid'] = 'fixture-uid'
        state.setdefault('objects', {})[namespace + '/' + obj['kind'].lower() + '/' + obj['metadata']['name']] = obj
        (root / 'state.json').write_text(json.dumps(state))
        print(json.dumps(obj))
        sys.exit(0)
    if args[0] == 'delete':
        state.get('objects', {}).pop(namespace + '/' + args[1] + '/' + args[2], None)
        (root / 'state.json').write_text(json.dumps(state))
        sys.exit(0)
    if args[0] == 'patch':
        sys.exit(0)
    if args[0] == 'logs':
        name = args[1].removeprefix('job/')
        if name == 'platform-session-key-state':
            print(json.dumps({'active':state['active']}))
        elif name == 'platform-identity-check':
            print(json.dumps({'keys':[state['active']]}))
        elif name == 'platform-owner-state-' + state['org']:
            print('t')
        elif name.startswith(('platform-identity-issuer', 'platform-control-credentials-')):
            obj = state['objects'][namespace + '/job/' + name]
            query = obj['spec']['template']['spec']['containers'][0]['args'][-1]
            print('["active","inactive"]' if 'json_build_array' in query else 't')
        sys.exit(0)
    if args[0] == 'get':
        resource = namespace + '/' + args[1]
        if resource == state.get('fail_read'):
            sys.exit(1)
        if args[1].startswith('job/'):
            obj = state.get('objects', {}).get(resource)
            if obj:
                obj['status'] = {'conditions':[{'type':'Complete','status':'True'}]}
                print(json.dumps(obj))
            sys.exit(0)
        if state.get('reapply') and args[1] in ['secret/wamn-nats-authorization', 'secret/wamn-oauth']:
            sys.exit(0)
        if resource == state.get('missing'):
            sys.exit(0)
        if args[1] == 'cluster.postgresql.cnpg.io/wamn-pg':
            print(json.dumps({'spec':{'imageName':'postgres:18'}, 'status': {'currentPrimary': 'wamn-pg-1'}}))
            sys.exit(0)
        if args[1].startswith(('secret/', 'configmap/')):
            data = {key: 'fixture' for key in ['url', 'ca.crt', 'tls.crt', 'tls.key',
                'key', 'api-key', '.dockerconfigjson', 'target.json', 'password']}
            if resource in state.get('credentials', {}):
                data['url'] = state['credentials'][resource]
            if args[1].startswith('secret/'):
                data = {key: base64.b64encode(value.encode()).decode() for key, value in data.items()}
            print(json.dumps({'data': data}))
            sys.exit(0)
    if args[0] == 'exec':
        print(json.dumps({'active': state.get('active'), 'published': None}))
        sys.exit(0)
sys.exit(1)
"#;

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
        let values = chart.0.join("platform/identity/values.yaml");
        std::fs::write(
            &values,
            std::fs::read_to_string(&values)?.replace("memory: 512Mi", "memory: 1Gi"),
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
            database_secret: "wamn-identity-db",
            tls_secret: "identity-tls",
            operator_ca_secret: "identity-operator-ca",
            resend_secret: "",
            resend_from: "",
            invite_url: "",
            mode: identity::Mode::Session {
                targets: vec!["retained-target".into()],
                nats: None,
            },
        })?;
        let mut identity = entry["values"].clone();
        identity["resources"] = json!({"limits":{"memory":"256Mi"}});
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

    fn run(
        &self,
        selection: Option<&str>,
        state: &Value,
    ) -> anyhow::Result<(Output, Vec<Value>, Value)> {
        std::fs::write(self.bin.join("state.json"), serde_json::to_vec(state)?)?;
        std::fs::write(self.bin.join("calls.jsonl"), "")?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_wamn"));
        command
            .args(["platform", "deploy"])
            .arg(&self.file)
            .arg("--chart")
            .arg(&self.chart.0)
            .env("PATH", &self.path);
        if let Some(selection) = selection {
            command.args(["--only", selection]);
        }
        let auth = self.bin.join("auth.json");
        std::fs::write(
            &auth,
            r#"{"auths":{"localhost:5001":{"username":"user","password":"pass"}}}"#,
        )?;
        let output = command.env("WAMN_REGISTRY_AUTH_FILE", auth).output()?;
        let calls = std::fs::read_to_string(self.bin.join("calls.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()?;
        let after = serde_json::from_slice(&std::fs::read(self.bin.join("state.json"))?)?;
        if selection.is_none() {
            // The fake kind nodes mount nothing, so their retained work directory is disposable.
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(directory) = stdout
                .lines()
                .find_map(|line| line.strip_prefix("work directory: "))
            {
                std::fs::remove_dir_all(directory)?;
            }
        }
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
                        assert!(args.iter().any(|arg| arg == "--wait"), "{call}");
                        assert!(
                            args.iter().any(|arg| arg == "--rollback-on-failure"),
                            "{call}"
                        );
                        assert!(!args.iter().any(|arg| arg == "--take-ownership"), "{call}");
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
                state["owners"][&owner]["platform"]["chart"] =
                    json!(fixture.previous.0.join("absent"));
            }
            state["owners"]["platform/wamn"]["platform"]["chart"] =
                json!(fixture.previous.0.join("absent-default"));
            if selection == "control" {
                state["active"] = Value::Null;
            }
            let (output, calls, mut after) = fixture.run(Some(selection), &state)?;
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
                    values["runtime-operator"]["runtime"]["hostGroups"][0]["image"],
                    state["owners"][&owner]["runtime-operator"]["runtime"]["hostGroups"][0]["image"]
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
                let deployment = calls
                    .iter()
                    .filter_map(|call| call["rendered"].as_array())
                    .flatten()
                    .find(|object| object["kind"] == "Deployment")
                    .unwrap();
                assert_eq!(
                    deployment["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]
                        ["memory"],
                    "1Gi"
                );
                assert_eq!(values["sessionTargetSecrets"], json!([]));
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
        ("identity", "identity/secret/wamn-identity-db".into()),
    ] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        for signing_key in [true, false]
            .into_iter()
            .filter(|present| *present || selection == "identity")
        {
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
            let (output, calls, after) = fixture.run(Some(selection), &state)?;
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
        let (output, calls, after) = fixture.run(Some(selection), &state)?;
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
                let (output, calls, after) = fixture.run(Some(selection), &state)?;
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

#[test]
fn interrupted_platform_reapply_retains_default_and_service_selections() -> anyhow::Result<()> {
    use wamn_control_provision::identity_issuer::identity_issuer_generation_role;
    use wamn_control_provision::{
        CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
    };

    let fixture = Fixture::new()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let service = prepared_chart(&repository)?;
    let control = format!("hosts/c-{}", fixture.document.org);
    // One service has its own selection. The other inherits the platform default.
    for own in [&control, "identity/identity"] {
        let mut state = fixture.state(&UpgradeTarget::Control)?;
        for (owner, values) in state["owners"].as_object_mut().unwrap() {
            if owner == own {
                values["platform"]["chart"] = json!(service.0);
            } else if owner != "platform/wamn" {
                values.as_object_mut().unwrap().remove("platform");
            }
        }
        state["reapply"] = true.into();
        state["org"] = fixture.document.org.clone().into();
        state["fail_read"] = "platform/secret/wamn-pg-superuser".into();
        let (output, first_calls, mut interrupted) = fixture.run(None, &state)?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("wamn-pg-superuser"));
        let operator = first_calls
            .iter()
            .find(|call| call["values"]["platform"]["chart"].is_string())
            .unwrap();
        assert_eq!(
            operator["values"]["platform"]["chart"],
            fixture.previous.0.to_str().unwrap()
        );
        assert_eq!(
            interrupted["owners"]["platform/wamn"]["platform"]["chart"],
            state["owners"]["platform/wamn"]["platform"]["chart"]
        );
        for owner in [&control, "identity/identity"] {
            assert_eq!(interrupted["owners"][owner], state["owners"][owner]);
        }
        interrupted.as_object_mut().unwrap().remove("fail_read");
        let control_role = workload_generation_role(
            WorkloadRoleFamily::Control,
            WorkloadRoleScope::Org {
                org: &fixture.document.org,
                database: "wamn_system",
            },
            CredentialGeneration::A,
        )?;
        let identity_role = identity_issuer_generation_role(
            "https://identity.identity.svc.cluster.local",
            CredentialGeneration::A,
        )?;
        interrupted["credentials"] = json!({
            format!("hosts/secret/wamn-control-{}", fixture.document.org): format!("postgresql://{control_role}:fixture@wamn-pg-rw.platform.svc.cluster.local/wamn_system"),
            "identity/secret/wamn-identity-db": format!("postgresql://{identity_role}:fixture@wamn-pg-rw.platform.svc.cluster.local/wamn_system")
        });
        // Stop after both services apply, before edge readiness and the HTTP probe.
        interrupted["stop_release"] = "hosts/wamn-edge".into();
        let (output, retry_calls, after) = fixture.run(None, &interrupted)?;
        assert!(!output.status.success());
        for owner in ["platform/wamn", &control, "identity/identity"] {
            let expected = if owner == own {
                &service.0
            } else {
                &fixture.previous.0
            };
            assert_eq!(
                after["owners"][owner]["platform"]["chart"],
                expected.to_str().unwrap(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for owner in [&control, "identity/identity"] {
            let (namespace, name) = owner.split_once('/').unwrap();
            let upgrade = retry_calls
                .iter()
                .find(|call| {
                    let args = call["args"].as_array().unwrap_or_else(|| {
                        panic!(
                            "missing upgrade for {owner}: {}",
                            String::from_utf8_lossy(&output.stderr)
                        )
                    });
                    call["tool"] == "helm"
                        && args[0] == "upgrade"
                        && args[2] == name
                        && args
                            .windows(2)
                            .any(|pair| pair == [json!("--namespace"), json!(namespace)])
                })
                .unwrap_or_else(|| {
                    panic!(
                        "missing upgrade for {owner}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )
                });
            assert!(
                upgrade["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("--wait"))
            );
        }
    }
    Ok(())
}
