//! Command coverage with recorded cluster tools and real Helm rendering.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use wamn_environment_control::environment::{document::EnvironmentDocument, ensure, write};
use wamn_environment_control::platform::artifact_plan::test_util::{Chart, prepared_chart};
use wamn_environment_control::platform::document::{ClusterDocument, UpgradeTarget};
use wamn_environment_control::platform::{artifact_plan, control, identity};

const TOOL: &str = r#"#!/usr/bin/python3
import base64, json, pathlib, re, subprocess, sys, time, yaml
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
        pattern = args[args.index('--filter') + 1] if '--filter' in args else '.*'
        print(json.dumps([{'name': key.split('/')[1], 'status': state.get('pending', {}).get(key, 'deployed')} for key in state['owners']
            if key.split('/')[0] == namespace and re.fullmatch(pattern, key.split('/')[1])]))
        sys.exit(0)
    if args[:2] == ['get', 'values']:
        print(json.dumps(state['owners'][namespace + '/' + args[2]]))
        sys.exit(0)
    if args[0] == 'rollback':
        owner = namespace + '/' + args[1]
        state['owners'][owner] = state['recovered'].pop(owner)
        state['pending'].pop(owner)
        (root / 'state.json').write_text(json.dumps(state))
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
    if args[0] == 'port-forward':
        print('Forwarding from 127.0.0.1:' + str(state['database']['port']) + ' -> 5432', flush=True)
        time.sleep(120)
        sys.exit(0)
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
        if state.get('hang_read') == namespace + '/' + args[1]:
            time.sleep(120)
        if args[1] == 'pods':
            selector = args[args.index('-l') + 1] if '-l' in args else ''
            print(json.dumps({'items': state.get('pods', {}).get(namespace + '/' + selector, [])}))
            sys.exit(0)
        if namespace + '/' + args[1] in state.get('workloads', {}):
            print(json.dumps(state['workloads'][namespace + '/' + args[1]]))
            sys.exit(0)
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
        if args[1] == 'service/wamn-pg-rw':
            print(json.dumps({'spec':{'selector':{'app':'postgres'}}}))
            sys.exit(0)
        if args[1] == 'cluster.postgresql.cnpg.io/wamn-pg':
            print(json.dumps({'spec':{'imageName':'postgres:18'}, 'status': {'currentPrimary': state.get('primary', 'wamn-pg-1')}}))
            sys.exit(0)
        if args[1].startswith(('secret/', 'configmap/')):
            data = {key: 'fixture' for key in ['url', 'ca.crt', 'tls.crt', 'tls.key',
                'key', 'api-key', '.dockerconfigjson', 'target.json', 'password']}
            if resource == 'platform/secret/wamn-pg-superuser' and 'database' in state:
                data['password'] = state['database']['password']
            if resource in state.get('credentials', {}):
                data['url'] = state['credentials'][resource]
            if args[1].startswith('secret/'):
                data = {key: base64.b64encode(value.encode()).decode() for key, value in data.items()}
            print(json.dumps({'data': data}))
            sys.exit(0)
    if args[0] == 'exec':
        if state.get('hang_exec'):
            (root / 'exec.pid').write_text(str(__import__('os').getpid()))
            time.sleep(120)
        if state.get('exec_error'):
            print(state['exec_error'], file=sys.stderr)
            sys.exit(1)
        print(state.get('exec_output', json.dumps(state.get('active') is not None)))
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
                .replace("localhost:5001/host", "selected.example/host")
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
            let digest = format!("sha256:{}", "c".repeat(64));
            let environment = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
                "org":self.document.org,"project":"fixture","env":"dev","target":"cloud","policy":"dev",
                "applications":{"alpha":{"packages":["alpha"],"current":digest,"hostname":"alpha.example"}}
            }))?)?;
            let manifest = serde_json::from_value(json!({
                "format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
                "release":{"packages":[]}, "components":[], "routes":[], "attachments":{}
            }))?;
            let mut values = if release == "shared" {
                ensure::materializer_host_values(&environment, 70, 1, 120, None, false, None)?
            } else {
                serde_json::to_value(
                    write::release_values(
                        &environment,
                        "alpha",
                        &manifest,
                        &digest,
                        "registry.example/releases",
                        70,
                        "fixture",
                        None,
                        None,
                    )?
                    .0,
                )?
            };
            let groups = values["runtime-operator"]["runtime"]["hostGroups"]
                .as_array_mut()
                .unwrap();
            groups[0]["name"] = group.as_str().into();
            let mut other = groups[0].clone();
            other["name"] = "untargeted".into();
            other["image"] = json!({"registry":"retained.example", "repository":"host",
                "tag":"retained", "pull_policy":"Never"});
            groups.insert(0, other);
            owners[format!("hosts/{release}")] = values;
        }
        let mut workloads = json!({});
        let mut pods = json!({});
        for (namespace, name, selector) in [
            (
                "identity",
                "identity".to_owned(),
                "app.kubernetes.io/name=wamn-identity".to_owned(),
            ),
            (
                "platform",
                "runtime-operator".to_owned(),
                "wasmcloud.com/name=runtime-operator".to_owned(),
            ),
        ]
        .into_iter()
        .chain(
            owners
                .as_object()
                .unwrap()
                .iter()
                .filter(|(owner, _)| owner.starts_with("hosts/"))
                .flat_map(|(_, values)| {
                    values
                        .pointer("/runtime-operator/runtime/hostGroups")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|group| {
                            let name = group["name"].as_str().unwrap();
                            (
                                "hosts",
                                format!("hostgroup-{name}"),
                                format!("wasmcloud.com/hostgroup={name}"),
                            )
                        })
                }),
        ) {
            let image = format!("running.example/{name}@sha256:{}", "b".repeat(64));
            workloads[format!("{namespace}/Deployment/{name}")] = json!({
                "kind":"Deployment", "metadata":{"name":name,"generation":1},
                "spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"main","image":image}]}}},
                "status":{"observedGeneration":1,"readyReplicas":1,"updatedReplicas":1,"availableReplicas":1}
            });
            let pod = json!({"kind":"Pod", "metadata":{"name":format!("{name}-pod")},
                "spec":{"containers":[{"name":"main","image":image}]},
                "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],
                    "containerStatuses":[{"name":"main","image":image,"imageID":image,"state":{"running":{}}}]}});
            pods[format!("{namespace}/{selector}")] = json!([pod]);
        }
        workloads[format!("hosts/WorkloadDeployment/c-{}-http", self.document.org)] = json!({
            "kind":"WorkloadDeployment", "metadata":{"generation":1}, "spec":{"replicas":1},
            "status":{"replicas":{"ready":1}, "conditions":[{"type":"Ready","status":"True","observedGeneration":1}]}
        });
        Ok(
            json!({"cluster":self.document.cluster_name, "owners":owners, "helm":self.helm,
            "workloads":workloads,"pods":pods,
            "active":{"kid":"retained-key", "x":"retained-public-key"}}),
        )
    }

    fn run(
        &self,
        selection: Option<&str>,
        state: &Value,
    ) -> anyhow::Result<(Output, Vec<Value>, Value)> {
        self.run_with_chart(selection, Some(&self.chart.0), state)
    }

    fn run_with_chart(
        &self,
        selection: Option<&str>,
        chart: Option<&Path>,
        state: &Value,
    ) -> anyhow::Result<(Output, Vec<Value>, Value)> {
        self.run_command("deploy", selection, chart, state)
    }

    fn run_command(
        &self,
        verb: &str,
        selection: Option<&str>,
        chart: Option<&Path>,
        state: &Value,
    ) -> anyhow::Result<(Output, Vec<Value>, Value)> {
        std::fs::write(self.bin.join("state.json"), serde_json::to_vec(state)?)?;
        std::fs::write(self.bin.join("calls.jsonl"), "")?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_wamn"));
        command
            .args(["platform", verb])
            .arg(&self.file)
            .env("PATH", &self.path);
        if let Some(chart) = chart {
            command.arg("--chart").arg(chart);
        }
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
            "kubectl" => assert!(
                args[6] == "get" || args[6] == "exec" || args[6] == "port-forward",
                "{call}"
            ),
            "helm" => {
                let args = if args[0] == "--kubeconfig" {
                    &args[4..]
                } else {
                    args.as_slice()
                };
                if args[0] == "show" || args[0] == "template" {
                    continue;
                }
                if args[0] == "list" || args[0] == "get" {
                    continue;
                }
                let namespace = args.iter().position(|arg| arg == "--namespace").unwrap();
                assert_eq!(args[namespace + 1], owner.1, "{call}");
                let name = match args[0].as_str().unwrap() {
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
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains(&format!(
                    "selected chart ({namespace}/{name}): {}",
                    fixture.chart.0.canonicalize()?.display()
                )),
                "{stdout}"
            );
            assert_eq!(
                calls
                    .iter()
                    .any(|call| call["tool"] == "kubectl" && call["args"][6] == "exec"),
                selection == "identity"
            );
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
        let mut state = fixture.state(&target)?;
        state["missing"] = missing.clone().into();
        let (input_namespace, resource) = missing.split_once('/').unwrap();
        let (_, secret) = resource.split_once('/').unwrap();
        let (output, calls, after) = fixture.run(Some(selection), &state)?;
        let error = String::from_utf8(output.stderr)?;
        assert!(!output.status.success());
        assert!(
            error.contains(&format!("secret {input_namespace}/{secret}")),
            "{error}"
        );
        assert_operations(&calls, (&name, namespace), false);
        assert_eq!(after, state);
    }
    Ok(())
}

#[test]
fn identity_update_refuses_missing_signing_key_without_mutation() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let mut state = fixture.state(&UpgradeTarget::Identity)?;
    state["active"] = Value::Null;
    let (output, calls, after) = fixture.run(Some("identity"), &state)?;
    let error = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(
        error.contains("missing active identity signing key"),
        "{error}"
    );
    assert!(error.contains("platform deploy without --only"), "{error}");
    assert_operations(&calls, ("identity", "identity"), false);
    assert_eq!(after, state);
    Ok(())
}

#[test]
fn identity_update_refuses_unreadable_signing_state_without_mutation() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    for (field, value, expected) in [
        (
            "exec_error",
            json!("exec failed"),
            "cannot read identity signing state",
        ),
        (
            "exec_error",
            json!("canceling statement due to statement timeout"),
            "cannot read identity signing state",
        ),
        (
            "exec_output",
            json!("not json"),
            "cannot decode identity signing state as a boolean",
        ),
        // The accepted result is a boolean. Key-material objects are not accepted.
        (
            "exec_output",
            json!(
                r#"{"active":{"kid":"key-id","x":"public-key","private_pkcs8":"private-key"},"published":null}"#
            ),
            "cannot decode identity signing state as a boolean",
        ),
        (
            "missing",
            json!("platform/cluster.postgresql.cnpg.io/wamn-pg"),
            "missing Cluster platform/wamn-pg",
        ),
        (
            "fail_read",
            json!("platform/cluster.postgresql.cnpg.io/wamn-pg"),
            "cannot read Cluster platform/wamn-pg",
        ),
        ("primary", Value::Null, "missing PostgreSQL primary"),
        ("primary", json!(""), "missing PostgreSQL primary"),
    ] {
        let mut state = fixture.state(&UpgradeTarget::Identity)?;
        state[field] = value;
        let (output, calls, after) = fixture.run(Some("identity"), &state)?;
        let error = String::from_utf8(output.stderr)?;
        assert!(!output.status.success());
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("platform deploy without --only"), "{error}");
        assert_operations(&calls, ("identity", "identity"), false);
        assert_eq!(after, state);
    }
    Ok(())
}

#[test]
fn platform_updates_refuse_missing_owners_without_mutation() -> anyhow::Result<()> {
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
    }
    Ok(())
}

#[test]
fn platform_host_updates_upgrade_only_the_existing_owner() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("wamn_system")?;
    for selection in ["host:alpha/alpha", "host:shared/shared"] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        let owner = format!("{namespace}/{name}");
        let mut state = fixture.state(&target)?;
        set_database(&mut state, database.url())?;
        let (output, calls, mut after) = fixture.run(Some(selection), &state)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_operations(&calls, (&name, namespace), true);
        let image =
            &after["owners"][&owner]["runtime-operator"]["runtime"]["hostGroups"][1]["image"];
        assert_eq!(image["registry"], "selected.example");
        assert_eq!(
            image["tag"],
            artifact_plan::select(&fixture.chart.0)?.values()["runtime-operator"]["runtime"]["image"]
                ["tag"]
        );
        after["owners"][&owner]["runtime-operator"]["runtime"]["hostGroups"][1]
            .as_object_mut()
            .unwrap()
            .remove("image");
        assert_eq!(after, state, "only the selected host image changes");
    }
    Ok(())
}

fn set_database(state: &mut Value, url: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(url)?;
    state["database"] = json!({"port":url.port().unwrap(), "password":url.password().unwrap()});
    Ok(())
}

#[test]
fn platform_updates_use_recovered_values() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("wamn_system")?;
    for selection in ["host:alpha/alpha", "host:shared/shared", "control"] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        let owner = format!("{namespace}/{name}");
        let mut state = fixture.state(&target)?;
        set_database(&mut state, database.url())?;
        let mut recovered = state["owners"][&owner].clone();
        if selection == "control" {
            recovered["runtime-operator"]["runtime"]["hostGroups"][0]["image"]["tag"] =
                "recovered-control".into();
        } else {
            if selection == "host:alpha/alpha" {
                let digest = format!("sha256:{}", "d".repeat(64));
                recovered["release"]["manifestDigest"] = digest.clone().into();
                recovered["applications"]["alpha"]["current"] = digest.clone().into();
                recovered["runtime-operator"]["runtime"]["podAnnotations"]["wamn.release-digest"] =
                    digest.clone().into();
                let applications = format!("--applications={}", recovered["applications"]);
                for argument in
                    recovered["runtime-operator"]["runtime"]["hostGroups"][1]["extraArgs"]
                        .as_array_mut()
                        .unwrap()
                {
                    if argument
                        .as_str()
                        .unwrap()
                        .starts_with("--release-manifest-digest=")
                    {
                        *argument = format!("--release-manifest-digest={digest}").into();
                    } else if argument.as_str().unwrap().starts_with("--applications=") {
                        *argument = applications.as_str().into();
                    }
                }
                recovered["routeHost"] = "recovered.example".into();
            }
            recovered["runtime-operator"]["runtime"]["hostGroups"][1]["replicas"] = 2.into();
            recovered["runtime-operator"]["runtime"]["hostGroups"][0]["image"]["tag"] =
                "recovered-other".into();
        }
        state["pending"] = json!({&owner:"pending-upgrade"});
        state["recovered"] = json!({&owner:recovered});
        let (output, calls, mut after) = fixture.run(Some(selection), &state)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let upgraded = calls.iter().find_map(|call| call.get("values")).unwrap();
        assert_eq!(after["owners"][&owner], *upgraded);
        if let UpgradeTarget::Host { group, .. } = &target {
            let mut upgraded = upgraded.clone();
            let groups = upgraded["runtime-operator"]["runtime"]["hostGroups"]
                .as_array_mut()
                .unwrap();
            let selected = groups
                .iter_mut()
                .find(|entry| entry["name"] == *group)
                .unwrap();
            let image = selected.as_object_mut().unwrap().remove("image").unwrap();
            assert_eq!(image["registry"], "selected.example");
            assert_eq!(
                image["tag"],
                artifact_plan::select(&fixture.chart.0)?.values()["runtime-operator"]["runtime"]["image"]
                    ["tag"]
            );
            assert_eq!(
                upgraded, recovered,
                "only the selected image changes from recovered values"
            );
        } else {
            assert_eq!(
                after["owners"][&owner]["runtime-operator"]["runtime"]["hostGroups"][0]["image"],
                recovered["runtime-operator"]["runtime"]["hostGroups"][0]["image"]
            );
        }
        after["owners"].as_object_mut().unwrap().remove(&owner);
        state["owners"].as_object_mut().unwrap().remove(&owner);
        state["pending"] = json!({});
        state["recovered"] = json!({});
        assert_eq!(after, state, "unrelated owners and state stay unchanged");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_host_update_refuses_a_held_application_or_environment_lock() -> anyhow::Result<()>
{
    use wamn_control_registry::Triple;
    use wamn_environment_control::environment::lock::LifecycleLock;

    let fixture = Fixture::new()?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("wamn_system")?;
    let triple = Triple::new(&fixture.document.org, "fixture", "dev");
    for (selection, application) in [
        ("host:alpha/alpha", Some("alpha")),
        ("host:alpha/alpha", None),
        ("host:shared/shared", None),
    ] {
        let target: UpgradeTarget = selection.parse()?;
        let mut state = fixture.state(&target)?;
        set_database(&mut state, database.url())?;
        let (name, namespace) = target.owner(&fixture.document);
        let owner = format!("{namespace}/{name}");
        state["pending"] = json!({&owner:"pending-upgrade"});
        state["recovered"] = json!({&owner:state["owners"][&owner]});
        let held = if let Some(application) = application {
            LifecycleLock::acquire_application(database.url(), &triple, application).await?
        } else {
            LifecycleLock::acquire(database.url(), &triple).await?
        };
        // Keep the holder's connection and heartbeat active while the CLI runs.
        let (output, calls, after) =
            tokio::task::block_in_place(|| fixture.run(Some(selection), &state))?;
        held.release().await;
        assert!(
            !output.status.success(),
            "competing host update must refuse"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(wamn_environment_control::environment::lock::DEPLOYMENT_IN_PROGRESS),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let (name, namespace) = target.owner(&fixture.document);
        assert_operations(&calls, (&name, namespace), false);
        assert_eq!(after, state, "a held exclusion prevents writes");
    }
    Ok(())
}

#[test]
fn platform_release_reads_accept_pinned_helm_flags() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let version = Command::new(&fixture.helm)
        .args(["version", "--template", "{{.Version}}"])
        .output()?;
    assert!(version.status.success());
    assert_eq!(String::from_utf8(version.stdout)?.trim(), "v4.2.2");
    let mut state = fixture.state(&UpgradeTarget::Control)?;
    state["owners"] = json!({});
    let (_, calls, _) = fixture.run(Some("control"), &state)?;
    let kubeconfig = fixture.bin.join("empty-kubeconfig");
    std::fs::write(&kubeconfig, "")?;
    let releases = fixture.bin.join("empty-releases.yaml");
    std::fs::write(&releases, "[]\n")?;
    let lists: Vec<_> = calls
        .iter()
        .filter(|call| {
            call["tool"] == "helm"
                && call["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "list")
        })
        .collect();
    assert!(!lists.is_empty(), "the command must read releases");
    for call in lists {
        let mut args: Vec<_> = call["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned())
            .collect();
        let index = args.iter().position(|arg| arg == "--kubeconfig").unwrap();
        args[index + 1] = kubeconfig.to_str().unwrap().into();
        let index = args.iter().position(|arg| arg == "--kube-context").unwrap();
        args[index + 1].clear();
        let result = Command::new(&fixture.helm)
            .args(args)
            .env("HELM_DRIVER", "memory")
            .env("HELM_MEMORY_DRIVER_DATA", &releases)
            .output()?;
        assert!(
            result.status.success(),
            "Helm rejected release read arguments: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(())
}

#[test]
fn platform_host_updates_refuse_invalid_groups_without_mutation() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let control = format!("host:c-{}/alpha", fixture.document.org);
    for (selection, reason) in [
        ("host:alpha/alpha", "no host groups"),
        ("host:alpha/missing", "selected host group is absent"),
        ("host:alpha/alpha", "selected host group is ambiguous"),
        ("host:alpha/alpha", "selected host group must be in hosts"),
        (
            control.as_str(),
            "select the control service with --only control",
        ),
    ] {
        // Start from the same installed owner, including for an absent target name.
        let mut state = fixture.state(&"host:alpha/alpha".parse()?)?;
        match reason {
            "no host groups" => state["owners"]["hosts/alpha"] = json!({}),
            "selected host group is ambiguous" => {
                let groups =
                    state["owners"]["hosts/alpha"]["runtime-operator"]["runtime"]["hostGroups"]
                        .as_array_mut()
                        .unwrap();
                groups.push(groups[1].clone());
            }
            "selected host group must be in hosts" => {
                state["owners"]["hosts/alpha"]["runtime-operator"]["runtime"]["hostGroups"][1]["namespace"] =
                    "outside".into();
            }
            _ => {}
        }
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        let (output, calls, after) = fixture.run(Some(selection), &state)?;
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(reason), "{error}");
        assert_operations(&calls, (&name, namespace), false);
        assert_eq!(after, state);
    }
    Ok(())
}

#[test]
fn platform_deploy_refuses_required_chart_without_mutation() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    for selection in [
        None,
        Some("control"),
        Some("identity"),
        Some("host:alpha/alpha"),
    ] {
        for cluster_exists in [true, false] {
            let mut state = fixture.state(&UpgradeTarget::Control)?;
            if selection.is_none() {
                state["owners"]
                    .as_object_mut()
                    .unwrap()
                    .remove("platform/wamn");
            }
            if !cluster_exists {
                state["cluster"] = "".into();
            }
            let (output, calls, after) = fixture.run_with_chart(selection, None, &state)?;
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            let expected = if selection.is_some() {
                "--chart is required with --only"
            } else {
                "--chart is required for a fresh installation without release platform/wamn"
            };
            assert!(error.contains(expected), "{error}");
            assert!(error.contains("No resources changed."), "{error}");
            assert_eq!(after, state);
            for call in calls {
                let args = call["args"].as_array().unwrap();
                let read_only = match call["tool"].as_str().unwrap() {
                    "kind" => args[0] == "get" || args[0] == "export",
                    "helm" => args[4] == "list" || args[4] == "get",
                    "kubectl" => args[6] == "get" || args[6] == "port-forward",
                    _ => false,
                };
                assert!(read_only, "unexpected mutation: {call}");
            }
        }
    }
    Ok(())
}

#[test]
fn platform_installation_reports_the_supplied_charts() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let mut state = fixture.state(&UpgradeTarget::Control)?;
    state["owners"] = json!({});
    state["reapply"] = true.into();
    state["fail_read"] = "platform/secret/wamn-pg-superuser".into();
    let (output, _, after) = fixture.run(None, &state)?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("wamn-pg-superuser"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("warning: recorded chart selections"),
        "{stdout}"
    );
    for label in ["platform default", "control", "identity"] {
        assert!(
            stdout.contains(&format!(
                "{label} chart: {}",
                fixture.chart.0.canonicalize()?.display()
            )),
            "{stdout}"
        );
    }
    assert_eq!(
        after["owners"]["platform/wamn"]["platform"]["chart"],
        fixture.chart.0.canonicalize()?.to_str().unwrap()
    );
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
        let (output, first_calls, mut interrupted) = fixture.run_with_chart(None, None, &state)?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("wamn-pg-superuser"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("warning: recorded chart selections"),
            "{stdout}"
        );
        for (label, owner) in [
            ("platform default", "platform/wamn"),
            ("control", control.as_str()),
            ("identity", "identity/identity"),
        ] {
            let expected = if owner == own {
                &service.0
            } else {
                &fixture.previous.0
            };
            assert!(
                stdout.contains(&format!("{label} chart: {}", expected.display())),
                "{stdout}"
            );
        }
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
        let unused = fixture.chart.0.join("unavailable");
        let (output, retry_calls, after) =
            fixture.run_with_chart(None, Some(&unused), &interrupted)?;
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(
                "warning: recorded chart selections take precedence over --chart during reapply."
            ),
            "{stdout}"
        );
        assert!(!stdout.contains(unused.to_str().unwrap()), "{stdout}");
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
            let label = if owner == "platform/wamn" {
                "platform default"
            } else if owner == control {
                "control"
            } else {
                "identity"
            };
            assert!(
                stdout.contains(&format!("{label} chart: {}", expected.display())),
                "{stdout}"
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

#[test]
fn platform_status_reports_selected_artifacts_and_unhealthy_targets() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let selection: UpgradeTarget = "host:alpha/alpha".parse()?;
    let mut state = fixture.state(&selection)?;
    state["owners"]["identity/identity"]["platform"]["chart"] = json!(fixture.chart.0);
    // Control inherits the platform default. Identity can fail independently.
    state["owners"][format!("hosts/c-{}", fixture.document.org)]
        .as_object_mut()
        .unwrap()
        .remove("platform");
    state["workloads"]["identity/Deployment/identity"]["status"]["conditions"] = json!([
        {"type":"Progressing","status":"False","reason":"ProgressDeadlineExceeded"}
    ]);
    let (output, calls, after) = fixture.run_command("status", None, None, &state)?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!output.status.success());
    assert!(
        stdout.contains("target platform default: ready"),
        "{stdout}"
    );
    assert!(stdout.contains("target control: ready"), "{stdout}");
    assert!(stdout.contains("target identity: failed"), "{stdout}");
    let identity_report = stdout
        .split("target identity:")
        .nth(1)
        .unwrap()
        .split("\ntarget ")
        .next()
        .unwrap();
    assert!(
        identity_report.contains(&format!("chart={}", fixture.chart.0.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("chart={}", fixture.previous.0.display())),
        "{stdout}"
    );
    for (owner, values) in state["owners"].as_object().unwrap() {
        for group in values
            .pointer("/runtime-operator/runtime/hostGroups")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if group.get("image").is_none() || owner == &format!("hosts/c-{}", fixture.document.org)
            {
                continue;
            }
            let release = owner.strip_prefix("hosts/").unwrap();
            let name = group["name"].as_str().unwrap();
            assert!(
                stdout.contains(&format!("target host:{release}/{name}: ready")),
                "{stdout}"
            );
            assert!(
                stdout.contains(&format!(
                    "selected image host: {}/{}:{}",
                    group["image"]["registry"].as_str().unwrap(),
                    group["image"]["repository"].as_str().unwrap(),
                    group["image"]["tag"].as_str().unwrap()
                )),
                "{stdout}"
            );
        }
    }
    for pods in state["pods"].as_object().unwrap().values() {
        for pod in pods.as_array().unwrap() {
            for container in pod["status"]["containerStatuses"].as_array().unwrap() {
                assert!(
                    stdout.contains(container["imageID"].as_str().unwrap()),
                    "{stdout}"
                );
            }
        }
    }
    assert_operations(&calls, ("", ""), false);
    assert_eq!(after, state);
    Ok(())
}

#[test]
fn platform_targeted_update_reports_only_affected_readiness() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    for failed_upgrade in [false, true] {
        let mut state = fixture.state(&UpgradeTarget::Identity)?;
        state["workloads"]["identity/Deployment/identity"]["status"]["conditions"] = json!([
            {"type":"Progressing","status":"False","reason":"ProgressDeadlineExceeded"}
        ]);
        if failed_upgrade {
            state["stop_release"] = "identity/identity".into();
        }
        let (output, calls, after) = fixture.run(Some("identity"), &state)?;
        let stdout = String::from_utf8(output.stdout)?;
        assert!(!output.status.success());
        assert!(stdout.contains("target identity: failed"), "{stdout}");
        assert!(!stdout.contains("platform:"), "{stdout}");
        assert!(!stdout.contains("target control:"), "{stdout}");
        let expected = if failed_upgrade {
            &fixture.previous.0
        } else {
            &fixture.chart.0
        };
        assert!(
            stdout.contains(&format!("chart={}", expected.display())),
            "{stdout}"
        );
        let image = state["pods"]["identity/app.kubernetes.io/name=wamn-identity"][0]["status"]["containerStatuses"][0]["imageID"].as_str().unwrap();
        assert!(stdout.contains(image), "{stdout}");
        assert_operations(&calls, ("identity", "identity"), true);
        assert_eq!(
            after["owners"]["platform/wamn"],
            state["owners"]["platform/wamn"]
        );
    }
    Ok(())
}

#[test]
fn platform_targeted_reselection_and_retry_use_only_the_owner() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("wamn_system")?;
    for selection in [
        "control",
        "identity",
        "host:alpha/alpha",
        "host:shared/shared",
    ] {
        let target: UpgradeTarget = selection.parse()?;
        let (name, namespace) = target.owner(&fixture.document);
        let owner = format!("{namespace}/{name}");
        let mut state = fixture.state(&target)?;
        set_database(&mut state, database.url())?;
        state["stop_release"] = owner.as_str().into();
        let (output, calls, mut interrupted) = fixture.run(Some(selection), &state)?;
        assert!(!output.status.success());
        assert_operations(&calls, (&name, namespace), true);
        assert_eq!(
            interrupted, state,
            "failed upgrade retains the earlier selection"
        );
        state.as_object_mut().unwrap().remove("stop_release");
        interrupted.as_object_mut().unwrap().remove("stop_release");
        let (output, calls, retried) = fixture.run(Some(selection), &interrupted)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_operations(&calls, (&name, namespace), true);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("target {selection}: ready")),
            "{stdout}"
        );
        let (output, calls, mut earlier) =
            fixture.run_with_chart(Some(selection), Some(&fixture.previous.0), &retried)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_operations(&calls, (&name, namespace), true);
        match &target {
            UpgradeTarget::Host { group, .. } => {
                let image = earlier["owners"][&owner]["runtime-operator"]["runtime"]["hostGroups"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["name"] == *group)
                    .unwrap();
                assert_eq!(image["image"]["registry"], "localhost:5001");
                assert_eq!(image["image"]["repository"], "host");
                assert_eq!(
                    image["image"]["tag"],
                    artifact_plan::select(&fixture.previous.0)?.values()["runtime-operator"]["runtime"]
                        ["image"]["tag"]
                );
            }
            _ => assert_eq!(
                earlier["owners"][&owner]["platform"]["chart"],
                json!(fixture.previous.0)
            ),
        }
        earlier["owners"][&owner] = state["owners"][&owner].clone();
        assert_eq!(
            earlier, state,
            "reselection changes only the selected target"
        );
    }
    Ok(())
}

#[test]
fn identity_signing_inspection_stops_hung_kubectl_before_mutation() -> anyhow::Result<()> {
    const CHILD: &str = "WAMN_TEST_SIGNING_TIMEOUT";
    if let Some(directory) = std::env::var_os(CHILD) {
        let target = wamn_release_control::release_chart::Target {
            kubeconfig: PathBuf::from(directory).join("kubeconfig"),
            context: "fixture".into(),
            namespace: "platform".into(),
        };
        let runtime = tokio::runtime::Runtime::new()?;
        let error = runtime
            .block_on(identity::require_keys(
                &target,
                std::time::Duration::from_millis(350),
            ))
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("timed out"), "{error}");
        assert!(error.contains("platform deploy without --only"), "{error}");
        return Ok(());
    }
    let fixture = Fixture::new()?;
    for field in ["hang_read", "hang_exec"] {
        let mut state = fixture.state(&UpgradeTarget::Identity)?;
        state[field] = if field == "hang_read" {
            json!("platform/cluster.postgresql.cnpg.io/wamn-pg")
        } else {
            json!(true)
        };
        std::fs::write(fixture.bin.join("state.json"), serde_json::to_vec(&state)?)?;
        std::fs::write(fixture.bin.join("calls.jsonl"), "")?;
        let started = std::time::Instant::now();
        let output = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "identity_signing_inspection_stops_hung_kubectl_before_mutation",
                "--nocapture",
            ])
            .env(CHILD, &fixture.bin)
            .env("PATH", &fixture.path)
            .output()?;
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let calls = std::fs::read_to_string(fixture.bin.join("calls.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Value>, _>>()?;
        assert_operations(&calls, ("identity", "identity"), false);
        if field == "hang_exec" {
            let pid = std::fs::read_to_string(fixture.bin.join("exec.pid"))?;
            assert!(
                !Path::new("/proc").join(pid).exists(),
                "kubectl child survived the timeout"
            );
        }
        let after: Value = serde_json::from_slice(&std::fs::read(fixture.bin.join("state.json"))?)?;
        assert_eq!(after, state);
    }
    Ok(())
}
