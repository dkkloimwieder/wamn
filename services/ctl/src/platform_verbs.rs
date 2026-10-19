//! Arguments and output of the `platform` verbs.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use wamn_control::platform::document::ClusterDocument;
use wamn_control::platform::steps::install_steps;

/// One `platform` verb.
#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// Print a cluster.k file in its standard KCL form.
    Show(ShowArgs),
    /// Print the ordered install steps and owner inputs of a cluster.k file.
    Apply(ApplyArgs),
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// The cluster.k document.
    pub file: PathBuf,
    /// Print the install plan without changing resources.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The cluster.k document.
    pub file: PathBuf,
}

pub fn run(command: PlatformCommand, mut output: impl Write) -> anyhow::Result<()> {
    match command {
        PlatformCommand::Show(args) => {
            let document = ClusterDocument::compile(&args.file)?;
            output
                .write_all(document.to_kcl().as_bytes())
                .context("write the cluster document")?;
        }
        PlatformCommand::Apply(args) => {
            ensure!(
                args.dry_run,
                "platform apply: only --dry-run exists until E2"
            );
            let document = ClusterDocument::compile(&args.file)?;
            for step in install_steps(&document)? {
                writeln!(output, "{} {}", step.number, step.title)?;
                for (name, value) in step.inputs {
                    let mut lines = value.lines();
                    writeln!(output, "  {name}: {}", lines.next().unwrap_or_default())?;
                    for line in lines {
                        writeln!(output, "    {line}")?;
                    }
                }
                if step.owner_inputs.is_empty() {
                    writeln!(output, "  owner inputs: none")?;
                }
                for input in step.owner_inputs {
                    writeln!(output, "  owner input: {}", input.name)?;
                    for line in input.instructions.lines() {
                        writeln!(output, "    {line}")?;
                    }
                }
                writeln!(output)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ClusterDocument, run};
    use crate::{Cli, Command};
    use clap::Parser as _;
    use std::path::Path;

    #[test]
    fn apply_without_dry_run_refuses_before_reading_the_file() {
        let cli = Cli::try_parse_from(["wamn-ctl", "platform", "apply", "/missing/cluster.k"])
            .expect("parse platform apply");
        let Command::Platform(command) = cli.command else {
            panic!("expected a platform command");
        };
        let mut output = Vec::new();
        let error = run(command, &mut output).unwrap_err();
        assert_eq!(
            error.to_string(),
            "platform apply: only --dry-run exists until E2"
        );
        assert!(output.is_empty());
    }

    #[test]
    fn apply_dry_run_prints_the_committed_cluster_plan() {
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
        let cli = Cli::try_parse_from([
            "wamn-ctl".as_ref(),
            "platform".as_ref(),
            "apply".as_ref(),
            committed.as_os_str(),
            "--dry-run".as_ref(),
        ])
        .expect("parse platform apply --dry-run");
        let Command::Platform(command) = cli.command else {
            panic!("expected a platform command");
        };
        let mut output = Vec::new();
        run(command, &mut output).expect("print the install plan");
        assert_eq!(String::from_utf8(output).unwrap(), EXPECTED_PLAN);
    }

    const EXPECTED_PLAN: &str = r#"4.1 Work directory and binaries
  work directory: $HOME/.cache/wamn-install
  binaries: wamn, wamn-ctl, wamn-scenario-worker
  owner inputs: none

4.2 Cluster
  provider: gcp
  project_id: wamn-dev
  zone: us-central1-a
  cluster_name: wamn
  release_channel: regular
  network: wamn
  subnetwork: wamn-us-central1
  pod_secondary_range_name: pods
  service_secondary_range_name: services
  node_service_account: wamn-nodes@wamn-dev.iam.gserviceaccount.com
  workload pool: wamn-dev.svc.id.goog
  node pools: main: machine_type=e2-standard-2, spot=true, disk_type=pd-standard, disk_size=30, size=1, label="", taint=""
    bench: machine_type=e2-standard-2, spot=true, disk_type=pd-standard, disk_size=30, size=1, label="role=bench-host", taint="role=bench-host:NoSchedule"
    pg: machine_type=e2-standard-2, spot=false, disk_type=pd-standard, disk_size=50, size=1, label="role=postgres", taint="role=postgres:NoSchedule"
  temporary pool: default-pool
  owner input: VPC wamn and subnet wamn-us-central1
    made by the owner: docs/history/gcp-install-history.md sections 2.2 (docs/operations/gcp.md section 2)
  owner input: node service account wamn-nodes@wamn-dev.iam.gserviceaccount.com and IAM
    made by the owner: docs/history/gcp-install-history.md sections 2.3 (docs/operations/gcp.md section 2)
  owner input: APIs and quotas for wamn-dev
    made by the owner: docs/history/gcp-install-history.md sections 1 (docs/operations/gcp.md section 2)

4.3 cert-manager and the edge certificate
  project_id: wamn-dev
  owner_email: dkkloimwieder@gmail.com
  route hosts: control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
  certificate DNS name: *.wamn.dev
  issuers: letsencrypt-staging, letsencrypt
  certificate and Secret: edge/wamn-edge-tls
  owner input: Cloud DNS zone wamn-dev; records control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
    made by the owner: docs/history/gcp-install-history.md sections 4.3 (docs/operations/gcp.md section 2)
  owner input: roles/dns.admin for cert-manager/cert-manager on zone wamn-dev
    made by the owner: docs/history/gcp-install-history.md sections 2.5, 4.3 (docs/operations/gcp.md section 2)

4.4 Namespaces, runtime operator and internal CA
  namespaces: platform, identity, hosts, edge
  operator release: platform/wamn (runtime-operator 2.10.3); watches hosts
  CA Secret and issuer: platform/wasmcloud-ca, cert-manager/wasmcloud-ca; ClusterIssuer wasmcloud-ca
  runtime certificates: hosts/wasmcloud-runtime-tls, hosts/wasmcloud-data-tls
  scheduler NATS: nats://nats.platform.svc.cluster.local:4222
  owner inputs: none

4.4.1 Collector
  project_id: wamn-dev
  region: us-central1
  cluster_name: wamn
  buckets.otel: wamn-dev-otel
  collector: platform/otel-collector
  service account: wamn-otel@wamn-dev.iam.gserviceaccount.com
  owner input: telemetry bucket gs://wamn-dev-otel, lifecycle 30 days, writer wamn-otel@wamn-dev.iam.gserviceaccount.com and Workload Identity
    echo '{"rule":[{"action":{"type":"Delete"},"condition":{"age":30}}]}' > /tmp/otel-lifecycle.json
    gcloud storage buckets create gs://wamn-dev-otel --project wamn-dev --location us-central1 \
      --uniform-bucket-level-access --lifecycle-file=/tmp/otel-lifecycle.json
    gcloud iam service-accounts create wamn-otel --project wamn-dev
    for role in roles/storage.objectCreator roles/storage.legacyBucketReader; do
      gcloud storage buckets add-iam-policy-binding gs://wamn-dev-otel \
        --member serviceAccount:wamn-otel@wamn-dev.iam.gserviceaccount.com --role $role
    done
    gcloud iam service-accounts add-iam-policy-binding wamn-otel@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
      --role roles/iam.workloadIdentityUser --member "serviceAccount:wamn-dev.svc.id.goog[platform/otel-collector]"

4.5 PostgreSQL
  project_id: wamn-dev
  region: us-central1
  node pools: main: label="", taint=""; bench: label="role=bench-host", taint="role=bench-host:NoSchedule"; pg: label="role=postgres", taint="role=postgres:NoSchedule"
  cluster, database, Secret: platform/wamn-pg, wamn_system, platform/wamn-pg-superuser
  database host: wamn-pg-rw.platform.svc.cluster.local
  storage class: premium-rwo (pd-balanced)
  owner input: quota preference wamn-ssd-us-central1 (SSD_TOTAL_GB=200)
    gcloud beta quotas preferences create wamn-ssd-us-central1 --project wamn-dev \
      --service compute.googleapis.com --quota-id SSD-TOTAL-GB-per-project-region \
      --dimensions region=us-central1 --preferred-value 200

4.6 Packages and platform build
  platform_version: 0.1.34
  registry: us-central1-docker.pkg.dev/wamn-dev/wamn
  environments: dkk/receiving/dev, dkk/wms/dev
  platform images: us-central1-docker.pkg.dev/wamn-dev/wamn/{wamn-host,wamn-runner,wamn-gates,wamn-identity,wamn/http,wamn/materializer}:0.1.34
  release chart: $HOME/.cache/wamn-install/release-chart
  packages: package roots and versions from environment documents
  owner input: Artifact Registry us-central1-docker.pkg.dev/wamn-dev/wamn (immutable tags)
    made by the owner: docs/history/gcp-install-history.md sections 1 (docs/operations/gcp.md section 2)

4.7 System database, org and identity issuer
  org: dkk
  owner_email: dkkloimwieder@gmail.com
  platform domain: wamn.dev
  pool and database: wamn-pg, wamn_system
  identity issuer: https://identity.identity.svc.cluster.local
  identity Secrets: identity/wamn-identity-db, identity/identity-tls, identity/identity-operator-ca
  CA ConfigMaps: hosts/identity-ca, edge/identity-ca
  owner inputs: none

4.8 Event NATS
  environments: dkk/receiving/dev, dkk/wms/dev
  tenants: dkk--receiving--dev, dkk--wms--dev
  event NATS: nats://evt-nats.platform.svc.cluster.local:4222
  broker Secrets: platform/evt-nats-authorization, platform/evt-nats-bootstrap
  tap stream and Job: WAMN_TAP, platform/evt-nats-tap-stream
  users per tenant: provisioning, publisher, observer, runtime, materializer
  owner inputs: none

4.9 Platform install inputs
  project_id: wamn-dev
  registry: us-central1-docker.pkg.dev/wamn-dev/wamn
  environments: dkk/receiving/dev, dkk/wms/dev
  node placement: main: label="", taint=""; bench: label="role=bench-host", taint="role=bench-host:NoSchedule"; pg: label="role=postgres", taint="role=postgres:NoSchedule"
  host service account: hosts/wamn-host -> wamn-blob@wamn-dev.iam.gserviceaccount.com
  registry helper and Role: hosts/wamn-registry-helper, hosts/wamn-control-status
  auth NATS certificate: platform/wamn-ctl-auth-nats
  owner input: Workload Identity hosts/wamn-host -> wamn-blob@wamn-dev.iam.gserviceaccount.com
    gcloud iam service-accounts add-iam-policy-binding wamn-blob@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
      --role roles/iam.workloadIdentityUser --member "serviceAccount:wamn-dev.svc.id.goog[hosts/wamn-host]"
  owner input: IAM wamn-blob@wamn-dev.iam.gserviceaccount.com: bucket binding and roles/artifactregistry.reader on us-central1-docker.pkg.dev/wamn-dev/wamn
    made by the owner: docs/history/gcp-install-history.md sections 5.1 (docs/operations/gcp.md section 2)

4.10 Platform file
  registry: us-central1-docker.pkg.dev/wamn-dev/wamn
  package artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/packages
  release artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/releases
  environments: dkk/receiving/dev, dkk/wms/dev
  release namespace: hosts
  database host: wamn-pg-rw.platform.svc.cluster.local
  database namespace: platform
  platform file: $HOME/.cache/wamn-install/platform.sh
  owner inputs: none

4.11 First apply
  environments: dkk/receiving/dev, dkk/wms/dev
  environment documents: deploy/gcp/environments/dkk--receiving--dev.k, deploy/gcp/environments/dkk--wms--dev.k
  tenants: dkk--receiving--dev, dkk--wms--dev
  database names: wamn-db-dkk--receiving--dev--<new instance suffix>, wamn-db-dkk--wms--dev--<new instance suffix>
  release: none
  release namespace: hosts
  owner inputs: none

4.12 Identity and session key
  org: dkk
  environments: dkk/receiving/dev, dkk/wms/dev
  session target Secrets: identity/wamn-session-role-reader-dkk--receiving--dev, identity/wamn-session-role-reader-dkk--wms--dev
  identity image: us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-identity:0.1.34 (digest from 4.6)
  resend_from: noreply@wamn.dev
  route hosts: control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
  identity issuer: https://identity.identity.svc.cluster.local
  invite Job: identity/invite (docs/operations/gcp.md section 5.3)
  owner_email: dkkloimwieder@gmail.com
  operator certificate: identity/operator-dkk
  admin Secret: identity/wamn-system-admin (owned by invite Job)
  owner input: Secret identity/identity-resend, key api-key (nonempty, no whitespace)
    kubectl -n identity create secret generic identity-resend --from-literal=api-key="$(tr -d '[:space:]' < <file>)"
  owner input: Cloud DNS zone wamn-dev; records control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
    made by the owner: docs/history/gcp-install-history.md sections 4.3 (docs/operations/gcp.md section 2)

4.13 Packages
  environments: dkk/receiving/dev, dkk/wms/dev
  environment documents: deploy/gcp/environments/dkk--receiving--dev.k, deploy/gcp/environments/dkk--wms--dev.k
  tenants: dkk--receiving--dev, dkk--wms--dev
  package artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/packages
  component artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/components
  package versions: from environment documents
  owner input: Artifact Registry us-central1-docker.pkg.dev/wamn-dev/wamn (immutable tags)
    made by the owner: docs/history/gcp-install-history.md sections 1 (docs/operations/gcp.md section 2)

4.14 Releases
  org: dkk
  environments: dkk/receiving/dev, dkk/wms/dev
  tenants: dkk--receiving--dev, dkk--wms--dev
  publisher principals: wamn-management-author-dkk--receiving--dev, wamn-management-author-dkk--wms--dev
  package artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/packages
  release artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/releases
  operator certificate: identity/operator-dkk
  PAT Job and Secret: identity/mint-pat, identity/wamn-system-admin
  packages and wirings: from environment documents and package artifacts
  owner input: Artifact Registry us-central1-docker.pkg.dev/wamn-dev/wamn (immutable tags)
    made by the owner: docs/history/gcp-install-history.md sections 1 (docs/operations/gcp.md section 2)

4.15 Qualification
  environments: dkk/receiving/dev, dkk/wms/dev
  platform_version: 0.1.34
  release artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/releases
  release chart: $HOME/.cache/wamn-install/release-chart
  release digests: from 4.14
  owner input: Artifact Registry us-central1-docker.pkg.dev/wamn-dev/wamn (immutable tags)
    made by the owner: docs/history/gcp-install-history.md sections 1 (docs/operations/gcp.md section 2)

4.15.1 Benchmark
  project_id: wamn-dev
  zone: us-central1-a
  platform_version: 0.1.34
  environments: dkk/receiving/dev, dkk/wms/dev
  result: tests/bench/platform-0.1.34.json
  first install prerequisite: complete 4.17 through 4.21; bench VM and binary from section 5.8
  owner inputs: none

4.16 Record the releases
  environments: dkk/receiving/dev, dkk/wms/dev
  environment documents: deploy/gcp/environments/dkk--receiving--dev.k, deploy/gcp/environments/dkk--wms--dev.k
  route hosts: control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
  web bucket: gs://wamn-dev-web/clients/<package>/<release digest hex>/
  release digests: from 4.14, qualified by 4.15
  owner inputs: none

4.17 Apply with the release
  environments: dkk/receiving/dev, dkk/wms/dev
  environment documents: deploy/gcp/environments/dkk--receiving--dev.k, deploy/gcp/environments/dkk--wms--dev.k
  registry: us-central1-docker.pkg.dev/wamn-dev/wamn
  platform_version: 0.1.34
  release namespace: hosts
  releases and host groups: r-dev-ee95c37ee965257ff856, r-dev-8f6b3ee8ac75e40affb1
  release digests: from 4.16
  identity CA: hosts/identity-ca
  owner input: Workload Identity hosts/wamn-host -> wamn-blob@wamn-dev.iam.gserviceaccount.com
    gcloud iam service-accounts add-iam-policy-binding wamn-blob@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
      --role roles/iam.workloadIdentityUser --member "serviceAccount:wamn-dev.svc.id.goog[hosts/wamn-host]"
  owner input: IAM wamn-blob@wamn-dev.iam.gserviceaccount.com: bucket binding and roles/artifactregistry.reader on us-central1-docker.pkg.dev/wamn-dev/wamn
    made by the owner: docs/history/gcp-install-history.md sections 5.1 (docs/operations/gcp.md section 2)

4.18 CDC readers
  environments: dkk/receiving/dev, dkk/wms/dev
  replication Secrets: hosts/wamn-cdc-dkk--receiving--dev, hosts/wamn-cdc-dkk--wms--dev
  registry-reader Secrets: hosts/wamn-registry-reader-dkk--receiving--dev, hosts/wamn-registry-reader-dkk--wms--dev
  reader image: us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-cdc-reader (digest from deploy/gcp/cdc-reader*.yaml)
  event NATS: nats://evt-nats.platform.svc.cluster.local:4222
  service account: hosts/event-reader
  owner inputs: none

4.19 Web clients
  environments: dkk/receiving/dev, dkk/wms/dev
  package artifact base: us-central1-docker.pkg.dev/wamn-dev/wamn/packages
  buckets.web: wamn-dev-web
  client destination: gs://wamn-dev-web/clients/<package>/<release digest hex>/
  owner input: web bucket gs://wamn-dev-web and upload IAM
    made by the owner: docs/history/gcp-install-history.md sections 1, 4.3 (docs/operations/gcp.md section 2)

4.19.1 Control host and platform UI
  org: dkk
  owner_email: dkkloimwieder@gmail.com
  platform_version: 0.1.34
  registry: us-central1-docker.pkg.dev/wamn-dev/wamn
  route_hosts.control: control.wamn.dev
  control release, Deployment, Service: hosts/c-dkk, hosts/hostgroup-c-dkk, hosts/c-dkk-http
  control Secrets: hosts/wamn-control-dkk, hosts/wamn-control-administration-dkk, hosts/operator-dkk
  platform UI destination: gs://wamn-dev-web/platform-ui/0.1.34/
  owner input: web bucket gs://wamn-dev-web and upload IAM
    made by the owner: docs/history/gcp-install-history.md sections 1, 4.3 (docs/operations/gcp.md section 2)
  owner input: Cloud DNS zone wamn-dev; records control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
    made by the owner: docs/history/gcp-install-history.md sections 4.3 (docs/operations/gcp.md section 2)

4.20 Edge and load balancer
  project_id: wamn-dev
  zone: us-central1-a
  route hosts: control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
  buckets.web: wamn-dev-web
  platform_version: 0.1.34
  control Service: hosts/c-dkk-http
  edge release and endpoint group: edge/wamn-edge, wamn-edge
  backend service and URL map: wamn-edge-platform, wamn-edge
  certificate Secret and CronJob: edge/wamn-edge-tls, edge/edge-cert
  owner input: Cloud DNS zone wamn-dev; records control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
    made by the owner: docs/history/gcp-install-history.md sections 4.3 (docs/operations/gcp.md section 2)
  owner input: load balancer wamn-edge in wamn-dev: address, health check, firewall rule, backend bucket, backend service, URL map, proxy, forwarding rule, certificate entry
    made by the owner: docs/history/gcp-install-history.md sections 4.3 (docs/operations/gcp.md section 2)
  owner input: IAM projects/wamn-dev/roles/wamnEdgeCert and binding for edge/edge-cert
    made by the owner: docs/history/gcp-install-history.md sections 4.3.1 (docs/operations/gcp.md section 2)

4.21 Check
  route hosts: control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
  HTTP checks: / and /config.json: 200; unauthenticated application API calls: 401
  certificate Secret: edge/wamn-edge-tls
  owner input: Cloud DNS zone wamn-dev; records control.wamn.dev, receiving.wamn.dev, wms.wamn.dev
    made by the owner: docs/history/gcp-install-history.md sections 4.3 (docs/operations/gcp.md section 2)

"#;

    fn show(file: &Path) -> Vec<u8> {
        let cli = Cli::try_parse_from([
            "wamn-ctl".as_ref(),
            "platform".as_ref(),
            "show".as_ref(),
            file.as_os_str(),
        ])
        .expect("parse platform show");
        let Command::Platform(command) = cli.command else {
            panic!("expected a platform command");
        };
        let mut output = Vec::new();
        run(command, &mut output).expect("show the cluster document");
        output
    }

    #[test]
    fn show_preserves_the_committed_document_and_repeats_the_same_bytes() {
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
        let document =
            ClusterDocument::compile(&committed).expect("compile the committed document");
        let output = show(&committed);
        let directory =
            std::env::temp_dir().join(format!("wamn-platform-show-{}", std::process::id()));
        std::fs::create_dir(&directory).expect("create the directory");
        let file = directory.join("cluster.k");
        std::fs::write(&file, &output).expect("write show's output");
        let compiled = ClusterDocument::compile(&file).expect("compile show's output");
        let repeated = show(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");

        assert_eq!(compiled, document);
        assert_eq!(repeated, output);
    }
}
