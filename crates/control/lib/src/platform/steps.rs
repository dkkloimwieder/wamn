//! Ordered install steps and their inputs (docs/operations/gcp.md section 4).
//!
//! Planning reads only the compiled document and the supplied chart version.
//! Commands on owner inputs are instructions for the owner, never commands
//! for the installer to execute.

use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;

use super::document::{ClusterDocument, Provider};

/// One install step, with resolved document values and resource names.
#[derive(Debug)]
pub struct InstallStep {
    pub number: &'static str,
    pub title: &'static str,
    pub runs: Runner,
    pub inputs: Vec<(&'static str, String)>,
    pub owner_inputs: Vec<OwnerInput>,
}

/// Where a step runs: from the verb, as a Kubernetes Job, or by the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    /// From the verb, through the named tools.
    Verb(&'static str),
    /// As the named Kubernetes Job, `namespace/name`.
    Job(&'static str),
    /// By the owner, through the named tools.
    Owner(&'static str),
}

impl std::fmt::Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Verb(tools) => write!(f, "from the verb ({tools})"),
            Self::Job(job) => write!(f, "Kubernetes Job {job}"),
            Self::Owner(tools) => write!(f, "by the owner ({tools})"),
        }
    }
}

/// A resource that the owner supplies before the step can run.
#[derive(Debug, Clone)]
pub struct OwnerInput {
    pub name: String,
    /// The runbook command, or the history reference for an existing resource.
    pub instructions: String,
}

/// Read the platform version from the release chart's top-level version.
///
/// # Errors
///
/// Returns an error naming the path if the chart cannot be read or decoded.
pub fn read_platform_version(path: &Path) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Chart {
        version: String,
    }

    let source = std::fs::read_to_string(path)
        .with_context(|| format!("read platform chart {}", path.display()))?;
    let chart: Chart = serde_yaml::from_str(&source).with_context(|| {
        format!(
            "read top-level version from platform chart {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        !chart.version.trim().is_empty(),
        "platform chart {} has an empty version",
        path.display()
    );
    Ok(chart.version)
}

/// Resolve the install table without reading live resources or running commands.
pub fn install_steps(document: &ClusterDocument, platform_version: &str) -> Vec<InstallStep> {
    let project = &document.project_id;
    let org = &document.org;
    let registry = &document.registry;
    let region = document
        .zone
        .rsplit_once('-')
        .map_or("", |(region, _)| region);
    let workload_pool = format!("{project}.svc.id.goog");
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);
    let routes = format!(
        "{}, each environment's route_host from the registry",
        document.route_hosts.control
    );
    let environments = "environments/*.k beside cluster.k (read at the step)";
    let env_keys = "<org>--<project>--<env>";
    let blob_account = format!("wamn-blob@{project}.iam.gserviceaccount.com");
    let otel_account = format!("wamn-otel@{project}.iam.gserviceaccount.com");
    let quota = OwnerInput {
        name: format!("quota preference wamn-ssd-{region} (SSD_TOTAL_GB=200)"),
        instructions: format!(
            "gcloud beta quotas preferences create wamn-ssd-{region} --project {project} \\\n  --service compute.googleapis.com --quota-id SSD-TOTAL-GB-per-project-region \\\n  --dimensions region={region} --preferred-value 200"
        ),
    };
    let blob_binding = OwnerInput {
        name: format!("Workload Identity hosts/wamn-host -> {blob_account}"),
        instructions: format!(
            "gcloud iam service-accounts add-iam-policy-binding {blob_account} --project {project} \\\n  --role roles/iam.workloadIdentityUser --member \"serviceAccount:{workload_pool}[hosts/wamn-host]\""
        ),
    };
    let registry_owner = history(
        format!("Artifact Registry {registry} (immutable tags)"),
        "1",
    );
    let blob_iam = history(
        format!(
            "IAM {blob_account}: bucket binding and roles/artifactregistry.reader on {registry}"
        ),
        "5.1",
    );
    let dns = history(format!("Cloud DNS zone {project}; records {routes}"), "4.3");
    let web_bucket = history(
        format!("web bucket gs://{} and upload IAM", document.buckets.web),
        "1, 4.3",
    );

    let mut steps = vec![
        InstallStep {
            number: "4.1", title: "Work directory and binaries",
            runs: Runner::Verb("local build"),
            inputs: vec![("work directory", "$HOME/.cache/wamn-install".into()), ("binaries", "wamn, wamn-ctl, wamn-scenario-worker".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.2", title: "Cluster",
            runs: Runner::Owner("tofu, from project.k"),
            inputs: vec![
                ("provider", match document.provider { Provider::Gcp => "gcp", Provider::Kind => "kind" }.into()),
                ("project_id", project.clone()), ("zone", document.zone.clone()),
                ("cluster_name", document.cluster_name.clone()),
            ],
            owner_inputs: vec![history(format!("APIs and quotas for {project}"), "1")],
        },
        InstallStep {
            number: "4.3", title: "cert-manager and the edge certificate",
            runs: Runner::Verb("Helm SDK, Kubernetes API"),
            inputs: vec![("project_id", project.clone()), ("owner_email", document.owner_email.clone()), ("route hosts", routes.clone()), ("certificate DNS name", format!("*.{domain}")), ("issuers", "letsencrypt-staging, letsencrypt".into()), ("certificate and Secret", "edge/wamn-edge-tls".into())],
            owner_inputs: vec![dns.clone(), history(format!("roles/dns.admin for cert-manager/cert-manager on zone {project}"), "2.5, 4.3")],
        },
        InstallStep {
            number: "4.4", title: "Namespaces, runtime operator and internal CA",
            runs: Runner::Verb("Helm SDK, Kubernetes API"),
            inputs: vec![("namespaces", "platform, identity, hosts, edge".into()), ("operator release", "platform/wamn (runtime-operator 2.11.0); watches hosts".into()), ("CA Secret and issuer", "platform/wasmcloud-ca, cert-manager/wasmcloud-ca; ClusterIssuer wasmcloud-ca".into()), ("runtime certificates", "hosts/wasmcloud-runtime-tls, hosts/wasmcloud-data-tls".into()), ("scheduler NATS", "nats://nats.platform.svc.cluster.local:4222".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.4.1", title: "Collector",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("project_id", project.clone()), ("region", region.into()), ("cluster_name", document.cluster_name.clone()), ("buckets.otel", document.buckets.otel.clone()), ("collector", "platform/otel-collector".into()), ("service account", otel_account.clone())],
            owner_inputs: vec![OwnerInput {
                name: format!("telemetry bucket gs://{}, lifecycle 30 days, writer {otel_account} and Workload Identity", document.buckets.otel),
                instructions: format!("echo '{{\"rule\":[{{\"action\":{{\"type\":\"Delete\"}},\"condition\":{{\"age\":30}}}}]}}' > /tmp/otel-lifecycle.json\ngcloud storage buckets create gs://{} --project {project} --location {region} \\\n  --uniform-bucket-level-access --lifecycle-file=/tmp/otel-lifecycle.json\ngcloud iam service-accounts create wamn-otel --project {project}\nfor role in roles/storage.objectCreator roles/storage.legacyBucketReader; do\n  gcloud storage buckets add-iam-policy-binding gs://{} \\\n    --member serviceAccount:{otel_account} --role $role\ndone\ngcloud iam service-accounts add-iam-policy-binding {otel_account} --project {project} \\\n  --role roles/iam.workloadIdentityUser --member \"serviceAccount:{project}.svc.id.goog[platform/otel-collector]\"", document.buckets.otel, document.buckets.otel),
            }],
        },
        InstallStep {
            number: "4.5", title: "PostgreSQL",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("project_id", project.clone()), ("region", region.into()), ("node placement", "label=role=postgres, taint=role=postgres:NoSchedule".into()), ("cluster, database, Secret", "platform/wamn-pg, wamn_system, platform/wamn-pg-superuser".into()), ("database host", "wamn-pg-rw.platform.svc.cluster.local".into()), ("storage class", "premium-rwo (pd-balanced)".into())],
            owner_inputs: vec![quota],
        },
        InstallStep {
            number: "4.6", title: "Packages and platform build",
            runs: Runner::Verb("local build"),
            inputs: vec![("platform_version", platform_version.into()), ("registry", registry.clone()), ("platform images", format!("{registry}/{{wamn-host,wamn-runner,wamn-gates,wamn-identity,wamn-edge-web,wamn/http,wamn/materializer}}:{platform_version}")), ("release chart", "$HOME/.cache/wamn-install/release-chart".into())],
            owner_inputs: vec![registry_owner.clone()],
        },
        InstallStep {
            number: "4.7", title: "System database, org and identity issuer",
            runs: Runner::Job("platform/platform-system"),
            inputs: vec![("org", org.clone()), ("owner_email", document.owner_email.clone()), ("platform domain", domain.into()), ("pool and database", "wamn-pg, wamn_system".into()), ("identity issuer", "https://identity.identity.svc.cluster.local".into()), ("identity Secrets", "identity/wamn-identity-db, identity/identity-tls, identity/identity-operator-ca".into()), ("CA ConfigMaps", "hosts/identity-ca, edge/identity-ca".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.8", title: "Event NATS",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("environments", environments.into()), ("environment keys", env_keys.into()), ("event NATS", "nats://evt-nats.platform.svc.cluster.local:4222".into()), ("broker Secret", "platform/evt-nats-authorization".into()), ("users per tenant", "provisioning, publisher, observer, runtime, materializer".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.9", title: "Platform install inputs",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("project_id", project.clone()), ("registry", registry.clone()), ("node placement", "label=role=bench-host, taint=role=bench-host:NoSchedule".into()), ("host service account", format!("hosts/wamn-host -> {blob_account}")), ("registry helper and Role", "hosts/wamn-registry-helper, hosts/wamn-control-status".into()), ("auth NATS certificate", "platform/wamn-ctl-auth-nats".into())],
            owner_inputs: vec![blob_binding.clone(), blob_iam.clone()],
        },
        InstallStep {
            number: "4.10", title: "Platform file",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("registry", registry.clone()), ("package artifact base", format!("{registry}/packages")), ("release artifact base", format!("{registry}/releases")), ("release namespace", "hosts".into()), ("database host", "wamn-pg-rw.platform.svc.cluster.local".into()), ("database namespace", "platform".into()), ("platform inputs", "existing WAMN_* environment variables and mounted Job inputs; no platform.sh file".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.11", title: "First apply",
            runs: Runner::Job("platform/platform-first-apply-<org>--<project>--<env>"),
            inputs: vec![("environments", environments.into()), ("environment keys", env_keys.into()), ("database names", "wamn-db-<org>--<project>--<env>--<new instance suffix>".into()), ("release", "none".into()), ("release namespace", "hosts".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.12", title: "Identity and session key",
            runs: Runner::Job("identity/platform-identity"),
            inputs: vec![("org", org.clone()), ("environments", environments.into()), ("session target Secrets", "identity/wamn-session-role-reader-<org>--<project>--<env>".into()), ("identity image", format!("{registry}/wamn-identity:{platform_version} (digest from 4.6)")), ("resend_from", document.resend_from.clone()), ("route hosts", routes.clone()), ("identity issuer", "https://identity.identity.svc.cluster.local".into()), ("invite Job", "identity/invite (docs/operations/gcp.md section 5.3)".into()), ("owner_email", document.owner_email.clone()), ("operator certificate", format!("identity/operator-{org}")), ("admin Secret", "identity/wamn-system-admin (owned by invite Job)".into())],
            owner_inputs: vec![match document.provider {
                Provider::Gcp => OwnerInput { name: "Secret Manager secret identity-resend (nonempty, no whitespace), readable by the agent".into(), instructions: "gcp.md section 1 item 3".into() },
                Provider::Kind => OwnerInput { name: "Secret identity/identity-resend, key api-key (nonempty, no whitespace)".into(), instructions: "kubectl -n identity create secret generic identity-resend --from-literal=api-key=\"$(tr -d '[:space:]' < <file>)\"".into() },
            }, dns.clone()],
        },
        InstallStep {
            number: "4.13", title: "Packages",
            runs: Runner::Job("platform/platform-packages-<org>--<project>--<env>"),
            inputs: vec![("environments", environments.into()), ("environment keys", env_keys.into()), ("package artifact base", format!("{registry}/packages")), ("component artifact base", format!("{registry}/components")), ("package versions", "from environment documents".into())],
            owner_inputs: vec![registry_owner.clone()],
        },
        InstallStep {
            number: "4.14", title: "Publish releases",
            runs: Runner::Job("platform/platform-release-<org>--<project>--<env>"),
            inputs: vec![("org", org.clone()), ("environments", environments.into()), ("environment keys", env_keys.into()), ("publisher principals", "wamn-management-author-<org>--<project>--<env>".into()), ("package artifact base", format!("{registry}/packages")), ("release artifact base", format!("{registry}/releases")), ("operator certificate", format!("identity/operator-{org}")), ("skip inputs", "package artifact digests and database path".into()), ("packages", "from environment documents and package artifacts".into())],
            owner_inputs: vec![registry_owner.clone()],
        },
        InstallStep {
            number: "4.15", title: "Qualification",
            runs: Runner::Owner("qualify-release on the build machine"),
            inputs: vec![("environments", environments.into()), ("platform_version", platform_version.into()), ("release artifact base", format!("{registry}/releases")), ("release chart", "$HOME/.cache/wamn-install/release-chart".into()), ("release digests", "from 4.14".into())],
            owner_inputs: vec![registry_owner],
        },
        InstallStep {
            number: "4.15.1", title: "Benchmark",
            runs: Runner::Job("platform/platform-benchmark-<org>--<project>--<env>"),
            inputs: vec![("project_id", project.clone()), ("zone", document.zone.clone()), ("platform_version", platform_version.into()), ("environments", environments.into()), ("result", format!("tests/bench/platform-{platform_version}.json")), ("first install prerequisite", "complete 4.17 through 4.21; bench VM and binary from section 5.8".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.16", title: "Record the releases",
            runs: Runner::Owner("commit the release lines printed by platform apply"),
            inputs: vec![("environments", environments.into()), ("route hosts", routes.clone()), ("web bucket", format!("gs://{}/clients/<package>/<release digest hex>/", document.buckets.web)), ("release digests", "from 4.14, qualified by 4.15".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.17", title: "Apply with the release",
            runs: Runner::Job("platform/platform-apply-<org>--<project>--<env>"),
            inputs: vec![("environments", environments.into()), ("registry", registry.clone()), ("platform_version", platform_version.into()), ("release namespace", "hosts".into()), ("releases and host groups", "r-<env>-<coordinate hash>".into()), ("release digests", "from 4.16".into()), ("identity CA", "hosts/identity-ca".into())],
            owner_inputs: vec![blob_binding, blob_iam],
        },
        InstallStep {
            number: "4.18", title: "CDC readers",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("environments", environments.into()), ("replication Secrets", "hosts/wamn-cdc-<org>--<project>--<env>".into()), ("registry-reader Secrets", "hosts/wamn-registry-reader-<org>--<project>--<env>".into()), ("reader image", "stamped platform.gates.image; /usr/local/bin/wamn-cdc-reader".into()), ("event NATS", "nats://evt-nats.platform.svc.cluster.local:4222".into()), ("service account", "hosts/event-reader".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.19", title: "Web clients",
            runs: Runner::Job("hosts/platform-web-<org>--<project>--<env>"),
            inputs: vec![("environments", environments.into()), ("package artifact base", format!("{registry}/packages")), ("buckets.web", document.buckets.web.clone()), ("client destination", format!("gs://{}/clients/<package>/<release digest hex>/", document.buckets.web))],
            owner_inputs: vec![web_bucket.clone()],
        },
        InstallStep {
            number: "4.19.1", title: "Control host and platform UI",
            runs: Runner::Job("hosts/platform-control"),
            inputs: vec![("org", org.clone()), ("owner_email", document.owner_email.clone()), ("platform_version", platform_version.into()), ("registry", registry.clone()), ("route_hosts.control", document.route_hosts.control.clone()), ("control release, Deployment, Service", format!("hosts/c-{org}, hosts/hostgroup-c-{org}, hosts/c-{org}-http")), ("control Secrets", format!("hosts/wamn-control-{org}, hosts/wamn-control-administration-{org}, hosts/operator-{org}")), ("platform UI destination", format!("gs://{}/platform-ui/{}/", document.buckets.web, platform_version))],
            owner_inputs: vec![web_bucket, dns.clone()],
        },
        InstallStep {
            number: "4.20", title: "Edge and load balancer",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("project_id", project.clone()), ("zone", document.zone.clone()), ("route hosts", routes.clone()), ("buckets.web", document.buckets.web.clone()), ("platform_version", platform_version.into()), ("control Service", format!("hosts/c-{org}-http")), ("Rust edge Deployment and Service", "edge/wamn-edge-web".into()), ("edge release and endpoint group", "edge/wamn-edge, wamn-edge".into()), ("certificate, backend service, URL map and HTTPS proxy", "wamn-edge".into()), ("certificate Secret", "edge/wamn-edge-tls".into())],
            owner_inputs: vec![OwnerInput {
                name: format!("edge/wamn-edge-web Workload Identity read on {}", document.buckets.web),
                instructions: format!("gcloud iam service-accounts create wamn-edge --project {project}\ngcloud storage buckets add-iam-policy-binding gs://{} --member serviceAccount:wamn-edge@{project}.iam.gserviceaccount.com --role roles/storage.objectViewer\ngcloud iam service-accounts add-iam-policy-binding wamn-edge@{project}.iam.gserviceaccount.com --project {project} --role roles/iam.workloadIdentityUser --member 'serviceAccount:{project}.svc.id.goog[edge/wamn-edge-web]'", document.buckets.web),
            }, dns.clone(), OwnerInput { name: format!("HTTPS load balancer wamn-edge in {project}"), instructions: "After 4.20 creates the NEG, run tofu apply deploy/gcp and create the DNS authorization CNAME it outputs, then rerun platform apply for 4.21.".into() }],
        },
        InstallStep {
            number: "4.21", title: "Check",
            runs: Runner::Verb("Kubernetes API"),
            inputs: vec![("route hosts", routes), ("HTTP checks", "/ and /config.json: 200; unauthenticated application API calls: 401".into()), ("certificate Secret", "edge/wamn-edge-tls".into())],
            owner_inputs: vec![dns],
        },
    ];
    // Each owner input is named once, at the first step that reads it.
    let mut named = std::collections::BTreeSet::new();
    for step in &mut steps {
        step.owner_inputs
            .retain(|input| named.insert(input.name.clone()));
    }
    steps
}

fn history(name: String, sections: &str) -> OwnerInput {
    OwnerInput {
        name,
        instructions: format!(
            "made by the owner: docs/history/gcp-install-history.md sections {sections} (docs/operations/gcp.md section 2)"
        ),
    }
}
