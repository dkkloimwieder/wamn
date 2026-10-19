//! Ordered install steps and their inputs (docs/operations/gcp.md section 4).
//!
//! Planning reads only the compiled document. Commands on owner inputs are
//! instructions for the owner, never commands for the installer to execute.

use super::document::{ClusterDocument, Provider};

/// One install step, with resolved document values and resource names.
#[derive(Debug)]
pub struct InstallStep {
    pub number: &'static str,
    pub title: &'static str,
    pub inputs: Vec<(&'static str, String)>,
    pub owner_inputs: Vec<OwnerInput>,
}

/// A resource that the owner supplies before the step can run.
#[derive(Debug, Clone)]
pub struct OwnerInput {
    pub name: String,
    /// The runbook command, or the history reference for an existing resource.
    pub instructions: String,
}

/// Resolve the install table without reading live resources or running commands.
///
/// # Errors
///
/// Returns an error when an environment coordinate cannot name a Helm release.
pub fn install_steps(document: &ClusterDocument) -> anyhow::Result<Vec<InstallStep>> {
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
    let routes = std::iter::once(document.route_hosts.control.clone())
        .chain(document.route_hosts.environments.values().cloned())
        .collect::<Vec<_>>()
        .join(", ");
    let environments = document
        .route_hosts
        .environments
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let tenants = environment_names(document, "", "");
    let environment_files = environment_names(document, "deploy/gcp/environments/", ".k");
    let host_groups = document
        .route_hosts
        .environments
        .keys()
        .map(|coordinate| {
            let mut parts = coordinate.split('/');
            crate::release_chart::release_name(
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(", ");
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

    Ok(vec![
        InstallStep {
            number: "4.1", title: "Work directory and binaries",
            inputs: vec![("work directory", "$HOME/.cache/wamn-install".into()), ("binaries", "wamn, wamn-ctl, wamn-scenario-worker".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.2", title: "Cluster",
            inputs: vec![
                ("provider", match document.provider { Provider::Gcp => "gcp", Provider::Kind => "kind" }.into()),
                ("project_id", project.clone()), ("zone", document.zone.clone()),
                ("cluster_name", document.cluster_name.clone()), ("release_channel", document.release_channel.clone()),
                ("network", document.network.clone()), ("subnetwork", document.subnetwork.clone()),
                ("pod_secondary_range_name", document.pod_secondary_range_name.clone()),
                ("service_secondary_range_name", document.service_secondary_range_name.clone()),
                ("node_service_account", document.node_service_account.clone()),
                ("workload pool", workload_pool),
                ("node pools", document.node_pools.iter().map(|pool| format!("{}: machine_type={}, spot={}, disk_type={}, disk_size={}, size={}, label={:?}, taint={:?}", pool.name, pool.machine_type, pool.spot, pool.disk_type, pool.disk_size, pool.size, pool.label, pool.taint)).collect::<Vec<_>>().join("\n")),
                ("temporary pool", "default-pool".into()),
            ],
            owner_inputs: vec![history(format!("VPC {} and subnet {}", document.network, document.subnetwork), "2.2"), history(format!("node service account {} and IAM", document.node_service_account), "2.3"), history(format!("APIs and quotas for {project}"), "1")],
        },
        InstallStep {
            number: "4.3", title: "cert-manager and the edge certificate",
            inputs: vec![("project_id", project.clone()), ("owner_email", document.owner_email.clone()), ("route hosts", routes.clone()), ("certificate DNS name", format!("*.{domain}")), ("issuers", "letsencrypt-staging, letsencrypt".into()), ("certificate and Secret", "edge/wamn-edge-tls".into())],
            owner_inputs: vec![dns.clone(), history(format!("roles/dns.admin for cert-manager/cert-manager on zone {project}"), "2.5, 4.3")],
        },
        InstallStep {
            number: "4.4", title: "Namespaces, runtime operator and internal CA",
            inputs: vec![("namespaces", "platform, identity, hosts, edge".into()), ("operator release", "platform/wamn (runtime-operator 2.10.3); watches hosts".into()), ("CA Secret and issuer", "platform/wasmcloud-ca, cert-manager/wasmcloud-ca; ClusterIssuer wasmcloud-ca".into()), ("runtime certificates", "hosts/wasmcloud-runtime-tls, hosts/wasmcloud-data-tls".into()), ("scheduler NATS", "nats://nats.platform.svc.cluster.local:4222".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.4.1", title: "Collector",
            inputs: vec![("project_id", project.clone()), ("region", region.into()), ("cluster_name", document.cluster_name.clone()), ("buckets.otel", document.buckets.otel.clone()), ("collector", "platform/otel-collector".into()), ("service account", otel_account.clone())],
            owner_inputs: vec![OwnerInput {
                name: format!("telemetry bucket gs://{}, lifecycle 30 days, writer {otel_account} and Workload Identity", document.buckets.otel),
                instructions: format!("echo '{{\"rule\":[{{\"action\":{{\"type\":\"Delete\"}},\"condition\":{{\"age\":30}}}}]}}' > /tmp/otel-lifecycle.json\ngcloud storage buckets create gs://{} --project {project} --location {region} \\\n  --uniform-bucket-level-access --lifecycle-file=/tmp/otel-lifecycle.json\ngcloud iam service-accounts create wamn-otel --project {project}\nfor role in roles/storage.objectCreator roles/storage.legacyBucketReader; do\n  gcloud storage buckets add-iam-policy-binding gs://{} \\\n    --member serviceAccount:{otel_account} --role $role\ndone\ngcloud iam service-accounts add-iam-policy-binding {otel_account} --project {project} \\\n  --role roles/iam.workloadIdentityUser --member \"serviceAccount:{project}.svc.id.goog[platform/otel-collector]\"", document.buckets.otel, document.buckets.otel),
            }],
        },
        InstallStep {
            number: "4.5", title: "PostgreSQL",
            inputs: vec![("project_id", project.clone()), ("region", region.into()), ("node pools", document.node_pools.iter().map(|pool| format!("{}: label={:?}, taint={:?}", pool.name, pool.label, pool.taint)).collect::<Vec<_>>().join("; ")), ("cluster, database, Secret", "platform/wamn-pg, wamn_system, platform/wamn-pg-superuser".into()), ("database host", "wamn-pg-rw.platform.svc.cluster.local".into()), ("storage class", "premium-rwo (pd-balanced)".into())],
            owner_inputs: vec![quota],
        },
        InstallStep {
            number: "4.6", title: "Packages and platform build",
            inputs: vec![("platform_version", document.platform_version.clone()), ("registry", registry.clone()), ("environments", environments.clone()), ("platform images", format!("{registry}/{{wamn-host,wamn-runner,wamn-gates,wamn-identity,wamn/http,wamn/materializer}}:{}", document.platform_version)), ("release chart", "$HOME/.cache/wamn-install/release-chart".into()), ("packages", "package roots and versions from environment documents".into())],
            owner_inputs: vec![registry_owner.clone()],
        },
        InstallStep {
            number: "4.7", title: "System database, org and identity issuer",
            inputs: vec![("org", org.clone()), ("owner_email", document.owner_email.clone()), ("platform domain", domain.into()), ("pool and database", "wamn-pg, wamn_system".into()), ("identity issuer", "https://identity.identity.svc.cluster.local".into()), ("identity Secrets", "identity/wamn-identity-db, identity/identity-tls, identity/identity-operator-ca".into()), ("CA ConfigMaps", "hosts/identity-ca, edge/identity-ca".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.8", title: "Event NATS",
            inputs: vec![("environments", environments.clone()), ("tenants", tenants.clone()), ("event NATS", "nats://evt-nats.platform.svc.cluster.local:4222".into()), ("broker Secrets", "platform/evt-nats-authorization, platform/evt-nats-bootstrap".into()), ("tap stream and Job", "WAMN_TAP, platform/evt-nats-tap-stream".into()), ("users per tenant", "provisioning, publisher, observer, runtime, materializer".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.9", title: "Platform install inputs",
            inputs: vec![("project_id", project.clone()), ("registry", registry.clone()), ("environments", environments.clone()), ("node placement", document.node_pools.iter().map(|pool| format!("{}: label={:?}, taint={:?}", pool.name, pool.label, pool.taint)).collect::<Vec<_>>().join("; ")), ("host service account", format!("hosts/wamn-host -> {blob_account}")), ("registry helper and Role", "hosts/wamn-registry-helper, hosts/wamn-control-status".into()), ("auth NATS certificate", "platform/wamn-ctl-auth-nats".into())],
            owner_inputs: vec![blob_binding.clone(), blob_iam.clone()],
        },
        InstallStep {
            number: "4.10", title: "Platform file",
            inputs: vec![("registry", registry.clone()), ("package artifact base", format!("{registry}/packages")), ("release artifact base", format!("{registry}/releases")), ("environments", environments.clone()), ("release namespace", "hosts".into()), ("database host", "wamn-pg-rw.platform.svc.cluster.local".into()), ("database namespace", "platform".into()), ("platform file", "$HOME/.cache/wamn-install/platform.sh".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.11", title: "First apply",
            inputs: vec![("environments", environments.clone()), ("environment documents", environment_files.clone()), ("tenants", tenants.clone()), ("database names", environment_names(document, "wamn-db-", "--<new instance suffix>")), ("release", "none".into()), ("release namespace", "hosts".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.12", title: "Identity and session key",
            inputs: vec![("org", org.clone()), ("environments", environments.clone()), ("session target Secrets", environment_names(document, "identity/wamn-session-role-reader-", "")), ("identity image", format!("{registry}/wamn-identity:{} (digest from 4.6)", document.platform_version)), ("resend_from", document.resend_from.clone()), ("route hosts", routes.clone()), ("identity issuer", "https://identity.identity.svc.cluster.local".into()), ("invite Job", "identity/invite (docs/operations/gcp.md section 5.3)".into()), ("owner_email", document.owner_email.clone()), ("operator certificate", format!("identity/operator-{org}")), ("admin Secret", "identity/wamn-system-admin (owned by invite Job)".into())],
            owner_inputs: vec![OwnerInput { name: "Secret identity/identity-resend, key api-key (nonempty, no whitespace)".into(), instructions: "kubectl -n identity create secret generic identity-resend --from-literal=api-key=\"$(tr -d '[:space:]' < <file>)\"".into() }, dns.clone()],
        },
        InstallStep {
            number: "4.13", title: "Packages",
            inputs: vec![("environments", environments.clone()), ("environment documents", environment_files.clone()), ("tenants", tenants.clone()), ("package artifact base", format!("{registry}/packages")), ("component artifact base", format!("{registry}/components")), ("package versions", "from environment documents".into())],
            owner_inputs: vec![registry_owner.clone()],
        },
        InstallStep {
            number: "4.14", title: "Releases",
            inputs: vec![("org", org.clone()), ("environments", environments.clone()), ("tenants", tenants.clone()), ("publisher principals", environment_names(document, "wamn-management-author-", "")), ("package artifact base", format!("{registry}/packages")), ("release artifact base", format!("{registry}/releases")), ("operator certificate", format!("identity/operator-{org}")), ("PAT Job and Secret", "identity/mint-pat, identity/wamn-system-admin".into()), ("packages and wirings", "from environment documents and package artifacts".into())],
            owner_inputs: vec![registry_owner.clone()],
        },
        InstallStep {
            number: "4.15", title: "Qualification",
            inputs: vec![("environments", environments.clone()), ("platform_version", document.platform_version.clone()), ("release artifact base", format!("{registry}/releases")), ("release chart", "$HOME/.cache/wamn-install/release-chart".into()), ("release digests", "from 4.14".into())],
            owner_inputs: vec![registry_owner],
        },
        InstallStep {
            number: "4.15.1", title: "Benchmark",
            inputs: vec![("project_id", project.clone()), ("zone", document.zone.clone()), ("platform_version", document.platform_version.clone()), ("environments", environments.clone()), ("result", format!("tests/bench/platform-{}.json", document.platform_version)), ("first install prerequisite", "complete 4.17 through 4.21; bench VM and binary from section 5.8".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.16", title: "Record the releases",
            inputs: vec![("environments", environments.clone()), ("environment documents", environment_files.clone()), ("route hosts", routes.clone()), ("web bucket", format!("gs://{}/clients/<package>/<release digest hex>/", document.buckets.web)), ("release digests", "from 4.14, qualified by 4.15".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.17", title: "Apply with the release",
            inputs: vec![("environments", environments.clone()), ("environment documents", environment_files), ("registry", registry.clone()), ("platform_version", document.platform_version.clone()), ("release namespace", "hosts".into()), ("releases and host groups", host_groups), ("release digests", "from 4.16".into()), ("identity CA", "hosts/identity-ca".into())],
            owner_inputs: vec![blob_binding, blob_iam],
        },
        InstallStep {
            number: "4.18", title: "CDC readers",
            inputs: vec![("environments", environments.clone()), ("replication Secrets", environment_names(document, "hosts/wamn-cdc-", "")), ("registry-reader Secrets", environment_names(document, "hosts/wamn-registry-reader-", "")), ("reader image", format!("{registry}/wamn-cdc-reader (digest from deploy/gcp/cdc-reader*.yaml)")), ("event NATS", "nats://evt-nats.platform.svc.cluster.local:4222".into()), ("service account", "hosts/event-reader".into())],
            owner_inputs: vec![],
        },
        InstallStep {
            number: "4.19", title: "Web clients",
            inputs: vec![("environments", environments), ("package artifact base", format!("{registry}/packages")), ("buckets.web", document.buckets.web.clone()), ("client destination", format!("gs://{}/clients/<package>/<release digest hex>/", document.buckets.web))],
            owner_inputs: vec![web_bucket.clone()],
        },
        InstallStep {
            number: "4.19.1", title: "Control host and platform UI",
            inputs: vec![("org", org.clone()), ("owner_email", document.owner_email.clone()), ("platform_version", document.platform_version.clone()), ("registry", registry.clone()), ("route_hosts.control", document.route_hosts.control.clone()), ("control release, Deployment, Service", format!("hosts/c-{org}, hosts/hostgroup-c-{org}, hosts/c-{org}-http")), ("control Secrets", format!("hosts/wamn-control-{org}, hosts/wamn-control-administration-{org}, hosts/operator-{org}")), ("platform UI destination", format!("gs://{}/platform-ui/{}/", document.buckets.web, document.platform_version))],
            owner_inputs: vec![web_bucket, dns.clone()],
        },
        InstallStep {
            number: "4.20", title: "Edge and load balancer",
            inputs: vec![("project_id", project.clone()), ("zone", document.zone.clone()), ("route hosts", routes.clone()), ("buckets.web", document.buckets.web.clone()), ("platform_version", document.platform_version.clone()), ("control Service", format!("hosts/c-{org}-http")), ("edge release and endpoint group", "edge/wamn-edge, wamn-edge".into()), ("backend service and URL map", "wamn-edge-platform, wamn-edge".into()), ("certificate Secret and CronJob", "edge/wamn-edge-tls, edge/edge-cert".into())],
            owner_inputs: vec![dns.clone(), history(format!("load balancer wamn-edge in {project}: address, health check, firewall rule, backend bucket, backend service, URL map, proxy, forwarding rule, certificate entry"), "4.3"), history(format!("IAM projects/{project}/roles/wamnEdgeCert and binding for edge/edge-cert"), "4.3.1")],
        },
        InstallStep {
            number: "4.21", title: "Check",
            inputs: vec![("route hosts", routes), ("HTTP checks", "/ and /config.json: 200; unauthenticated application API calls: 401".into()), ("certificate Secret", "edge/wamn-edge-tls".into())],
            owner_inputs: vec![dns],
        },
    ])
}

fn environment_names(document: &ClusterDocument, prefix: &str, suffix: &str) -> String {
    document
        .route_hosts
        .environments
        .keys()
        .map(|coordinate| format!("{prefix}{}{suffix}", coordinate.replace('/', "--")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn history(name: String, sections: &str) -> OwnerInput {
    OwnerInput {
        name,
        instructions: format!(
            "made by the owner: docs/history/gcp-install-history.md sections {sections} (docs/operations/gcp.md section 2)"
        ),
    }
}
