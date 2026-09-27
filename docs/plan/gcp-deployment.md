# Google Cloud deployment

This plan is Epic 23. It deploys the platform and its first application to Google Cloud for the first time.
Beads epic `wamn-ghx2` holds the work, with one issue for each step of section 5.
The owner reviewed the plan on 2026-09-25, and each step waits for its own start.
The rules of the web host come from [web deployment](web-deployment.md).
The commands of each finished step are in [Google Cloud operations](../operations/gcp.md).

## 1. What exists

The owner created these on 2026-09-25, and they cost almost nothing while no workload runs.

| Item | Value |
| --- | --- |
| Project | `wamn-dev`, display name `wamn`, number `540250462877`, no organization |
| Billing | account `01E392-13CC0D-277806`, linked |
| Enabled APIs | `dns.googleapis.com` and the defaults of a new project |
| DNS | public Cloud DNS zone `wamn-dev` for `wamn.dev`, delegated by the `.dev` registry since 2026-09-25 21:31 EDT |
| DNS records | the Namecheap mail forwarding records, copied: five `eforward` MX records and one SPF TXT record |
| Certificate issuer | Let's Encrypt, contact `dkkloimwieder@gmail.com` |
| Region | `us-central1`, zone `us-central1-a` |

## 2. One subdomain per application

Owner ruling of 2026-09-25: each deployed application first gets its own subdomain, for example `receiving.wamn.dev`, because many applications will be tested.
This replaces decision 3.2 of [web deployment](web-deployment.md) for Google Cloud testing.
It has these consequences:

- Each application publishes its release with `--route-host <app>.wamn.dev`.
- The session cookies have no `Domain`, so each subdomain holds its own session. A person signs in to each application separately.
- One wildcard certificate for `*.wamn.dev` covers every application. Let's Encrypt issues a wildcard certificate only through the DNS challenge, which the Cloud DNS zone supports.
- The load balancer gets one host rule per application, and each host serves the web client path of its own release.
- The edge chart takes one host today. It needs a list of applications, each with a host and a bucket path, before a second application deploys. That change is step 5 below.

## 3. The shape

```mermaid
flowchart LR
  B[browser] -->|https receiving.wamn.dev| LB[global load balancer<br/>URL map, wildcard certificate]
  LB -->|/api/ and /password/| E[edge pods<br/>in GKE]
  LB -->|every other path| BB[backend bucket<br/>Cloud CDN]
  BB --> GCS[(GCS web bucket)]
  E --> R[route ingress and hosts]
  E --> I[identity]
  I --> M[Resend mail]
  R --> PG[(PostgreSQL<br/>CloudNativePG)]
  R --> N[NATS]
  R --> AR[(Artifact Registry)]
```

The kind edge case already tests the edge rules. Only the load balancer, the bucket and the certificate are new.

## 4. Product choices and why

| Need | Choice | Why | Rejected alternative |
| --- | --- | --- | --- |
| Kubernetes | GKE Standard, one zone | The runtime operator and the hosts run as they do in kind, with no Autopilot pod restrictions to test first. One zonal cluster per billing account gets the monthly management credit, which covers its management fee. | Autopilot: its pod rules are unmeasured for the hosts. A regional cluster: three times the nodes, and no credit. |
| Region | `us-central1` (Iowa), zone `us-central1-a` | Owner ruling of 2026-09-25. It is among the cheapest regions for the machines below. | None. |
| Machines | 2 x `e2-standard-2` Spot, or 1 x `e2-standard-4` Spot | This is the only mode. Section 7 gives its rules. The benchmark of section 6 measures the machines. Only a measured need brings back a larger mode. | 3 x `e2-standard-4` on demand: removed until a measurement asks for it. |
| Disks | `pd-standard`, for the boot disks and the volumes | It costs less than half of `pd-balanced`. A test deployment has no disk speed target. | `pd-balanced`, the GKE default: 2.5 times the price. |
| Google Cloud resources | `gcloud` commands in [Google Cloud operations](../operations/gcp.md) | Section 4.1 gives the reasons. | Config Connector. Terraform: not installed here, and a second tool with its own state. |
| PostgreSQL | CloudNativePG in the cluster, PostgreSQL 18, one instance | The repository already owns its manifests and its [backup and recovery runbook](../operations/backup-and-recovery.md). Provisioning creates a database and roles per project environment, as it does in kind. | Cloud SQL: its limited superuser can refuse the role and ownership work that provisioning does, and no test here covers that. |
| Backups | CloudNativePG backups to a GCS bucket | The runbook uses an object store, and GCS serves the S3 API. | None for a test deployment. |
| NATS | In the cluster, from `deploy/infra` | Google Cloud has no managed NATS. | None. |
| Images and components | Artifact Registry in `us-central1` | It stores container images and OCI artifacts. It replaces the kind registry for the host and identity images and for the component and release artifacts. It uses Google Cloud identity, with no password file. | A registry in the cluster: one more stateful workload to back up. |
| Web files | GCS bucket behind a Cloud CDN backend bucket | This is decision 3.1 of web deployment. `wamn web upload` writes to it with a `gs://` sink and Application Default Credentials, the owner's `gcloud auth application-default login` token. No HMAC key exists. | None. |
| Load balancer | Global external Application Load Balancer | Its URL map sends paths to the edge or to the bucket, rewrites paths, and serves Cloud CDN. | The classic load balancer: it has no path template rewrite. |
| Certificate | cert-manager with a Let's Encrypt DNS challenge on the Cloud DNS zone | A wildcard certificate needs the DNS challenge, and cert-manager renews it. It needs a service account that can change records in the zone. | Google-managed certificates: no wildcard without Certificate Manager DNS authorization, and a second renewal system. |
| Mail | Resend, sending from `noreply@wamn.dev` | Identity sends real invitations, so the first account signs up by email. The Resend domain records go into the Cloud DNS zone, and the API key goes into a Kubernetes secret that the owner creates (section 4.2). | Invitations from the database: the kind edge case already tests that path. |

### 4.1 Config Connector is replaced

Owner question of 2026-09-25: measure the CPU of Config Connector on the 2-node pool, or replace it with `gcloud` commands.
This plan replaces it. The edge chart renders no Google Cloud resource, and step 4 removes its Config Connector template.
These are the reasons:

- Config Connector runs a controller in the cluster, and the pool has about 4 CPU and 12 GB for everything.
- Config Connector needs a service account with rights to change most resources of the project. The `gcloud` commands run as the owner, one visible command at a time.
- A cluster deleted before its Config Connector resources leaves them running and billing. The `gcloud` commands have a matching delete list in the operations page.

The endpoint groups stay. GKE creates them from the annotation on the edge Service, without Config Connector.

### 4.2 The mail secret

Owner ruling of 2026-09-25: the owner creates the Resend API key and puts it into the cluster. Step 3 tells the owner the namespace of identity and the moment that it needs the key.
The secret has this name and shape:

| Field | Value |
| --- | --- |
| Kind | `Secret`, type `Opaque` |
| Name | `identity-resend` |
| Namespace | the namespace of the identity release |
| Key | `api-key`, the Resend API key |

The identity values then set `resendSecret: identity-resend` and `resendFrom: noreply@wamn.dev`.
To create it without the key in the shell history, write the key to a file first:

```bash
kubectl create secret generic identity-resend --namespace <identity namespace> \
  --from-file=api-key=<file that holds the key>
```

## 5. Steps

Each step ends with a test of its result. Unless the next step follows on the same day, each step also ends with the pause of section 9.2.
Each step writes its commands into [Google Cloud operations](../operations/gcp.md), so the next deployment repeats them.

1. Cost controls and storage, before anything runs. Enable the APIs. Lower the quotas of section 8.1. Create the guard of section 8.2: the Pub/Sub topic, the guard function, the budget and the daily scale-to-zero job. Test the unlink of billing once for real. Create the Artifact Registry repository and the two buckets (web files and backups). No machine runs.
2. Create the GKE cluster with Workload Identity and no Config Connector. Its one node pool `main` has the rules of section 7. Install cert-manager, the Let's Encrypt `ClusterIssuer` for the DNS challenge, and the `*.wamn.dev` certificate. Make sure that the certificate is ready, then scale `main` to 0. Section 5.1 gives the owner rulings of this step.
3. Add the Resend domain records to the Cloud DNS zone, and make sure that Resend reports the domain as verified. Build and push the host and identity images and the Receiving components to Artifact Registry. Install the runtime operator, NATS and CloudNativePG, then identity and the hosts, with values files for Google Cloud. The owner creates the secret of section 4.2. The identity values name it and the sender `noreply@wamn.dev`. Provision Receiving and publish its release with `--route-host receiving.wamn.dev`.
4. Remove the Config Connector template from the edge chart. Upload the Receiving client with `wamn web upload`. Install the edge chart with the Google Cloud values. Create the bucket read access, the backend bucket, the backend service, the URL map, the certificate, the address and the forwarding rule with `gcloud`. Add the DNS record for `receiving.wamn.dev`. Send an invitation by email, then sign up and sign in from the mail in a browser. Complete a supplier change. Measure a CDN hit on an asset and no CDN on `/api`.
5. Change the edge chart to take a list of applications, each with its host and bucket path. Deploy a second application beside Receiving.

### 5.1 Step 2 rulings

Owner rulings of 2026-09-25:

- The cluster `wamn` runs on the VPC `wamn`, subnet `wamn-us-central1`: primary range `10.10.0.0/24`, secondary ranges `pods` `10.20.0.0/16` and `services` `10.30.0.0/20`. The VPC has no firewall rule of ours. GKE adds its own, and step 4 adds the health check rule. There is no SSH rule. The first attempts on the `default` network failed, and [Google Cloud operations](../operations/gcp.md) records them.
- gcloud cannot name the first pool of a new cluster. The cluster starts with a temporary `default-pool` of 1 Spot node. Step 2 adds `main` and then deletes `default-pool`.
- The cluster uses the regular release channel.
- The nodes run as the service account `wamn-nodes`, with only `roles/logging.logWriter`, `roles/monitoring.metricWriter` and `roles/artifactregistry.reader`. The Compute default account has Editor on the project, so the nodes do not use it.
- GKE sends system logs only. Workload logs and Managed Prometheus are off. The free logging tier of 50 GiB a month is the ceiling, not the target.
- The cert-manager Kubernetes service account gets `roles/dns.admin` on zone `wamn-dev` only, through Workload Identity. There is no Google service account and no key.
- The ClusterIssuer is `letsencrypt`. The Certificate and its secret are `wamn-edge-tls`, in namespace `edge`.
- The issuer first uses the Let's Encrypt staging server, then production, so a mistake does not use the production rate limit of `*.wamn.dev`.

### 5.2 Step 3 procedure

The kind cluster cases are not a deployment to repeat on GKE. They run PostgreSQL, the event NATS, the OCI registry and MinIO as local Docker containers, and they provision and publish through library calls inside the test process.
Step 3 uses the command path of [deployment](../operations/deployment.md) and [repository delivery](../operations/delivery.md) instead, with the standing manifests in `deploy/infra` and `deploy/platform` and the Google Cloud copies in `deploy/gcp`.
The steps ran in this order. Section 5.3 gives the owner rulings, and section 3 of [Google Cloud operations](../operations/gcp.md) gives each command and its time.

1. Scale `main` to 2.
2. Build the host and identity images with `tools/journey-image-cache`, and push them to `us-central1-docker.pkg.dev/wamn-dev/wamn`. The nodes pull them as `wamn-nodes`, with `roles/artifactregistry.reader`.
3. Build the Receiving components with `tools/build-components`, which applies the path remapping of `tools/guest-rustflags`, so the digests match a local build.
4. Install the runtime operator chart `2.10.0` with `deploy/infra/values-wamn.yaml`, and the internal CA of `deploy/infra/wasmcloud-ca-issuer.yaml`.
5. Run the example program `event_broker_files`, and make the Secrets `evt-nats-authorization` and `evt-nats-bootstrap`. Install the event NATS from `deploy/gcp/nats-jetstream.yaml` with one replica, and run its tap-stream Job. Make the three user Secrets in `platform` and the two host Secrets in `hosts`.
6. Install CloudNativePG from `deploy/infra/cnpg-operator.yaml` and the cluster `wamn-pg` with one instance on the storage class `standard`. Its `initdb` makes the database `wamn_system` with the owner `wamn_system`.
7. Run `wamn-ctl provision-system`, which installs the control store and sets `registry.meta.platform_domain`.
8. Run `provision-org` and `provision-project-env` through a port-forward to PostgreSQL. Apply the role SQL, the `Database`, the privilege SQL and the Secret. Run the NATS program again for org `dkk`, restart the event NATS and run the tap-stream Job again.
9. Run `provision-identity-issuer`, `reconcile-run-plane`, `apply-package`, `reconcile-package-data-access` and `push-component --declaration-template`.
10. Apply the identity certificates and the operator CA, copy the ConfigMap `identity-ca` into `hosts` and `edge`, and apply the session target. The owner creates the Resend Secret of section 4.2. Install identity and make sure that it answers from `hosts`.
11. Prepare generation `a` of the five host credentials. Create the Google service account `wamn-registry-reader` and its bindings, and install the registry token CronJob with a first run by hand.
12. Publish and activate the session key.
13. Mint the management-author PAT through the temporary `/etc/hosts` line and a port-forward to identity.
14. Run `publish-release` with `--route-host receiving.wamn.dev`, `push-release-manifest` and `print-release-env`. Write the host values with the example program `host_values_files`, and install the host with one replica at a request of 0.5 CPU. The host crash-looped until step 15 made the source stream.
15. Run `enable-cdc-project-env`, and apply its role SQL, CDC SQL and Secret. Prepare generation `a` of the registry-reader credential. Build and push the CDC reader image, deploy `deploy/gcp/cdc-reader.yaml`, and restart the host.
16. Make sure that the host serves the release through a port-forward. Scale `main` to 0, unless step 4 follows on the same day.

On 2026-09-27 the daily guard scaled `main` to 0 during step 3, so step 1 ran again. Two steps are temporary: the `/etc/hosts` line of step 13 (finding `wamn-n5d1`), and the `jq` edits of the namespace and the database host in emitted Secrets (findings `wamn-6b4g` and `wamn-lczu`).

### 5.3 Step 3 rulings

Owner rulings of 2026-09-26:

- Publish with `publish-release` directly. The first cloud deployment is not a release qualification, and the qualified path repeats the kind cases that already ran.
- No service account key. The host pods run as a Kubernetes service account that Workload Identity binds to a Google service account with `roles/artifactregistry.reader`, and there is no docker config Secret. If the component pull cannot use Workload Identity today, that is a finding, and a CronJob refreshes a short-lived token. A key is never the answer.
- One CloudNativePG cluster with one instance holds the system database and the tenant databases as separate databases. Two clusters are the production shape, not the test shape.
- Namespaces: `platform` (operator, NATS, CloudNativePG), `identity`, `hosts` and `edge`. Organization `dkk`, the owner's handle, because the org id `wamn` is under the reserved `wamn` prefix. Project `receiving`, tenant `dev`, environment `dev`.
- The platform domain of the principal rows is `wamn.dev`.
- Identity uses the internal CA of the cluster for its TLS. The public `*.wamn.dev` certificate ends at the edge.
- The CDC reader and the materializer run as workloads.
- Tempo and the OpenTelemetry collector are left out. Observability comes with a later epic.
- CloudNativePG backups come later. The operations page states that they are not configured.
- The event NATS users and permissions come from a small example program that calls `event_broker::prepare` of `test-support/infrastructure`. A hand-written file would state again what the program derives. If the program cannot run outside the test crate, that is a finding, and the interim is a file that holds the program output verbatim, with its command in the operations page.
- The host pulls components with a short-lived Artifact Registry token that a CronJob refreshes into the docker config Secret, because the host cannot use Workload Identity for that pull (finding `wamn-i87m`). The CronJob runs as a Kubernetes service account that Workload Identity binds to a Google service account with only `roles/artifactregistry.reader`.
- The example program adds one user after `prepare()`: `tap-admin`. It can publish only `$JS.API.STREAM.INFO.WAMN_TAP`, `$JS.API.STREAM.CREATE.WAMN_TAP`, `$JS.API.STREAM.UPDATE.WAMN_TAP` and `_INBOX_tap-admin.>`, and subscribe only to `_INBOX_tap-admin.>`. The program writes its nats CLI context as `context.json` for the Secret `evt-nats-bootstrap`.
- The program writes only the `authorization` block, as `authorization.conf`, for the Secret `evt-nats-authorization`. It holds the users of `prepare()` and `tap-admin`. The manifest supplies `listen`, `http`, `jetstream` and the `include`.
- The Google Cloud copy of the NATS manifest is `deploy/gcp/nats-jetstream.yaml` in namespace `platform`. It has one replica, no `cluster` block or cluster port, and no anti-affinity spread. It keeps the headless Service and the volume claim. `deploy/infra` does not change.
- The Secret `wamn-event-nats` in `hosts` holds the `runtime` user with the keys of `cluster/deployment.rs`, and `stream_replicas` is `1`. The Secret `wamn-materializer-nats` in `hosts` holds the binding with the server `nats://evt-nats.platform.svc.cluster.local:4222`. The host values and the CDC reader use the same URL.
- The `provisioning` user creates the event streams and consumers in the provisioning step only. Its Secret, and the Secrets of `publisher` and `observer`, stay in `platform` and are never mounted into a host. Their names are `evt-nats-provisioning`, `evt-nats-publisher` and `evt-nats-observer`, with the keys `username` and `password`.
- The CloudNativePG cluster `wamn-pg` bootstraps the database `wamn_system` with the owner `wamn_system`, as `deploy/platform/wamn-sysdb.yaml` does. CloudNativePG holds the owner login in the Secret `wamn-pg-app` and the superuser in `wamn-pg-superuser`.
- A new verb `wamn-ctl provision-system --system-url <superuser URL> --platform-domain wamn.dev` installs the control store from `CONTROL_BOOTSTRAP_SQL` and writes `registry.meta.platform_domain`. It refuses when the schema `registry` exists. It is the first verb of the deployment ordering, and it runs from this machine through a port-forward.
- `deploy/sql/postgres-init.sql` is a test fixture with seed data and is not applied.
- `provision-org` uses `--template trials --pool wamn-pg`, the one template that places an org on the shared cluster.
- `provision-project-env` runs with `--namespace platform --secret-namespace hosts`. The `Database` resource goes to `platform`, the namespace of its `Cluster`. The credential Secret goes to `hosts`, with its `metadata.namespace` set by `jq` until the finding on the one `--namespace` flag is fixed.
- The identity issuer is `https://identity.identity.svc.cluster.local`: release `identity` in namespace `identity`, the form of the kind cases. Its certificate comes from the internal CA and carries that DNS name. The edge sends `/password` there and checks the CA through `identityCaConfigMap`. Identity has no public host.
- `reconcile-run-plane --schema wamn_run` runs after `provision-identity-issuer` and before `apply-package`, because it installs the catalog schema that `apply-package` writes into.
- The event NATS pod restarts now to load the `dkk` users. The operations page records a check that `WAMN_TAP` exists and that the `dkk` provisioning user connects.
- `WAMN_TAP` uses memory storage, so an event NATS restart drops it by design. The tap Job runs again after every restart.
- Every credential Secret that a verb emits gets the host `wamn-pg-rw.platform.svc.cluster.local:5432` by `jq`, because the verbs copy the host of the admin URL, which is the port-forward. A finding asks for the `--database-host` flag that the event-reader verb has.
- `push-component` gets `--declaration-template` with `--tenant`, which renders `publication/components/*.json.in` with `render_declaration_document`, as the dev coordinator does.
- Components go to `us-central1-docker.pkg.dev/wamn-dev/wamn/components` with `--admit-platform-package wamn:node --admit-platform-package wamn:postgres`. The operator push uses a `.dockerconfigjson` with user `oauth2accesstoken` and a `gcloud auth print-access-token` token, in a mode 0600 file of a private temporary directory, deleted after the push. The token never appears on a command line.
- Identity TLS comes from `deploy/gcp/identity-certificate.yaml`: the `Certificate` `identity` from `ClusterIssuer wasmcloud-ca`, DNS name `identity.identity.svc.cluster.local`, Secret `identity-tls`. Its `ca.crt` is copied into the ConfigMap `identity-ca` in `hosts` and in `edge`, for the host's `WAMN_IDENTITY_CA_FILE` and the edge's `identityCaConfigMap`.
- The session target is the `session-role-reader` generation `a` in `identity`, with the host set by `jq`, passed as `sessionTargetSecrets[0]`.
- The operator CA is a separate CA from `deploy/gcp/identity-operator-ca.yaml`: a `SelfSigned` `Issuer`, the CA `Certificate` `identity-operator-ca`, the CA `Issuer` `identity-operator`, and one client `Certificate` `operator-dkk`. Identity uses `operatorCaSecret: identity-operator-ca`, because step 4 sends an invitation. It is not `wasmcloud-ca`, because a host certificate must not act as an operator. The owner reads `tls.crt` and `tls.key` of `operator-dkk` into mode 0600 files when sending the invitation, and never commits them.
- `deploy/gcp/values-identity.yaml` names the issuer, `wamn-identity-db`, `identity-tls`, `identity-operator-ca`, `identity-resend`, `noreply@wamn.dev`, the session target and the identity image by digest, never by tag.
- After the install, a pod in `hosts` makes sure that `GET /.well-known` answers over TLS against `identity-ca`. Then the host installs.
- `publish-release` runs before the host installs, because the host refuses a release digest that does not exist.
- The host Secrets are generation `a` of guest (`wamn-host-db`), `executor-platform`, `identity-reader`, `http-admitter` and `event-materializer`, in `hosts`, with the host set by `jq`.
- `deploy/gcp/values-host.yaml` is the Receiving overlay with namespace `hosts`, 1 replica, requests of 500m CPU and 256Mi, and limits of 4Gi memory and 2 CPU, the whole node. The event NATS URL and the CDC reader host name `platform`.
- The registry token CronJob of `deploy/gcp/registry-token.yaml` runs every 30 minutes as the Kubernetes service account `registry-token` in `hosts`. Workload Identity binds it to the Google service account `wamn-registry-reader`, which has `roles/artifactregistry.reader` on repository `wamn` only. The job reads a token from the metadata server and writes the Secret `wamn-registry-pull` with user `oauth2accesstoken` for `us-central1-docker.pkg.dev`. A Role allows `get`, `create` and `update` on that one Secret name. A first run by `kubectl create job --from` comes before the host starts.
- The CronJob runs `curlimages/curl` pinned by digest, with no `kubectl`. The manifest carries `wamn-registry-pull` as an empty `kubernetes.io/dockerconfigjson` Secret, and the Role allows only `get` and `patch` on that one name. The job reads the token from the metadata server, and patches the Secret with `application/merge-patch+json` through the pod's service account token. It uses `concurrencyPolicy: Forbid`, one kept history entry each way, `restartPolicy: OnFailure`, and requests of 10m CPU and 16Mi. The token never reaches the log, because the script prints only the HTTP status of the patch. After the first run by hand, the Secret data must be non-empty before the host installs.
- `deploy/gcp/values-host.yaml` is the output of an example program over `render_host_values`, with base `values-host-default.yaml`, the Receiving overlay, namespace `hosts` and 1 replica. The program makes the Google Cloud edits: requests of 500m and 256Mi, limits of 2 CPU and 4Gi, the event NATS URL in `platform`, the host image by digest, and the release digest from `print-release-env`.
- The same program adds the session entries of `session_cluster::adjust_host`: `WAMN_SESSION_ISSUER` `https://identity.identity.svc.cluster.local`, `WAMN_SESSION_INSTANCE_SUFFIX` `zf7o454t`, and `WAMN_SESSION_JWKS_CA` `/etc/identity-ca/ca.crt`, with the ConfigMap `identity-ca` mounted at `/etc/identity-ca`.
- The publisher is the management-author service principal, as in kind. It is minted over the real identity name: the owner maps `identity.identity.svc.cluster.local` to `127.0.0.1` in `/etc/hosts`, and a port-forward serves `svc/identity` on port 8443. `provision-project-env` runs again with only the PAT flags and the `operator-dkk` client certificate. The emitted PAT Secret is not applied yet, and its file stays at mode 0600 in the work directory.
- A new verb `push-release-manifest` pushes the release manifest with no qualification commit, because this deployment is not a qualification. The alias of that name leaves `publish-qualified-release`.
- The release is `--effective-release-id 1`, pushed to `us-central1-docker.pkg.dev/wamn-dev/wamn/releases` with the push credential of `push-component`.
- The session key is published and activated with `wamn-identity publish` and `wamn-identity activate <kid>` in the identity pod. The key set at `/.well-known/jwks.json` then shows the key.
- The event streams come from `enable-cdc-project-env --schema receiving --stream-replicas 1 --dup-window-secs 120 --db-host wamn-pg-rw.platform.svc.cluster.local --namespace platform --secret-namespace platform`, with the `evt-nats-provisioning` user through a port-forward and the source stream name of the NATS program. Its role SQL, CDC SQL and Secret are applied in that order, the Secret to `platform`.
- The replication password is `openssl rand -hex 32` in a mode 0600 file, passed as `WAMN_REPLICATION_PASSWORD` on that one command, and the file is deleted after the Secret is applied.
- The CDC reader runs in `platform` from the image `us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-cdc-reader` (Dockerfile target `cdc-reader`), by digest. `deploy/gcp/cdc-reader.yaml` uses the `event-reader` ServiceAccount of `event-reader-rbac.yaml` in `platform`, `automountServiceAccountToken: false`, one replica, the environment that the kind case gives `cdc::start` (`WAMN_CDC_URL`, `WAMN_SYSTEM_URL`, the event NATS URL in `platform` and the `publisher` user), and requests of 50m and 64Mi with no limits. The host restarts after the reader reaches the publication.

Owner rulings of 2026-09-27:

- The CDC reader keeps its default feedback and slot monitor intervals, 5 and 30 seconds. The kind values 1 and 0 are test-speed settings, and the slot monitor stays on in a deployment.
- The reader Deployment uses `strategy: Recreate`, because one replication slot has one reader.

- The `flow-http` and materializer workloads deploy as the kind case deploys them. `tools/build-components all` builds them, and they go to `us-central1-docker.pkg.dev/wamn-dev/wamn/components` with their digests recorded. They render into `hosts` with catalog `default`, environment `hosts`, and project and schema `receiving`. The materializer intervals are deployment values: fetch 1000 ms, sweep 5000 ms. The serve check runs after them, and step 3 then closes.

### 5.4 Step 4 rulings

Owner rulings of 2026-09-27:

- No HMAC key of any kind. `wamn web upload` takes a `gs://` sink, built by `object_store`'s `GoogleCloudStorageBuilder` from the environment, with Application Default Credentials. The owner runs `gcloud auth application-default login` once on this machine. That is a user OAuth token in the gcloud configuration, not a key. The `s3://` sink stays for kind. One unit test makes sure that the sink scheme selects the builder. This replaces the first HMAC ruling of the same day, because `gsutil hmac create` accepts only a service account.
- Before the invitation, from outside the cluster: `index.html` answers `Cache-Control: no-cache`. One hashed asset answers `public, max-age=31536000, immutable`, with a CDN hit on the second request. `/api/location/list` answers 401 with no CDN header. A `/password/...` GET reaches identity, not the bucket. Then the deployment agent sends the invitation to the owner's address with the operator verb and `operator-dkk`, and the owner signs up from the mail.
- The `gcloud` load balancer commands copy the removed Config Connector template exactly: names from the release `wamn-edge`, a health check on port 8443 at `/healthz`, a firewall rule for `35.191.0.0/16` and `130.211.0.0/22`, and the certificate uploaded from `edge/wamn-edge-tls`. The NEG annotation of the edge Service stays behind a `neg` value.
- The DNS record for `receiving.wamn.dev` is an A record with TTL 300.
- The load balancer copy of the certificate does not renew with cert-manager. That is a finding, not a procedure. Its fix is a CronJob in `edge`, like the registry token CronJob, that copies the renewed Secret into a new `gcloud compute ssl-certificates` entry and moves the proxy to it.
- Pool `main` stays at 2 nodes until step 4 ends. If the guard scales it to 0 first, scale it back and continue. The guard does not change.
- A new verb `wamn-ctl invite --principal <id>` sends the invitation. It uses the identity client and the operator certificate flags of the PAT mint (`--pat-issuer`, `--pat-client-cert`, `--pat-client-key`, `--pat-server-ca`), posts `/invitations`, and prints identity's reply without the token. One unit test covers the request shape, and the enrollment section of `docs/operations/deployment.md` names the verb.
- The owner's principal comes from `create-human --subject dkkloimwieder@gmail.com --display-name dkk`, then `grant-project-env-membership` for `dkk/receiving/dev`, then `reconcile-run-plane`. The invitation goes to that address.
- Order: the upload, the `index.html` and asset measurements, then the three verbs and the invitation through the port-forward.
- Sign-in refusal (2026-09-27): the owner signed up, `/password/login` passes, and `/password/environments` answers 401. Find the cause from the cluster side, in this order: the identity log at the refusal; whether identity connects with the session target `sessionTargetSecrets[0]`, whose URL must name `wamn-pg-rw.platform.svc.cluster.local:5432` and the Receiving database; the `Set-Cookie` attributes and the CSRF cookie of a wrong-password `POST /password/login` from outside, compared with the kind edge case (`Domain`, `Path`, `Secure`, a missing CSRF cookie); and the edge log, whether `Cookie` and `x-wamn-csrf` reach the edge and go on to identity. Fix configuration. File code defects with the log line quoted. Then the owner signs in again.
- Mail: Gmail shows "via amazonses.com", because the message has no DKIM signature for `wamn.dev` and its return path is on SES. The `wamn.dev` zone gets the DKIM record `resend._domainkey`, the custom return path `send.wamn.dev` (one MX and one TXT) from the values of the Resend domain page that the owner pastes, and `_dmarc.wamn.dev TXT "v=DMARC1; p=none;"`. Wait until Resend shows DKIM verified, and record the three records on the operations page.
- One finding, outside this epic: the invitation mail shows a raw code to paste and mentions a PAT. A web sign-up mail carries one link with the code in it.


## 6. Benchmark

A separate issue of the epic builds `tools/bench`, a benchmark tool that runs from any machine over HTTPS against a deployed host.
It drives the Receiving route interface: a read, a write, and a list at the seed of 1000 rows.
Each run has a stated concurrency and duration.
Each run writes one JSON file with p50, p95 and p99 latency, throughput and error rate, tagged with the tier.

| Tier | Nodes |
| --- | --- |
| 1 | 1 x `e2-standard-2` Spot |
| 2 | 2 x `e2-standard-2` Spot |
| 3 | 1 x `e2-standard-4` Spot |
| 4 | 2 x `e2-standard-4` on demand |

Each tier gets one scale-up, one run and one scale-down, and its result file goes under `tests/bench/`.
The node choice of section 7 follows the results.
Tier 4 uses all 8 CPU of the quota, so no other machine runs at the same time.

## 7. The only mode

A test deployment runs on little, for a short time. There is no other mode until a measured run needs one.

- The node pool `main` has 2 x `e2-standard-2` Spot machines, or 1 x `e2-standard-4` Spot. Google can stop a Spot machine at any time, with a notice of 30 seconds, and a test then repeats.
- The pool has no autoscaler, because an autoscaler starts nodes again for pending pods after a scale to 0. Its size is at most 2, and you set it by hand.
- The pool upgrades with a surge of 0 and one unavailable node, so an upgrade does not pass the CPU quota.
- The boot disks are `pd-standard`, 30 GB, not the default 100 GB `pd-balanced`.
- The volumes of PostgreSQL and NATS use the GKE storage class `standard`, which is `pd-standard`. The default class `standard-rwo` is `pd-balanced`, and the SSD quota of section 8.1 refuses it.
- One host replica runs with a request of 0.5 CPU, not two replicas with 2 CPU. The Google Cloud values file sets it. The kind values stay as they are.
- NATS runs with one replica and stream replicas 1. The environment configuration declares the stream replicas, so it states 1.
- Scale the node pool to 0 after each session. The cluster and its disks stay, and the next session scales it up again in a few minutes.
- If the next session is more than a day away, delete the load balancer with the commands of the operations page.

### 7.1 One PostgreSQL instance on Spot machines

CloudNativePG runs one instance, and its node can stop at any time.
This is the result:

- When Google stops the node, PostgreSQL stops without a clean shutdown. It recovers from its write-ahead log at the next start, as after a power loss. A committed transaction survives.
- The volume is a zonal disk. If the node stops, the disk stays. The pod starts again on a node in the same zone, and the database is down until then.
- With 2 nodes, the other node can take the pod. With 1 node, the database is down until Google gives the pool a machine again.
- Nothing fails over. The platform returns errors while the database is down, and a test run in that time fails.
- The backups in the GCS bucket are the only copy outside the disk. If the disk is lost, the recovery of the backup runbook restores the last archived state.

This is acceptable for test data. It is not acceptable for data that a person needs to keep.

## 8. Cost controls

Each control acts on the project `wamn-dev` only. No control acts on the billing account or on a list of projects.
Billing data reaches the budget hours late. The quotas are the hard cap, and the guard is the backstop.

### 8.1 Quotas, the hard cap

Step 1 lowers these Compute Engine quotas of `wamn-dev` through the Cloud Quotas API. Google refuses a request that passes a quota, so no bill can grow past them.

| Quota | Scope | Value | Result |
| --- | --- | --- | --- |
| CPUs, all regions | project | 8 | At most 2 x `e2-standard-4`, in any region. |
| CPUs | `us-central1` | 8 | The same, in the region. |
| Global static IP addresses | project | 1 | One load balancer address. |
| Standard disk | `us-central1` | 200 GB | Every boot disk and volume together. |
| SSD disk | `us-central1` | 0 GB | No disk escapes the 200 GB cap through `pd-balanced` or `pd-ssd`. |

Owner ruling of 2026-09-25: the SSD quota stays at 0, and every boot disk and volume is `pd-standard` for now. Only a measured run that needs SSD raises the SSD quota.
The preemptible CPU quota of `us-central1` is 0 by design. With 0, Spot machines count against the standard CPU quota, so the CPU quota of 8 is the one regional limit for every machine. Owner ruling of 2026-09-26: do not raise it. Once a region has preemptible quota, Spot machines count only against that quota, and the region then allows 8 standard CPUs and the Spot CPUs on top. Source: [Preemptible VM instances](https://docs.cloud.google.com/compute/docs/instances/preemptible).
The disk quotas are regional. The CPU quota of all regions stops a machine in any other region, so a disk elsewhere has no machine to use it.

### 8.2 The guard

If the budget runs out, the guard stops the machines and then billing. It has four parts, all in `wamn-dev`:

- A Pub/Sub topic `wamn-guard`.
- One budget of 150 USD a month on billing account `01E392-13CC0D-277806`, filtered to `wamn-dev`. Section 8.3 gives the amount. Owner ruling of 2026-09-25: it counts cost before credits, so the guard acts on gross spend. At 50, 90 and 100 percent it mails the billing administrators, and it publishes every update to `wamn-guard`.
- A Cloud Run function `wamn-guard` on the topic, with its own service account. It reads the project id `wamn-dev`, the zone and the cluster name `wamn` from its `config.json`, and a message cannot change them. From 50 percent of the budget, it sets every node pool of cluster `wamn` to 0 nodes. From 100 percent, it unlinks the billing account from `wamn-dev`, as section 9.5 does by hand.
- A Cloud Scheduler job that publishes a scale-to-zero message to `wamn-guard` once a day, at 03:00 America/New_York. A session that you forget stops by the next morning.

Owner ruling of 2026-09-25: the guard stops every node pool of cluster `wamn`, benchmark pools included, because a guard that spares a pool is not a guard.
The source of the function is Python in `deploy/gcp/guard`. Its test feeds it a fake budget message for each threshold.
Step 1 also tests the 100 percent action for real, once, before any workload exists. It publishes a fake 100 percent message, sees billing unlink, links billing again at once, and makes sure that the project serves again.

The service account of the function has two grants, both on the project `wamn-dev`. A custom role allows it to read the cluster and set a node pool size. Project Billing Manager allows it to unlink billing from this project. It has no grant on the billing account.

### 8.3 The budget amount

Owner ruling of 2026-09-25: the budget is 150 USD, and it counts cost before credits. Do not lower it to 50 USD without this arithmetic.

- The GKE management fee is 0.10 USD an hour for each hour that the cluster exists, also at 0 nodes. A month has about 720 hours, so the fee is about 72 USD.
- The zonal credit pays the fee back on the invoice. The budget counts cost before credits, so it sees the full 72 USD.
- With a budget of 50 USD, the fee alone reaches 50 percent in about 10 days and 100 percent in about 21 days. The guard then unlinks billing, although no machine ran.
- With 150 USD, the fee leaves 150 - 72 = 78 USD of real spend. At about 0.11 USD an hour for 2 x `e2-standard-2` Spot and the load balancer, that is about 700 hours.
- The 50 percent line is 75 USD. A cluster that stands all month reaches it only near the end, with little other spend, and the guard then stops the pools, which costs nothing.

## 9. Shutdown

Use the level that fits. Each level lists its commands and a test of the result.
Run every command against `--project wamn-dev`.

### 9.1 The guard, before anything runs

Step 1 creates the guard of section 8.2. It stops the machines at 50 percent of the budget, and it unlinks billing at 100 percent. Section 8.3 gives the amount.
The operations page lists its commands.

### 9.2 Pause: stop the machines, keep everything else

The cluster, its disks and the load balancer stay. Only the machines stop.

```bash
gcloud container clusters resize wamn --project wamn-dev --zone us-central1-a \
  --node-pool main --num-nodes 0
```

Make sure that no machine runs:

```bash
gcloud compute instances list --project wamn-dev
```

To resume, run the same resize with the node count of section 7.

### 9.3 Stop the load balancer

Delete the load balancer parts with the delete list of the operations page, in its order.
Then make sure that both lists are empty:

```bash
gcloud compute forwarding-rules list --project wamn-dev --global
gcloud compute addresses list --project wamn-dev --global
```

### 9.4 Delete the cluster

```bash
gcloud container clusters delete wamn --project wamn-dev --zone us-central1-a
```

The disks of the PostgreSQL and NATS volumes can outlive the cluster. List them.
When their data is no longer needed, delete them:

```bash
gcloud compute disks list --project wamn-dev
gcloud compute disks delete <disk> --project wamn-dev --zone us-central1-a
```

The endpoint groups of the edge can also outlive the cluster. Then make sure that no load balancer part remains. Delete each part that remains:

```bash
gcloud compute forwarding-rules list --project wamn-dev --global
gcloud compute target-https-proxies list --project wamn-dev
gcloud compute url-maps list --project wamn-dev
gcloud compute backend-services list --project wamn-dev --global
gcloud compute backend-buckets list --project wamn-dev
gcloud compute health-checks list --project wamn-dev --global
gcloud compute ssl-certificates list --project wamn-dev --global
gcloud compute addresses list --project wamn-dev --global
gcloud compute network-endpoint-groups list --project wamn-dev
```

### 9.5 Stop all billing at once

If costs must stop now, unlink billing.
Google then stops the billable services of the project, and it can delete their resources.
The project, its DNS zone and its configuration stay.

```bash
gcloud billing projects unlink wamn-dev
```

To end everything, delete the project. It can be restored for 30 days.

```bash
gcloud projects delete wamn-dev
```

The DNS zone goes with the project. Before you delete it, set the Namecheap name servers back to Namecheap, or `wamn.dev` stops resolving, and mail forwarding stops with it.

## 10. Cost

Step 1 read these list prices in `us-central1` from the Cloud Billing catalog on 2026-09-25. The GKE management fee and the zonal credit come from the GKE price page.
A machine price is the sum of its CPU price and its memory price.

| Item | Price | 2 x `e2-standard-2` Spot |
| --- | --- | --- |
| Nodes | `e2-standard-2`: 0.040 USD an hour Spot, 0.067 on demand. `e2-standard-4`: 0.080 USD an hour Spot, 0.134 on demand. | 0.080 USD an hour |
| Node external addresses | 0.0025 USD an hour on a Spot machine, 0.005 on demand | 0.005 USD an hour |
| Cluster management | 0.10 USD an hour, covered by the zonal credit | 0 |
| Load balancer | 0.025 USD an hour for the first forwarding rule, and a small charge per GB | 0.025 USD an hour, only while it exists |
| Disks | `pd-standard` 0.04 USD per GB each month, after 30 free GB. `pd-balanced` is 0.10 USD. | 2 x 30 GB boot and about 20 GB of volumes: about 2 USD a month. The disks stay after the nodes scale to 0. |
| Cloud DNS | 0.20 USD a month for the zone | the same |
| GCS, Artifact Registry, CDN egress, the guard | cents for a test | the same |
| Resend | free up to its monthly limit | 0 |
| Total while running | | about 0.11 USD an hour |

Two test hours a day on five days cost about 1.10 USD a week.
Tier 4 of the benchmark, 2 x `e2-standard-4` on demand, costs about 0.30 USD an hour.
While nothing runs, the disks, the buckets, the registry and the DNS zone cost a few USD a month.

## 11. Out

- A second environment, for example staging and production.
- A regional cluster or more than one PostgreSQL instance.
- A mode with larger or on-demand machines, until the benchmark measures a need.
- CI that deploys.
