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

[deploy/gcp/cnpg-cluster.yaml](../../deploy/gcp/cnpg-cluster.yaml) is `deploy/infra/cnpg-cluster.yaml` in namespace `platform`, with storage class `standard` (`pd-standard`). Its one instance holds the system database and every tenant database. Its `initdb` bootstrap creates the database `wamn_system` with the owner `wamn_system`, as `deploy/platform/wamn-sysdb.yaml` does. CloudNativePG keeps the owner login in the Secret `wamn-pg-app` and the superuser login in `wamn-pg-superuser`.

```bash
kubectl apply --server-side -f deploy/infra/cnpg-operator.yaml
kubectl -n cnpg-system rollout status deploy/cnpg-controller-manager --timeout=180s
kubectl apply -f deploy/gcp/cnpg-cluster.yaml
kubectl -n platform wait cluster/wamn-pg --for=condition=Ready --timeout=600s
```

On 2026-09-26 this took 76 seconds. The first cluster used the database `app`. It was empty, so it was deleted and applied again with `wamn_system`, in 43 seconds:

```bash
kubectl -n platform delete cluster wamn-pg --wait=true
kubectl -n platform wait pvc/wamn-pg-1 --for=delete --timeout=120s
kubectl apply -f deploy/gcp/cnpg-cluster.yaml
kubectl -n platform wait cluster/wamn-pg --for=condition=Ready --timeout=600s
```

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

```bash
R=us-central1-docker.pkg.dev/wamn-dev/wamn
for image in wamn-host:src-<identity> wamn-identity:src-<identity>; do
  docker tag $image $R/$image && docker push $R/$image
done
```

On 2026-09-26 the host image took 419 seconds to build and the identity image 59 seconds. The pushed images are:

| Image | Digest |
| --- | --- |
| `wamn-host:src-490a0d098a176e39` | `sha256:b44a6f944a410ca42dccf378c0948226946c54fefa17c4bf3cc246e25dcd9dd2` |
| `wamn-identity:src-bf477a549dc55932` | `sha256:b0896c8fb3f94920c097762f75019fe68a81254fc49a4fa6768a29db97794827` |


### 3.5 Event NATS

Write the users of the environment into a private directory. The org was `wamn` at first and is now `dkk` (section 3.7), so the program ran again and every Secret below was replaced with `kubectl create --dry-run=client -o yaml | kubectl apply -f -`. The example program calls `event_broker::prepare`, the derivation of the cluster tests, and adds the `tap-admin` user that creates the `WAMN_TAP` stream:

```bash
mkdir -m 700 <private directory>
cargo run -p wamn-test-infrastructure --example event_broker_files -- \
  <private directory> nats://evt-nats.platform.svc.cluster.local:4222 \
  dkk receiving dev dev 1 "$PWD/apps/wamn_receiving/wamn.json"
E=<private directory>/event-nats
```

The Receiving manifest declares no event registration, so the program makes no materializer consumer.

Make the two broker Secrets, then install the broker:

```bash
kubectl -n platform create secret generic evt-nats-authorization --from-file=authorization.conf=$E/authorization.conf
kubectl -n platform create secret generic evt-nats-bootstrap --from-file=context.json=$E/context.json
kubectl apply -f deploy/gcp/nats-jetstream.yaml
kubectl -n platform rollout status statefulset/evt-nats --timeout=300s
kubectl -n platform wait --for=condition=complete job/evt-nats-tap-stream --timeout=180s
```

Keep the other three users in `platform`. The provisioning step uses `evt-nats-provisioning`, and no host mounts these Secrets:

```bash
for role in provisioning publisher observer; do
  kubectl -n platform create secret generic evt-nats-$role \
    --from-file=username=$E/$role-username --from-file=password=$E/$role-password
done
```

Make the two host Secrets. The host connects as the `runtime` user, and the materializer reads the binding:

```bash
kubectl -n hosts create secret generic wamn-event-nats \
  --from-file=username=$E/runtime-username --from-file=password=$E/runtime-password \
  --from-literal=org=dkk --from-literal=project=receiving --from-literal=environment=dev \
  --from-literal=stream_replicas=1 --from-literal=dup_window_secs=120
kubectl -n hosts create secret generic wamn-materializer-nats --from-file=binding.json=$E/binding.json
```

On 2026-09-26 the first start failed. The broker crash-looped, because `deploy/infra/nats-jetstream.yaml` includes the authorization file by an absolute path, and nats-server resolves an include relative to the configuration file (finding `wamn-lrf1`). The Google Cloud copy uses a relative path. The tap-stream Job reached its backoff limit while the broker was down, so it was deleted and applied again. It then completed in 18 seconds.

After the Secrets change, restart the broker so that it reads the new users. `WAMN_TAP` uses memory storage, so a restart drops it by design. Run the tap-stream Job again after every event NATS restart:

```bash
kubectl -n platform delete pod evt-nats-0
kubectl -n platform rollout status statefulset/evt-nats --timeout=180s
kubectl -n platform delete job evt-nats-tap-stream
kubectl apply -f deploy/gcp/nats-jetstream.yaml
kubectl -n platform wait --for=condition=complete job/evt-nats-tap-stream --timeout=120s
```

Make sure that `WAMN_TAP` exists and that the provisioning user connects:

```bash
kubectl apply -f deploy/gcp/evt-nats-check.yaml
kubectl -n platform wait pod/evt-nats-check --for=jsonpath='{.status.phase}'=Succeeded --timeout=90s
kubectl -n platform logs evt-nats-check
kubectl -n platform delete pod evt-nats-check
```

The log shows `"config":{"name":"WAMN_TAP"` for tap-admin. For the provisioning user it shows `"err_code":10059,"description":"stream not found"`, because the source stream does not exist yet. A wrong password or permission gives an authorization or permissions error instead. On 2026-09-26 the restart took 23 seconds, the Job 24 seconds and the check 7 seconds.

### 3.6 System database

Install the control store and set the platform domain with `wamn-ctl provision-system`. Run it from this machine through a port-forward. Read the superuser password from its Secret into a variable, and do not print it:

```bash
kubectl -n platform port-forward svc/wamn-pg-rw 15432:5432 &
PW=$(kubectl -n platform get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
export WAMN_SYSTEM_ADMIN_URL="postgresql://postgres:${PW}@127.0.0.1:15432/wamn_system"
cargo build -p wamn-ctl
target/debug/wamn-ctl provision-system --platform-domain wamn.dev
```

The verb runs once. A second run refuses, because the schema `registry` exists. On 2026-09-26 it took 2 seconds. `deploy/sql/postgres-init.sql` is a test fixture and is not applied.

### 3.7 Org and project environment

The org id is `dkk`, because the id `wamn` is under the reserved `wamn` prefix. Keep the port-forward and `WAMN_SYSTEM_ADMIN_URL` of section 3.6. Record the org on the shared cluster `wamn-pg`:

```bash
target/debug/wamn-ctl provision-org --org dkk --template trials --pool wamn-pg
```

Render the project environment into a private directory `P`. The verb writes files and applies nothing:

```bash
target/debug/wamn-ctl provision-project-env --org dkk --project receiving --env dev --tenant dev \
  --namespace platform --secret-namespace hosts \
  --emit-database $P/database.json --emit-role-sql $P/role.sql \
  --emit-privilege-sql $P/privilege.sql --emit-secret $P/secret.json
```

Apply the files in this order. The verb writes the `Database` namespace as `wamn-system` and the Secret namespace from `--namespace`, so `jq` sets both (finding `wamn-6b4g`):

```bash
kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d postgres -v ON_ERROR_STOP=1 -q < $P/role.sql
jq '.metadata.namespace="platform"' $P/database.json | kubectl apply -f -
kubectl -n platform wait database/wamn-db-dkk--receiving--dev--zf7o454t --for=jsonpath='{.status.applied}'=true --timeout=120s
kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d postgres -v ON_ERROR_STOP=1 -q < $P/privilege.sql
jq '.metadata.namespace="hosts"' $P/secret.json | kubectl apply -f -
```

The instance suffix `zf7o454t` comes from the registry, so a new environment has a different database name. On 2026-09-26 `provision-org` took 2 seconds, `provision-project-env` 3 seconds, and the apply steps 8 seconds.

### 3.8 Identity issuer, run plane and package

Keep the port-forward and `WAMN_SYSTEM_ADMIN_URL` of section 3.6. Set the superuser URL of the tenant database, without printing it:

```bash
T="postgresql://postgres:${PW}@127.0.0.1:15432/wamn-db-dkk--receiving--dev--zf7o454t"
target/debug/wamn-ctl provision-identity-issuer --issuer https://identity.identity.svc.cluster.local \
  --prepare-generation a --namespace identity --emit-secret $P/identity-db.json
target/debug/wamn-ctl reconcile-run-plane --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$T" \
  --org dkk --project receiving --tenant dev --env dev --schema wamn_run
target/debug/wamn-ctl apply-package --package apps/wamn_receiving --database-url "$T" --tenant dev
target/debug/wamn-ctl reconcile-package-data-access --package apps/wamn_receiving --database-url "$T" --tenant dev
```

`reconcile-run-plane` runs before `apply-package`, because it installs the catalog schema that `apply-package` writes into. On 2026-09-26 the four verbs took 4, 13, 37 and 16 seconds.

The verbs copy the host of the admin URL, the port-forward `127.0.0.1:15432`, into every credential URL they emit. Set the cluster host with `jq` before you apply such a Secret (finding `wamn-lczu`):

```bash
jq '.stringData.url |= sub("@127\\.0\\.0\\.1:15432/"; "@wamn-pg-rw.platform.svc.cluster.local:5432/")' \
  $P/identity-db.json > $P/identity-db.cluster.json
kubectl apply -f $P/identity-db.cluster.json
```

### 3.9 Component

Write a push credential for Artifact Registry into a private temporary directory. The token goes through a pipe, so it never appears on a command line. `wamn-ctl` reads only the `username` and `password` fields:

```bash
A=$(mktemp -d); chmod 700 $A
gcloud auth print-access-token | python3 -c '
import json, os, sys
token = sys.stdin.read().strip()
fd = os.open(os.path.join(sys.argv[1], "config.json"), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.write(fd, json.dumps({"auths": {"us-central1-docker.pkg.dev": {"username": "oauth2accesstoken", "password": token}}}).encode())
os.close(fd)' $A
```

Push the component. `--declaration-template` renders `publication/components/receiving.json.in` with the tenant and the base digests of `wamn.json`:

```bash
export WAMN_PG_ADMIN_URL="$T"
target/debug/wamn-ctl push-component --package apps/wamn_receiving \
  --component-bytes target/virtualized/std-empty-environment/receiving.wasm \
  --declaration-template apps/wamn_receiving/publication/components/receiving.json.in --tenant dev \
  --artifact-base us-central1-docker.pkg.dev/wamn-dev/wamn/components --registry-auth-file $A/config.json \
  --admit-platform-package wamn:node --admit-platform-package wamn:postgres
rm -rf $A
```

The printed digest must equal the `sha256sum` of the local `receiving.wasm`. On 2026-09-26 both were `sha256:36b94783af587a75c3054deeb7ab4f721a89a3832b3b2d788e86a60981a36d0e`, and the push took 14 seconds. This credential is for the operator push only. The host pulls with the CronJob token of finding `wamn-i87m`.

### 3.10 Identity

Make the serving certificate and the operator CA. Copy the CA of the serving certificate into `hosts` and `edge`:

```bash
kubectl apply -f deploy/gcp/identity-certificate.yaml -f deploy/gcp/identity-operator-ca.yaml
kubectl -n identity wait certificate --all --for=condition=Ready --timeout=120s
kubectl -n identity get secret identity-tls -o jsonpath='{.data.ca\.crt}' | base64 -d > $P/identity-ca.crt
for ns in hosts edge; do kubectl -n $ns create configmap identity-ca --from-file=ca.crt=$P/identity-ca.crt; done
```

To send an invitation, the owner reads `tls.crt` and `tls.key` of the Secret `operator-dkk` into mode 0600 files. Never commit them.

Prepare the session target, set its database host inside `target.json`, and apply it:

```bash
target/debug/wamn-ctl provision-project-env --org dkk --project receiving --env dev --tenant dev \
  --namespace identity --target-admin-database-url "$T" \
  --prepare-session-role-reader-generation a --emit-session-role-reader-secret $P/session-target.json
jq '.stringData["target.json"] |= (fromjson | .database_url |= sub("@127\\.0\\.0\\.1:15432/"; "@wamn-pg-rw.platform.svc.cluster.local:5432/") | tojson)' \
  $P/session-target.json > $P/session-target.cluster.json
kubectl apply -f $P/session-target.cluster.json
```

The verb prints two PostgreSQL warnings that the `postgres` role did not grant a role membership. The generation still authenticates, as the verb reports. On 2026-09-26 the certificates took 5 seconds and the session target 11 seconds.

The owner creates the Secret `identity-resend` (namespace `identity`, key `api-key`) from a file, as plan section 4.2 states. Then install identity with [values-identity.yaml](../../deploy/gcp/values-identity.yaml), which names the image by digest:

```bash
kubectl -n identity get secret identity-resend
helm install identity deploy/platform/identity -n identity -f deploy/gcp/values-identity.yaml
kubectl -n identity rollout status deploy/identity --timeout=300s
```

Make sure that identity answers over TLS from a pod in `hosts`, verified against `identity-ca`:

```bash
kubectl -n hosts run identity-check --restart=Never --rm -i --quiet \
  --image=curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777 \
  --overrides='{"spec":{"volumes":[{"name":"ca","configMap":{"name":"identity-ca"}}],"containers":[{"name":"identity-check","image":"curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777","command":["curl","-sS","-w","\\nHTTP %{http_code}\\n","--cacert","/ca/ca.crt","https://identity.identity.svc.cluster.local/.well-known/jwks.json"],"volumeMounts":[{"name":"ca","mountPath":"/ca"}]}]}}'
```

On 2026-09-26 the install took 8 seconds, and the check returned `{"keys":[]}` with `HTTP 200`. The key set is empty until a session key is published.

### 3.11 Host Secrets and registry token account

Prepare generation `a` of the five host credentials, one family for each run. `identity-reader` addresses the system database, so it takes no `--target-admin-database-url`:

```bash
for f in guest executor-platform http-admitter event-materializer; do
  target/debug/wamn-ctl provision-project-env --org dkk --project receiving --env dev --tenant dev \
    --namespace hosts --target-admin-database-url "$T" --prepare-$f-generation a --emit-$f-secret $P/$f.json
done
target/debug/wamn-ctl provision-project-env --org dkk --project receiving --env dev --tenant dev \
  --namespace hosts --prepare-identity-reader-generation a --emit-identity-reader-secret $P/identity-reader.json
```

Set the database host in each Secret, name the guest Secret `wamn-host-db`, and apply them:

```bash
for f in guest executor-platform identity-reader http-admitter event-materializer; do
  filter='.stringData.url |= sub("@127\\.0\\.0\\.1:15432/"; "@wamn-pg-rw.platform.svc.cluster.local:5432/")'
  [ $f = guest ] && filter="$filter | .metadata.name = \"wamn-host-db\""
  (umask 077; jq "$filter" $P/$f.json > $P/$f.cluster.out) && kubectl apply -f - < $P/$f.cluster.out
done
```

On 2026-09-26 the five generations took 107 seconds and the apply 13 seconds.

Create the Google service account of the registry token CronJob. Give it read access to repository `wamn` only, and bind it to the Kubernetes service account `hosts/registry-token`:

```bash
gcloud iam service-accounts create wamn-registry-reader --project wamn-dev --display-name "wamn registry token CronJob"
gcloud artifacts repositories add-iam-policy-binding wamn --project wamn-dev --location us-central1 \
  --member serviceAccount:wamn-registry-reader@wamn-dev.iam.gserviceaccount.com --role roles/artifactregistry.reader
gcloud iam service-accounts add-iam-policy-binding wamn-registry-reader@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
  --role roles/iam.workloadIdentityUser --member "serviceAccount:wamn-dev.svc.id.goog[hosts/registry-token]"
```

If the repository binding fails with "Service account ... does not exist", the new account is not visible yet. Make sure that `gcloud iam service-accounts describe` shows it, wait a short time, and run the binding again. On 2026-09-26 the first binding failed this way, and the second one succeeded.

### 3.12 Registry token CronJob

[registry-token.yaml](../../deploy/gcp/registry-token.yaml) holds the service account, the empty Secret `wamn-registry-pull`, the Role that allows only `get` and `patch` on that Secret, and the CronJob. Apply it, run the job once by hand, and make sure that the Secret holds a token:

```bash
kubectl apply -f deploy/gcp/registry-token.yaml
kubectl -n hosts create job registry-token-first --from=cronjob/registry-token
kubectl -n hosts wait job/registry-token-first --for=condition=complete --timeout=180s
kubectl -n hosts logs job/registry-token-first
kubectl -n hosts get secret wamn-registry-pull -o jsonpath='{.data.\.dockerconfigjson}' | base64 -d \
  | jq -c '.auths|to_entries[]|{host:.key,user:.value.username,password_length:(.value.password|length)}'
```

The log shows only `patch status 200`. Make sure that the token comes from `wamn-registry-reader` and not from the node account:

```bash
kubectl -n hosts run wi-check --restart=Never --rm -i --quiet \
  --image=curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777 \
  --overrides='{"spec":{"serviceAccountName":"registry-token"}}' -- \
  curl -sS -H "Metadata-Flavor: Google" http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/email
```

On 2026-09-26 the first run took 10 seconds, and the email was `wamn-registry-reader@wamn-dev.iam.gserviceaccount.com`.

### 3.13 Pinned images

| Image | Digest | Use |
| --- | --- | --- |
| `wamn-host:src-490a0d098a176e39` | `sha256:b44a6f944a410ca42dccf378c0948226946c54fefa17c4bf3cc246e25dcd9dd2` | host |
| `wamn-identity:src-bf477a549dc55932` | `sha256:b0896c8fb3f94920c097762f75019fe68a81254fc49a4fa6768a29db97794827` | identity |
| `curlimages/curl:8.22.0` | `sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777` | registry token CronJob |

### 3.14 Host values

Run the host values program after `publish-release`, with the release artifact base and the manifest digest that `print-release-env` prints. The program calls `render_host_values`, the derivation of the kind cases, and writes both files:

```bash
cargo run -p wamn-test-infrastructure --example host_values_files -- \
  deploy/gcp <release artifact base> <release manifest digest>
```

The program makes these differences from kind:

| Difference | Reason |
| --- | --- |
| `WAMN_ORG` and the four role Secret names say `dkk`, not `acme`. | The org id is `dkk`, and the checked-in overlay names `acme`. |
| No `--allow-insecure-registries`. | The flag switches every registry to plain HTTP. Artifact Registry uses HTTPS with public roots. |
| No `OTEL_*` entries. | No collector runs on Google Cloud. |
| `global.nats.schedulerUrl` and `dataUrl` are `nats://nats.platform.svc.cluster.local:4222`. | The control NATS runs in `platform`. |
| The event NATS URL is in `platform`, and the images come from Artifact Registry by digest. | The event NATS runs in `platform`. |
| Requests are 500m CPU and 256Mi, and limits are 2 CPU and 4Gi. | One replica runs on an `e2-standard-2` node. |
| `WAMN_SESSION_ISSUER`, `WAMN_SESSION_INSTANCE_SUFFIX` and `WAMN_SESSION_JWKS_CA`, with the ConfigMap `identity-ca` at `/etc/identity-ca`. | The session routes need the identity issuer, as in `session_cluster::adjust_host` of the kind cases. |

The two `wamn-system` values in the base host group have no effect, because the overlay replaces the whole `hostGroups` list. Install the host as release `wamn-host`, with the base file first:

```bash
helm install wamn-host oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.10.0 -n hosts \
  -f deploy/gcp/values-host-base.yaml -f deploy/gcp/values-host.yaml --wait --timeout 5m
```

### 3.15 Session key

Publish a session key in the identity pod, and activate it with the printed `kid`:

```bash
kubectl -n identity exec deploy/identity -- wamn-identity publish
kubectl -n identity exec deploy/identity -- wamn-identity activate --kid <kid>
```

Run the check of section 3.10 again. The key set now lists the `kid`. On 2026-09-26 the key was `a24410b7-e7da-423c-944a-27479ed4b0f2`.

### 3.16 Management-author PAT (temporary procedure)

This procedure is temporary (finding `wamn-n5d1`). No in-cluster run path exists yet for the verbs that call identity. The owner adds this line to `/etc/hosts` before the mint and removes it after the mint:

```text
127.0.0.1 identity.identity.svc.cluster.local
```

Forward identity to port 8443. Read the `operator-dkk` certificate into mode 0600 files of a private directory `C`, and delete the directory after the mint:

```bash
kubectl -n identity port-forward svc/identity 8443:443 &
C=$(mktemp -d); chmod 700 $C
(umask 077
 kubectl -n identity get secret operator-dkk -o jsonpath='{.data.tls\.crt}' | base64 -d > $C/client.crt
 kubectl -n identity get secret operator-dkk -o jsonpath='{.data.tls\.key}' | base64 -d > $C/client.key)
```
