# Google Cloud

This page installs the platform and the environments `dkk/receiving/dev` and `dkk/wms/dev` on project `wamn-dev`. It starts at an empty project with no cluster and ends with two qualified releases that serve on `receiving.wamn.dev` and `wms.wamn.dev`. Run the sections in order. The [plan](../history/gcp-deployment.md) gives the reasons and the costs. [Install history](../history/gcp-install-history.md) holds the earlier hand procedure and the records of each run.

Every command on this page runs in a shell with these two variables and nothing else:

```bash
export CLOUDSDK_CONFIG=$HOME/.config/gcloud-wamn-agent
export KUBECONFIG=$HOME/.kube/wamn-dev
```

Do not run `gcloud config configurations activate`. Do not read or write `~/.config/gcloud` or `~/.kube/config`. The billing commands and the API enables are the owner's, and no step on this page runs them.

## 1. Owner inputs

The owner supplies these inputs. The install stops at the first step that reads a missing one.

1. The agent identity. `CLOUDSDK_CONFIG` holds the login of `wamn-agent@wamn-dev.iam.gserviceaccount.com` and application default credentials, which `wamn web upload` reads. `KUBECONFIG` names a file of the agent's own. The `get-credentials` command of section 4.2 writes the cluster into it.
2. The SSD quota. The quota preference `wamn-ssd-us-central1` sets `SSD_TOTAL_GB` in `us-central1` to 200, not 0. The PostgreSQL volume is `premium-rwo` (pd-balanced), which counts against it. A teardown keeps the preference, because it is a project quota. If the preference is missing, the owner sets it again:

   ```bash
   gcloud beta quotas preferences create wamn-ssd-us-central1 --project wamn-dev \
     --service compute.googleapis.com --quota-id SSD-TOTAL-GB-per-project-region \
     --dimensions region=us-central1 --preferred-value 200
   ```

3. The Resend key, as the Secret `identity-resend` in namespace `identity`, key `api-key`. The owner makes it after section 4.4 makes the namespace and before section 4.12. Write the key to a mode 0600 file. Identity refuses whitespace in the key (`Resend configuration refused`). Most editors end a file with a line end, so the command removes all whitespace:

   ```bash
   kubectl -n identity create secret generic identity-resend --from-literal=api-key="$(tr -d '[:space:]' < <file>)"
   ```

4. The Workload Identity binding of the host service account. The Kubernetes service account `hosts/wamn-host` acts as the Google service account `wamn-blob`. A teardown keeps this binding, because it is IAM. If the binding is missing, the owner runs:

   ```bash
   gcloud iam service-accounts add-iam-policy-binding wamn-blob@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
     --role roles/iam.workloadIdentityUser --member "serviceAccount:wamn-dev.svc.id.goog[hosts/wamn-host]"
   ```

5. The telemetry bucket and its writer (`docs/plan/observability.md` R1, R7). The bucket `wamn-dev-otel` deletes an object 30 days after it lands. The Google service account `wamn-otel` writes to it and reads its metadata, because the collector exporter reads the bucket at start (`storage.buckets.get`). The Kubernetes service account `platform/otel-collector` acts as `wamn-otel`. A teardown keeps all three, because they are a bucket and IAM:

   ```bash
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

8. Cloud Build submission permission for `wamn-agent@wamn-dev.iam.gserviceaccount.com` on `wamn-dev`. If the grant is missing, the owner runs:

   ```bash
   gcloud projects add-iam-policy-binding wamn-dev --member=serviceAccount:wamn-agent@wamn-dev.iam.gserviceaccount.com --role=roles/cloudbuild.builds.editor
   ```

9. Artifact Registry writer permission for the default Cloud Build service account on `us-central1-docker.pkg.dev/wamn-dev/wamn`. The command reads the account that Cloud Build uses in `us-central1`. If the grant is missing, the owner runs:

   ```bash
   gcloud artifacts repositories add-iam-policy-binding wamn --project=wamn-dev --location=us-central1 --member="serviceAccount:$(gcloud builds get-default-service-account --project=wamn-dev --region=us-central1)" --role=roles/artifactregistry.writer
   ```

10. A monthly USD 30 budget for Cloud Build in `wamn-dev`, with alerts at 10%, 50%, and 90%. The billing account is the account recorded in [install history](../history/gcp-install-history.md). The [Cloud Build billing service](https://cloud.google.com/skus/sku-groups/deprecate-global-skus) is `8B5D-EF7D-EB12`. If the budget is missing, the owner runs:

    ```bash
    gcloud billing budgets create --billing-account=01E392-13CC0D-277806 --billing-project=wamn-dev --display-name=wamn-dev-cloud-build --budget-amount=30USD --calendar-period=month --filter-projects=projects/wamn-dev --filter-services=services/8B5D-EF7D-EB12 --threshold-rule=percent=0.10 --threshold-rule=percent=0.50 --threshold-rule=percent=0.90
    ```

    The [budget command](https://cloud.google.com/sdk/gcloud/reference/billing/budgets/create) sends alerts to the default billing recipients. The budget does not stop builds. The daily cap in [building](building.md#cloud-platform-builds) controls new submissions.

11. Bucket read access for the agent on the state bucket. `gcloud builds submit` reads the metadata of `gs://wamn-dev-tofu-state` before it stages the source below `build/source/`. If the grant is missing, the owner runs:

    ```bash
    gcloud storage buckets add-iam-policy-binding gs://wamn-dev-tofu-state --member=serviceAccount:wamn-agent@wamn-dev.iam.gserviceaccount.com --role=roles/storage.legacyBucketReader
    ```

12. Log read access for the agent on `wamn-dev`. Cloud Build writes its build logs to Cloud Logging only, and the agent reads them when a cloud build fails. If the grant is missing, the owner runs:

    ```bash
    gcloud projects add-iam-policy-binding wamn-dev --member=serviceAccount:wamn-agent@wamn-dev.iam.gserviceaccount.com --role=roles/logging.viewer
    ```

13. Read access to `buckets.web` for the Rust edge. The Google service account is `wamn-edge@wamn-dev.iam.gserviceaccount.com`.
    The Kubernetes service account `edge/wamn-edge-web` uses this identity.
    The bucket grants `roles/storage.objectViewer` to the Google account.
    The Google account grants `roles/iam.workloadIdentityUser` to `serviceAccount:wamn-dev.svc.id.goog[edge/wamn-edge-web]`.
    If these inputs are absent, the owner creates them:

    ```bash
    gcloud iam service-accounts create wamn-edge --project wamn-dev
    gcloud storage buckets add-iam-policy-binding gs://wamn-dev-web \
      --member serviceAccount:wamn-edge@wamn-dev.iam.gserviceaccount.com --role roles/storage.objectViewer
    gcloud iam service-accounts add-iam-policy-binding wamn-edge@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
      --role roles/iam.workloadIdentityUser --member 'serviceAccount:wamn-dev.svc.id.goog[edge/wamn-edge-web]'
    ```

    For another project, replace `wamn-dev` with `project_id` and the bucket with `buckets.web`.
    The installer creates the Kubernetes account and its annotation, but the owner supplies these IAM grants.

`build-platform` uses these owner inputs and never creates their grants or budget. Missing access causes the cloud operation to fail.

14. The edge NEG zone in `deploy/gcp/project.k`, currently `us-central1-a`, must match the zone reported by step 4.20.
    The fixed NEG name is `wamn-edge`.
    DNS for every route host must point to the global address that OpenTofu creates.
    The owner supplies a firewall rule for TCP port 8443 from `35.191.0.0/16` and `130.211.0.0/22` to the cluster nodes.
    Before step 4.20 creates the NEG, target only the cluster and node pools in the section 4.2 plan and apply.
    After the NEG exists, apply the full rendered project as described in section 4.20.

## 2. Project resources

The teardown keeps these resources, and the install expects them. [Install history](../history/gcp-install-history.md) sections 1, 2.2, 2.3, 4.3, 4.3.1 and 5.1 hold the commands that made them:

- The APIs, the quotas and the daily guard that sets pool `main` to 0 nodes at 03:00 New York time.
- The project custom role `wamnAgentIam`, bound to `wamn-agent@wamn-dev` with a condition. The condition admits the project resource, for `iam.serviceAccounts.create` alone, and the two `wamn-edge` resources: `projects/-/serviceAccounts/101495263876384747666` and `projects/_/buckets/wamn-dev-web`. The agent holds IAM only on the `wamn-edge` resources, never on itself (owner ruling 2026-10-23).
- The Google service account `wamn-edge`, its `roles/storage.objectViewer` binding on `gs://wamn-dev-web`, and the Workload Identity binding of `edge/wamn-edge-web` (section 4.20).
- The cost guard `wamn-guard` acts once when a budget message crosses a threshold. At 50 percent it only logs, at 90 percent it sets every pool to 0 nodes, and at 100 percent it unlinks billing. It keeps the last threshold of the month in `gs://wamn-dev-tofu-state/guard/state.json`. The first message of a new month resets it.
- The Artifact Registry repositories `wamn` and `wamn-kind`, with immutable tags. `wamn` holds the platform images, the packages, the components and the releases.
- The buckets `wamn-dev-web`, `wamn-dev-backups` and `wamn-dev-labels`.
- The VPC `wamn`, its subnet `wamn-us-central1` and the node service account `wamn-nodes`.
- The Cloud DNS zone `wamn-dev`, its records, and the `roles/dns.admin` binding of the cert-manager principal on the zone.
- The Google service account `wamn-blob`, its bucket binding and its `roles/artifactregistry.reader` binding on `wamn`.
- The firewall rule that permits load balancer traffic to nginx on TCP port 8443. OpenTofu owns the load balancer objects in section 4.20.

Make sure that they exist:

```bash
gcloud artifacts repositories list --project wamn-dev --location us-central1
gcloud storage buckets list --project wamn-dev --format="value(name)"
gcloud compute networks subnets list --project wamn-dev --network wamn
gcloud compute backend-services list --project wamn-dev
```

## 3. Teardown

The teardown deletes the cluster `wamn` and every object in it, the CloudNativePG databases included. Run it only with the approval of the owner.

List the disks before the delete. Write down the names of the `pvc-*` disks of the cluster:

```bash
gcloud compute disks list --project wamn-dev --format="table(name,sizeGb,users.basename(),description)"
gcloud container clusters delete wamn --project wamn-dev --zone us-central1-a --quiet
gcloud container clusters list --project wamn-dev
gcloud compute disks list --project wamn-dev --format="table(name,sizeGb,users.basename(),description)"
```

The disks of the volumes of `wamn-pg-1` and `data-evt-nats-0` stay after the delete. Delete each one by name, one command for each disk:

```bash
gcloud compute disks delete <pvc disk name> --project wamn-dev --zone us-central1-a --quiet
```

The network endpoint group `wamn-edge` stays while a backend service names it. Before cluster replacement, the owner must detach and remove the old group. Platform apply refuses a missing new NEG status.

The work directory of section 4.1 holds the files of the deleted cluster. These are the event NATS users, the gate logins, the release digests, the stamped chart and the platform file. Section 4 makes them again, and it refuses a directory that exists. Stop the port-forwards of the old cluster and remove the work directory:

```bash
pkill -f '^kubectl -n platform port-forward'
rm -rf $HOME/.cache/wamn-install
```

## 4. Install

### 4.1 Work directory and binaries

The install keeps its files in the private work directory `$HOME/.cache/wamn-install`. Each step that uses it sets `W` to it, or reads the platform file of section 4.10, which sets `W`. Run every step from a checkout of `main`:

```bash
W=$HOME/.cache/wamn-install
mkdir -p -m 700 $W
eval "$(tools/lane-start "$PWD")"
```

### 4.2 Cluster

The owner runs this procedure. Use the owner's own credentials for OpenTofu. The state bucket of section 1 and the project resources of section 2 must exist first.

[deploy/gcp/project.k](../../deploy/gcp/project.k) declares the cluster and the pools `main`, `pg` and `bench`. The pool `main` holds one Spot node. A taint restricts which pods can use a node. The pool `pg` holds one node for PostgreSQL alone. Its taint keeps every other pod off, and the `Cluster` selects it (section 4.5).

The pool `bench` holds one Spot node for the Receiving host alone. Its taint keeps every other pod off, and an admission policy places the host there (section 4.9). The 8-vCPU project quota sets its size.

From the checkout used in section 4.1, render the document into a private directory:

```bash
W=$HOME/.cache/wamn-install
mkdir -p -m 700 "$W/project"
target/debug/wamn-ctl project render deploy/gcp/project.k \
  --state-bucket wamn-dev-tofu-state --state-prefix project "$W/project"
```

The renderer writes `main.tf.json`. Its import blocks adopt the existing cluster and pools into OpenTofu state on the first apply.

Install OpenTofu from the checkout with `tools/install-tofu`. The installer prints the path to its binary:

```bash
TOFU=$(tools/install-tofu)
```

With the owner's credentials, initialize OpenTofu in the rendered directory:

```bash
(
  cd "$W/project"
  "$TOFU" init
)
```

Review the plan for the existing cluster and pools:

```bash
(
  cd "$W/project"
  "$TOFU" plan -target=google_container_cluster.cluster -target=google_container_node_pool.main \
    -target=google_container_node_pool.pg -target=google_container_node_pool.bench
)
```

Apply the plan:

```bash
(
  cd "$W/project"
  "$TOFU" apply -target=google_container_cluster.cluster -target=google_container_node_pool.main \
    -target=google_container_node_pool.pg -target=google_container_node_pool.bench
)
```

After the apply, make sure that the plan shows no changes:

```bash
(
  cd "$W/project"
  "$TOFU" plan -target=google_container_cluster.cluster -target=google_container_node_pool.main \
    -target=google_container_node_pool.pg -target=google_container_node_pool.bench
)
```

Write the cluster credentials to the agent's kubeconfig, the file that selects its cluster. Use the agent's `CLOUDSDK_CONFIG` and `KUBECONFIG` for this command (rule 7):

```bash
CLOUDSDK_CONFIG=$HOME/.config/gcloud-wamn-agent \
KUBECONFIG=$HOME/.kube/wamn-dev \
gcloud container clusters get-credentials wamn --project wamn-dev --zone us-central1-a
```

The pools have a fixed size and no autoscaler, so a scale to 0 holds. If `kubectl get nodes` lists no node after 03:00 New York time, run section 5.1.

### 4.3 cert-manager and the edge certificate

Set `WAMN_REGISTRY_AUTH_FILE` to the credentials file prepared in section 4.10.
From the repository root, run `wamn-ctl platform apply <cluster.k>`.
The verb runs sections 4.3 through 4.14 in order, then resumes at 4.17 after the owner commits release digests.
It holds one install lock and writes its kubeconfig, stamped chart, and Job logs in its work directory.
Each done or skipped progress line includes elapsed seconds.
On GCP, it requests the production certificate directly.
It stamps the registry images in 4.6, provisions in 4.7, and writes platform inputs in 4.9 without editing committed files.
Step 4.8 derives event users from the built packages of each `environments/*.k` beside `cluster.k`.
It preserves `platform-seal` and existing passwords, merges authorization, and writes each environment's credential Secrets.
The `platform-seal` user denies all publish and subscribe subjects.
The Secrets use `evt-nats-<org>--<project>--<env>-<role>` names.
Provisioning, publisher, and observer Secrets live in `platform`.
Runtime, materializer, and a publisher copy live in `hosts`.
An unchanged apply writes no broker Secret.
Before `platform apply`, build each new platform version once with `build-platform`, as section 4.6 describes.
Step 4.11 runs `env apply` for each environment as a Job, with `release = "none"`.
The Job mounts a private document copy, database credentials, and the scheduler NATS certificate.
It uses the runner's pinned KCL compiler and its own Kubernetes account.
Step 4.10 needs no file because the verb supplies the existing environment variables directly.
An unchanged completed Job skips when its input digest matches.
Step 4.12 reads the owner's `identity-resend` Secret and refuses a missing, empty, or whitespace-bearing key with the section 1 command.
It prepares session targets as Jobs, then installs the identity chart with values derived from the document and stamped images.
Jobs read the active session key, publish and activate a key when none is active, and request the public keys over TLS.
The public Job results stay with their completion digests so a retry can reuse a published key.
Step 4.13 pushes missing packages and applies them as Jobs.
Step 4.14 publishes each release through a Job.
For a wired release, one pod runs the gate, authorship, and publication with an in-memory PAT.
The verb prints release lines for the owner to commit, then resumes at 4.17 on a rerun.
Qualification stays on the build machine in section 4.15.
The verb also runs sections 4.18 through 4.19.1.
Run section 4.20 onward by hand with the commands below.

[deploy/gcp/letsencrypt.yaml](../../deploy/gcp/letsencrypt.yaml) holds the ClusterIssuer `letsencrypt` and the wildcard Certificate `wamn-edge-tls` in namespace `edge`. Issue from the staging server first, then from production:

```bash
kubectl apply -f deploy/infra/cert-manager.yaml
kubectl -n cert-manager rollout status deploy/cert-manager deploy/cert-manager-webhook deploy/cert-manager-cainjector
sed -e 's#acme-v02.api#acme-staging-v02.api#' -e 's#^      name: letsencrypt$#      name: letsencrypt-staging#' \
  deploy/gcp/letsencrypt.yaml | kubectl apply -f -
kubectl -n edge wait certificate/wamn-edge-tls --for=condition=Ready --timeout=600s
kubectl apply -f deploy/gcp/letsencrypt.yaml
kubectl wait clusterissuer/letsencrypt --for=condition=Ready --timeout=120s
kubectl -n edge delete secret wamn-edge-tls
kubectl -n edge wait certificate/wamn-edge-tls --for=condition=Ready --timeout=600s
kubectl -n cert-manager delete secret letsencrypt-staging
kubectl -n edge get secret wamn-edge-tls -o jsonpath='{.data.tls\.crt}' | base64 -d | openssl x509 -noout -issuer -enddate
```

The issuer must be Let's Encrypt production, not `(STAGING)`.

### 4.4 Namespaces, runtime operator and internal CA

| Namespace | Holds |
| --- | --- |
| `platform` | the runtime operator, its NATS and CA, the event NATS, PostgreSQL, the collector |
| `identity` | identity |
| `hosts` | the release namespace: the hosts of every environment and the CDC readers |
| `edge` | the edge and the public certificate |

The operator release runs in `platform`. [values-operator.yaml](../../deploy/gcp/values-operator.yaml) makes it watch `hosts`. cert-manager reads a ClusterIssuer CA from its own namespace, so the operator CA is copied there:

```bash
for n in platform identity hosts; do kubectl create namespace $n; done
helm upgrade --install --namespace platform wamn oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.11.0 \
  -f deploy/infra/values-wamn.yaml -f deploy/gcp/values-operator.yaml --wait --timeout 5m
sed -e 's/__ENVIRONMENT_NAMESPACE__/hosts/' -e 's/namespace: wamn-system/namespace: platform/' \
  deploy/platform/runtime-operator-events-rbac.example.yaml | kubectl apply -f -
kubectl -n platform get secret wasmcloud-ca -o json | python3 -c "
import json,sys;d=json.load(sys.stdin);m=d['metadata'];d['metadata']={'name':m['name'],'namespace':'cert-manager'};print(json.dumps(d))" \
  | kubectl apply -f -
kubectl apply -f deploy/infra/wasmcloud-ca-issuer.yaml
kubectl wait --for=condition=Ready clusterissuer/wasmcloud-ca --timeout=60s
sed 's/__ENVIRONMENT_NAMESPACE__/hosts/' deploy/platform/host-environment-certs.example.yaml | kubectl apply -f -
kubectl -n hosts wait --for=condition=Ready certificate/wasmcloud-runtime-tls certificate/wasmcloud-data-tls --timeout=120s
```

The owner can now make the Secret `identity-resend` of section 1.

### 4.4.1 Collector

[deploy/platform/otel-collector.yaml](../../deploy/platform/otel-collector.yaml) is the one OpenTelemetry Collector of the cluster (`docs/plan/observability.md` section 4). [deploy/gcp/otel-collector-values.yaml](../../deploy/gcp/otel-collector-values.yaml) gives it the bucket and the cluster name. It writes traces and logs to the bucket of section 1 as gzip OTLP JSON, below `traces/` and `logs/` with hour partitions. It drops metrics. Apply it, then let it act as `wamn-otel`:

```bash
kubectl -n platform apply -f deploy/platform/otel-collector.yaml -f deploy/gcp/otel-collector-values.yaml
kubectl -n platform annotate serviceaccount otel-collector --overwrite \
  iam.gke.io/gcp-service-account=wamn-otel@wamn-dev.iam.gserviceaccount.com
kubectl -n platform rollout restart deploy/otel-collector
kubectl -n platform rollout status deploy/otel-collector --timeout=300s
```

If the bucket or the binding of section 1 is missing, the collector exits at start and its log names the bucket.

### 4.5 PostgreSQL

[deploy/gcp/cnpg-cluster.yaml](../../deploy/gcp/cnpg-cluster.yaml) is one instance that holds the system database `wamn_system` and every environment database. Its volume uses the storage class `premium-rwo` (pd-balanced), which needs the SSD quota of section 1. CloudNativePG keeps the superuser login in the Secret `wamn-pg-superuser`:

```bash
kubectl apply --server-side -f deploy/infra/cnpg-operator.yaml
kubectl -n cnpg-system rollout status deploy/cnpg-controller-manager --timeout=180s
kubectl apply -f deploy/gcp/cnpg-cluster.yaml
kubectl -n platform wait cluster/wamn-pg --for=condition=Ready --timeout=600s
```

Forward the database pod to this machine. Later steps use the forward. A forward to the Service closes long connections, so forward the pod:

```bash
W=$HOME/.cache/wamn-install
setsid kubectl -n platform port-forward pod/wamn-pg-1 15432:5432 > $W/pf-pg.log 2>&1 < /dev/null &
```

If a step cannot connect to `127.0.0.1:15432`, start the forward again.

The Cluster sets `pg_stat_statements.max` and `pg_stat_statements.track`. From these parameters, CloudNativePG preloads the library and creates the extension in every database. If no `pg_stat_statements.*` parameter is set, CloudNativePG drops the extension from every database.

### 4.6 Packages and platform build

`wamn build` needs a PostgreSQL server ([building](building.md#wamn-build)). Run a local one on a loopback port, and build every package that the runner image reads. The server takes some seconds to start. Wait for its answer on TCP:

```bash
docker run -d --name wamn-install-build-pg -e POSTGRES_HOST_AUTH_METHOD=trust -p 127.0.0.1:0:5432 postgres:18
PORT=$(docker port wamn-install-build-pg 5432/tcp | head -1 | sed 's/.*://')
until docker exec wamn-install-build-pg pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; do sleep 1; done
DATABASE_URL=postgres://postgres@127.0.0.1:$PORT/postgres target/debug/wamn build \
  apps/wamn_receiving apps/client_acme_receiving apps/wamn_wms
docker rm -f wamn-install-build-pg
```

`wamn-ctl build-platform` builds the host, runner, gates and identity images and the `http` and `materializer` role components. When every build succeeded, it pushes them with the tag of the chart version and stamps their digests into the release chart ([delivery](delivery.md#release-qualification)). The repository `wamn` has immutable tags, so a version that is already in the registry cannot be built again. A reinstall at an unchanged version uses the registry (`docs/plan/platform-deploy.md` R15). With `--stamp-only`, the command builds nothing. It reads the registry digests of the six images of the version and stamps them into the release chart. It refuses if one of them is missing. Both modes record the local platform UI digest beside the stamped chart ([building](building.md#platform-ui)).

`platform apply` builds the environment packages and the local UI, then always runs `build-platform --stamp-only`.
It never builds platform images.
If registry images are missing, it stops with the version and the explicit build command.

Before applying a new platform version, build it once with the agent identity.
Use the registry from `deploy/gcp/cluster.k`.
Give Docker its own configuration with the gcloud credential helper, which refreshes tokens during long pushes.
The file holds no credential.
The package outputs from the preceding commands must exist.

```bash
export CLOUDSDK_CONFIG=$HOME/.config/gcloud-wamn-agent
W=$HOME/.cache/wamn-install
mkdir -p -m 700 "$W/docker"
echo '{"credHelpers":{"us-central1-docker.pkg.dev":"gcloud"}}' > "$W/docker/config.json"
DOCKER_CONFIG=$W/docker target/debug/wamn-ctl build-platform \
  --registry us-central1-docker.pkg.dev/wamn-dev/wamn --out "$W/release-chart"
```

For an existing version, build the local UI and stamp the registry images into a new output directory:

```bash
pnpm --filter @wamn/platform-ui build
DOCKER_CONFIG=$W/docker target/debug/wamn-ctl build-platform --stamp-only \
  --registry us-central1-docker.pkg.dev/wamn-dev/wamn --out "$W/reinstall-chart"
```

Three files in `deploy/gcp` pin images of the chart. Write the new runner and identity images into them:

```bash
W=$HOME/.cache/wamn-install
RUNNER=$(grep -o 'us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-runner:[^ ]*' $W/release-chart/values.yaml)
IDENTITY=$(grep -o 'us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-identity:[^ ]*' $W/release-chart/values.yaml)
sed -i "s#image: us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-runner:.*#image: $RUNNER#" \
  deploy/gcp/operator/mint-pat.yaml deploy/gcp/operator/invite.yaml
sed -i "s/^  tag: .*/  tag: ${IDENTITY##*@sha256:}/" deploy/gcp/values-identity.yaml
git diff --stat
```

### 4.7 System database, org and identity issuer

`provision-system` installs the control store. `provision-org --admin` records the org `dkk` and creates its first admin user. `provision-identity-issuer` prepares the login of identity, which the whole platform shares:

```bash
W=$HOME/.cache/wamn-install
PW=$(kubectl -n platform get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
export WAMN_SYSTEM_ADMIN_URL="postgresql://postgres:${PW}@127.0.0.1:15432/wamn_system"
target/debug/wamn-ctl provision-system --platform-domain wamn.dev
target/debug/wamn-ctl provision-org --org dkk --template trials --pool wamn-pg --admin dkkloimwieder@gmail.com
(umask 077; target/debug/wamn-ctl provision-identity-issuer --issuer https://identity.identity.svc.cluster.local \
  --prepare-generation a --namespace identity --emit-secret $W/identity-db.json \
  --db-host wamn-pg-rw.platform.svc.cluster.local)
kubectl apply -f $W/identity-db.json
rm $W/identity-db.json
```

Make the serving certificate and the operator CA of identity. Copy the CA of the serving certificate into `hosts` and `edge`:

```bash
W=$HOME/.cache/wamn-install
kubectl apply -f deploy/gcp/identity-certificate.yaml -f deploy/gcp/identity-operator-ca.yaml
kubectl -n identity wait certificate --all --for=condition=Ready --timeout=120s
kubectl -n identity get secret identity-tls -o jsonpath='{.data.ca\.crt}' | base64 -d > $W/identity-ca.crt
for ns in hosts edge; do kubectl -n $ns create configmap identity-ca --from-file=ca.crt=$W/identity-ca.crt; done
```

### 4.8 Event NATS

`platform apply` now runs this step.
The following commands describe the manual procedure.

One event NATS serves both environments. Write the users of each environment into its own private directory. The tenant is `<org>--<project>--<env>`, the tenant that `env apply` gives the environment:

```bash
W=$HOME/.cache/wamn-install
for project in receiving wms; do
  mkdir -m 700 $W/evt-$project
  cargo run --locked -p wamn-test-infrastructure --example event_broker_files -- \
    $W/evt-$project nats://evt-nats.platform.svc.cluster.local:4222 \
    dkk $project dev dkk--$project--dev 1 "$PWD/apps/target/wamn/wamn_$project/wamn.json"
done
```

`authorization.conf` is `authorization: ` followed by JSON. Merge the users of the two files:

```bash
W=$HOME/.cache/wamn-install; E=$W/evt-receiving/event-nats; F=$W/evt-wms/event-nats
(umask 077; python3 - $E/authorization.conf $F/authorization.conf > $W/merged-authorization.conf <<'PY'
import json, sys
def users(path):
    return json.loads(open(path).read().split(':', 1)[1])['users']
merged = users(sys.argv[1]) + users(sys.argv[2])
print('authorization: ' + json.dumps({'users': merged}))
PY
)
```

Make the broker Secret, install the broker, and make the user Secrets:

```bash
W=$HOME/.cache/wamn-install; E=$W/evt-receiving/event-nats; F=$W/evt-wms/event-nats
kubectl -n platform create secret generic evt-nats-authorization --from-file=authorization.conf=$W/merged-authorization.conf
kubectl apply -f deploy/gcp/nats-jetstream.yaml
kubectl -n platform rollout status statefulset/evt-nats --timeout=300s
for role in provisioning publisher observer; do
  kubectl -n platform create secret generic evt-nats-$role \
    --from-file=username=$E/$role-username --from-file=password=$E/$role-password
  kubectl -n platform create secret generic evt-nats-wms-$role \
    --from-file=username=$F/$role-username --from-file=password=$F/$role-password
done
kubectl -n hosts create secret generic evt-nats-publisher \
  --from-file=username=$E/publisher-username --from-file=password=$E/publisher-password
kubectl -n hosts create secret generic evt-nats-wms-publisher \
  --from-file=username=$F/publisher-username --from-file=password=$F/publisher-password
kubectl -n hosts create secret generic wamn-event-nats \
  --from-file=username=$E/runtime-username --from-file=password=$E/runtime-password \
  --from-literal=org=dkk --from-literal=project=receiving --from-literal=environment=dev \
  --from-literal=stream_replicas=1 --from-literal=dup_window_secs=120
kubectl -n hosts create secret generic wamn-materializer-nats --from-file=binding.json=$E/binding.json
kubectl -n hosts create secret generic wamn-event-nats-wms \
  --from-file=username=$F/runtime-username --from-file=password=$F/runtime-password \
  --from-literal=org=dkk --from-literal=project=wms --from-literal=environment=dev \
  --from-literal=stream_replicas=1 --from-literal=dup_window_secs=120
kubectl -n hosts create secret generic wamn-materializer-nats-wms --from-file=binding.json=$F/binding.json
```

Make sure that the provisioning user can read the source stream. The log shows `"config":{"name":"EVT_3_dkk_9_receiving_3_dev"`:

```bash
kubectl apply -f deploy/gcp/evt-nats-check.yaml
kubectl -n platform wait pod/evt-nats-check --for=jsonpath='{.status.phase}'=Succeeded --timeout=90s
kubectl -n platform logs evt-nats-check
kubectl -n platform delete pod evt-nats-check
```

Forward the event NATS and the scheduler NATS to this machine. `wamn-ctl` publishes its auth changes on the scheduler NATS:

```bash
W=$HOME/.cache/wamn-install
setsid kubectl -n platform port-forward svc/evt-nats 14222:4222 > $W/pf-nats.log 2>&1 < /dev/null &
setsid kubectl -n platform port-forward svc/nats 14223:4222 > $W/pf-sched.log 2>&1 < /dev/null &
```

### 4.9 Platform install inputs

`env apply` makes none of these objects and touches no IAM. The registry helper names the `wamn` credential helper for the host pulls and holds no credential. Every host runs as the service account `wamn-host`, which acts as `wamn-blob` (section 1). The Role `wamn-control-status` gives the control operation `environment.status` its reads. The policy [bench-host-placement.yaml](../../deploy/gcp/bench-host-placement.yaml) gives the Receiving host pod the selector and the toleration of pool `bench`, because the host chart has no field for them. [auth-nats-clients.yaml](../../deploy/gcp/auth-nats-clients.yaml) holds the scheduler NATS client certificates of identity and `wamn-ctl`, which publish auth changes to the hosts. The two platform manifests state no namespace, so apply them into the release namespace:

```bash
kubectl apply -f deploy/gcp/registry-helper.yaml
kubectl apply -f deploy/gcp/bench-host-placement.yaml
kubectl apply -f deploy/gcp/auth-nats-clients.yaml
kubectl -n hosts apply -f deploy/platform/wamn-host-serviceaccount.yaml
kubectl -n hosts annotate serviceaccount wamn-host --overwrite \
  iam.gke.io/gcp-service-account=wamn-blob@wamn-dev.iam.gserviceaccount.com
kubectl -n hosts apply -f deploy/platform/control-status-rbac.yaml
```

The owner made the `wamn-blob` binding (section 1). IAM is owner-only, so the install identity does not read it. The first apply with a release (4.17) is the proof. If the binding is missing, the host pull fails and names the account.

### 4.10 Platform file

`wamn-ctl env` reads its platform inputs from the environment (`Platform::from_env` in `crates/control/lib/src/environment/mod.rs`). Write them into one file. Every later step starts with `. $HOME/.cache/wamn-install/platform.sh`:

```bash
W=$HOME/.cache/wamn-install
cat > $W/platform.sh <<'EOF'
export W=$HOME/.cache/wamn-install
PW=$(kubectl -n platform get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
export WAMN_SYSTEM_ADMIN_URL="postgresql://postgres:${PW}@127.0.0.1:15432/wamn_system"
export WAMN_CONTROL_DATABASE_URL="$WAMN_SYSTEM_ADMIN_URL"
export WAMN_RELEASE_NAMESPACE=hosts
export WAMN_DATABASE_NAMESPACE=platform
export WAMN_DATABASE_HOST=wamn-pg-rw.platform.svc.cluster.local
export WAMN_RELEASE_CHART=$W/release-chart
export WAMN_RELEASE_ARTIFACT_BASE=us-central1-docker.pkg.dev/wamn-dev/wamn/releases
export WAMN_PACKAGE_ARTIFACT_BASE=us-central1-docker.pkg.dev/wamn-dev/wamn/packages
export WAMN_REGISTRY_AUTH_FILE=$W/registry/config.json
export WAMN_EVENT_NATS_URL=nats://127.0.0.1:14222
# The scheduler NATS of the auth changes, through the port-forward of section 4.8.
export WAMN_AUTH_NATS_URL=nats://127.0.0.1:14223
export WAMN_AUTH_NATS_TLS_SERVER_NAME=nats.platform.svc.cluster.local
export WAMN_AUTH_NATS_TLS_CA=$W/auth-nats/ca.crt
export WAMN_AUTH_NATS_TLS_CERT=$W/auth-nats/tls.crt
export WAMN_AUTH_NATS_TLS_KEY=$W/auth-nats/tls.key
if [ ! -d $W/auth-nats ]; then
  (umask 077; mkdir $W/auth-nats
   for k in ca.crt tls.crt tls.key; do
     kubectl -n platform get secret wamn-ctl-auth-nats -o jsonpath="{.data.${k//./\\.}}" | base64 -d > $W/auth-nats/$k
   done)
fi
export DOCKER_CONFIG=$W/docker
# The registry token lasts one hour. Write a new one before each step that reads it.
registry_token() {
  rm -rf $W/registry; mkdir -m 700 $W/registry
  gcloud auth print-access-token | python3 -c '
import json, os, sys
token = sys.stdin.read().strip()
fd = os.open(os.path.join(sys.argv[1], "config.json"), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.write(fd, json.dumps({"auths": {"us-central1-docker.pkg.dev": {"username": "oauth2accesstoken", "password": token}}}).encode())
os.close(fd)' $W/registry
}
# The provisioning user of the event NATS for one environment.
use_env() {
  export WAMN_EVENT_NATS_USERNAME="$(cat $W/evt-$1/event-nats/provisioning-username)"
  export WAMN_EVENT_NATS_PASSWORD_FILE=$W/evt-$1/event-nats/provisioning-password
}
# The instance suffix and the superuser URL of one environment database.
suffix() { kubectl -n platform get database -o name | sed -n "s#.*/wamn-db-dkk--$1--dev--##p"; }
db_url() { echo "postgresql://postgres:${PW}@127.0.0.1:15432/wamn-db-dkk--$1--dev--$(suffix $1)"; }
# The private directory of the authoring gate, and the URL of one emitted Secret file.
G=$W/gate
url() { python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["stringData"]["url"])' "$1"; }
EOF
chmod 600 $W/platform.sh
```

Leave `WAMN_IDENTITY_URL` unset. The identity certificate names only `identity.identity.svc.cluster.local`, so this machine cannot reach identity, and `apply` issues no PAT. Section 4.14 mints the PAT in the cluster.

### 4.11 First apply

`platform apply` now runs this step with private document copies.
It does not edit the committed host group files.
The following commands describe the manual procedure.

`publish-release` needs the environment database and its installed packages, so the first apply names no release. The documents in [deploy/gcp/environments](../../deploy/gcp/environments) name a release, so apply a private copy with `release = "none"`. The apply records the environment with a new instance suffix, and makes the `Database`, the roles, the run plane and the credential Secrets:

```bash
. $HOME/.cache/wamn-install/platform.sh
for project in receiving wms; do
  use_env $project
  sed 's/^    release = .*/    release = "none"/' deploy/gcp/environments/dkk--$project--dev.k > $W/dkk--$project--dev.k
  target/debug/wamn-ctl env apply $W/dkk--$project--dev.k --dry-run
  target/debug/wamn-ctl env apply $W/dkk--$project--dev.k
  echo "$project $(suffix $project)"
done
```

Each host group file holds the suffix of its environment's first apply. Write the new suffixes:

```bash
. $HOME/.cache/wamn-install/platform.sh
for project in receiving wms; do
  sed -i "/name: WAMN_SESSION_INSTANCE_SUFFIX/{n;s/value: .*/value: $(suffix $project)/}" deploy/gcp/host-group-$project.yaml
done
git diff deploy/gcp/host-group-*.yaml
```

### 4.12 Identity and session key

`platform apply` now runs this step after the owner creates the Resend Secret from section 1.
The following commands describe the manual procedure.

Prepare the session target of each environment in namespace `identity`. [values-identity.yaml](../../deploy/gcp/values-identity.yaml) names both Secrets:

```bash
. $HOME/.cache/wamn-install/platform.sh
for project in receiving wms; do
  (umask 077; target/debug/wamn-ctl provision-project-env --org dkk --project $project --env dev --tenant dkk--$project--dev \
    --namespace identity --target-admin-database-url "$(db_url $project)" --db-host wamn-pg-rw.platform.svc.cluster.local \
    --prepare-session-role-reader-generation a --emit-session-role-reader-secret $W/session-target-$project.json)
  kubectl apply -f $W/session-target-$project.json
  rm $W/session-target-$project.json
done
```

The verb prints two PostgreSQL warnings that the `postgres` role did not grant a role membership. The generation still authenticates.

Make sure that the owner's Resend Secret exists, that it has the key `api-key`, and that the key holds no whitespace. The second and third commands print counts, never the key. The second count is the length of `api-key` and must be more than 0, because a missing or empty key also gives a whitespace count of 0. The third count must be 0. If the second count is 0, the owner makes the Secret again as section 1 says:

```bash
. $HOME/.cache/wamn-install/platform.sh
kubectl -n identity get secret identity-resend -o name
kubectl -n identity get secret identity-resend -o jsonpath='{.data.api-key}' | base64 -d | wc -c
kubectl -n identity get secret identity-resend -o jsonpath='{.data.api-key}' | base64 -d | tr -dc '[:space:]' | wc -c
```

Install identity from the file values only, with no `--set`. Then publish a session key, and activate it with the `kid` that `publish` prints:

```bash
helm install identity deploy/platform/identity -n identity -f deploy/gcp/values-identity.yaml
kubectl -n identity rollout status deploy/identity --timeout=300s
kubectl -n identity exec deploy/identity -- wamn-identity publish
kubectl -n identity exec deploy/identity -- wamn-identity activate --kid <kid>
```

Make sure that identity answers over TLS from a pod in `hosts`. The answer is the key set with the `kid`, and `HTTP 200`:

```bash
kubectl -n hosts run identity-check --restart=Never --rm -i --quiet \
  --image=curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777 \
  --overrides='{"spec":{"volumes":[{"name":"ca","configMap":{"name":"identity-ca"}}],"containers":[{"name":"identity-check","image":"curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777","command":["curl","-sS","-w","\\nHTTP %{http_code}\\n","--cacert","/ca/ca.crt","https://identity.identity.svc.cluster.local/.well-known/jwks.json"],"volumeMounts":[{"name":"ca","mountPath":"/ca"}]}]}}'
```

### 4.13 Packages

`platform apply` reads package roots from each environment's adjacent JSON file and versions from their built manifests.
It reads registry credentials from `WAMN_REGISTRY_AUTH_FILE` and optional CA files from `WAMN_OCI_CA_PATHS`.
It pushes a package only when its registry tag is absent.
It runs `apply-package` and `reconcile-package-data-access` as Jobs, then runs `reconcile-replica-identity` for every package.
The Jobs skip when their completed input digests match.
The commands below describe the manual equivalent.

The package artifacts `wamn_receiving-2.2.5` and `wamn_wms-2.1.3` are in the registry. If a tag is missing, push that package. The tags are immutable, so a push of other bytes under a present tag fails:

```bash
. $HOME/.cache/wamn-install/platform.sh
registry_token
declare -A V=([receiving]=2.2.5 [wms]=2.1.3)
for project in receiving wms; do
  gcloud artifacts docker images describe $WAMN_PACKAGE_ARTIFACT_BASE:wamn_$project-${V[$project]} --format="value(image_summary.digest)" \
  || target/debug/wamn-ctl push-package --package apps/wamn_$project --artifact-base $WAMN_PACKAGE_ARTIFACT_BASE \
       --component-artifact-base us-central1-docker.pkg.dev/wamn-dev/wamn/components --registry-auth-file $WAMN_REGISTRY_AUTH_FILE
done
```

Install each package into its environment. `reconcile-replica-identity` runs for WMS, because the workflow `movement_label` compares the old location of `wms.packaging`:

```bash
. $HOME/.cache/wamn-install/platform.sh
R="--artifact-base $WAMN_PACKAGE_ARTIFACT_BASE --registry-auth-file $WAMN_REGISTRY_AUTH_FILE"
declare -A V=([receiving]=2.2.5 [wms]=2.1.3)
for project in receiving wms; do
  target/debug/wamn-ctl apply-package --package-artifact wamn_$project-${V[$project]} $R \
    --database-url "$(db_url $project)" --tenant dkk--$project--dev
  target/debug/wamn-ctl reconcile-package-data-access --package-artifact wamn_$project-${V[$project]} $R \
    --database-url "$(db_url $project)" --tenant dkk--$project--dev
done
target/debug/wamn-ctl reconcile-replica-identity --admin-database-url "$(db_url wms)" --package apps/wamn_wms
```

### 4.14 Releases

`platform apply` runs `publish-release` as a Job for each environment without a wiring.
For a wired release, it creates one Job pod in namespace `identity`.
A setup container runs `provision-project-env` to prepare the identity reader, control author, and management admitter database logins.
It uses cluster service DNS names and creates Secrets owned by that Job.
The native sidecar runs `wamn-scenario-worker serve` on the pod's loopback address.
The main container runs `wamn-ctl publish-wired-release`.
It mints the management-author PAT in memory, waits for the sidecar, and posts each wiring with the PAT as a bearer header.
It requires a completed gate outcome, calls the `author-wiring` and `publish-release` library functions, and prints one release digest.
The PAT never enters a file or a Secret.
The Job mounts `operator-<org>` and `identity-tls` read-only, so the operator certificate stays in the cluster.
The runner saves all container logs before deleting the Job and its owned Secrets.

Publication skips when the completed input digest matches the package artifact digests, wiring document digests, database path, and Job inputs.
The manual commands below remain an alternative to `platform apply`.

`publish-release` prints the digest of the release manifest. Publish Receiving, which has no wiring:

```bash
. $HOME/.cache/wamn-install/platform.sh
registry_token
RELEASE_receiving=$(target/debug/wamn-ctl publish-release --database-url "$(db_url receiving)" \
  --control-database-url "$WAMN_SYSTEM_ADMIN_URL" --org dkk --tenant dkk--receiving--dev --environment dev \
  --verified-publisher-principal wamn-management-author-dkk--receiving--dev \
  --run-schema wamn_run --package wamn_receiving@2.2.5 \
  --artifact-base $WAMN_PACKAGE_ARTIFACT_BASE --registry-auth-file $WAMN_REGISTRY_AUTH_FILE)
echo "$RELEASE_receiving" > $W/release-receiving
```

The WMS release has the wiring `inventory_move_and_label`. The authoring gate must pass the wiring before `author-wiring` records it. The gate runs on this machine, so its database logins name the port-forward. Prepare the gate logins into a private directory `G`, and do not apply them:

```bash
. $HOME/.cache/wamn-install/platform.sh
mkdir -m 700 $G
(umask 077; target/debug/wamn-ctl provision-project-env --org dkk --project wms --env dev --tenant dkk--wms--dev \
  --namespace hosts --target-admin-database-url "$(db_url wms)" --db-host 127.0.0.1 --db-port 15432 \
  --prepare-management-admitter-generation a --emit-management-admitter-secret $G/management-admitter.json)
(umask 077; target/debug/wamn-ctl provision-project-env --org dkk --project wms --env dev --tenant dkk--wms--dev \
  --namespace hosts --db-host 127.0.0.1 --db-port 15432 \
  --prepare-control-author-generation a --emit-control-author-secret $G/control-author.json)
(umask 077; kubectl -n hosts get secret wamn-identity-reader-dkk--wms--dev -o jsonpath='{.data.url}' | base64 -d \
  | sed 's#@wamn-pg-rw.platform.svc.cluster.local:5432/#@127.0.0.1:15432/#' > $G/identity-reader.url)
```

`control-author` addresses the control database, so it takes no `--target-admin-database-url`.

Mint the management-author PAT of WMS in the cluster. The Job [mint-pat.yaml](../../deploy/gcp/operator/mint-pat.yaml) runs in namespace `identity` with the mounted operator certificate, so the certificate never reaches this machine. Fill a private copy of the manifest, apply it and remove it:

```bash
. $HOME/.cache/wamn-install/platform.sh
(umask 077; sed -e 's/__ORG__/dkk/' -e 's/__PROJECT__/wms/' -e 's/__ENV__/dev/' -e 's/__TENANT__/dkk--wms--dev/' \
  -e 's/__NAMESPACE__/hosts/' -e 's/__PAT_FLAG__/--emit-management-author-pat-secret/' \
  deploy/gcp/operator/mint-pat.yaml > $G/mint-pat.yaml)
kubectl apply -f $G/mint-pat.yaml
rm $G/mint-pat.yaml
```

The pod waits for the Secret `wamn-system-admin`, the superuser URL of `wamn_system`. Make it with an owner reference to the Job, so that Kubernetes deletes it with the Job. Use `kubectl create`, because `kubectl apply` copies the data into an annotation. The pod keeps the PAT file for at most 300 seconds, so read it at once:

```bash
. $HOME/.cache/wamn-install/platform.sh
JOB_UID=$(kubectl -n identity get job mint-pat -o jsonpath='{.metadata.uid}')
printf 'postgresql://postgres:%s@wamn-pg-rw.platform.svc.cluster.local:5432/wamn_system' "$PW" |
  kubectl -n identity create secret generic wamn-system-admin --from-file=url=/dev/stdin --dry-run=client -o json |
  jq --arg uid "$JOB_UID" '.metadata.ownerReferences = [{apiVersion: "batch/v1", kind: "Job", name: "mint-pat", uid: $uid}]' |
  kubectl create -f -
for i in $(seq 150); do kubectl -n identity exec job/mint-pat -- test -e /out/pat.json 2>/dev/null && break; sleep 2; done
(umask 077; kubectl -n identity exec job/mint-pat -- sh -c 'cat /out/pat.json && rm /out/pat.json' > $G/management-author-pat.json)
kubectl -n identity wait --for=condition=complete job/mint-pat --timeout=60s
kubectl -n identity logs job/mint-pat
kubectl -n identity delete job mint-pat
```

Run the gate on this machine, and post the gate request with the PAT as a bearer token. The PAT goes to `curl` as a header file, never on the command line. The `pkill` pattern starts with `^`. Without it, the pattern also matches the shell that runs this block, and `pkill` stops that shell:

```bash
. $HOME/.cache/wamn-install/platform.sh
WAMN_SYSTEM_URL="$(cat $G/identity-reader.url)" WAMN_CONTROL_AUTHORING_PG_URL="$(url $G/control-author.json)" \
WAMN_MANAGEMENT_ADMISSION_PG_URL="$(url $G/management-admitter.json)" WAMN_MANAGEMENT_ORG=dkk \
WAMN_MANAGEMENT_PROJECT=wms WAMN_MANAGEMENT_ENVIRONMENT=dev WAMN_MANAGEMENT_TENANT=dkk--wms--dev \
  setsid target/debug/wamn-scenario-worker serve --bind 127.0.0.1:18090 > $G/gate.log 2>&1 < /dev/null &
cargo run --locked -p wamn-test-infrastructure --example gate_request -- wamn_wms 2.1.3 wms dev \
  apps/wamn_wms/publication/wirings/inventory_move_and_label.json > $G/gate-request.json
for i in $(seq 30); do curl -s -o /dev/null http://127.0.0.1:18090/ && break; sleep 1; done
(umask 077; python3 -c 'import json,sys;print("Authorization: Bearer "+json.load(open(sys.argv[1]))["stringData"]["token"])' \
  $G/management-author-pat.json > $G/auth-header)
curl -sS -H @$G/auth-header -H 'content-type: application/json' --data-binary @$G/gate-request.json \
  http://127.0.0.1:18090/authoring > $G/gate-reply.json
rm -f $G/auth-header
pkill -f '^target/debug/wamn-scenario-worker serve --bind 127.0.0.1:18090'
python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(json.dumps(d.get("body",d).get("outcome",d))[:600])' $G/gate-reply.json
```

The outcome has `status` `completed`. Record the wiring and publish WMS:

```bash
. $HOME/.cache/wamn-install/platform.sh
registry_token
target/debug/wamn-ctl author-wiring --database-url "$(db_url wms)" --control-database-url "$WAMN_SYSTEM_ADMIN_URL" \
  --tenant dkk--wms--dev --package-id wamn_wms --package-version 2.1.3 \
  --wiring-document apps/wamn_wms/publication/wirings/inventory_move_and_label.json \
  --artifact-base $WAMN_PACKAGE_ARTIFACT_BASE --registry-auth-file $WAMN_REGISTRY_AUTH_FILE
RELEASE_wms=$(target/debug/wamn-ctl publish-release --database-url "$(db_url wms)" \
  --control-database-url "$WAMN_SYSTEM_ADMIN_URL" --org dkk --tenant dkk--wms--dev --environment dev \
  --verified-publisher-principal wamn-management-author-dkk--wms--dev \
  --run-schema wamn_run --package wamn_wms@2.1.3 --wiring "wamn_wms@2.1.3::inventory_move_and_label=2" \
  --artifact-base $WAMN_PACKAGE_ARTIFACT_BASE --registry-auth-file $WAMN_REGISTRY_AUTH_FILE)
echo "$RELEASE_wms" > $W/release-wms
shred -u $G/management-author-pat.json
```

### 4.15 Qualification

Qualification stays on the build machine, outside `platform apply`.
Step 4.15 reports a skip and never starts a qualification Job.

`qualify-release` runs the release's cases in kind clusters on this machine against the release chart. If every case passes, it pushes the qualification beside the release ([delivery](delivery.md#release-qualification)). It pulls the chart images with `docker pull`, so it reads `DOCKER_CONFIG` of the platform file. Each run takes longer than one hour, so each run gets a new registry token:

```bash
. $HOME/.cache/wamn-install/platform.sh
for project in receiving wms; do
  registry_token
  target/debug/wamn-ctl qualify-release --release-digest "$(cat $W/release-$project)" \
    --release-chart $WAMN_RELEASE_CHART --artifact-base $WAMN_RELEASE_ARTIFACT_BASE \
    --registry-auth-file $WAMN_REGISTRY_AUTH_FILE || break
done
```

### 4.15.1 Benchmark

Run the benchmark once after qualification, with the qualified release serving Receiving.
For a first install, complete sections 4.17 through 4.21 before this step.
Prepare the bench VM and its binary as described in section 5.8.
Keep the PostgreSQL forward on local port 15432 open.

The script reads `CHART_VERSION` from `crates/control/lib/src/release_chart.rs` and sources the platform file from section 4.10.
That file supplies `db_url receiving` and `$G/operator-pat.json`, the operator token file.
The script uses `CLOUDSDK_CONFIG` and `KUBECONFIG` from this page's opening commands.
It refuses an existing result for the current version.
Commit the result from the repository root:

```bash
tools/bench/tier.sh
version=$(sed -n 's/^pub const CHART_VERSION: &str = "\([^"]*\)";/\1/p' crates/control/lib/src/release_chart.rs)
git add "tests/bench/platform-$version.json"
git commit -m "Record platform $version benchmark"
```

### 4.16 Record the releases

`platform apply` prints each environment path and its `release = "sha256:..."` line.
It stops when any environment still declares `release = "none"`.
Commit the printed lines in the environment files, then rerun the verb for step 4.17.
The verb never edits a committed file.

The Rust edge reads each live release row. No edge values or URL map contains a copied release digest.

Commit every change of `deploy/gcp` from sections 4.6, 4.11 and 4.16 in one commit, and push it.

### 4.17 Apply with the release

`platform apply` runs `env apply` as a Job with the release declared in each environment file.
The Job mounts the stamped chart, registry credentials, and private copies of the host group inputs.
It reads `host-group-<project>.yaml` beside the cluster document and the `environments/` directory.
It derives event Secret names and the database instance suffix from the current environment.
The Job skips when its completed input digest matches.
An unqualified release fails through `env apply` itself.
The verb prints that refusal and the `qualify-release` command for the build machine.
After host qualification succeeds, rerun `platform apply` with the same release digest.
The Job applies that qualified release through the existing `env apply` path.

The apply installs the release chart of each environment in `hosts`. `WAMN_RELEASE_HOST_GROUP` names the platform part of its host group: the NATS URLs, the event NATS Secrets, the registry helper and the identity CA:

```bash
. $HOME/.cache/wamn-install/platform.sh
for project in receiving wms; do
  use_env $project
  export WAMN_RELEASE_HOST_GROUP=deploy/gcp/host-group-$project.yaml
  target/debug/wamn-ctl env apply deploy/gcp/environments/dkk--$project--dev.k --dry-run
  target/debug/wamn-ctl env apply deploy/gcp/environments/dkk--$project--dev.k
done
target/debug/wamn-ctl env show dkk/receiving/dev
target/debug/wamn-ctl env show dkk/wms/dev
```

### 4.18 CDC readers

`platform apply` renders one CDC reader Deployment in `hosts` for each environment that has a `wamn-cdc-<org>--<project>--<env>` Secret, and waits for its rollout.
`env apply` writes that Secret only for a release with an event registration.
For an environment without the Secret, the step prints `4.18 CDC reader <org>/<project>/<env>: skipped: no registration`.
The reader uses the stamped gates image, which contains `/usr/local/bin/wamn-cdc-reader`.
The Deployment mounts the replication, registry-reader, and event publisher Secrets by name.
The verb does not read their values.
The reader loads its registration from the environment row with its registry-reader login.
An unchanged apply skips the write when the rendered fields match the live objects.

For each environment with a reader, read its logs:

```bash
kubectl -n hosts logs deploy/cdc-reader-dkk--receiving--dev --tail=20
kubectl -n hosts logs deploy/cdc-reader-dkk--wms--dev --tail=20
```

Each log shows `registration loaded`, the three `preflight` lines and `walsender session open`.

### 4.19 Web clients

`platform apply` runs `wamn web upload` as a Job for each environment and its declared release digest.
The Job runs in `hosts` as `wamn-host`, which uses the owner-supplied `wamn-blob` Workload Identity binding.
It reads the built clients from package artifacts and writes them below `gs://<buckets.web>/clients/<package>/<digest hex>/`.
It leaves `WAMN_EVENT_NATS_URL` unset.
The Job runner saves the log before it deletes the Job and its temporary Secrets.
A completed Job with the same input digest skips the upload.

If `buckets.web` is empty, the report prints `skipped: no bucket` for each environment's upload and its check.
The verb starts no web Job in that case.
The kind fixture has no web bucket.

### 4.19.1 Control host and platform UI

`platform apply` runs `provision-org` in a Job with the mounted database credential.
The Job writes the control credential and the first-install administration Secret to `hosts`.
It preserves an existing administration Secret and its environment credentials.
A second Job copies the operator Secret with a typed metadata patch.
Credential values stay inside the cluster and never enter Job logs.

The verb renders the control host values from the document and the stamped chart.
It sets the HTTP image and control route in [control-service.yaml](../../deploy/gcp/control-service.yaml) in memory.
It compares the rendered objects with live objects and skips an unchanged release.
It waits for `hosts/hostgroup-c-<org>` to roll out.

Keep each environment's administration credential current through `provision-project-env --emit-control-administration-patch` before control writes.
The runtime TLS Secrets and service account from section 4.9 must exist in `hosts`.

The verb runs `wamn web upload --platform-ui --org <org> --bucket gs://<buckets.web>` in a Job.
It mounts the local bundle and the stamped build record as Job-owned inputs.
The upload refuses bytes that differ from the recorded digest.
An unchanged input digest skips the Job.
The destination is `gs://<buckets.web>/platform-ui/<platform version>/`, and `config.json` contains only the org.
Without `--bucket`, the standalone command keeps `gs://wamn-dev-web` as its default.
With an empty `buckets.web`, the upload and its check print `skipped: no bucket` and start no Job.

The control release chart writes its route host and platform version into the `hosts/c-<org>` ConfigMap.
Helm upgrade and rollback update that ConfigMap with the release.
The Rust edge reads it for each request.
The `control.wamn.dev` DNS record must point to the same load balancer as the application hosts.
The existing wildcard certificate covers this host.
Section 4.20 installs both routing owners.
They send `/api/` to `c-dkk-http`, `/password/` to identity, and every shell page to the platform UI index.

### 4.20 Edge and load balancer

`platform apply` installs the Rust edge Deployment and Service as `edge/wamn-edge-web` and waits for rollout.
It reads the image digest from `platform.edge.image` in the stamped release chart.
`build-platform` builds the `edge-web` Docker target for `linux/amd64` and stamps that image.
The `edge` target still exports the aarch64 edge-box binary.
The installer compares rendered objects with live objects and skips an unchanged installation.
It grants the edge only `get` on the named release ConfigMaps in `hosts`, without Secret access.

The edge reads `gs://wamn-dev-web` as the Google service account `wamn-edge` through Workload Identity on `edge/wamn-edge-web`.
The service account and its two bindings outlive a teardown, because they are IAM.
If they are missing, the agent runs these three commands as `wamn-agent@wamn-dev`.
The agent posts each command and runs it only after the owner approves it, the same approval as a cluster resize (owner ruling 2026-10-23):

```bash
gcloud iam service-accounts create wamn-edge --project wamn-dev
gcloud storage buckets add-iam-policy-binding gs://wamn-dev-web --member serviceAccount:wamn-edge@wamn-dev.iam.gserviceaccount.com --role roles/storage.objectViewer
gcloud iam service-accounts add-iam-policy-binding wamn-edge@wamn-dev.iam.gserviceaccount.com --project wamn-dev --role roles/iam.workloadIdentityUser --member 'serviceAccount:wamn-dev.svc.id.goog[edge/wamn-edge-web]'
```

Each environment can set `client_package` to the package id whose web client its `route_host` serves.
A release with one client needs no name.
With multiple clients and no name, `env apply` refuses and names the host.
The release chart owns the host-to-package map and the release digest in its ConfigMap.
The edge reads that row and fetches files under `clients/<package>/<release digest hex>/` with `object_store`.
For the control host, it reads files under `platform-ui/<platform version>/`.
It caches files in memory by their full versioned object path.
Digest paths carry `Cache-Control: public, max-age=31536000, immutable`.
The edge fetches `config.json` again for every request and returns `Cache-Control: no-cache`.
Unversioned page paths also return `no-cache`.

The nginx chart proxies web paths to `http://wamn-edge-web.edge.svc.cluster.local` with the original Host header.
It proxies `/api/` and `/password/` to their existing services.
It reads no bucket and needs no static digest or `bucketPath`.
Platform apply also installs the nginx chart below.

The verb runs a Job that requires HTTP 200 for `/` and `/config.json` on every declared host through the Rust Service.
With an empty `buckets.web`, this step prints `4.20 Edge bucket check: skipped: no bucket` and starts no Job.
The Rust edge still installs and rolls out.


Platform apply renders nginx values from the control host and environment documents.
It installs the `wamn-edge` release in `edge` and waits for its Deployment.
An unchanged release skips the Helm write.
On GCP, the Service requests the fixed network endpoint group (NEG) `wamn-edge` for HTTPS port 443.
The verb reads `cloud.google.com/neg-status` from that Service with `kubectl` and reports its name and zones.
A missing status after rollout refuses the install and names the annotation.
Kind prints `4.20 Edge NEG: skipped: kind`.

The project document declares a global TCP load balancer that sends all traffic to this NEG.
OpenTofu renders its health check, TCP backend service, target TCP proxy, global address, and forwarding rule on port 443.
It reads the NEG by its fixed name and declared zone.
It reads no environment files and renders no host rules, backend bucket, URL map, or certificate.
The connection target is 1,000 concurrent connections per endpoint.

The order is platform apply, OpenTofu apply, then platform apply again.
The first run creates the NEG before the load balancer can use it.
The agent renders `deploy/gcp/project.k` as in section 4.2 and runs `tofu plan` on that rendered directory as `wamn-agent@wamn-dev`.
The agent posts the plan, and it runs `tofu apply` of the saved plan only after the owner approves it, the same approval as a cluster resize (owner ruling 2026-10-23).
The diagnostic `tofu apply deploy/gcp` names this step.
The next platform apply skips unchanged resources and runs the final HTTPS checks.
The verb never runs OpenTofu or `gcloud compute`.

nginx terminates TLS with `edge/wamn-edge-tls`.
On GCP, cert-manager issues that Secret through `deploy/gcp/letsencrypt.yaml`.
On kind, the existing `edge-self-signed` issuer supplies the same Secret.
The load balancer holds no certificate and there is no certificate-copy CronJob.

### 4.21 Check

Platform apply runs this step last.
For every route host, `/` and `/config.json` must answer 200 when `buckets.web` is set.
A protected API route must answer 401 without credentials.
Each response must present the certificate serial in `edge/wamn-edge-tls`.
The verb reads only the public certificate from the Secret.
A failure names the host and the failing check.
On GCP, HTTPS connection failures and unexpected status codes also name `tofu apply deploy/gcp` as a likely missing step.

On kind, curl Jobs reach the edge Service through the cluster while retaining the route hostname for TLS and HTTP.
They trust the public certificate from the Secret and compare the served serial.
They use no port-forward.
The public load balancer prints `skipped: kind`.
An empty web bucket prints `skipped: no bucket` for `/` and `/config.json`.
The protected API checks still run.

A ConfigMap in `platform`, `wamn-install-platform-https-check`, records successful statuses, serials, skips, and the input digest.
An unchanged rerun preserves that record and starts no HTTPS Job.
Changes to the document, release rows, proxy, or certificate require the checks again.
Install Jobs allow 900 seconds so Helm can report its 600-second timeout first.
The verb retains Job logs under the work directory's `jobs` directory, including failures.

## 5. Operations

### 5.1 Pool after the daily guard

The guard sets pool `main` to 0 nodes every day. Scale `pg`, `main` and `bench` back to 1 node each. The `Cluster` keeps its old `Ready` condition while its pod is gone, so wait for the pod `wamn-pg-1`.:

```bash
gcloud container clusters resize wamn --node-pool pg --num-nodes 1 --zone us-central1-a --project wamn-dev --quiet
gcloud container clusters resize wamn --node-pool main --num-nodes 1 --zone us-central1-a --project wamn-dev --quiet
gcloud container clusters resize wamn --node-pool bench --num-nodes 1 --zone us-central1-a --project wamn-dev --quiet
until kubectl -n platform get pod wamn-pg-1 >/dev/null 2>&1; do sleep 5; done
kubectl -n platform wait pod/wamn-pg-1 --for=condition=Ready --timeout=900s
kubectl -n platform rollout status statefulset/evt-nats --timeout=600s
```

The port-forwards end with their pods, so start them again (sections 4.5 and 4.8).

### 5.2 Pause

Set pools `main`, `bench` and `pg` to 0 nodes. The PostgreSQL volume keeps the data:

```bash
gcloud container clusters resize wamn --project wamn-dev --zone us-central1-a --node-pool main --num-nodes 0 --quiet
gcloud container clusters resize wamn --project wamn-dev --zone us-central1-a --node-pool bench --num-nodes 0 --quiet
gcloud container clusters resize wamn --project wamn-dev --zone us-central1-a --node-pool pg --num-nodes 0 --quiet
gcloud compute instances list --project wamn-dev
```

The list must be empty.

### 5.3 Invite a user

`platform apply` invites the document's `owner_email` during step 4.19.1 with `--org-admin`.
A PostgreSQL Job reads whether the owner has a password or an unexpired, unused invitation.
Either state skips the invite Job.
Otherwise, the invite Job uses the database credential and operator certificate inside the cluster.
Identity sends the invitation by email.
The Job prints only a fixed success message, never the invitation link or secret.
The runner saves that log before it deletes the Job.
An unchanged rerun reads the current state and skips the invitation.

Other users receive invitations through the platform UI.

### 5.4 Rotate the Resend key

The owner makes the new key in Resend and writes it to a new mode 0600 file. The command removes the line end of the file. Replace the Secret and restart identity:

```bash
kubectl -n identity create secret generic identity-resend --from-literal=api-key="$(tr -d '[:space:]' < <file>)" --dry-run=client -o yaml | kubectl apply -f -
kubectl -n identity rollout restart deploy/identity
kubectl -n identity rollout status deploy/identity --timeout=300s
```

Send one recovery mail to an enrolled address to prove delivery. Then the owner revokes the old key in Resend and deletes the file.

### 5.5 Delete an environment

`wamn-ctl env delete <org/project/env> [--data]` deletes one environment, so that the next `env apply` of its document makes a new suffix (`docs/plan/platform-deploy.md` §12.3). It uninstalls the release chart, revokes the PATs, deletes the credential Secrets, the streams, the slot, the `Database` and the registry rows. With `--data` it also drops the database and its roles, and asks for the coordinate again at a terminal. Every step is "if present", so run it again after a failure:

```bash
. $HOME/.cache/wamn-install/platform.sh
use_env receiving
target/debug/wamn-ctl env delete dkk/receiving/dev
```

### 5.6 Upgrade an environment

`wamn-ctl upgrade-environment` takes one installed environment to the bytes of one commit (`docs/history/upgrade-environment.md`). An upgrade of an installed package also needs `--predecessor-release <digest>`, the release that the environment serves. The qualify stage starts kind clusters on this machine. No live run of the verb exists yet, and its first live run waits for the word of the owner (`wamn-m511.7`):

```bash
. $HOME/.cache/wamn-install/platform.sh
target/debug/wamn-ctl upgrade-environment --org dkk --project receiving --env dev --commit <40-hex commit>
```

### 5.7 CDC slot lost

If a CDC reader is down while the WAL passes `max_slot_wal_keep_size` (4 GB), Postgres invalidates its slot. The reader stays running and stopped, and logs `CDC_CAPTURE_GAP` once (`docs/plan/cdc-reader-slot.md` §4.3). Look at the slots:

```bash
kubectl -n platform exec wamn-pg-1 -c postgres -- psql -U postgres -Atc \
  "select slot_name, active, wal_status, invalidation_reason from pg_replication_slots"
```

`recover-capture-gap` drops the lost slot, makes it again under the same name and writes the open gap row. It connects as the observer user of the environment:

```bash
. $HOME/.cache/wamn-install/platform.sh
WAMN_PG_ADMIN_URL="$(db_url wms)" target/debug/wamn-ctl recover-capture-gap --org dkk --project wms --env dev \
  --nats-url nats://127.0.0.1:14222 --nats-username "$(cat $W/evt-wms/event-nats/observer-username)" \
  --nats-password-file $W/evt-wms/event-nats/observer-password
```

After the resync, or on the owner's word while no resync exists, run `close-capture-gap`. The reader resumes within 30 seconds:

```bash
. $HOME/.cache/wamn-install/platform.sh
target/debug/wamn-ctl close-capture-gap --org dkk --project wms --env dev
```

### 5.8 Benchmark

For a local host benchmark, use the [local run](benchmark.md#local-run).

The tier 2 benchmark runs from `wamn-bench-client` in project `wamn-dev`, zone `us-central1-a`.
The Receiving host runs alone on pool `bench`, and PostgreSQL runs alone on pool `pg`.
Every other pod runs on pool `main`.
Prepare the VM and its `~/wamn/target/release/wamn-bench` binary with [install history](../history/gcp-install-history.md#64-bench-client-and-pat), section 6.4.
Use the 1000-order Receiving seed from [install history](../history/gcp-install-history.md#61-morning-start-and-the-1000-seed), section 6.1.
Use the operator PAT from section 4.14 and the platform file from section 4.10.
Install Python 3 on the repository machine.
Keep the PostgreSQL forward on local port 15432 open for the VM's reverse SSH tunnel.
Use section 4.15.1 to run and commit the benchmark.

`tools/bench/tier.sh` runs get and update for 60 seconds each at c16, which means 16 concurrent requests.
It writes one `tests/bench/platform-<version>.json` file per platform version.
Each call records throughput, p50, p95, p99, calls, errors, and `pg_stat_statements` calls per request.
The script resets database statement counts before each call.
It divides each statement count by the request count, rounds each ratio, and sums the ratios.
This excludes setup queries and background polling from the per-request count.
The [benchmark page](benchmark.md) holds earlier measurements and the span breakdown.

The conformance test `benchmark::platform_benchmark_gate` compares the newest two result files in semantic version order.
It fails if get or update `throughput_per_sec` drops more than 20% at c16.
With fewer than two files, it passes and prints that no comparison ran.
The workspace sweep includes this test.
Run it from the repository root:

```bash
cargo test --offline -p wamn-conformance-tests benchmark::platform_benchmark_gate -- --nocapture
```
