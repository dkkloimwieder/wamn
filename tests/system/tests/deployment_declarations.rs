//! Check deployment authority, namespaces, readiness, and product image boundaries.
//!
//! Read platform manifests and rendered identity objects directly.
//! Compare mounted Secrets with their declarations and external prerequisites.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The tier this test owns.
const PLATFORM: &str = "deploy/platform";

/// The host image is supplied through Helm values.
const HOST_VALUES_FILE: &str = "values-host-default.yaml";
const IDENTITY_ISSUER: &str = "https://wamn-identity.wamn-system.svc";
const IDENTITY_TLS_SECRET: &str = "wamn-identity-serving-tls";
const HELM_VALUES_IMAGE_PARTS: [(&str, &str); 3] = [
    ("registry", "\"\""),
    ("repository", "wamn-host"),
    ("tag", "dev"),
];

/// Files removed from the tier that must not come back.
///
/// Each restriction names an artifact removed from the product path.
const RETIRED_FILES: [&str; 12] = [
    "api-gateway-workload.yaml",               // ac4572f8, wamn-0h0g.12.72
    "builder-job.yaml",                        // f6bc01eb, wamn-0h0g.6.3
    "builder-netpol.yaml",                     // f6bc01eb
    "builder-signing-key.yaml",                // f6bc01eb
    "serve-node.yaml",                         // f6bc01eb
    "runner-node-placements.example.yaml",     // f6bc01eb
    "event-reader.example.yaml",               // 2099d754
    "hello-workload.yaml",                     // 3554f140, wamn-0h0g.12.2
    "trace-relay-workload.yaml",               // d1c9e471, wamn-0h0g.12.3
    "replica-identity-reconcile.example.yaml", // d1831f63, wamn-0h0g.12.70
    "runner.yaml",                             // ea71c1c4, wamn-0h0g.26.7.2
    "runner-netpol.yaml",                      // ea71c1c4
];

/// Image name fragments that must not appear on the product path.
///
/// The gates image is `wamn-gates:*` and belongs to `deploy/gates` only; the
/// node plane's images went with `crates/node` at `f6bc01eb`.
const RETIRED_IMAGE_MARKERS: [&str; 4] = ["wamn-gates", "node-host", "serve-node", "trace-relay"];

/// Externally supplied Secrets and ConfigMaps with their provisioning owner.
/// Mounted names must be declared by a platform manifest or allowed here.
/// A Certificate declares its `spec.secretName` output, which cert-manager writes.
const EXTERNAL_PREREQUISITES: [(&str, &str); 5] = [
    (
        IDENTITY_TLS_SECRET,
        "operator-configured serving certificate from the existing issuer/CA",
    ),
    // READ by registry.yaml's namespaced CA Issuer (`spec.ca.secretName`) and
    // minted by the runtime-operator Helm release, not by anything here. The
    // chart hard-codes a 365-day CA and nothing renews it (wamn-ob2f).
    (
        "wasmcloud-ca",
        "runtime-operator Helm release (Secret in wamn-system)",
    ),
    // Generated at bootstrap from deploy/sql/postgres-init.sql; the command is
    // in the postgres.yaml header.
    (
        "pg-init",
        "kubectl create configmap pg-init --from-file=deploy/sql/postgres-init.sql",
    ),
    (
        "wamn-event-nats",
        "Receiving/WMS journey bootstrap: host registration/publication broker credential and scope",
    ),
    (
        "wamn-materializer-nats",
        "Receiving/WMS journey bootstrap: native materializer broker credential and binding",
    ),
];

/// Per-project-environment Secret name PREFIXES the scenario worker mounts.
///
/// These carry an `<org>--<project>--<env>` suffix, so they cannot be listed as
/// literals without pinning the demo triple into the test. What a prefix is
/// allowed to absorb is [`absorbed_by_a_prerequisite_prefix`], not
/// `starts_with`.
const EXTERNAL_PREREQUISITE_PREFIXES: [&str; 6] = [
    "wamn-authoring-",
    // `values-host-receiving-pat.yaml` mounts all four `provision-project-env`
    // outputs with `optional: false`, but only two were listed here. The
    // overlay's own header has named the set as four since it was written; this
    // table lagged it (`wamn-10yt.3.5.2`).
    "wamn-event-materializer-",
    "wamn-executor-platform-",
    "wamn-http-admitter-",
    "wamn-identity-reader-",
    "wamn-mgmt-admitter-",
];

/// Whether an [`EXTERNAL_PREREQUISITE_PREFIXES`] row accounts for `name`.
///
/// A BARE `starts_with` MADE EVERY PREFIX A SHADOW (`wamn-wmwl`). Each prefix
/// is the stem of a Secret this tier DECLARES — `wamn-executor-platform-` over
/// `wamn-executor-platform-db`, `wamn-http-admitter-` over
/// `wamn-http-admitter-db` — so the prefix already matched the declared name.
/// The shadow was invisible only because the `unaccounted` computation drops
/// declared names first; the day a carrier is deleted the prefix absorbs the
/// orphan and this gate stays green. That cost is measured, not hypothetical:
/// `wamn-executor-db` sat unaccounted from `wamn-0h0g.10.5` after `ea71c1c4`
/// deleted `runner-db.example.yaml`, and it was caught ONLY because no prefix
/// happened to share its stem.
///
/// So a prefix absorbs only a name carrying the marker the prefix exists for.
/// That marker is the `<org>--<project>--<env>` triple
/// `crates/control/provision/src/name.rs::workload_secret_name` appends —
/// `wamn-identity-reader-acme--receiving--dev` is the shape actually in the
/// tree. `validate_project_env` slug-checks all three components and refuses a
/// consecutive-hyphen run, so the triple is EXACTLY three non-empty `--`
/// segments. A bare `db` is not one, and stays unaccounted when its carrier
/// goes.
fn absorbed_by_a_prerequisite_prefix(name: &str) -> bool {
    EXTERNAL_PREREQUISITE_PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|triple| {
            let components: Vec<&str> = triple.split("--").collect();
            components.len() == 3 && components.iter().all(|part| !part.is_empty())
        })
    })
}

/// One Kubernetes object as this test reads it off a manifest.
#[derive(Debug)]
struct Object {
    kind: String,
    name: String,
    namespace: Option<String>,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the system proof package must live at tests/system")
        .to_path_buf()
}

/// Drop whole-line comments and blank lines. A trailing `#` is left alone: every
/// value this test reads is a single whitespace-delimited token, so a trailing
/// comment falls off when the token is taken.
fn significant(source: &str) -> Vec<&str> {
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .collect()
}

/// The first whitespace-delimited token after `key:`, unquoted.
fn scalar_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(key)?.strip_prefix(':')?;
    let token = rest.split_whitespace().next()?;
    Some(token.trim_matches(|c| c == '"' || c == '\''))
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Split a manifest into YAML documents on lines that are exactly `---`.
fn documents(source: &str) -> Vec<Vec<&str>> {
    let mut documents = vec![Vec::new()];
    for line in significant(source) {
        if line.trim() == "---" {
            documents.push(Vec::new());
        } else {
            documents
                .last_mut()
                .expect("at least one document")
                .push(line);
        }
    }
    documents.retain(|document| !document.is_empty());
    documents
}

/// Read one document's `kind` and `metadata.name`/`metadata.namespace`.
///
/// Deliberately strict: a document that declares a `kind` must state a name at
/// `metadata.name`, block style, at indent 2. A flow-style `metadata: { name: }`
/// would panic here rather than be skipped — silence is the failure mode this
/// whole file exists to prevent.
fn object(document: &[&str], file: &str) -> Option<Object> {
    let mut kind = None;
    let mut name = None;
    let mut namespace = None;
    let mut in_metadata = false;
    for line in document {
        if indent(line) == 0 {
            if let Some(value) = scalar_after(line, "kind") {
                kind = Some(value.to_string());
            }
            in_metadata = line.trim() == "metadata:";
            continue;
        }
        if in_metadata && indent(line) == 2 {
            if let Some(value) = scalar_after(line, "name") {
                name = Some(value.to_string());
            }
            if let Some(value) = scalar_after(line, "namespace") {
                namespace = Some(value.to_string());
            }
        }
    }
    let kind = kind?;
    let name = name.unwrap_or_else(|| {
        panic!("{file}: a document declaring `kind: {kind}` states no `metadata.name` at indent 2")
    });
    Some(Object {
        kind,
        name,
        namespace,
    })
}

/// Every `image:` value in a manifest, list item or map key alike.
fn images(source: &str) -> BTreeSet<String> {
    significant(source)
        .into_iter()
        .filter_map(|line| {
            let line = line.trim().strip_prefix("- ").unwrap_or(line.trim());
            scalar_after(line, "image").map(str::to_string)
        })
        .collect()
}

fn platform_files(root: &Path) -> BTreeSet<String> {
    let directory = root.join(PLATFORM);
    fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .expect("read a deploy/platform entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn read(root: &Path, file: &str) -> String {
    if file == "identity" {
        let output = render_identity(root, Some(IDENTITY_ISSUER), Some(IDENTITY_TLS_SECRET), None);
        assert!(
            output.status.success(),
            "render identity chart: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return String::from_utf8(output.stdout).expect("Helm output is UTF-8");
    }
    let path = root.join(PLATFORM).join(file);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn render_identity(
    root: &Path,
    issuer: Option<&str>,
    tls_secret: Option<&str>,
    operator_ca_secret: Option<&str>,
) -> Output {
    let mut command = Command::new("helm");
    command.current_dir(root).args([
        "template",
        "wamn-identity",
        "deploy/platform/identity",
        "--namespace",
        "wamn-system",
        "-f",
        "deploy/platform/values-identity-default.yaml",
    ]);
    if let Some(issuer) = issuer {
        command.args(["--set-string", &format!("issuer={issuer}")]);
    }
    if let Some(tls_secret) = tls_secret {
        command.args(["--set-string", &format!("tlsSecret={tls_secret}")]);
    }
    if let Some(operator_ca_secret) = operator_ca_secret {
        command.args([
            "--set-string",
            &format!("operatorCaSecret={operator_ca_secret}"),
        ]);
    }
    command
        .output()
        .expect("run Helm to render the identity chart")
}

#[test]
fn the_identity_chart_requires_operator_inputs_and_renders_its_https_boundary() {
    let root = repository_root();
    for (issuer, tls, reason) in [
        (None, Some(IDENTITY_TLS_SECRET), "Set issuer"),
        (Some(IDENTITY_ISSUER), None, "Set tlsSecret"),
        (
            Some("http://identity.invalid"),
            Some(IDENTITY_TLS_SECRET),
            "issuer must use HTTPS",
        ),
    ] {
        let output = render_identity(&root, issuer, tls, None);
        assert!(
            !output.status.success(),
            "missing or insecure identity input rendered"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains(reason));
    }
    let source = read(&root, "identity");
    let rendered = documents(&source);
    let deployment = rendered
        .iter()
        .find(|document| {
            object(document, "identity").is_some_and(|object| object.kind == "Deployment")
        })
        .expect("rendered identity Deployment");
    let service = rendered
        .iter()
        .find(|document| {
            object(document, "identity").is_some_and(|object| object.kind == "Service")
        })
        .expect("rendered identity Service");
    for (key, expected) in [
        ("type", "ClusterIP"),
        ("port", "443"),
        ("targetPort", "https"),
    ] {
        assert!(
            service
                .iter()
                .any(|line| scalar_after(line, key) == Some(expected)),
            "identity Service {key}"
        );
    }
    for (key, expected) in [
        ("automountServiceAccountToken", "false"),
        ("containerPort", "8443"),
        ("scheme", "HTTPS"),
        ("path", "/healthz"),
        ("mountPath", "/var/run/wamn-identity/tls"),
        ("readOnly", "true"),
        ("secretName", IDENTITY_TLS_SECRET),
    ] {
        assert!(
            deployment
                .iter()
                .any(|line| scalar_after(line, key) == Some(expected)),
            "identity Deployment {key}"
        );
    }
    let args_index = deployment
        .iter()
        .position(|line| line.trim() == "args:")
        .expect("identity Deployment argument list");
    let args: Vec<&str> = deployment[args_index + 1..]
        .iter()
        .take_while(|line| indent(line) > indent(deployment[args_index]))
        .map(|line| line.trim().strip_prefix("- ").expect("argument list item"))
        .collect();
    assert_eq!(args, ["serve"], "unconfigured identity Deployment args");
    let environment: BTreeSet<&str> = deployment
        .iter()
        .filter_map(|line| {
            scalar_after(line.trim().strip_prefix("- ").unwrap_or(line), "name")
                .filter(|name| name.starts_with("WAMN_"))
        })
        .collect();
    assert_eq!(
        environment,
        [
            "WAMN_IDENTITY_BIND",
            "WAMN_IDENTITY_ISSUER",
            "WAMN_IDENTITY_DATABASE_URL",
            "WAMN_IDENTITY_TLS_CERT",
            "WAMN_IDENTITY_TLS_KEY",
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn identity_operator_ca_renders_only_when_explicitly_enabled() {
    let root = repository_root();
    for operator_ca in [None, Some("provisioning-operator-ca")] {
        let output = render_identity(
            &root,
            Some(IDENTITY_ISSUER),
            Some(IDENTITY_TLS_SECRET),
            operator_ca,
        );
        assert!(
            output.status.success(),
            "render operator CA configuration: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let source = String::from_utf8(output.stdout).expect("UTF-8 rendered identity chart");
        let rendered = documents(&source);
        let deployment = rendered
            .iter()
            .find(|document| {
                object(document, "identity").is_some_and(|object| object.kind == "Deployment")
            })
            .expect("rendered identity Deployment");
        let containers = manifest_list(deployment, "containers");
        assert_eq!(containers.len(), 1, "one identity process");
        let environment = manifest_list(&containers[0], "env");
        let mounts = manifest_list(&containers[0], "volumeMounts");
        let volumes = manifest_list(deployment, "volumes");
        let enabled = usize::from(operator_ca.is_some());
        assert_eq!(environment.len(), 5 + enabled);
        assert_eq!(mounts.len(), 1 + enabled);
        assert_eq!(volumes.len(), 1 + enabled);

        let operator_environment: Vec<_> = environment
            .iter()
            .filter(|entry| list_field(entry, "name") == Some("WAMN_IDENTITY_OPERATOR_CA"))
            .collect();
        let operator_mounts: Vec<_> = mounts
            .iter()
            .filter(|entry| list_field(entry, "name") == Some("operator-ca"))
            .collect();
        let operator_volumes: Vec<_> = volumes
            .iter()
            .filter(|entry| list_field(entry, "name") == Some("operator-ca"))
            .collect();
        assert_eq!(operator_environment.len(), enabled);
        assert_eq!(operator_mounts.len(), enabled);
        assert_eq!(operator_volumes.len(), enabled);
        if let Some(secret_name) = operator_ca {
            assert_eq!(
                list_field(operator_environment[0], "value"),
                Some("/var/run/wamn-identity/operator/ca.crt")
            );
            assert_eq!(
                list_field(operator_mounts[0], "mountPath"),
                Some("/var/run/wamn-identity/operator")
            );
            assert_eq!(list_field(operator_mounts[0], "readOnly"), Some("true"));
            let volume = operator_volumes[0];
            assert_eq!(list_field(volume, "secretName"), Some(secret_name));
            assert_eq!(list_field(volume, "optional"), Some("false"));
            assert_eq!(list_field(volume, "defaultMode"), Some("0400"));
            let items = manifest_list(volume, "items");
            assert_eq!(items.len(), 1, "mount only the operator CA certificate");
            assert_eq!(list_field(&items[0], "key"), Some("ca.crt"));
            assert_eq!(list_field(&items[0], "path"), Some("ca.crt"));
        }
    }
}

// Scope each entry to its own rendered list, so another volume cannot satisfy it.
fn manifest_list<'a>(document: &[&'a str], field: &str) -> Vec<Vec<&'a str>> {
    let start = document
        .iter()
        .position(|line| line.trim() == format!("{field}:"))
        .unwrap_or_else(|| panic!("rendered manifest has no {field} list"));
    let list_indent = indent(document[start]);
    let mut entries: Vec<Vec<&str>> = Vec::new();
    for line in &document[start + 1..] {
        if indent(line) <= list_indent {
            break;
        }
        if indent(line) == list_indent + 2 && line.trim().starts_with("- ") {
            entries.push(Vec::new());
        }
        entries
            .last_mut()
            .expect("rendered list starts with an entry")
            .push(line);
    }
    entries
}

fn list_field<'a>(entry: &[&'a str], key: &str) -> Option<&'a str> {
    let values: Vec<_> = entry
        .iter()
        // A nested secretKeyRef name is not the environment entry's name.
        .filter(|line| key != "name" || indent(line) <= indent(entry[0]) + 2)
        .filter_map(|line| manifest_field(line, key))
        .collect();
    assert!(values.len() <= 1, "rendered entry repeats {key}");
    values.first().copied()
}

// Read fields from block or flow-style manifest lines, never from comments.
fn manifest_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(['{', '}', ',']).find_map(|part| {
        let part = part
            .trim()
            .trim_start_matches("- ")
            .trim_matches(['[', ']']);
        scalar_after(part, key)
    })
}

fn secret_references(source: &str, file: &str) -> BTreeSet<String> {
    let lines = significant(source);
    let mut names = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(name) = manifest_field(line, "secretName") {
            names.insert(name.to_owned());
        }
        let fields: Vec<_> = line.split(['{', '}', ',']).collect();
        for (field_index, field) in fields.iter().enumerate() {
            if !matches!(
                field
                    .trim()
                    .trim_start_matches("- ")
                    .trim_matches(['[', ']']),
                "secretKeyRef:" | "secretRef:" | "secret:"
            ) {
                continue;
            }
            let name = fields[field_index + 1..]
                .iter()
                .find_map(|part| {
                    scalar_after(part, "name").or_else(|| scalar_after(part, "secretName"))
                })
                .or_else(|| {
                    lines[index + 1..]
                        .iter()
                        .take_while(|following| indent(following) > indent(line))
                        .find_map(|following| {
                            scalar_after(following, "name")
                                .or_else(|| scalar_after(following, "secretName"))
                        })
                })
                .unwrap_or_else(|| panic!("{file}: Secret reference names nothing"));
            names.insert(name.to_owned());
        }
    }
    names
}

fn environment_secret_bindings(source: &str, file: &str) -> BTreeMap<String, (String, String)> {
    let lines = significant(source);
    let mut bindings = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim() != "secretKeyRef:" {
            continue;
        }
        let entry = lines[..index]
            .iter()
            .rev()
            .find(|entry| indent(entry) + 4 == indent(line))
            .and_then(|entry| entry.trim().strip_prefix("- "))
            .and_then(|entry| scalar_after(entry, "name"))
            .unwrap_or_else(|| panic!("{file}: credential reference has no environment entry"));
        let reference: Vec<_> = lines[index + 1..]
            .iter()
            .take_while(|following| indent(following) > indent(line))
            .collect();
        let name = reference
            .iter()
            .find_map(|line| scalar_after(line, "name"))
            .unwrap_or_else(|| panic!("{file}: credential Secret has no name"));
        let key = reference
            .iter()
            .find_map(|line| scalar_after(line, "key"))
            .unwrap_or_else(|| panic!("{file}: credential Secret has no key"));
        assert!(
            bindings
                .insert(entry.to_owned(), (name.to_owned(), key.to_owned()))
                .is_none(),
            "{file}: duplicate credential environment entry {entry}"
        );
    }
    bindings
}

#[test]
fn only_identity_receives_issuer_credentials_and_other_consumers_keep_their_classes() {
    let root = repository_root();
    let mut issuer_secret_consumers = Vec::new();
    let mut issuer_url_consumers = Vec::new();
    let mut identity_image_consumers = Vec::new();
    let files = platform_files(&root);
    for file in files.iter().map(String::as_str) {
        let source = read(&root, file);
        let references = secret_references(&source, file);
        if references.contains("wamn-identity-db") {
            issuer_secret_consumers.push(file);
        }
        if significant(&source)
            .iter()
            .any(|line| manifest_field(line, "name") == Some("WAMN_IDENTITY_DATABASE_URL"))
        {
            issuer_url_consumers.push(file);
        }
        if images(&source)
            .iter()
            .any(|image| image.starts_with("wamn-identity:"))
        {
            identity_image_consumers.push(file);
        }
        if file != "identity" {
            assert!(
                !references.contains(IDENTITY_TLS_SECRET),
                "{file} mounts identity's private serving TLS key"
            );
        }
    }
    assert_eq!(issuer_secret_consumers, ["identity"]);
    assert_eq!(issuer_url_consumers, ["identity"]);
    assert_eq!(identity_image_consumers, ["identity"]);
    for (file, project) in [
        ("values-host-default.yaml", None),
        ("values-host-receiving-pat.yaml", Some("receiving")),
        ("values-host-wms-pat.yaml", Some("wms")),
    ] {
        let source = read(&root, file);
        let mut expected = BTreeMap::from([
            (
                "WAMN_PG_URL".to_owned(),
                ("wamn-host-db".to_owned(), "url".to_owned()),
            ),
            (
                "WAMN_EVT_NATS_USERNAME".to_owned(),
                ("wamn-event-nats".to_owned(), "username".to_owned()),
            ),
        ]);
        if project.is_none() {
            for (variable, key) in [
                ("WAMN_EVT_ORG", "org"),
                ("WAMN_EVT_PROJECT", "project"),
                ("WAMN_EVT_ENV", "environment"),
                ("WAMN_EVT_STREAM_REPLICAS", "stream_replicas"),
                ("WAMN_EVT_DUP_WINDOW_SECS", "dup_window_secs"),
            ] {
                expected.insert(
                    variable.to_owned(),
                    ("wamn-event-nats".to_owned(), key.to_owned()),
                );
            }
        }
        if let Some(project) = project {
            for (variable, class) in [
                ("WAMN_SYSTEM_URL", "identity-reader"),
                ("WAMN_EXECUTOR_PLATFORM_PG_URL", "executor-platform"),
                ("WAMN_HTTP_ADMITTER_PG_URL", "http-admitter"),
                ("WAMN_EVENT_MATERIALIZER_PG_URL", "event-materializer"),
            ] {
                expected.insert(
                    variable.to_owned(),
                    (
                        format!("wamn-{class}-acme--{project}--dev"),
                        "url".to_owned(),
                    ),
                );
            }
            assert!(
                !significant(&source)
                    .iter()
                    .any(|line| line.trim() == "image:" || manifest_field(line, "image").is_some()),
                "{file} must retain the base host image, not add a different process"
            );
        }
        assert_eq!(
            environment_secret_bindings(&source, file),
            expected,
            "{file} credential classes changed"
        );
    }
    let gate = read(&root, "scenario-worker.yaml");
    let expected = [
        (
            "WAMN_SYSTEM_URL",
            "wamn-identity-reader-acme--receiving--dev",
        ),
        (
            "WAMN_CONTROL_AUTHORING_PG_URL",
            "wamn-authoring-acme--receiving--dev",
        ),
        (
            "WAMN_MANAGEMENT_ADMISSION_PG_URL",
            "wamn-mgmt-admitter-acme--receiving--dev",
        ),
    ]
    .into_iter()
    .map(|(name, secret)| (name.to_owned(), (secret.to_owned(), "url".to_owned())))
    .collect();
    assert_eq!(
        environment_secret_bindings(&gate, "scenario-worker.yaml"),
        expected,
        "the authoring Gate must retain its three existing database authorities"
    );
    assert_eq!(
        images(&gate),
        BTreeSet::from(["wamn-scenario-worker:dev".to_owned()])
    );
    assert_eq!(
        environment_secret_bindings(&read(&root, "identity"), "identity"),
        BTreeMap::from([(
            "WAMN_IDENTITY_DATABASE_URL".to_owned(),
            ("wamn-identity-db".to_owned(), "url".to_owned())
        )])
    );

    // The host tier's image is three keys, not one.
    let host_values = read(&root, HOST_VALUES_FILE);
    assert!(
        images(&host_values).is_empty(),
        "{HOST_VALUES_FILE} gained a literal image: line; the host image is \
         registry+repository+tag under runtime.image"
    );
    for (key, value) in HELM_VALUES_IMAGE_PARTS {
        assert!(
            significant(&host_values)
                .iter()
                .any(
                    |line| scalar_after(line, key) == Some(value.trim_matches('"'))
                        || line.trim() == format!("{key}: {value}")
                ),
            "{HOST_VALUES_FILE} must pin runtime.image.{key} = {value}"
        );
    }
}

/// EVERY OBJECT LANDS IN ONE NAMESPACE.
///
/// The exceptions are per-environment templates whose namespace is a
/// substitution placeholder — an object applied as-is would otherwise land in
/// the wrong environment.
#[test]
fn every_platform_object_is_namespaced_to_wamn_system_or_templated() {
    let root = repository_root();
    let files = platform_files(&root);
    for file in files.iter().map(String::as_str) {
        let source = read(&root, file);
        for document in documents(&source) {
            let Some(object) = object(&document, file) else {
                continue;
            };
            let namespace = object.namespace.as_deref().unwrap_or_else(|| {
                panic!(
                    "{file}: {}/{} states no namespace",
                    object.kind, object.name
                )
            });
            assert!(
                namespace == "wamn-system" || namespace == "__ENVIRONMENT_NAMESPACE__",
                "{file}: {}/{} is namespaced {namespace}",
                object.kind,
                object.name
            );
        }
    }
}

/// NONE OF THE REMOVED ARTIFACTS ARE HERE.
///
/// Asserted over PARSED objects and images rather than raw text, deliberately.
/// `executor.yaml` names `deploy/platform/runner.yaml` in a dated historical
/// clause, and a text ban would force that true sentence out to satisfy a guard
/// — the inverse of the defect. What must be absent is the ARTIFACT.
#[test]
fn the_platform_tier_carries_no_retired_artifact() {
    let root = repository_root();
    let present = platform_files(&root);
    for retired in RETIRED_FILES {
        assert!(
            !present.contains(retired),
            "retired manifest returned: {PLATFORM}/{retired}"
        );
    }

    for file in present.iter().map(String::as_str) {
        for image in images(&read(&root, file)) {
            for marker in RETIRED_IMAGE_MARKERS {
                assert!(
                    !image.contains(marker),
                    "{file} schedules {image}, which carries the retired marker \
                     {marker:?} — the gates image and the node plane are not on \
                     the product path"
                );
            }
        }
    }
}

/// THE READINESS CONTRACT IS PROVISIONED ONCE.
///
/// `wamn-0h0g.5.16` gave the executor the final readiness shape: a `/readyz`
/// endpoint that holds Ready only on complete closure, bound by
/// `WAMN_READINESS_BIND`. Exactly one workload in the tier carries it. The other
/// two probes in the tier are a fixture's `pg_isready` exec and the registry's
/// `tcpSocket` — different mechanisms on purpose, and neither is this contract.
#[test]
fn the_readiness_contract_is_provisioned_once() {
    let root = repository_root();
    let mut readyz = Vec::new();
    let mut binds = Vec::new();
    let files = platform_files(&root);
    for file in files.iter().map(String::as_str) {
        let source = read(&root, file);
        for line in significant(&source) {
            if scalar_after(line, "path") == Some("/readyz") {
                readyz.push(file);
            }
            if line.contains("WAMN_READINESS_BIND") {
                binds.push(file);
            }
        }
    }
    assert_eq!(
        readyz,
        ["executor.yaml"],
        "the /readyz contract must be provisioned exactly once, by the executor"
    );
    assert_eq!(
        binds,
        ["executor.yaml"],
        "WAMN_READINESS_BIND must be set exactly once, by the executor"
    );
}

/// EVERY MOUNTED SECRET IS DECLARED HERE OR NAMED AS A PREREQUISITE.
///
/// Computed, not transcribed: the test collects `secretName`, `secretKeyRef`
/// and `configMap` names off the manifests, subtracts what the tier declares,
/// and compares the remainder against [`EXTERNAL_PREREQUISITES`]. A pod that
/// starts mounting an undeclared, unlisted Secret fails here — which is how
/// `wamn-executor-db` was found: it is on the list, with the reason it has no
/// carrier written down beside it.
#[test]
fn every_mounted_secret_is_declared_here_or_named_a_prerequisite() {
    let root = repository_root();

    let mut declared = BTreeSet::new();
    let mut mounted: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let files = platform_files(&root);
    for file in files.iter().map(String::as_str) {
        let source = read(&root, file);
        for document in documents(&source) {
            let declared_object = object(&document, file);
            if let Some(Object { kind, name, .. }) = &declared_object {
                if kind == "Secret" {
                    declared.insert(name.clone());
                }
            }
            // A Certificate does not MOUNT the Secret it names — it DECLARES
            // it, and cert-manager writes the bytes. Recording that here is
            // what keeps three issued Secrets off the prerequisite list.
            if declared_object.is_some_and(|object| object.kind == "Certificate") {
                let issued = document
                    .iter()
                    .find_map(|line| {
                        (indent(line) == 2)
                            .then(|| scalar_after(line, "secretName"))
                            .flatten()
                    })
                    .unwrap_or_else(|| panic!("{file}: a Certificate issues no spec.secretName"));
                declared.insert(issued.to_string());
                continue;
            }
            for (index, line) in document.iter().enumerate() {
                // `secretName: x` is a direct reference. `secretKeyRef:` and
                // `configMap:` open a block whose `name:` is the reference.
                if let Some(name) = scalar_after(line, "secretName") {
                    mounted.entry(name.to_string()).or_default().insert(file);
                }
                let opens_block = matches!(
                    line.trim(),
                    "secretKeyRef:" | "configMap:" | "secretRef:" | "configMapKeyRef:"
                );
                if opens_block {
                    let name = document[index + 1..]
                        .iter()
                        .take_while(|following| indent(following) > indent(line))
                        .find_map(|following| scalar_after(following, "name"))
                        .unwrap_or_else(|| panic!("{file}: {} block names nothing", line.trim()));
                    mounted.entry(name.to_string()).or_default().insert(file);
                }
            }
        }
    }

    let prerequisites: BTreeSet<&str> = EXTERNAL_PREREQUISITES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let unaccounted: Vec<(String, BTreeSet<&str>)> = mounted
        .into_iter()
        .filter(|(name, _)| !declared.contains(name))
        .filter(|(name, _)| !prerequisites.contains(name.as_str()))
        .filter(|(name, _)| !absorbed_by_a_prerequisite_prefix(name))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "deploy/platform mounts Secrets it neither declares nor names as a \
         prerequisite: {unaccounted:?}"
    );

    // The list is not allowed to rot in the other direction either: a
    // prerequisite nothing mounts any more is a stale row.
    for (name, minted_by) in EXTERNAL_PREREQUISITES {
        assert!(
            !declared.contains(name),
            "{name} is declared by this tier and must not also be listed as an \
             external prerequisite minted by {minted_by}"
        );
    }

    // The PREFIXES had no such coverage, which is how the shadow got in
    // (`wamn-wmwl`). Same direction, one grain finer: a prefix that absorbs a
    // name this tier DECLARES is a shadow lying in wait for the day that
    // carrier is deleted, so the marker must keep the two apart HERE, while
    // both still exist, rather than the day one of them goes.
    for name in &declared {
        assert!(
            !absorbed_by_a_prerequisite_prefix(name),
            "{name} is declared by this tier and is also absorbed by an \
             external-prerequisite prefix; delete its carrier and the prefix \
             would account for it silently"
        );
    }
}

/// THE DOORBELL ROUTE IS AN EXECUTION TARGET, RENDERED WHERE THE SHAPE ALLOWS.
///
/// `wamn-0h0g.5.9`'s placement contract makes the doorbell subject segment an
/// EXECUTION TARGET. Two carriers in this tier can state one and now do, and
/// they must agree: the dispatcher publishes at the projects-file entry's
/// `execution_target_id`, and the waker subscribes at the `--wake` left-hand
/// side. Omitting the projects-file key silently routes by the RLS claim
/// instead, through the MVP tenant-to-target adapter.
///
/// `materializer.example.yaml` is NOT checked here: the guest config shape
/// carries no key for a target, so it has nothing to render (see its header).
#[test]
fn the_rendered_execution_target_agrees_across_dispatcher_and_waker() {
    let root = repository_root();

    let projects = read(&root, "dispatcher-projects.example.yaml");
    let published: Vec<&str> = significant(&projects)
        .into_iter()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once(':')?;
            (key.trim_matches('"') == "execution_target_id")
                .then(|| value.trim().trim_end_matches(',').trim_matches('"'))
        })
        .collect();
    assert!(
        !published.is_empty(),
        "every dispatcher projects-file entry must render execution_target_id \
         explicitly; omitted, project_spec falls back to the MVP \
         tenant-to-target adapter and routes by the RLS claim"
    );

    let waker = read(&root, "waker.yaml");
    let subscribed: Vec<&str> = significant(&waker)
        .into_iter()
        .filter_map(|line| line.trim().strip_prefix("- \"")?.strip_suffix('"'))
        .filter_map(|value| value.split_once('='))
        .map(|(target, _deployment)| target)
        .collect();
    assert!(
        !subscribed.is_empty(),
        "waker.yaml must carry at least one --wake <execution-target-id>=<Deployment>"
    );
    for target in &published {
        assert!(
            subscribed.contains(target),
            "the dispatcher publishes at execution target {target:?} and no \
             --wake mapping in waker.yaml subscribes to it: {subscribed:?}"
        );
    }
}
