# Google Cloud

GCP deployment support is unfinished. This page preserves external cloud prerequisites and operations for project `wamn-dev`.
See [the deployment boundary](#4-platform-deployment-boundary) before deployment.

Set these variables before each command on this page.

```bash
export CLOUDSDK_CONFIG=$HOME/.config/gcloud-wamn-agent
export KUBECONFIG=$HOME/.kube/wamn-dev
```

## Cloud credentials and prerequisites

The operator maintains the gcloud login in `CLOUDSDK_CONFIG`.
Keep these variables set for `gcloud` and `kubectl` reads and writes.
Keep the operator's default gcloud and Kubernetes files separate from the agent files above.
The owner runs billing commands and enables APIs.

The GCP provider runs `gcloud container clusters get-credentials` with the cluster document's project, zone, and cluster name.
It writes credentials to the caller's kubeconfig file and selects the matching GKE context.
Install `gke-gcloud-auth-plugin` on the deployment host so kubectl can use the gcloud login.

## 1. Owner inputs

The owner supplies these external cloud inputs.

1. The agent identity. `CLOUDSDK_CONFIG` holds the login of `wamn-agent@wamn-dev.iam.gserviceaccount.com` and application default credentials, which `wamn web upload` reads. `KUBECONFIG` names a file of the agent's own. Cluster access must use that dedicated file.
2. The SSD quota. The quota preference `wamn-ssd-us-central1` sets `SSD_TOTAL_GB` in `us-central1` to 200, not 0. The PostgreSQL volume is `premium-rwo` (pd-balanced), which counts against it. A teardown keeps the preference, because it is a project quota. If the preference is missing, the owner sets it again:

   ```bash
   gcloud beta quotas preferences create wamn-ssd-us-central1 --project wamn-dev \
     --service compute.googleapis.com --quota-id SSD-TOTAL-GB-per-project-region \
     --dimensions region=us-central1 --preferred-value 200
   ```

3. Read access for the agent on the Secret Manager secret `identity-resend`, which holds the Resend key. The secret and the Secret Manager API are project resources, so a teardown keeps them, but deployment integration that writes the cluster Secret remains unfinished. The owner enables the API, makes the secret once and grants the agent read access on that one secret. Write the key to a mode 0600 file. Identity refuses whitespace in the key (`Resend configuration refused`), so the third command removes all whitespace:

   ```bash
   gcloud services enable secretmanager.googleapis.com --project wamn-dev
   gcloud secrets create identity-resend --project wamn-dev --replication-policy automatic
   tr -d '[:space:]' < <file> | gcloud secrets versions add identity-resend --project wamn-dev --data-file=-
   gcloud secrets add-iam-policy-binding identity-resend --project wamn-dev \
     --member serviceAccount:wamn-agent@wamn-dev.iam.gserviceaccount.com \
     --role roles/secretmanager.secretAccessor
   ```

4. Workload Identity grants for the rendered worker accounts.
   Workload Identity authenticates Kubernetes workloads to Google Cloud without service-account keys.
   Grant access to each account below. Do not grant access to an entire namespace. Remove former namespace grants from existing deployments.
   The control chart renders `hosts/wamn-control-<org>`.
   Environment provisioning creates `hosts/wamn-host-<org>--<env>--<project>` for application and shared hosts.
   Control operation Jobs run as `platform/<coordinate>-code` or `platform/<coordinate>-sql`.
   The coordinate is the environment ConfigMap name in `hosts`.
   `operations.rs` creates both operation accounts from that same coordinate.

   Set these values for one configured target before you grant access:

   ```bash
   NUMBER=$(gcloud projects describe wamn-dev --format 'value(projectNumber)')
   ORG=<organization>
   PROJECT=<project>
   ENV=<environment>
   COORDINATE=<environment-configmap-name>
   POOL="principal://iam.googleapis.com/projects/$NUMBER/locations/global/workloadIdentityPools/wamn-dev.svc.id.goog/subject/ns"
   ```

   Grant registry reads to the accounts that read OCI artifacts:

   ```bash
   for ACCOUNT in "hosts/wamn-control-$ORG" "hosts/wamn-host-$ORG--$PROJECT--$ENV" \
     "platform/$COORDINATE-code" "platform/$COORDINATE-sql"
   do
     gcloud artifacts repositories add-iam-policy-binding wamn --project wamn-dev --location us-central1 \
       --member "$POOL/${ACCOUNT%/*}/sa/${ACCOUNT#*/}" --role roles/artifactregistry.reader
   done
   ```

   Grant labels access only to the environment host that uses the labels connection:

   ```bash
   gcloud storage buckets add-iam-policy-binding gs://wamn-dev-labels \
     --member "$POOL/hosts/sa/wamn-host-$ORG--$PROJECT--$ENV" --role roles/storage.objectAdmin
   ```

   For another target, use its configured repositories and buckets.
   These direct grants need no Google service account or `iam.gke.io` annotation.
   A teardown keeps the grants because IAM owns them.
   The project number of `wamn-dev` is 540250462877.
   [GKE workload authentication](https://docs.cloud.google.com/kubernetes-engine/docs/how-to/workload-identity) describes the direct principal format.

5. The telemetry bucket and its writer. The bucket `wamn-dev-otel` deletes an object 30 days after it lands. The Google service account `wamn-otel` writes to it and reads its metadata, because the collector exporter reads the bucket at start (`storage.buckets.get`). The Kubernetes service account `platform/otel-collector` acts as `wamn-otel`. A teardown keeps all three, because they are a bucket and IAM:

   ```bash
   echo '{"rule":[{"action":{"type":"Delete"},"condition":{"age":30}}]}' > /tmp/otel-lifecycle.json
   gcloud storage buckets create gs://wamn-dev-otel --project wamn-dev --location us-central1 \
     --uniform-bucket-level-access --lifecycle-file=/tmp/otel-lifecycle.json
   gcloud iam service-accounts create wamn-otel --project wamn-dev
   for role in roles/storage.objectCreator roles/storage.legacyBucketReader
   do
     gcloud storage buckets add-iam-policy-binding gs://wamn-dev-otel \
       --member serviceAccount:wamn-otel@wamn-dev.iam.gserviceaccount.com --role $role
   done
   gcloud iam service-accounts add-iam-policy-binding wamn-otel@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
     --role roles/iam.workloadIdentityUser --member "serviceAccount:wamn-dev.svc.id.goog[platform/otel-collector]"
   ```

6. The approval of the teardown of section 3. The teardown deletes every database.

7. The OpenTofu state bucket `gs://wamn-dev-tofu-state` in `us-central1`. State records the resources that OpenTofu manages. The bucket uses uniform bucket-level access and object versioning. Uniform bucket-level access applies permissions to the whole bucket. Object versioning keeps earlier versions of the state. A teardown keeps it, because it is a bucket.

   If the bucket is missing, the owner creates it:

   ```bash
   gcloud storage buckets create gs://wamn-dev-tofu-state --project wamn-dev --location us-central1 \
     --uniform-bucket-level-access
   ```

   Enable object versioning on the bucket:

   ```bash
   gcloud storage buckets update gs://wamn-dev-tofu-state --project wamn-dev --versioning
   ```

8. Read access to `buckets.web` for the Rust edge.
    The edge chart renders the Kubernetes service account `hosts/wamn-edge`.
    Workload Identity authenticates Kubernetes workloads to Google Cloud.
    Grant that account `roles/storage.objectViewer` on the web bucket through Workload Identity.
    If the grant is missing, the owner runs these commands.

    ```bash
    NUMBER=$(gcloud projects describe wamn-dev --format 'value(projectNumber)')
    gcloud storage buckets add-iam-policy-binding gs://wamn-dev-web \
      --member "principal://iam.googleapis.com/projects/$NUMBER/locations/global/workloadIdentityPools/wamn-dev.svc.id.goog/subject/ns/hosts/sa/wamn-edge" \
      --role roles/storage.objectViewer
    ```

    For another project, use its project number, `project_id`, and `buckets.web`.
    The direct grant uses the Kubernetes service account identity. [GKE workload authentication](https://docs.cloud.google.com/kubernetes-engine/docs/how-to/workload-identity)

14. The edge Service `hosts/wamn-edge` creates the network endpoint group (NEG), which lists the load balancer's backend endpoints.
    Its annotation names the group `wamn-edge`.
    OpenTofu reads that group in the cluster's `zone` from `deploy/gcp/project.k`.
    DNS for every route host must point to the global address that OpenTofu creates.
    The owner supplies a firewall rule for TCP port 8080 from `35.191.0.0/16` and `130.211.0.0/22` to the cluster nodes.
    Platform deployment creates the Service before OpenTofu plans the load balancer.
    The edge mounts the Secret `hosts/wamn-edge-tls`, which cert-manager creates from `deploy/gcp/letsencrypt.yaml`.
    The issuer uses Cloud DNS zone `wamn-dev` in project `wamn-dev`.
    The owner grants `roles/dns.admin` on that zone to the manifest's service account `cert-manager/cert-manager`.
    The load balancer uses its own Google-managed certificate from Certificate Manager.

15. Read and create access to `buckets.web` for web uploads.
    The uploader reads the completion index and existing objects before it writes.
    The agent runs `wamn web upload`. Control operation Jobs do not upload web artifacts.
    Grant the agent both roles:

    ```bash
    for ROLE in roles/storage.objectViewer roles/storage.objectCreator
    do
      gcloud storage buckets add-iam-policy-binding gs://wamn-dev-web \
        --member serviceAccount:wamn-agent@wamn-dev.iam.gserviceaccount.com --role "$ROLE"
    done
    ```

Cloud artifact production uses these owner inputs and never creates their grants or budget. If an input is missing, the cloud operation fails.

## 2. Project resources

The teardown keeps these resources for later cloud integration. The owner makes them once, with the commands below:

- The APIs, the quotas and the daily guard that sets every pool to 0 nodes at 03:00 New York time (07:00 UTC).
- The project custom role `wamnAgentTofuRead`, bound to `wamn-agent@wamn-dev` with no condition. It holds only `dns.managedZones.get` and `storage.buckets.getIamPolicy`, so that the agent's `tofu plan` can read the DNS zone and the IAM policy of the state bucket. It grants no write.
- The direct `roles/storage.objectViewer` grant on `gs://wamn-dev-web` for `hosts/wamn-edge` (section 1, item 13).
- The cost guard `wamn-guard` acts once when a budget message crosses a threshold. At 50 percent it only logs, at 90 percent it sets every pool to 0 nodes, and at 100 percent it unlinks billing. It keeps the last threshold of the month in `gs://wamn-dev-tofu-state/guard/state.json`. The first message of a new month resets it.
- The Artifact Registry repository configured by the target holds platform images and product artifacts.
  The layout is `<registry>/wamn/{packages,components,releases}`.
  In `wamn-dev`, the registry is `us-central1-docker.pkg.dev/wamn-dev/wamn`.
  The separate `wamn-kind` repository expires tags after seven days for local test artifacts.
  Both repositories use immutable tags.
  The project document declares this repository. [Project document](../../deploy/gcp/project.k)
- The buckets `wamn-dev-web`, `wamn-dev-backups` and `wamn-dev-labels`.
- The VPC `wamn`, its subnet `wamn-us-central1` and the node service account `wamn-nodes`.
- The Cloud DNS zone `wamn-dev`, its records, and the `roles/dns.admin` binding of the cert-manager principal on the zone. `project.k` declares the zone. OpenTofu does not own the records or the binding.
- The direct worker grants on `wamn`, `gs://wamn-dev-labels` and `gs://wamn-dev-web` (section 1, items 4 and 15).
- The firewall rule that permits ingress traffic to the Rust web front on TCP port 8080. The project document declares the load balancer objects.

Run every command as the project owner, with `--project wamn-dev`. The default project of the gcloud configuration can be another project, so a command without `--project` acts on the wrong one.

Enable the APIs:

```bash
gcloud services enable --project wamn-dev \
  compute.googleapis.com container.googleapis.com storage.googleapis.com \
  artifactregistry.googleapis.com iam.googleapis.com cloudquotas.googleapis.com \
  billingbudgets.googleapis.com cloudbilling.googleapis.com pubsub.googleapis.com \
  cloudfunctions.googleapis.com run.googleapis.com cloudbuild.googleapis.com \
  eventarc.googleapis.com cloudscheduler.googleapis.com logging.googleapis.com \
  certificatemanager.googleapis.com
```

Set the quota caps. Google refuses a decrease of more than 10 percent without `--allow-high-percentage-quota-decrease`. Owner input 2 sets the SSD quota:

```bash
lower() {
  gcloud beta quotas preferences create --project=wamn-dev \
    --service=compute.googleapis.com --quota-id="$1" --preferred-value="$2" \
    --preference-id="$3" $4 --justification="wamn-dev test deployment cost cap" \
    --email=<owner email> --allow-high-percentage-quota-decrease
}
lower CPUS-ALL-REGIONS-per-project 8 wamn-cpus-all-regions ""
lower CPUS-per-project-region 8 wamn-cpus-us-central1 --dimensions=region=us-central1
lower STATIC-ADDRESSES-per-project 1 wamn-global-static-addresses ""
lower DISKS-TOTAL-GB-per-project-region 200 wamn-disks-us-central1 --dimensions=region=us-central1
gcloud beta quotas preferences list --project wamn-dev --format="value(quotaId,quotaConfig.grantedValue)"
```

To change a cap, run `gcloud beta quotas preferences update` with the same preference id.

Make the network and the node service account. `deploy/gcp/project.k` names both.
GKE uses `wamn-nodes` to [pull container images](https://docs.cloud.google.com/kubernetes-engine/docs/concepts/workload-identity), including Job images.
OpenTofu enables the GKE metadata server on each node pool for workload requests inside containers.

```bash
gcloud compute networks create wamn --project wamn-dev --subnet-mode=custom
gcloud compute networks subnets create wamn-us-central1 --project wamn-dev \
  --network wamn --region us-central1 --range 10.10.0.0/24 \
  --secondary-range pods=10.20.0.0/16,services=10.30.0.0/20
gcloud iam service-accounts create wamn-nodes --project wamn-dev --display-name="wamn GKE nodes"
NSA=wamn-nodes@wamn-dev.iam.gserviceaccount.com
for role in roles/logging.logWriter roles/monitoring.metricWriter
do
  gcloud projects add-iam-policy-binding wamn-dev --member=serviceAccount:$NSA --role=$role --condition=None
done
gcloud artifacts repositories add-iam-policy-binding wamn --project wamn-dev --location us-central1 \
  --member serviceAccount:$NSA --role roles/artifactregistry.reader
```

Make the firewall rule for the load balancer health tests (owner input 14):

```bash
gcloud compute firewall-rules create wamn-edge-health-probe --project wamn-dev --network wamn --direction INGRESS \
  --allow tcp:8080 --source-ranges 35.191.0.0/16,130.211.0.0/22
```

Make the registry and the buckets. The labels bucket has no public read:

```bash
gcloud artifacts repositories create wamn --project wamn-dev --location us-central1 \
  --repository-format=docker --description="wamn images and OCI artifacts"
gcloud storage buckets create gs://wamn-dev-web --project wamn-dev \
  --location us-central1 --uniform-bucket-level-access
gcloud storage buckets create gs://wamn-dev-backups --project wamn-dev \
  --location us-central1 --uniform-bucket-level-access
gcloud storage buckets create gs://wamn-dev-labels --project wamn-dev --location us-central1 \
  --uniform-bucket-level-access --public-access-prevention
```

Grant each rendered worker access with the commands in section 1, items 4 and 15.

Make the read-only role of the agent's OpenTofu plan, and bind it to `wamn-agent`:

```bash
gcloud iam roles create wamnAgentTofuRead --project wamn-dev --title="wamn agent tofu read" \
  --permissions=dns.managedZones.get,storage.buckets.getIamPolicy --stage=GA
gcloud projects add-iam-policy-binding wamn-dev --member=serviceAccount:wamn-agent@wamn-dev.iam.gserviceaccount.com \
  --role=projects/wamn-dev/roles/wamnAgentTofuRead --condition=None
```

Make the cost guard. The source is in `deploy/gcp/guard`, and its `config.json` names the project, the zone, the cluster and the state bucket. Run its test first:

```bash
(cd deploy/gcp/guard && python3 -m unittest)
```

Make the topic, the service account and its two grants:

```bash
gcloud pubsub topics create wamn-guard --project wamn-dev
gcloud iam service-accounts create wamn-guard --project wamn-dev --display-name="wamn cost guard"
gcloud iam roles create wamnGuard --project wamn-dev --title="wamn cost guard" \
  --description="Read cluster wamn and set its node pool sizes" --stage=GA \
  --permissions=container.clusters.get,container.clusters.update,container.operations.get
SA=wamn-guard@wamn-dev.iam.gserviceaccount.com
gcloud projects add-iam-policy-binding wamn-dev --member=serviceAccount:$SA \
  --role=projects/wamn-dev/roles/wamnGuard --condition=None
gcloud projects add-iam-policy-binding wamn-dev --member=serviceAccount:$SA \
  --role=roles/billing.projectManager --condition=None
```

The guard reads and writes `guard/state.json` in the state bucket, so give `$SA` the role `roles/storage.objectUser` on `gs://wamn-dev-tofu-state`, with the condition `guard-state` that admits only objects under `guard/`. `project.k` declares this binding. The state bucket stays out of OpenTofu, because it holds the state of OpenTofu. Deploy the function. The Pub/Sub trigger calls it as the same service account, which needs the invoker role on the function. The grant takes about a minute to apply, and until then the log shows `403` answers:

```bash
gcloud functions deploy wamn-guard --gen2 --project wamn-dev --region us-central1 \
  --runtime python314 --source deploy/gcp/guard --entry-point on_message \
  --trigger-topic wamn-guard --service-account $SA --trigger-service-account $SA --quiet
gcloud run services add-iam-policy-binding wamn-guard --project wamn-dev \
  --region us-central1 --member=serviceAccount:$SA --role=roles/run.invoker
```

Make the budget that sends its threshold messages to the topic, and the daily job. The job sends `{"action":"scale-to-zero"}`:

```bash
gcloud billing budgets create --billing-account=01E392-13CC0D-277806 \
  --billing-project=wamn-dev --display-name="wamn-dev" --budget-amount=150USD \
  --filter-projects=projects/wamn-dev \
  --threshold-rule=percent=0.5 --threshold-rule=percent=0.9 --threshold-rule=percent=1.0 \
  --notifications-rule-pubsub-topic=projects/wamn-dev/topics/wamn-guard \
  --credit-types-treatment=exclude-all-credits
gcloud scheduler jobs create pubsub wamn-guard --project wamn-dev --location us-central1 \
  --schedule="0 3 * * *" --time-zone="America/New_York" --topic=wamn-guard \
  --message-body='{"action":"scale-to-zero"}'
```

Run the daily job once, and read the guard log:

```bash
gcloud scheduler jobs run wamn-guard --project wamn-dev --location us-central1
gcloud logging read 'resource.type="cloud_run_revision" AND resource.labels.service_name="wamn-guard"' \
  --project wamn-dev --freshness=10m --format="value(timestamp,textPayload)"
```

Make sure that they exist:

```bash
gcloud artifacts repositories list --project wamn-dev --location us-central1
gcloud storage buckets list --project wamn-dev --format="value(name)"
gcloud compute networks subnets list --project wamn-dev --network wamn
gcloud compute backend-services list --project wamn-dev
```

## 3. Teardown

The teardown deletes the cluster `wamn` and every object in it, the CloudNativePG databases included. Run it only with the approval of the owner.

First remove the resources that the full OpenTofu apply creates in section 4.
These load balancer and certificate resources are project resources, not cluster resources.
A cluster delete leaves them in place, and the forwarding rule and the reserved address continue to bill.
Use the directory, state bucket, and state prefix from that apply.
Keep the cluster running until this step finishes because OpenTofu reads its network endpoint group.

The rendered `main.tf.json` names the resources below.
The CDN backend is `google_compute_backend_service.edge_https` with `enable_cdn = true`.
OpenTofu orders the deletes from their references.
It removes the forwarding rule before its proxy and address, then the URL map, backend, and health test.
It removes the certificate map entry before the map and certificate, then the DNS authorization.

```bash
cd <existing OpenTofu directory>
tofu init
tofu output -raw edge_dns_authorization_name
tofu output -raw edge_dns_authorization_data
tofu plan -destroy \
  -target=google_compute_global_forwarding_rule.edge \
  -target=google_compute_target_https_proxy.edge \
  -target=google_compute_url_map.edge \
  -target=google_compute_backend_service.edge_https \
  -target=google_compute_health_check.edge \
  -target=google_compute_global_address.edge \
  -target=google_certificate_manager_certificate_map_entry.edge \
  -target=google_certificate_manager_certificate_map.edge \
  -target=google_certificate_manager_certificate.edge \
  -target=google_certificate_manager_dns_authorization.edge \
  -out=teardown.tfplan
```

Make sure that the plan deletes only those resources.
Apply the saved plan after the owner approves it:

```bash
tofu apply teardown.tfplan
```

The DNS owner removes the authorization CNAME printed above and the route records that point to the released `wamn-edge` address.
Keep the DNS zone and the separate cert-manager challenge record.
Keep all section 1 inputs, including buckets, IAM grants, APIs, quota preferences, secrets, and budgets.
The targets also leave the kind registry and the guard state binding intact.
Do not run an untargeted destroy.

Next list the disks. Write down the names of the `pvc-*` disks of the cluster:

```bash
gcloud compute disks list --project wamn-dev --format="table(name,sizeGb,users.basename(),description)"
gcloud container clusters delete wamn --project wamn-dev --zone us-central1-a --quiet
gcloud container clusters list --project wamn-dev
gcloud compute disks list --project wamn-dev --format="table(name,sizeGb,users.basename(),description)"
```

The disks of the volumes of `wamn-pg-1`, `data-wamn-nats-0`, and `otel-collector-queue` stay after the delete.
The collector's [volume claim](../../deploy/platform/installation/values-gcp.yaml) requests `1Gi`.
Delete each one by name, one command for each disk:

```bash
gcloud compute disks delete <pvc disk name> --project wamn-dev --zone us-central1-a --quiet
```

If the network endpoint group `wamn-edge` remains after cluster deletion, delete it by name.
The OpenTofu step already removes the backend service that references it.

```bash
gcloud compute network-endpoint-groups delete wamn-edge --zone us-central1-a --project wamn-dev --quiet
```

Last, make sure that no load balancer resource remains.
Each line must show an empty list:

```bash
for r in "compute forwarding-rules" "compute target-https-proxies" "compute url-maps" \
  "compute backend-services" "compute health-checks" "compute addresses" \
  "compute network-endpoint-groups" "certificate-manager certificates" \
  "certificate-manager maps" "certificate-manager dns-authorizations"; do
  echo "$r: $(gcloud $r list --project wamn-dev --format='value(name)')"
done
```

## 4. Platform deployment boundary

GCP and kind use the same platform artifact set and prepared chart contract.
`cargo xtask build-platform` builds and pins the shared images, including `ctl`, and the role components.
The cluster document selects UI storage. BuildKit runs on the calling machine.
The prepared chart supplies the pinned `ctl` image for control operation Jobs in namespace `platform`.

Use the target's configured registry for artifact production and publication.
Loopback registries use HTTP.
[Building](building.md#platform-artifacts) describes artifact production.

[Deployment](deployment.md#local-platform) describes prepared chart deployment, status, access, and cleanup on kind.
The deployment plan defines the remaining GCP integration work. [Deployment plan](../plan/deploy.md)
Public ingress, web uploads, telemetry, and production policies still need cloud verification.
The operations below use the external cloud credentials described above.

Use the existing infrastructure and platform owners in this order.

1. Render `deploy/gcp/project.k` with `wamn-ctl project render FILE --state-bucket BUCKET --state-prefix PREFIX DIRECTORY`.
   Use the existing state bucket and prefix for this project.
   Initialize OpenTofu in that directory.
   For a fresh cluster, target the node pool resource addresses from `main.tf.json`.
   Their dependencies include the cluster.
   Save the plan and apply it after approval.
   OpenTofu resource targeting includes each target's dependencies. [OpenTofu plan](https://opentofu.org/docs/cli/commands/plan/)
2. Run `wamn platform deploy deploy/gcp/cluster.k --chart <prepared-chart>`.
   The platform owner deploys cert-manager, the edge certificate, and the edge Helm release.
   GKE creates the NEG from the Service annotation.
3. Run a full `tofu plan` in the same directory and apply the approved saved plan.
   OpenTofu reads the Service-owned NEG and creates the HTTPS load balancer, address, CDN, and Google-managed certificate.
   The DNS owner publishes the certificate authorization CNAME from the OpenTofu outputs.
   The DNS owner points each route host at the global address `wamn-edge`.

Keep the cert-manager DNS challenge record and the load balancer's certificate authorization CNAME separate.
The OpenTofu authorization uses a per-project DNS name.
cert-manager uses `_acme-challenge` for the certificate mounted by the edge.

### One approved GKE verification

Get owner approval for one run and the section 3 teardown before you start.
Complete the section 1 prerequisites with the dedicated cloud login and kubeconfig.
Use a fresh disposable platform with no `journey` project.
Prepare the GCP chart and images with [building](building.md#platform-artifacts).
Upload its `platformUi` digest with `wamn web upload --platform-ui` as [web upload](web-upload.md#platform-ui) describes.

Build `wamn` and `wamn-ctl` beside the test executable in `$CARGO_TARGET_DIR/debug`.
Install the pinned KCL compiler, gcloud, `gke-gcloud-auth-plugin`, kubectl, Helm, OpenSSL, and `script`.
Keep Git, Cargo, the `wasm32-wasip2` toolchain, and offline build dependencies available for fixture publication.
Set these inputs:

- `PATH` with the `debug` directory of `$CARGO_TARGET_DIR`.
- `CLOUDSDK_CONFIG` and `KUBECONFIG` from this page.
- `WAMN_TEST_CLUSTER` as the absolute path to `deploy/gcp/cluster.k`.
- `WAMN_RELEASE_CHART` and `WAMN_KCL` as absolute paths to the prepared chart and pinned compiler.
- `WAMN_TEST_OWNER_PASSWORD` as the enrolled owner's password from your secret store.
- `DATABASE_URL` as a separate disposable PostgreSQL 18 server with permission to create and drop build databases.
- `WAMN_FLOW_HTTP_COMPONENT` as the absolute path to the raw pinned `http_route.wasm`.
- `WAMN_REGISTRY_AUTH_FILE` as a private credential file with Artifact Registry write access.
  Its `auths` entry must supply `username` and `password` for `us-central1-docker.pkg.dev`.
  Refresh this publishing credential before the run.

Grant the section 1 worker permissions for organization `dkk`, project `journey`, and environment `dev` before the run.
The environment coordinate is `e-dev-` followed by the first 20 hexadecimal SHA-256 digits of `dkk/journey/dev`.
Use that coordinate for the code and SQL service accounts.
The test creates those accounts through product provisioning and creates no IAM grants.
It derives all artifact repository bases from the cluster document's registry.

Run these steps in order:

1. Follow the OpenTofu order above with the existing directory and state prefix.
   Render the project and apply the approved node pool targets first.
2. Run `wamn platform deploy "$WAMN_TEST_CLUSTER" --chart "$WAMN_RELEASE_CHART"`.
   This creates the edge Service and its network endpoint group.
3. Apply the approved full OpenTofu plan.
   Publish the certificate authorization CNAME and point the route host at the load balancer address.
   Wait for the edge and load balancer certificates to become valid.
4. Enroll the owner through the hosted UI and load its password into `WAMN_TEST_OWNER_PASSWORD`.
   Record disposable consent with `kubectl annotate namespace platform wamn.dev/disposable=true` in the dedicated kubeconfig.
5. Run this exact case:

   ```bash
   cargo test --locked --offline -p wamn-dev-control --test platform_apply_kind \
     control_operations_gke -- --ignored --exact --nocapture
   ```

6. Run section 3 after success or failure.
   Remove the load balancer and certificate resources before the cluster, while its network endpoint group still exists.
   Then delete the cluster, retained PVC disks, and any remaining network endpoint group.

The case has a 90-minute bound.
It reapplies the platform chart and requires readiness.
It reuses `ControlOperations` for authenticated deploy, rollback, and schema apply through the public HTTPS control route.
It makes one public HTTPS GET and requires the UI document with certificate validation enabled.
Application requests use a TLS port-forward with hostnames under the document's control domain.
The GKE body omits the kind-only outsider enrollment from the local mail file.

The case attempts environment deletion and PAT revocation after operation success or failure.
It removes its temporary publishing checkout and local private files.
It leaves the platform running for section 3.
Cluster deletion removes the project row, operation Jobs, and partial resources that product cleanup cannot remove.
Fixture publication adds immutable packages, components, and releases under the configured registry.
Section 3 keeps that registry, the uploaded UI, buckets, IAM grants, and other project inputs.

## 5. Operations

### 5.1 Pool after the daily guard

The guard sets every pool to 0 nodes at 03:00 New York time (07:00 UTC) every day. Run the wake verb to set every pool back to the size that `deploy/gcp/project.k` declares:

```bash
$CARGO_TARGET_DIR/debug/wamn-ctl platform wake deploy/gcp/cluster.k
```

The verb resizes pool `pg` first, then the other pools. It prints one line for each pool. The `Cluster` keeps its old `Ready` condition while its pod is gone, so the verb then waits up to 900 seconds for the pod `wamn-pg-1` to exist and be Ready. Last, it waits up to 600 seconds for the statefulset `wamn-nats` to roll out.

Pod replacement ends existing port-forwards. Restart them for the services that you need.

### 5.2 Pause

Run the sleep verb to set every pool to 0 nodes. The PostgreSQL volume keeps the data:

```bash
$CARGO_TARGET_DIR/debug/wamn-ctl platform sleep deploy/gcp/cluster.k
```

The verb prints one line for each pool, and then the names of the pools that it set to 0. Run section 5.1 to start the cluster again.

### Application operations

Local kind deployment invites the initial owner through identity's local mail file. It sends no external email and deploys no applications.
GCP invitation delivery remains part of unfinished cloud integration.
[Deployment](deployment.md) describes separate application and invitation commands.
The removed installer no longer creates a local `platform.sh` credential file.

### Rotate the Resend key

Write the new Resend key to a private file with mode `0600`.
Add it as a Secret Manager version:

```bash
tr -d '[:space:]' < <file> | gcloud secrets versions add identity-resend --project wamn-dev --data-file=-
```

Secret Manager updates do not update the running identity service.
GCP deployment integration for this step remains unfinished.
Keep the old key active until the replacement serves a successful recovery request.

### 5.5 Delete an environment

`wamn-ctl env delete <org/env/project> [--data]` deletes one environment, so that the next `env apply` of its document makes a new suffix. It uninstalls the release chart, revokes the PATs, deletes the credential Secrets, the streams, the slot, the `Database` and the registry rows. With `--data` it also drops the database and its roles, and asks for the coordinate again at a terminal. Every step is "if present", so run it again after a failure:

```bash
$CARGO_TARGET_DIR/debug/wamn-ctl env delete dkk/dev/receiving
```

### 5.7 CDC slot lost

If a CDC reader is down while the WAL passes `max_slot_wal_keep_size` (4 GB), Postgres invalidates its slot. The reader stays running and stopped, and logs `CDC_CAPTURE_GAP` once. Look at the slots:

```bash
kubectl -n platform exec wamn-pg-1 -c postgres -- psql -U postgres -Atc \
  "select slot_name, active, wal_status, invalidation_reason from pg_replication_slots"
```

`recover-capture-gap` drops the lost slot, makes it again under the same name and writes the open gap row. It connects as `wamn-ctl` with explicit database credentials and the existing `WAMN_NATS_*` inputs:

```bash
$CARGO_TARGET_DIR/debug/wamn-ctl recover-capture-gap --org dkk --project wms --env dev --admin-database-url "$WAMN_PG_ADMIN_URL"
```

After the resync, or on the owner's word while no resync exists, run `close-capture-gap`. The reader resumes within 30 seconds:

```bash
$CARGO_TARGET_DIR/debug/wamn-ctl close-capture-gap --org dkk --project wms --env dev
```

1. The reader stops capture and does not open a session. It stays running and does not exit.
2. It logs one structured event `CDC_CAPTURE_GAP` per process. The event names the slot and the reason (`missing`, or the `invalidation_reason`). Its log line carries the detection time.
3. Every 30 seconds, on the cadence of the slot monitor, it reads the newest `registry.capture_gap` row of its own `registry.event_readers` row. That read logs nothing.
4. Two conditions let it capture: its slot is healthy, and that row has `resync_at` set or no row exists. It then resumes on the slot that the registry names, with no restart. A new slot alone does not start capture again.

The reader has no readiness probe, and this spec adds none. The reader stays running and stopped. The gap shows in the `CDC_CAPTURE_GAP` event and in `pg_replication_slots`, and `recover-capture-gap` writes the row. The reader keeps its `SELECT` credential.

The gap record is one row per gap in `registry.capture_gap`. Its key is the `registry.event_readers` row (`org`, `project`, `env`) and its own `created_at`. That `created_at` carries the time of the recovery verb. Its fields:

| Field | Value |
| --- | --- |
| `slot` | The name of the lost slot. |
| `start_lsn` | The invalidated slot's `confirmed_flush_lsn`. For a missing slot, the LSN in the `Nats-Msg-Id` of the last CDC event on the source stream. A stream with no CDC event gives null, which means "from registration". |
| `start_at` | The `commit_ts` of the last CDC event on the source stream. When the stream has no CDC event, the `created_at` of the `registry.event_readers` row. |
| `reason` | `missing`, or the `invalidation_reason`. |
| `end_lsn` | The first position of the new slot. |
| `resync_at` | Null until `close-capture-gap` sets it. |

The last CDC event is the newest event whose `Nats-Msg-Id` is `<project>_<env>:<lsn>`. Derived events share the source stream with the id `derived:<hash>` and carry no LSN, so the verb skips them.

After issue 1, the confirmed position can sit past the last commit, so `start_at` is an earlier bound than the true gap start. The resync then reads more rows than the gap changed, and misses none.

Two verbs write the row with the control credential:

- `wamn-ctl recover-capture-gap --org --project --env` drops the lost slot and creates it again under the same name, `cdc_object_name(org, project, env, instance)`. The publication and the replication role keep that name and stay untouched. The verb writes the row with its end position. It reads the last CDC event with the observer credential, through the three NATS flags of `enable-cdc-project-env`. It takes `--system-database-url` as `enable-cdc-project-env` does, and `--admin-database-url` for the project database owner. It refuses a healthy slot, an active slot, and a connection to any database other than the project-env database.
- `wamn-ctl close-capture-gap --org --project --env` sets `resync_at` on the newest row. It refuses when no row exists and when the newest row already has `resync_at`. Until the resync exists, the operator runs it on the owner's word.

- A lost slot is not recoverable. A new slot starts at the current WAL position, and the WAL between the old confirmed position and that point is removed. The rows written in that interval produce no events.

- The reader never creates a slot. `enable-cdc-project-env` creates it with `create_failover_slot_sql` (`crates/control/provision/src/sql/cdc.rs:93`).

- No automatic re-slot. No component creates a slot again without an operator.

- On invalidation the reader stops, reports the gap boundaries, and requires a full resync of the materialized set before capture resumes.

- Keeping a slot alive is never an operator's job. The reader keeps its slot moving, and the limit covers only a reader that is down.

The fix has two parts. The fork `dkkloimwieder/pg-walstream` changes only so that `next_event` returns a keepalive as an event with its `wal_end`. The reader applies the rule, because only the reader knows that every publish was acknowledged. With no transaction open and the last returned commit acknowledged, the reader confirms the keepalive's `wal_end`. Up to that position, the reader published every change of this database. The rule "the position advances only past acknowledged events" therefore still holds. One live test: an idle environment's slot follows the writes of another database.

When Postgres stops or moves, as on 2026-09-28 in `wamn-fwzc`, the slot stays on the volume and no WAL is written. The reader's preflight fails to connect, the reader exits, and Kubernetes restarts it with a back-off of up to 5 minutes. When Postgres returns, the reader resumes at `confirmed_flush_lsn`, and no row is lost. The only cost is delay. On 2026-09-28 both slots survived the full disk and the volume resize.

A true gap has one of three causes: a slot limit passed, a slot dropped by hand, or a restore that does not carry the slot. In each case the slot is missing or invalidated. The reader tells the two apart already. A connection failure is a restart, and a missing or invalidated slot is `CAPTURE GAP`. The reader reports the gap and stops capture. A reader that restarts after a Postgres move does not report a gap.

- The gap row (2026-09-29): `start_lsn` and `start_at` come as the gap record states, with a null `start_lsn` only for an empty stream. No `detected_at`: the reader's event carries the detection time, and the row's `created_at` carries the verb's time. The reader stays `SELECT` only. The row is keyed by the `registry.event_readers` row and keeps the lost slot's name as a field. One `CDC_CAPTURE_GAP` per process, then a silent read every 30 seconds.

- The slot name (2026-09-29): the slot keeps `cdc_object_name(org, project, env, instance)`, which the publication and the role share. The row stays keyed by the `registry.event_readers` row and keeps the slot name as a field, because an instance change renames the slot.

- The keepalive confirm is split (2026-09-29). The fork returns a keepalive from `next_event` as an event with its `wal_end`, and nothing more. The reader applies the confirm rule. The new fork rev is pinned in `Cargo.toml`. A reader that raises its flushed position while idle and relies on the library's cap is out.

- The verbs (2026-09-29). `start_lsn` and `start_at` come from the newest CDC event, and a `derived:<hash>` event is skipped. The verb reads the stream with the observer credential, because reading a stream is the observer's job. Provisioning gets no new permission. The flag and variable names follow `enable-cdc-project-env`. An active slot means a reader that still streams, because a reader stopped on a gap holds no session. Each refusal names its reason on one line.

### 5.8 Benchmark

For a local host benchmark, use the [local run](benchmark.md#local-run).

The tier 2 benchmark runs from `wamn-bench-client` in project `wamn-dev`, zone `us-central1-a`.
The Receiving host runs alone on pool `bench`, and PostgreSQL runs alone on pool `pg`.
Every other pod runs on pool `main`.
Make the VM, copy the source of the commit to it, and build `xtask`. `cargo xtask bench-tier` expects the source in `~/wamn`, the xtask command in that checkout and a mode 0700 directory `~/pat`:

```bash
gcloud compute instances create wamn-bench-client --project wamn-dev --zone us-central1-a --machine-type e2-small \
  --image-family debian-13 --image-project debian-cloud --boot-disk-size 30GB --boot-disk-type pd-standard \
  --no-service-account --no-scopes --labels purpose=bench
gcloud compute ssh wamn-bench-client --project wamn-dev --zone us-central1-a --command 'sudo fallocate -l 4G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile && sudo apt-get update && sudo apt-get install -y clang mold build-essential cmake pkg-config curl && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none'
git archive --format=tar.gz -o src.tar.gz HEAD
gcloud compute scp src.tar.gz wamn-bench-client:src.tar.gz --project wamn-dev --zone us-central1-a
gcloud compute ssh wamn-bench-client --project wamn-dev --zone us-central1-a --command 'mkdir -p wamn && tar -xzf src.tar.gz -C wamn && umask 077 && mkdir -p ~/pat'
gcloud compute ssh wamn-bench-client --project wamn-dev --zone us-central1-a --command 'cd wamn && . ~/.cargo/env && cargo build --release -j 2 -p xtask'
```

The VM counts 1 CPU against the quota of 8. `tier.sh` copies the PAT file to `~/pat` itself. After the runs, delete the PAT on the VM, the VM, and the local copy of the PAT:

```bash
gcloud compute ssh wamn-bench-client --project wamn-dev --zone us-central1-a --command 'shred -u ~/pat/* && rmdir ~/pat'
gcloud compute instances delete wamn-bench-client --project wamn-dev --zone us-central1-a --quiet
```

The removed installer no longer prepares the seed database or operator PAT file.
GCP deployment integration for these inputs remains unfinished.
Use the [benchmark page](benchmark.md) for the independent benchmark procedure.

`cargo xtask bench-tier` runs get and update for 60 seconds each at c16, which means 16 concurrent requests.
It writes one result file in its printed temporary directory.
Each call records throughput, p50, p95, p99, calls, errors, and `pg_stat_statements` calls per request.
The script resets database statement counts before each call.
It divides each statement count by the request count, rounds each ratio, and sums the ratios.
This excludes setup queries and background polling from the per-request count.
The [benchmark page](benchmark.md) holds the current baselines.
