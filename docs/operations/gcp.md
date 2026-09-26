# Google Cloud

This page holds the commands of the Google Cloud deployment in project `wamn-dev`. The [plan](../plan/gcp-deployment.md) gives the reasons, the costs and the shutdown levels.
Each section is one finished step of that plan.

Run every command as the project owner, with `--project wamn-dev`. The default project of the gcloud configuration here is another project, so a command without `--project` acts on the wrong one.

## 1. Cost controls and storage

Step 1 ran on 2026-09-25, before any workload existed.

### 1.1 APIs

```bash
gcloud services enable --project wamn-dev \
  compute.googleapis.com container.googleapis.com storage.googleapis.com \
  artifactregistry.googleapis.com iam.googleapis.com cloudquotas.googleapis.com \
  billingbudgets.googleapis.com cloudbilling.googleapis.com pubsub.googleapis.com \
  cloudfunctions.googleapis.com run.googleapis.com cloudbuild.googleapis.com \
  eventarc.googleapis.com cloudscheduler.googleapis.com logging.googleapis.com
```

### 1.2 Quotas

A quota preference sets each cap. Google refuses a decrease of more than 10 percent without `--allow-high-percentage-quota-decrease`.

```bash
lower() {
  gcloud beta quotas preferences create --project=wamn-dev \
    --service=compute.googleapis.com --quota-id="$1" --preferred-value="$2" \
    --preference-id="$3" $4 --justification="wamn-dev test deployment cost cap" \
    --email=dkkloimwieder@gmail.com --allow-high-percentage-quota-decrease
}
lower CPUS-ALL-REGIONS-per-project 8 wamn-cpus-all-regions ""
lower CPUS-per-project-region 8 wamn-cpus-us-central1 --dimensions=region=us-central1
lower STATIC-ADDRESSES-per-project 1 wamn-global-static-addresses ""
lower DISKS-TOTAL-GB-per-project-region 200 wamn-disks-us-central1 --dimensions=region=us-central1
lower SSD-TOTAL-GB-per-project-region 0 wamn-ssd-us-central1 --dimensions=region=us-central1
```

The earlier values were 32, 200, 8, 4096 and 500.
Make sure that each preference has its granted value:

```bash
gcloud beta quotas preferences list --project wamn-dev \
  --format="value(quotaId,quotaConfig.grantedValue)"
```

To change a cap, run `gcloud beta quotas preferences update` with the same preference id. An increase can wait for Google review.

### 1.3 The guard

The source is in [deploy/gcp/guard](../../deploy/gcp/guard). Its `config.json` names the project, the zone and the cluster. Run its test first:

```bash
cd deploy/gcp/guard && python3 -m unittest
```

Create the topic, the service account and its two grants on the project:

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

Deploy the function. The Pub/Sub trigger calls it as the same service account, which needs the invoker role on the function.

```bash
gcloud functions deploy wamn-guard --gen2 --project wamn-dev --region us-central1 \
  --runtime python314 --source deploy/gcp/guard --entry-point on_message \
  --trigger-topic wamn-guard --service-account $SA --trigger-service-account $SA --quiet
gcloud run services add-iam-policy-binding wamn-guard --project wamn-dev \
  --region us-central1 --member=serviceAccount:$SA --role=roles/run.invoker
```

The invoker grant takes about a minute to apply. Until then, the log shows `403` answers, and Pub/Sub sends the message again.

Create the budget. Its `--billing-project` keeps the call on `wamn-dev`.

```bash
gcloud billing budgets create --billing-account=01E392-13CC0D-277806 \
  --billing-project=wamn-dev --display-name="wamn-dev" --budget-amount=150USD \
  --filter-projects=projects/wamn-dev \
  --threshold-rule=percent=0.5 --threshold-rule=percent=0.9 --threshold-rule=percent=1.0 \
  --notifications-rule-pubsub-topic=projects/wamn-dev/topics/wamn-guard \
  --credit-types-treatment=exclude-all-credits
```

Create the daily job:

```bash
gcloud scheduler jobs create pubsub wamn-guard --project wamn-dev --location us-central1 \
  --schedule="0 3 * * *" --time-zone="America/New_York" --topic=wamn-guard \
  --message-body='{"action":"scale-to-zero"}'
```

### 1.4 Test the guard

Run the daily job once, and read the guard log:

```bash
gcloud scheduler jobs run wamn-guard --project wamn-dev --location us-central1
gcloud logging read 'resource.type="cloud_run_revision" AND resource.labels.service_name="wamn-guard"' \
  --project wamn-dev --freshness=10m --format="value(timestamp,textPayload)"
```

Before the cluster exists, the log says `projects/wamn-dev/locations/us-central1-a/clusters/wamn does not exist, so no pool runs`.

The unlink test stops billing. Run it only before any workload exists, and relink billing at once.

```bash
gcloud pubsub topics publish wamn-guard --project wamn-dev \
  --message='{"costAmount":50.0,"budgetAmount":50.0,"alertThresholdExceeded":1.0,"currencyCode":"USD"}'
gcloud billing projects describe wamn-dev --format="value(billingEnabled)"
gcloud billing projects link wamn-dev --billing-account=01E392-13CC0D-277806
```

On 2026-09-25 the guard unlinked billing 7 seconds after the message, and billing was off for 8 seconds.
Afterwards the APIs, the quotas, the DNS zone and the function were unchanged, and a scale-to-zero message ran again.

### 1.5 Registry and buckets

```bash
gcloud artifacts repositories create wamn --project wamn-dev --location us-central1 \
  --repository-format=docker --description="wamn images and OCI artifacts"
gcloud storage buckets create gs://wamn-dev-web --project wamn-dev \
  --location us-central1 --uniform-bucket-level-access
gcloud storage buckets create gs://wamn-dev-backups --project wamn-dev \
  --location us-central1 --uniform-bucket-level-access
```

Make sure that no machine runs:

```bash
gcloud compute instances list --project wamn-dev
```

### 1.6 Remove the guard

Remove the guard only together with the project. Without it, nothing stops a machine that you forget.

```bash
gcloud scheduler jobs delete wamn-guard --project wamn-dev --location us-central1
gcloud billing budgets list --billing-account=01E392-13CC0D-277806 --billing-project=wamn-dev
gcloud billing budgets delete <budget id> --billing-account=01E392-13CC0D-277806 --billing-project=wamn-dev
gcloud functions delete wamn-guard --gen2 --project wamn-dev --region us-central1
gcloud pubsub topics delete wamn-guard --project wamn-dev
```

## 2. Cluster and certificate

Step 2 ran on 2026-09-26. The cluster is `wamn` in zone `us-central1-a`, on the regular release channel.

### 2.1 Prerequisite

`kubectl` reaches GKE through `gke-gcloud-auth-plugin`. Install it once on the machine:

```bash
sudo apt install google-cloud-cli-gke-gcloud-auth-plugin
```

`gcloud container clusters create` and `get-credentials` write the cluster into `~/.kube/config` and make it the current context. Other sessions on the machine use that file, so write the credentials into a file of your own:

```bash
KUBECONFIG=<your file> gcloud container clusters get-credentials wamn \
  --project wamn-dev --zone us-central1-a
```

On 2026-09-26 the create command changed the shared current context from `kind-wamn` to the GKE cluster for about 5 minutes. The context was set back, and the GKE entry was removed.

### 2.2 Network

The VPC has no firewall rule of ours. GKE adds its own rules, and step 4 adds the health check rule. There is no SSH rule.

```bash
gcloud compute networks create wamn --project wamn-dev --subnet-mode=custom
gcloud compute networks subnets create wamn-us-central1 --project wamn-dev \
  --network wamn --region us-central1 --range 10.10.0.0/24 \
  --secondary-range pods=10.20.0.0/16,services=10.30.0.0/20
```

| Range | Name | CIDR |
| --- | --- | --- |
| Nodes | primary | `10.10.0.0/24` |
| Pods | `pods` | `10.20.0.0/16` |
| Services | `services` | `10.30.0.0/20` |

### 2.3 Node service account

```bash
gcloud iam service-accounts create wamn-nodes --project wamn-dev --display-name="wamn GKE nodes"
NSA=wamn-nodes@wamn-dev.iam.gserviceaccount.com
for role in roles/logging.logWriter roles/monitoring.metricWriter roles/artifactregistry.reader; do
  gcloud projects add-iam-policy-binding wamn-dev --member=serviceAccount:$NSA --role=$role --condition=None
done
```

### 2.4 Cluster and pool

gcloud cannot name the first pool of a new cluster, so the cluster starts with a temporary `default-pool` of one Spot node. The secondary range flags need `--enable-ip-alias`.

```bash
gcloud container clusters create wamn --project wamn-dev --zone us-central1-a \
  --release-channel regular --network wamn --subnetwork wamn-us-central1 \
  --enable-ip-alias --cluster-secondary-range-name pods \
  --services-secondary-range-name services --workload-pool wamn-dev.svc.id.goog \
  --logging=SYSTEM --monitoring=SYSTEM --no-enable-managed-prometheus \
  --service-account $NSA --disk-type pd-standard --disk-size 30 \
  --num-nodes 1 --machine-type e2-standard-2 --spot
```

The pool `main` has a fixed size and no autoscaler, so the scale to 0 of the guard holds. Its upgrades use a surge of 0 and one unavailable node, so an upgrade stays inside the CPU quota.

```bash
gcloud container node-pools create main --cluster wamn --project wamn-dev \
  --zone us-central1-a --machine-type e2-standard-2 --spot --num-nodes 2 \
  --disk-type pd-standard --disk-size 30 --service-account $NSA \
  --max-surge-upgrade 0 --max-unavailable-upgrade 1
gcloud container node-pools delete default-pool --cluster wamn --project wamn-dev \
  --zone us-central1-a
```

### 2.5 cert-manager and the certificate

Install cert-manager from the repository manifest:

```bash
kubectl apply -f deploy/infra/cert-manager.yaml
kubectl -n cert-manager rollout status deploy/cert-manager deploy/cert-manager-webhook \
  deploy/cert-manager-cainjector
```

Give the cert-manager Kubernetes service account `roles/dns.admin` on the zone `wamn-dev` only. The Workload Identity principal needs no Google service account and no key. gcloud has no zone-level IAM command, so call the Cloud DNS API:

```bash
M=principal://iam.googleapis.com/projects/540250462877/locations/global/workloadIdentityPools/wamn-dev.svc.id.goog/subject/ns/cert-manager/sa/cert-manager
U=https://dns.googleapis.com/dns/v1/projects/wamn-dev/managedZones/wamn-dev
curl -X POST -H "Authorization: Bearer $(gcloud auth print-access-token)" \
  -H "x-goog-user-project: wamn-dev" -H "Content-Type: application/json" \
  -d "{\"policy\":{\"bindings\":[{\"role\":\"roles/dns.admin\",\"members\":[\"$M\"]}]}}" \
  $U:setIamPolicy
```

That call replaces the whole policy of the zone. Read it first with `$U:getIamPolicy`, and keep its other bindings.

[deploy/gcp/letsencrypt.yaml](../../deploy/gcp/letsencrypt.yaml) holds the ClusterIssuer `letsencrypt` and the Certificate `wamn-edge-tls` in namespace `edge`. Issue from the staging server first:

```bash
sed -e 's#acme-v02.api#acme-staging-v02.api#' -e 's#^      name: letsencrypt$#      name: letsencrypt-staging#' \
  deploy/gcp/letsencrypt.yaml | kubectl apply -f -
kubectl -n edge wait certificate/wamn-edge-tls --for=condition=Ready --timeout=600s
```

When staging issues, switch to production and issue again:

```bash
kubectl apply -f deploy/gcp/letsencrypt.yaml
kubectl wait clusterissuer/letsencrypt --for=condition=Ready --timeout=120s
kubectl -n edge delete secret wamn-edge-tls
kubectl -n edge wait certificate/wamn-edge-tls --for=condition=Ready --timeout=600s
kubectl -n cert-manager delete secret letsencrypt-staging
```

Make sure that the issuer is Let's Encrypt production, not `(STAGING)`:

```bash
kubectl -n edge get secret wamn-edge-tls -o jsonpath='{.data.tls\.crt}' | base64 -d \
  | openssl x509 -noout -issuer -subject -enddate
```

On 2026-09-26 staging issued in 86 seconds and production in 84 seconds. The production issuer was `CN=YR1`, and the certificate is valid until 2026-12-25.

### 2.6 Pause

```bash
gcloud container clusters resize wamn --project wamn-dev --zone us-central1-a \
  --node-pool main --num-nodes 0
gcloud compute instances list --project wamn-dev
```

The list must be empty.

### 2.7 The failed first attempts

On 2026-09-25 and 2026-09-26, two cluster creates and several deletes failed before the create above succeeded:

- The first create on the `default` network ran 32 minutes and ended with `Failed to create cluster`. Every patch of the `default` subnet got `The resource 'projects/wamn-dev/global/networks/default' is not ready`. In the same second, GKE created and deleted two Network Connectivity internal ranges. It also failed to grant a role to the Network Connectivity service agent, because that account did not exist yet.
- Two deletes of that cluster ended with `Failed to delete cluster`. A create of `wamn-dev` on the VPC `wamn` ended with `[INACTIVE_BILLING_STATE]: Project 'wamn-dev' cannot accept requests to 'compute.instanceGroupManagers.insert' while in an inactive billing state.` Its delete ended with `Timed out waiting for Google Compute Engine operation.`
- These failures started after the unlink test of section 1.4. Nobody found the cause. The owner deleted the clusters from the console, and they were gone at 05:47 UTC on 2026-09-26.

If a create stays in `PROVISIONING` with no machine, read the Compute errors instead of waiting:

```bash
gcloud logging read 'protoPayload.status.code>0' --project wamn-dev --freshness=30m \
  --format="value(timestamp,resource.type,protoPayload.methodName,protoPayload.status.message)"
```

## 3. Platform and Receiving

Step 3 started on 2026-09-26. This section grows as each part runs.
The cluster cases in kind are not a deployment to repeat. They run PostgreSQL, the event NATS and the registry as local Docker containers, and they provision and publish inside the test process. Step 3 uses the commands of [deployment](deployment.md) with the manifests in `deploy/infra`, `deploy/platform` and `deploy/gcp`.

Run `kubectl` and `helm` with your own kubeconfig file, as section 2.1 says.

### 3.1 Namespaces

| Namespace | Holds |
| --- | --- |
| `platform` | the runtime operator, its NATS and CA, the event NATS, PostgreSQL |
| `identity` | identity |
| `hosts` | the hosts |
| `edge` | the edge and the public certificate |
| `cert-manager`, `cnpg-system` | the two operators, in the namespaces that their manifests fix |

```bash
gcloud container clusters resize wamn --project wamn-dev --zone us-central1-a \
  --node-pool main --num-nodes 2
for n in platform identity hosts; do kubectl create namespace $n; done
```

### 3.2 Runtime operator and internal CA

[deploy/gcp/values-operator.yaml](../../deploy/gcp/values-operator.yaml) moves the watched and host namespaces to `hosts`.

```bash
helm upgrade --install --namespace platform wamn \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.10.0 \
  -f deploy/infra/values-wamn.yaml -f deploy/gcp/values-operator.yaml --wait --timeout 5m
sed -e 's/__ENVIRONMENT_NAMESPACE__/hosts/' -e 's/namespace: wamn-system/namespace: platform/' \
  deploy/platform/runtime-operator-events-rbac.example.yaml | kubectl apply -f -
```

The operator writes its CA into `platform/wasmcloud-ca`. cert-manager reads a ClusterIssuer CA from its own namespace, so copy the Secret there:

```bash
kubectl -n platform get secret wasmcloud-ca -o json | python3 -c "
import json,sys;d=json.load(sys.stdin);m=d['metadata'];d['metadata']={'name':m['name'],'namespace':'cert-manager'};print(json.dumps(d))" \
  | kubectl apply -f -
kubectl apply -f deploy/infra/wasmcloud-ca-issuer.yaml
kubectl wait --for=condition=Ready clusterissuer/wasmcloud-ca --timeout=60s
sed 's/__ENVIRONMENT_NAMESPACE__/hosts/' deploy/platform/host-environment-certs.example.yaml \
  | kubectl apply -f -
kubectl -n hosts wait --for=condition=Ready certificate/wasmcloud-runtime-tls \
  certificate/wasmcloud-data-tls --timeout=120s
```

On 2026-09-26 the operator install took 35 seconds, and the CA and certificates took 11 seconds.

### 3.3 PostgreSQL

[deploy/gcp/cnpg-cluster.yaml](../../deploy/gcp/cnpg-cluster.yaml) is `deploy/infra/cnpg-cluster.yaml` in namespace `platform`, with storage class `standard` (`pd-standard`). Its one instance holds the system database and every tenant database.

```bash
kubectl apply --server-side -f deploy/infra/cnpg-operator.yaml
kubectl -n cnpg-system rollout status deploy/cnpg-controller-manager --timeout=180s
kubectl apply -f deploy/gcp/cnpg-cluster.yaml
kubectl -n platform wait cluster/wamn-pg --for=condition=Ready --timeout=600s
```

On 2026-09-26 this took 76 seconds.

Backups are not configured. The `wamn-dev-backups` bucket exists, but no ObjectStore or ScheduledBackup uses it yet.

### 3.4 Images

Build the host and identity images by their source identity, with `TMPDIR` on the main disk, because `/tmp` has a per-user quota:

```bash
TMPDIR=<directory on the main disk> tools/journey-image-cache ensure . host host "$(git rev-parse HEAD)" gcp gcp-wamn
TMPDIR=<directory on the main disk> tools/journey-image-cache ensure . identity identity "$(git rev-parse HEAD)" gcp gcp-wamn
```

Push them with a Docker configuration of your own, so the shared `~/.docker/config.json` stays unchanged:

```bash
export DOCKER_CONFIG=<your directory>
gcloud auth print-access-token | docker login -u oauth2accesstoken --password-stdin https://us-central1-docker.pkg.dev
```

The nodes pull these images as `wamn-nodes`, which has `roles/artifactregistry.reader`, so no pull Secret exists.

