//! THE `deploy/platform` BILL OF MATERIALS (`wamn-0h0g.10.5`).
//!
//! [`BILL_OF_MATERIALS`] below IS the bill of materials — there is no second
//! carrier. A BoM held in a doc alongside a test that checks something narrower
//! is the dual-representation shape this repository keeps closing; the table a
//! reader consults and the table the proof asserts are the same lines.
//!
//! WHAT THIS IS A PROOF ABOUT. It reads the manifests under `deploy/platform`
//! as ARTIFACTS and asserts their declared inventory — the Kubernetes objects
//! each file yields, the container images the tier schedules, and the Secrets it
//! mounts without declaring. It reads no Rust source as text (`wamn-hopk` R5);
//! a YAML manifest is the artifact, not the implementation.
//!
//! WHY THE TREE AND NOT THE CLUSTER. The frozen kind cluster still carries
//! objects this tree deleted, so a live `kubectl get` is a record of history,
//! not a target shape. The bill of materials is derived from the tree and the
//! cluster is only ever evidence of divergence.
//!
//! PLACEMENT, RECORDED SO IT CAN BE MOVED IN ONE STEP. This belongs in
//! `tests/conformance` beside the other static structural guards. It sits in
//! `wamn-proof-system` because `wamn-0h0g.12.10` owns the conformance retained-
//! manifest inventory and reconciles it against THIS table; landing both in one
//! package would have made the two edits collide. `wamn-proof-system` is the
//! black-box tier over deployed surfaces, which a deployment manifest is.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The tier this proof owns.
const PLATFORM: &str = "deploy/platform";

/// One row of the bill of materials: `(file, [(kind, name)], [image])`.
type BillOfMaterialsRow = (
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [&'static str],
);

/// EVERY object-bearing manifest in the tier.
///
/// The object list is ORDERED — it is the document order of the file, so a
/// re-ordering that changes apply order is a diff here rather than silence. The
/// image list is compared as a set.
///
/// Namespaces are not in the table because they are invariant and asserted
/// separately: every object is `wamn-system` except the per-environment
/// certificate template, which carries a substitution placeholder.
#[rustfmt::skip]
const BILL_OF_MATERIALS: [BillOfMaterialsRow; 18] = [
    // The dispatcher's projects Secret carries its database principal INSIDE
    // the file — the tier's only credential with no separate DB-URL Secret.
    ("dispatcher-projects.example.yaml",
        &[("Secret", "wamn-dispatch-projects")],
        &[]),
    ("dispatcher.yaml",
        &[("Deployment", "dispatcher"), ("PodDisruptionBudget", "dispatcher")],
        &["busybox:1.36", "wamn-dispatcher:dev"]),
    // A ServiceAccount with zero grants and no Deployment of its own: the
    // reader's Deployment example went at 2099d754 and the identity is consumed
    // by deploy/gates/m1-gate-job.yaml. Retained deliberately, not stranded.
    ("event-reader-rbac.yaml",
        &[("ServiceAccount", "event-reader")],
        &[]),
    // TWO Secrets in ONE carrier, because the executor is TWO principals
    // (wamn-0h0g.22.31): the guest-SQL url component calls run as, and the
    // executor-platform generation the queue claim dials with. Shipping them in
    // separate files would let an operator apply one and leave the other, which
    // is the failure the pair exists to prevent.
    ("executor-db.example.yaml",
        &[("Secret", "wamn-executor-db"), ("Secret", "wamn-executor-platform-db")],
        &[]),
    ("executor.yaml",
        &[("Deployment", "executor"), ("PodDisruptionBudget", "executor")],
        &["wamn-executor:dev"]),
    ("host-db.example.yaml",
        &[("Secret", "wamn-host-db")],
        &[]),
    ("host-environment-certs.example.yaml",
        &[("Certificate", "wasmcloud-runtime-tls"), ("Certificate", "wasmcloud-data-tls")],
        &[]),
    ("http-route-workload.example.yaml",
        &[("Service", "flow-http"), ("WorkloadDeployment", "flow-http")],
        &["registry.wamn-system.svc.cluster.local:5000/wamn/flow-http:dev"]),
    ("materializer.example.yaml",
        &[("WorkloadDeployment", "materializer-demo")],
        &["registry.wamn-system.svc.cluster.local:5000/wamn/materializer:dev"]),
    ("postgres.yaml",
        &[("Deployment", "postgres"), ("Service", "postgres"), ("Secret", "postgres-fixture-superuser")],
        &["postgres:18"]),
    ("registry-credentials.example.yaml",
        &[("Secret", "wamn-registry-server-auth"), ("Secret", "wamn-registry-pull"), ("Secret", "wamn-registry-push")],
        &[]),
    ("registry.yaml",
        &[("Issuer", "wasmcloud-ca"), ("Certificate", "wamn-registry-tls"),
          ("PersistentVolumeClaim", "registry-data"), ("Deployment", "registry"), ("Service", "registry")],
        &["registry:2"]),
    // The ctl Job of the tier. `wamn-0h0g.10.5` enumerated a "bootstrap ctl
    // Job"; after the 2026-08-24 re-scope this is the ctl Job that lives here,
    // and the bootstrap itself is deploy/mvp/bootstrap.sh, the classified
    // exception outside the SR8 tiers.
    ("run-plane-reconcile.example.yaml",
        &[("Job", "run-plane-reconcile-runner-demo")],
        &["wamn-ctl:dev"]),
    ("run-retention-db.example.yaml",
        &[("Secret", "wamn-run-retention-poc-f1")],
        &[]),
    // THE TIER'S ONE CronJob. `wamn-0h0g.10.5` removes "surplus CronJobs"; the
    // surplus was the recurring replica-identity repair `wamn-0h0g.12.70`
    // retired, and retention is the one recurring job that survives.
    ("run-retention.example.yaml",
        &[("CronJob", "run-retention-poc-f1")],
        &["wamn-ctl:dev"]),
    ("scenario-worker.yaml",
        &[("Deployment", "scenario-worker"), ("Service", "scenario-worker")],
        &["wamn-scenario-worker:dev"]),
    // Survives until the `wamn-0h0g.15.26` trigger fires (amendment of
    // 2026-08-16). Not a deletion candidate in this pass.
    ("waker.yaml",
        &[("ServiceAccount", "waker"), ("Role", "waker"), ("RoleBinding", "waker"), ("Deployment", "waker")],
        &["busybox:1.36", "wamn-waker:dev"]),
    ("wamn-sysdb.yaml",
        &[("Cluster", "wamn-sysdb")],
        &[]),
];

/// The one file in the tier that declares no Kubernetes object.
///
/// `values-host-default.yaml` is Helm input for the host tier: the hand-rolled
/// host Deployments became operator Host CRDs under `wamn-0h0g.15.15`/`.15.18`,
/// so the host's bill of materials is a chart release plus these values rather
/// than manifests in this directory. Its image is assembled from three keys
/// (`registry` + `repository` + `tag`), which is why it cannot be scanned for
/// an `image:` line like the rest.
const HELM_VALUES_FILE: &str = "values-host-default.yaml";
const HELM_VALUES_IMAGE_PARTS: [(&str, &str); 3] = [
    ("registry", "\"\""),
    ("repository", "wamn-host"),
    ("tag", "dev"),
];

/// Files removed from the tier that must not come back.
///
/// Each is a real path a commit deleted. A returning file is caught by the
/// exact-file-set assertion too; this list exists so the failure NAMES the
/// artifact instead of reporting an unexpected extra file.
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

/// Secrets and ConfigMaps the tier MOUNTS but does not DECLARE, each with the
/// authority that mints it. Anything mounted and not on this list must be
/// declared by a file in the table above.
///
/// This is the tier's external-prerequisite list, and it is derived rather than
/// asserted by hand: the proof computes mounted-minus-declared and compares.
///
/// A `Certificate` in this tier DECLARES the Secret its `spec.secretName`
/// names — cert-manager writes those bytes, so `wamn-registry-tls`,
/// `wasmcloud-runtime-tls` and `wasmcloud-data-tls` are declared here, not
/// prerequisites of it.
/// THE ONE HOLE IS CLOSED (`wamn-0h0g.10.14`). `wamn-executor-db` sat here from
/// `wamn-0h0g.10.5` — mounted by `executor.yaml`, declared by nothing, because
/// `ea71c1c4` deleted `runner-db.example.yaml` and no carrier came with it.
/// `deploy/platform/executor-db.example.yaml` now declares it, alongside the
/// `wamn-executor-platform-db` the `wamn-0h0g.22.31` cutover added, so both are
/// DECLARED rows in the table above rather than prerequisites of it. Every
/// database credential in this tier ships a carrier again.
const EXTERNAL_PREREQUISITES: [(&str, &str); 2] = [
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
];

/// Per-project-environment Secret name PREFIXES the scenario worker mounts.
///
/// These carry an `<org>--<project>--<env>` suffix, so they cannot be listed as
/// literals without pinning the demo triple into the proof.
const EXTERNAL_PREREQUISITE_PREFIXES: [&str; 3] = [
    "wamn-authoring-",
    "wamn-identity-reader-",
    "wamn-mgmt-admitter-",
];

/// One Kubernetes object as this proof reads it off a manifest.
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
/// value this proof reads is a single whitespace-delimited token, so a trailing
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
    let path = root.join(PLATFORM).join(file);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// THE BILL OF MATERIALS IS EXACT.
///
/// Three equalities, and the first is the one that matters: the tier's file set
/// equals the table's. A new manifest that no one recorded fails here, and so
/// does a recorded manifest someone deleted — the two halves of the defect this
/// bead exists to close.
#[test]
fn the_platform_tier_holds_exactly_the_bill_of_materials() {
    let root = repository_root();

    let recorded: BTreeSet<String> = BILL_OF_MATERIALS
        .iter()
        .map(|(file, _, _)| (*file).to_string())
        .chain(std::iter::once(HELM_VALUES_FILE.to_string()))
        .collect();
    assert_eq!(
        platform_files(&root),
        recorded,
        "deploy/platform is not the recorded bill of materials — add or remove \
         the row in BILL_OF_MATERIALS with the change that moved the file"
    );

    for (file, objects, expected_images) in BILL_OF_MATERIALS {
        let source = read(&root, file);
        let found: Vec<(String, String)> = documents(&source)
            .iter()
            .filter_map(|document| object(document, file))
            .map(|object| (object.kind, object.name))
            .collect();
        let recorded: Vec<(String, String)> = objects
            .iter()
            .map(|(kind, name)| ((*kind).to_string(), (*name).to_string()))
            .collect();
        assert_eq!(found, recorded, "{file} declares a different object list");

        let expected: BTreeSet<String> = expected_images.iter().map(|i| (*i).to_string()).collect();
        assert_eq!(images(&source), expected, "{file} schedules other images");
    }

    // The host tier's image is three keys, not one.
    let host_values = read(&root, HELM_VALUES_FILE);
    assert!(
        images(&host_values).is_empty(),
        "{HELM_VALUES_FILE} gained a literal image: line; the host image is \
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
            "{HELM_VALUES_FILE} must pin runtime.image.{key} = {value}"
        );
    }
}

/// EVERY OBJECT LANDS IN ONE NAMESPACE.
///
/// The single exception is the per-environment certificate template, whose
/// namespace is a substitution placeholder — an object applied as-is would
/// otherwise land in the wrong environment.
#[test]
fn every_platform_object_is_namespaced_to_wamn_system_or_templated() {
    let root = repository_root();
    for (file, _, _) in BILL_OF_MATERIALS {
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

    // Scanned off the manifests, not read back out of the table above: an
    // assertion over the table would only re-prove the table, and a mutant that
    // put the gates image into a real manifest would sail past it.
    for (file, _, _) in BILL_OF_MATERIALS {
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
    for (file, _, _) in BILL_OF_MATERIALS {
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
/// Computed, not transcribed: the proof collects `secretName`, `secretKeyRef`
/// and `configMap` names off the manifests, subtracts what the tier declares,
/// and compares the remainder against [`EXTERNAL_PREREQUISITES`]. A pod that
/// starts mounting an undeclared, unlisted Secret fails here — which is how
/// `wamn-executor-db` was found: it is on the list, with the reason it has no
/// carrier written down beside it.
#[test]
fn every_mounted_secret_is_declared_here_or_named_a_prerequisite() {
    let root = repository_root();

    let mut declared: BTreeSet<String> = BILL_OF_MATERIALS
        .iter()
        .flat_map(|(_, objects, _)| objects.iter())
        .filter(|(kind, _)| *kind == "Secret")
        .map(|(_, name)| (*name).to_string())
        .collect();

    let mut mounted: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let files = BILL_OF_MATERIALS
        .iter()
        .map(|(file, _, _)| *file)
        .chain(std::iter::once(HELM_VALUES_FILE));
    for file in files {
        let source = read(&root, file);
        for document in documents(&source) {
            // A Certificate does not MOUNT the Secret it names — it DECLARES
            // it, and cert-manager writes the bytes. Recording that here is
            // what keeps three issued Secrets off the prerequisite list.
            if object(&document, file).is_some_and(|object| object.kind == "Certificate") {
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
        .filter(|(name, _)| {
            !EXTERNAL_PREREQUISITE_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
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
