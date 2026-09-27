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
| `wamn-identity:src-cb10274981e78f44` | `sha256:0fe43f6bc52e98e327cb7abd9898a3e1268f9298e37ec56e12504fe7ffdeab42` |
| `wamn-identity:src-4d7d761fa53551b2` | `sha256:5ba5e9dc09043d36f6fbc2cf830b09d08dea2f5a63bde2be74dc246411a55164` |

On 2026-09-27 the second identity image, with the invitation link, took 53 seconds to build and 5 seconds to push. Roll it out with `helm upgrade identity deploy/platform/identity -n identity -f deploy/gcp/values-identity.yaml`.
On 2026-09-27 the third identity image, with the org and project filter of finding `wamn-9a3v`, took 102 seconds to build and 6 seconds to push. The rollout took 9 seconds.

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
| `wamn-host:src-b68fac526bdd771b` | `sha256:dc6b737abb8c0efc000489433d529939df015cb54901acf46f58180c2fb0aaa5` | host |
| `wamn-identity:src-4d7d761fa53551b2` | `sha256:5ba5e9dc09043d36f6fbc2cf830b09d08dea2f5a63bde2be74dc246411a55164` | identity |
| `wamn-cdc-reader:src-6bf15eedaa8cb8e8` | `sha256:fa38da50c62ff38679ecdcd3490df8650fb6a4b4d6adda3017fb134dadac797a` | CDC reader |
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

Run `provision-project-env` again with the PAT flags. The verb requires `--emit-secret`, so write that file into `C` too; it repeats the database Secret without a password and is never applied (finding `wamn-6b4g`):

```bash
(umask 077; target/debug/wamn-ctl provision-project-env --org dkk --project receiving --env dev --tenant dev \
  --namespace platform --secret-namespace hosts --emit-secret $C/database-secret.json \
  --pat-issuer https://identity.identity.svc.cluster.local:8443 --pat-server-ca $P/identity-ca.crt \
  --pat-client-cert $C/client.crt --pat-client-key $C/client.key \
  --emit-management-author-pat-secret $P/management-author-pat.json)
rm -rf $C
```

Then the owner removes the `/etc/hosts` line. On 2026-09-26 the mint took 5 seconds. It made the service principal `wamn-management-author-dkk--receiving--dev` and a PAT with prefix `6922769387dc9a19` that expires on 2026-10-27. The PAT Secret file stays at mode 0600 in the work directory and is not applied.

### 3.17 Pool after the daily guard

The guard job of section 1.3 sets pool `main` to 0 nodes every day at 03:00 New York time. On 2026-09-27 it removed both nodes, and every pod waited in `Pending`. Scale the pool back to 2 nodes for the work:

```bash
gcloud container clusters resize wamn --node-pool main --num-nodes 2 --zone us-central1-a --project wamn-dev --quiet
```

The event NATS starts again without `WAMN_TAP`, so run the tap-stream Job and the check of section 3.5 again. On 2026-09-27 the resize took 69 seconds, the pods were running 34 seconds later, the Job took 35 seconds and the check 6 seconds.

### 3.18 Event streams and CDC

Keep the port-forward and `WAMN_SYSTEM_ADMIN_URL` of section 3.6. Forward the event NATS, and write the replication password into a mode 0600 file of a private directory `C`:

```bash
kubectl -n platform port-forward svc/evt-nats 14222:4222 &
(umask 077; openssl rand -hex 32 > $C/replication-password)
```

Run `enable-cdc-project-env` as the provisioning user of section 3.5. It makes the source stream, records the reader registration and writes three files:

```bash
WAMN_REPLICATION_PASSWORD="$(cat $C/replication-password)" target/debug/wamn-ctl enable-cdc-project-env \
  --org dkk --project receiving --env dev --schema receiving \
  --stream-replicas 1 --dup-window-secs 120 \
  --db-host wamn-pg-rw.platform.svc.cluster.local --namespace platform --secret-namespace platform \
  --stream EVT_3_dkk_9_receiving_3_dev \
  --nats-url nats://127.0.0.1:14222 --nats-username "$(cat $E/provisioning-username)" \
  --nats-password-file $E/provisioning-password \
  --emit-role-sql $C/role.sql --emit-cdc-sql $C/cdc.sql --emit-secret $C/secret.json
```

Apply the role SQL, then the CDC SQL, then the Secret. Then delete the files that hold the password:

```bash
kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d postgres -v ON_ERROR_STOP=1 -q < $C/role.sql
kubectl -n platform exec -i wamn-pg-1 -c postgres -- \
  psql -U postgres -d wamn-db-dkk--receiving--dev--zf7o454t -v ON_ERROR_STOP=1 -q < $C/cdc.sql
kubectl apply -f $C/secret.json
rm -f $C/replication-password $C/role.sql $C/secret.json
```

The verb writes `role.sql` with mode 0664, so keep `C` at mode 0700. The `--db-host` flag puts the cluster host into the Secret URL, so no `jq` step is necessary here. On 2026-09-27 the verb took 3 seconds and the apply 5 seconds. The publication and the slot are `wamn_cdc_dkk__receiving__dev__zf7o454t`, and the Secret is `platform/wamn-cdc-dkk--receiving--dev`.

### 3.19 CDC reader

The reader reads the registration with generation `a` of the registry-reader credential, as the Receiving cluster tests do. Prepare it, set the cluster host (finding `wamn-lczu`), and apply it:

```bash
(umask 077; target/debug/wamn-ctl provision-project-env --org dkk --project receiving --env dev --tenant dev \
  --namespace platform --prepare-registry-reader-generation a --emit-registry-reader-secret $C/registry-reader.json)
(umask 077; jq '.stringData.url |= sub("@127\\.0\\.0\\.1:15432/"; "@wamn-pg-rw.platform.svc.cluster.local:5432/")' \
  $C/registry-reader.json > $C/registry-reader.cluster.json)
kubectl apply -f $C/registry-reader.cluster.json
rm -f $C/registry-reader.json $C/registry-reader.cluster.json
```

Build the image by its source identity and push it as in section 3.4:

```bash
TMPDIR=<directory on the main disk> tools/journey-image-cache ensure . cdc-reader cdc-reader "$(git rev-parse HEAD)" gcp gcp-wamn
docker tag wamn-cdc-reader:src-<identity> $R/wamn-cdc-reader:src-<identity> && docker push $R/wamn-cdc-reader:src-<identity>
```

The reader uses its default feedback and slot monitor intervals, 5 and 30 seconds. The kind values 1 and 0 are test-speed settings. The Deployment uses `strategy: Recreate`, because one replication slot has one reader. Deploy the reader, then restart the host so that it starts after the reader:

```bash
kubectl apply -f deploy/gcp/cdc-reader.yaml
kubectl -n platform rollout status deploy/cdc-reader --timeout=180s
kubectl -n platform logs deploy/cdc-reader
kubectl -n hosts rollout restart deploy/hostgroup-default
kubectl -n hosts rollout status deploy/hostgroup-default --timeout=300s
```

The reader log shows `registration loaded`, the three `preflight` lines and `walsender session open`. The slot then shows `active` as true in `pg_replication_slots`. On 2026-09-27 the credential took 12 seconds and its apply 2 seconds. The image build took 63 seconds, the push 6 seconds, the reader rollout 6 seconds and the host restart 10 seconds.

On 2026-09-27 the change to the defaults and `Recreate` rolled out in 5 seconds, and the reader opened its session again. The host crash-looped until the source stream existed. It then became Ready by itself at its next restart, before the reader started, so the ruled restart was its second start.


### 3.20 HTTP and materializer workloads

Build every component. The HTTP ingress and the materializer land in `apps/target/wasm32-wasip2/release`:

```bash
tools/build-components all
```

Push both with `wash`, as the kind cases do. Write the push credential into a mode 0600 file of a private directory `D`, and delete the directory after the push:

```bash
W=$(tools/install-wash)
R=us-central1-docker.pkg.dev/wamn-dev/wamn/components
(umask 077; gcloud auth print-access-token | python3 -c 'import sys,json,base64; t=sys.stdin.read().strip(); print(json.dumps({"auths":{"us-central1-docker.pkg.dev":{"username":"oauth2accesstoken","password":t,"auth":base64.b64encode(("oauth2accesstoken:"+t).encode()).decode()}}}))' > $D/config.json)
DOCKER_CONFIG=$D $W -o json oci push $R:flow-http apps/target/wasm32-wasip2/release/http_route.wasm
DOCKER_CONFIG=$D $W -o json oci push $R:materializer apps/target/wasm32-wasip2/release/materializer.wasm
rm -rf $D
```

Render both workloads with the example program, which calls `render_http_workload` and `render_materializer` of the kind cases, and apply them:

```bash
cargo run -p wamn-test-infrastructure --example workload_files -- deploy/gcp $R@<flow-http digest> $R@<materializer digest>
kubectl apply -f deploy/gcp/flow-http.yaml -f deploy/gcp/materializer.yaml
kubectl -n hosts wait --for=condition=Ready workloaddeployment/flow-http workloaddeployment/receiving-materializer --timeout=240s
```

The HTTP claims are catalog `default`, environment `hosts`, and project and schema `receiving`. The materializer fetches every 1000 ms and sweeps every 5000 ms. These are deployment values, and the kind cases use 500 ms for both.

| Component | File SHA-256 | Pushed digest |
| --- | --- | --- |
| `flow-http` (`http_route.wasm`) | `ad1124555cfaeb7f342d4596b5848c0675edefa1725c770c35fb94699d8130f4` | `sha256:f3d8d1bea0e3d9dd69dc2e30c004663a95ef234ed36a80faf4e8bcc35dc42fb4` |
| `materializer` | `7d78a4ad93a2484a3964e5d631e857f439316663f8227f4c08676659bf8f8153` | `sha256:5c310006273e00f8d24cbdd5604207162b723f4fbb7c545fb77d9ebcfe696db0` |

On 2026-09-27 the build took 353 seconds, the push 5 seconds, and the two workloads were Ready 8 seconds after the apply.

### 3.21 Serve check

Forward the host Service and call Receiving routes with the release host name:

```bash
kubectl -n hosts port-forward svc/hostgroup-default 18080:80 &
curl -s -i -H 'Host: receiving.wamn.dev' http://127.0.0.1:18080/location/list
curl -s -i -H 'Host: receiving.wamn.dev' http://127.0.0.1:18080/nope
```

A released route answers `401` with `{"error":{"code":"unauthorized"}}`, because the call has no session. An unknown path answers `404` with `route-not-found`.
Before the workloads existed, every path answered `503`, and the host log said `router is temporarily unavailable`. The host router in `crates/platform/engine/src/expected_router.rs` gives this answer when the release names the host but no workload is bound to it.

## 4. Public edge

Step 4 ran on 2026-09-27. Pool `main` stays at 2 nodes until step 4 ends. If the guard scales it to 0 first, scale it back as in section 3.17.

### 4.1 Edge

The edge chart renders no Google Cloud resource. Its `neg: true` value annotates the Service, so GKE creates the network endpoint group `wamn-edge` in the zone of the edge pod. The wildcard certificate of section 2.5 is already in `edge/wamn-edge-tls`, so the values set no issuer. Install the release `wamn-edge`:

```bash
helm install wamn-edge deploy/platform/edge -n edge -f deploy/gcp/values-edge.yaml --wait --timeout 3m
gcloud compute network-endpoint-groups list --project wamn-dev
```

The edge refuses to start until the Service `hosts/flow-http` of section 3.20 exists, because nginx resolves its upstream at start. On 2026-09-27 the first install timed out for this reason, and `helm upgrade` with the same values then took 43 seconds.

### 4.2 Web client

`wamn web upload` writes to Cloud Storage with a `gs://` sink and Application Default Credentials. No HMAC key exists. The owner runs this once on the machine. It stores a user OAuth token in the gcloud configuration, not a key:

```bash
gcloud auth application-default login
```

Build the `wamn` binary and upload the Receiving client of release 1:

```bash
cargo build -p wamn-ctl --bin wamn
target/debug/wamn web upload apps/wamn_receiving \
  --release sha256:aeb74968dfe09b113a115863ec8be5475923d9577914332081923052a41c16c4 \
  --bucket gs://wamn-dev-web/clients --org dkk
```

On 2026-09-27 the upload wrote 10 files in 17 seconds, after a 54 second build of `wamn`. The files go to `clients/wamn_receiving/<digest hex>/`, which `deploy/gcp/values-edge.yaml` and `deploy/gcp/url-map.yaml` name. The first ruling asked for an HMAC key on the owner's account. `gsutil hmac create` and `gcloud storage hmac create` accept only a service account, so that ruling was replaced.
The second upload on 2026-09-27 added the `/invite` page and took 10 seconds. Run the same command after each change to the client.
The `--org` value is a deployment value. The project comes from the client package name `@wamn/<project>-client`. The build receives both as `WAMN_ORG` and `WAMN_PROJECT`, and the shell offers and accepts only environments of that org and project. The third upload on 2026-09-27 added `--org` and took 15 seconds.

### 4.3 Load balancer

Create the resources in this order. The names come from the release `wamn-edge`, as in the removed Config Connector template:

```bash
P="--project wamn-dev"
gcloud compute firewall-rules create wamn-edge-health-check $P --network wamn --direction INGRESS \
  --allow tcp:8443 --source-ranges 35.191.0.0/16,130.211.0.0/22
gcloud compute health-checks create https wamn-edge $P --global --port 8443 --request-path /healthz
gcloud compute addresses create wamn-edge $P --global --ip-version IPV4
gcloud storage buckets add-iam-policy-binding gs://wamn-dev-web $P --member allUsers --role roles/storage.objectViewer
gcloud compute backend-buckets create wamn-edge-files $P --gcs-bucket-name wamn-dev-web \
  --enable-cdn --cache-mode USE_ORIGIN_HEADERS
```

Upload the certificate from its Secret through mode 0600 files of a private directory `C`, and delete the directory after the upload:

```bash
(umask 077
 kubectl -n edge get secret wamn-edge-tls -o jsonpath='{.data.tls\.crt}' | base64 -d > $C/tls.crt
 kubectl -n edge get secret wamn-edge-tls -o jsonpath='{.data.tls\.key}' | base64 -d > $C/tls.key)
gcloud compute ssl-certificates create wamn-edge $P --global --certificate $C/tls.crt --private-key $C/tls.key
rm -rf $C
```

The load balancer keeps this copy when cert-manager renews the Secret. The copy expires on 2026-12-25. Finding `wamn-ghx2.7` names the fix.

Create the backend service over the endpoint group, then the URL map, the proxy and the forwarding rule. `deploy/gcp/url-map.yaml` holds the rules of the removed template and the release path in the bucket:

```bash
gcloud compute backend-services create wamn-edge-platform $P --global \
  --load-balancing-scheme EXTERNAL_MANAGED --protocol HTTPS --health-checks wamn-edge
gcloud compute backend-services add-backend wamn-edge-platform $P --global \
  --network-endpoint-group wamn-edge --network-endpoint-group-zone us-central1-a \
  --balancing-mode RATE --max-rate-per-endpoint 100
gcloud compute url-maps import wamn-edge $P --global --source deploy/gcp/url-map.yaml --quiet
gcloud compute target-https-proxies create wamn-edge $P --global --url-map wamn-edge --ssl-certificates wamn-edge
gcloud compute forwarding-rules create wamn-edge $P --global --load-balancing-scheme EXTERNAL_MANAGED \
  --address wamn-edge --target-https-proxy wamn-edge --ports 443
```

Add the DNS record with the address:

```bash
gcloud dns record-sets create receiving.wamn.dev. $P --zone wamn-dev --type A --ttl 300 \
  --rrdatas "$(gcloud compute addresses describe wamn-edge $P --global --format='value(address)')"
```

On 2026-09-27 the address was `8.232.230.139`. The first six commands took 45 seconds, the backend service 97 seconds, the URL map 6 seconds, and the proxy, rule and record 29 seconds.

### 4.4 Measurements

Run these from outside the cluster:

```bash
curl -s -D - -o index.html https://receiving.wamn.dev/
A=$(grep -o '/assets/[^"]*\.js' index.html | head -1)
curl -s -D - -o /dev/null https://receiving.wamn.dev$A
curl -s -D - -o /dev/null https://receiving.wamn.dev$A
curl -s -D - -o /dev/null https://receiving.wamn.dev/api/location/list
curl -s -D - https://receiving.wamn.dev/password/session
```

On 2026-09-27 the results were these:

| Request | Result |
| --- | --- |
| `/` | 200, `cache-control: no-cache`, `text/html` |
| `/assets/index-CLU_h-Iu.js`, first | 200, `cache-control: public, max-age=31536000, immutable`, no `age` |
| the same asset, second | the same headers and `age: 3`, a Cloud CDN hit |
| `/api/location/list` | 401 `unauthorized`, `cache-control: no-store`, no `age` on two requests |
| `GET /password/session` | 404 `text/plain` `not found`, the answer of identity (`services/identity/src/lib.rs`), because the password routes take only POST. The bucket answers a missing key with XML `NoSuchKey`. |

### 4.5 Invitation

Make the owner's human principal, admit it to the environment, and write its tenant person row. Keep the port-forward and `WAMN_SYSTEM_ADMIN_URL` of section 3.6 and `T` of section 3.8:

```bash
cargo build -p wamn-ctl --features ops --bin wamn-ctl-ops
target/debug/wamn-ctl-ops create-human --subject dkkloimwieder@gmail.com --email dkkloimwieder@gmail.com --display-name dkk
target/debug/wamn-ctl grant-project-env-membership --org dkk --project receiving --env dev \
  --principal-id <principal id> --system-database-url "$WAMN_SYSTEM_ADMIN_URL"
target/debug/wamn-ctl reconcile-run-plane --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$T" \
  --org dkk --project receiving --tenant dev --env dev --schema wamn_run
```

Send the invitation through the identity port-forward and the temporary `/etc/hosts` line of section 3.16. Read the `operator-dkk` certificate into mode 0600 files of a private directory `C`, and delete the directory after the request:

```bash
target/debug/wamn-ctl invite --principal <principal id> \
  --pat-issuer https://identity.identity.svc.cluster.local:8443 --pat-server-ca $P/identity-ca.crt \
  --pat-client-cert $C/client.crt --pat-client-key $C/client.key
rm -rf $C
```

On 2026-09-27 the principal was `ccc4533d-a81a-465d-8a44-1414369ae2fd`. The three verbs took 2, 1 and 25 seconds, and identity answered `201 {"status":"accepted_for_delivery"}` in 1 second.
The first invitation showed a code for the terminal client, and the web client had no page to accept it (`wamn-ch2w`). With `inviteUrl` set in `deploy/gcp/values-identity.yaml`, the mail carries one link to `https://receiving.wamn.dev/invite#<code>`. The second invitation went out the same way and took 1 second. Restart the identity port-forward after identity rolls, because the forward ends with the old pod:

```bash
kubectl -n identity port-forward svc/identity 8443:443
```

Temporary step (`wamn-wq26`): no user role exists, so after sign-up the sign-in lists no environment. Give the owner the system role `route-caller` with one insert through the database port-forward, as the superuser, with the provisioning principal as the actor:

```sql
BEGIN;
SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true),
       set_config('app.operation', 'admin:temporary-route-caller-grant', true);
INSERT INTO app_system.user_roles (tenant_id, user_id, role_name)
  VALUES ('dev', 'ccc4533d-a81a-465d-8a44-1414369ae2fd', 'route-caller');
COMMIT;
```

Run it in database `wamn-db-dkk--receiving--dev--zf7o454t`. On 2026-09-27 it took 1 second. When `wamn-wq26` lands, remove this row and use `wamn-ctl grant-role`.

Load the small Receiving dataset through the same port-forward, as the superuser. It writes 10 items, 10 locations, 5 suppliers, 10 purchase orders and 55 lines:

```bash
psql -h 127.0.0.1 -p 15432 -U postgres -d wamn-db-dkk--receiving--dev--zf7o454t \
  -v ON_ERROR_STOP=1 -f apps/wamn_receiving/tests/fixtures/receiving-seed-small.sql
```

On 2026-09-27 the load took 3 seconds.

On 2026-09-27 the owner completed step 4 in a browser. The owner signed up from the mail link, signed in at `https://receiving.wamn.dev`, and changed the supplier of `PO-0006` to `SUPPLIER-0005`. Make sure that the change landed with this query in the Receiving database:

```sql
SELECT position, kind, operation, changed_by, after->>'supplier_id', after->>'row_version'
FROM receiving.purchase_order_history ORDER BY position DESC LIMIT 1;
```

The result was history row 11, kind `update`, operation `wamn-receiving:purchase-order/update@1.0.0`, by the owner's principal, at `row_version` 2.

### 4.6 Delete the public edge

Delete in the reverse order. The forwarding rule and the address bill while they exist:

```bash
gcloud dns record-sets delete receiving.wamn.dev. $P --zone wamn-dev --type A
gcloud compute forwarding-rules delete wamn-edge $P --global --quiet
gcloud compute target-https-proxies delete wamn-edge $P --global --quiet
gcloud compute url-maps delete wamn-edge $P --global --quiet
gcloud compute backend-services delete wamn-edge-platform $P --global --quiet
gcloud compute ssl-certificates delete wamn-edge $P --global --quiet
gcloud compute backend-buckets delete wamn-edge-files $P --quiet
gcloud storage buckets remove-iam-policy-binding gs://wamn-dev-web $P --member allUsers --role roles/storage.objectViewer
gcloud compute addresses delete wamn-edge $P --global --quiet
gcloud compute health-checks delete wamn-edge $P --global --quiet
gcloud compute firewall-rules delete wamn-edge-health-check $P --quiet
helm uninstall wamn-edge -n edge
```

## 5. Second application: WMS

WMS runs beside Receiving at `wms.wamn.dev`, behind the same certificate and load balancer. The edge chart takes a list of `applications`, one entry for each public host.

### 5.1 Label store

A WMS move stores a pallet label through the blob store capability. On Google Cloud the store is a Cloud Storage bucket, and the host signs with its pod's Google service account through Workload Identity. No HMAC key and no credential handle exist. Make the bucket, with uniform access and no public read:

```bash
gcloud storage buckets create gs://wamn-dev-labels --project wamn-dev --location us-central1 \
  --uniform-bucket-level-access --public-access-prevention
```

Make the Google service account `wamn-blob`, and give it `roles/storage.objectAdmin` on that bucket only:

```bash
gcloud iam service-accounts create wamn-blob --project wamn-dev --display-name "wamn host blob store"
gcloud storage buckets add-iam-policy-binding gs://wamn-dev-labels --project wamn-dev \
  --member serviceAccount:wamn-blob@wamn-dev.iam.gserviceaccount.com --role roles/storage.objectAdmin
```

If the bucket binding answers that the service account does not exist, the new account is not visible yet. Run the binding again after the account shows in `gcloud iam service-accounts describe`.

Let the host's Kubernetes service account `hosts/wamn-host-runtime-operator-runtime` act as `wamn-blob`:

```bash
gcloud iam service-accounts add-iam-policy-binding wamn-blob@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
  --role roles/iam.workloadIdentityUser \
  --member "serviceAccount:wamn-dev.svc.id.goog[hosts/wamn-host-runtime-operator-runtime]"
```

The host values annotate that service account with `iam.gke.io/gcp-service-account: wamn-blob@wamn-dev.iam.gserviceaccount.com`. The WMS label binding names the `gcs` provider and no credential handle:

```json
{"provider": "gcs", "container": "wamn-dev-labels", "prefix": "wms/"}
```

On 2026-09-27 the bucket, the account and the two bindings took 6 seconds. The first bucket binding failed because the account was not visible yet, and the second attempt passed.

The host values program writes the annotation into `runtime.serviceAccount` of `deploy/gcp/values-host-base.yaml`. The same run pins the host image `wamn-host:src-b68fac526bdd771b`, `sha256:dc6b737abb8c0efc000489433d529939df015cb54901acf46f58180c2fb0aaa5`, which carries the `gcs` store. Render the values again and upgrade the host:

```bash
cargo run -p wamn-test-infrastructure --example host_values_files -- \
  deploy/gcp us-central1-docker.pkg.dev/wamn-dev/wamn/releases <Receiving release manifest digest>
helm upgrade wamn-host oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.10.0 -n hosts \
  -f deploy/gcp/values-host-base.yaml -f deploy/gcp/values-host.yaml --wait --timeout 5m
```

Make sure that a pod with that service account gets the `wamn-blob` identity from the metadata server, with the check of section 3.12 and `"serviceAccountName":"wamn-host-runtime-operator-runtime"`. On 2026-09-27 the image build took 411 seconds, the push 13 seconds and the upgrade 24 seconds. The check printed `wamn-blob@wamn-dev.iam.gserviceaccount.com`, and Receiving still answered `/api/location/list` with 401.

### 5.2 WMS project environment

WMS uses org `dkk`, project `wms`, env `dev` and tenant `wms`. A tenant is one per project database, so tenant `dev` stays with Receiving. With tenant `dev`, `provision-project-env` refused with `tenant-environment-identity-projection-content-conflict`, after it had already recorded the registry row `dkk/wms/dev`. The run with tenant `wms` then reused that row.

Run the commands of sections 3.7, 3.8 and 3.11 with `--project wms --tenant wms` and the WMS database `wamn-db-dkk--wms--dev--bnarqpnc`. Name the guest Secret `wamn-host-db-wms`, because `wamn-host-db` belongs to Receiving in the same namespace. Prepare the session target of section 3.10 with `--project wms --tenant wms`, and apply it in `identity`. Add its Secret `wamn-session-role-reader-dkk--wms--dev` to `sessionTargetSecrets` in [values-identity.yaml](../../deploy/gcp/values-identity.yaml), and upgrade identity as in section 3.10. Without it, the sign-in page lists only Receiving, and the WMS host refuses the Receiving session with `You are not signed in.` On 2026-09-27 this step was first left out, and the upgrade then took 5 seconds.

On 2026-09-27 the project environment took 3 seconds and its apply 9 seconds. `reconcile-run-plane`, `apply-package` and `reconcile-package-data-access` took 12, 37 and 9 seconds. The session target took 21 seconds and the five host credentials 108 seconds.

### 5.3 WMS components, wiring and release

Build every component with `tools/build-components all`. The WMS components land in `apps/target/virtualized/std-empty-environment`, and `label_render.wasm` lands in `apps/platform/no-std/target/wasm32-wasip2/release`. Render the four declarations with tenant `wms`, package `wamn_wms` `1.0.0`, and the store alias `labels` for `blob-put`, as the WMS cluster case does:

```bash
for pair in wms:apps/wamn_wms/publication/components/wms.json.in \
  label-render:apps/platform/no-std/label-render/declaration.json.in \
  blob-put:apps/platform/execution/blob-put/declaration.json.in \
  jsonata:apps/platform/execution/jsonata/declaration.json.in; do
  sed -e 's/__TENANT_ID__/wms/g; s/__PACKAGE_ID__/wamn_wms/g; s/__PACKAGE_VERSION__/1.0.0/g; s/__STORE_ALIAS__/labels/g' \
    ${pair#*:} > $P/${pair%%:*}.declaration.json
done
```

Push each component with the credential file of section 3.9, `--package apps/wamn_wms --tenant wms --declaration $P/<name>.declaration.json`, and these admitted packages:

| Component | Bytes | `--admit-platform-package` | Digest on 2026-09-27 |
| --- | --- | --- | --- |
| `wms` | `wms.wasm` | `wamn:node`, `wamn:postgres` | `sha256:db401f0d89c1059e3d396b0643f26277c611f2dc65279b7f2ace64f18df91c87` |
| `label-render` | `label_render.wasm` | `wamn:node` | `sha256:57852602eddef0be442587ba7cc14eddfc4c73b049d3e9c8d85bf3ebc321b4c1` |
| `blob-put` | `blob_put.wasm` | `wamn:node`, `wasmcloud:blobstore` | `sha256:d93e0c6662ac885d1793b29dfe7a390e79cc494ca7653d58b990a74d6dcd2f8c` |
| `jsonata` | `jsonata_expression.wasm` | `wamn:node` | `sha256:4da6d8c78df00e81ea29a02b931ab553abc31ae86556749359294f7fbfa4b64c` |

Each printed digest equals the `sha256sum` of the local file.

The wiring `inventory_move_and_label` passes the authoring gate before `author-wiring` records it. Prepare the gate credentials `control-author` and `management-admitter` with `--prepare-<family>-generation a` into a private directory, and do not apply them. Mint the WMS management-author PAT as in section 3.16, with `--project wms --tenant wms`. Run the gate service on this machine, with the URLs of the identity-reader, control-author and management-admitter files:

```bash
cargo build -p wamn-scenario-worker
WAMN_SYSTEM_URL=<identity-reader url> WAMN_CONTROL_AUTHORING_PG_URL=<control-author url> \
WAMN_MANAGEMENT_ADMISSION_PG_URL=<management-admitter url> WAMN_MANAGEMENT_ORG=dkk \
WAMN_MANAGEMENT_PROJECT=wms WAMN_MANAGEMENT_ENVIRONMENT=dev WAMN_MANAGEMENT_TENANT=wms \
  target/debug/wamn-scenario-worker serve --bind 127.0.0.1:18090 &
cargo run -p wamn-test-infrastructure --example gate_request -- wamn_wms 1.0.0 wms dev \
  apps/wamn_wms/publication/wirings/inventory_move_and_label.json > $P/gate-request.json
```

Post `gate-request.json` to `http://127.0.0.1:18090/authoring` with the PAT as a bearer token, read from its file through a pipe. The reply has `body.outcome.status` `completed` and a `report-id`. Stop the service, then record the wiring, publish, bind the label store and push the manifest:

```bash
target/debug/wamn-ctl author-wiring --database-url "$T" --control-database-url "$SYS" --tenant wms \
  --package-id wamn_wms --package-version 1.0.0 --wiring-document apps/wamn_wms/publication/wirings/inventory_move_and_label.json
target/debug/wamn-ctl publish-release --database-url "$T" --control-database-url "$SYS" --org dkk --project wms \
  --tenant wms --effective-release-id 1 --environment dev --verified-publisher-principal wamn-management-author-dkk--wms--dev \
  --run-schema wamn_run --package wamn_wms@1.0.0 --wiring "wamn_wms@1.0.0::inventory_move_and_label=2" \
  --attachments apps/wamn_wms/publication/attachments.json --route-host wms.wamn.dev --package-manifest apps/wamn_wms/wamn.json
echo '{"provider":"gcs","container":"wamn-dev-labels","prefix":"wms/"}' > $P/labels-store.definition.json
target/debug/wamn-ctl bind-connection --database-url "$T" --tenant wms --environment dev --instance-id labels-store \
  --requirement-type blobstore --definition $P/labels-store.definition.json --effective-release-id 1 \
  --component-digest <blob-put digest> --store-alias labels
target/debug/wamn-ctl push-release-manifest --database-url "$T" --control-database-url "$SYS" --org dkk --project wms \
  --tenant wms --effective-release-id 1 --artifact-base us-central1-docker.pkg.dev/wamn-dev/wamn/releases --registry-auth-file $A/config.json
```

Receiving ran the same `publish-release`, `push-release-manifest` and `print-release-env` commands with `--project receiving --tenant dev`, no `--wiring`, and its own attachments and manifest.

On 2026-09-27 the pushes took 17, 11, 10 and 20 seconds, the gate 3 seconds, `author-wiring` 2 seconds, `publish-release` 10 seconds, `bind-connection` 2 seconds and the manifest push 6 seconds. The WMS release manifest is `sha256:6649148172e83eea8af3c9a5f133cb5f961d634de4f85244909da046475890a3`. The first PAT mint was refused before it reached identity, because a short Kubernetes API timeout left the password read empty. Stop when a credential read returns nothing.

### 5.4 WMS event streams and CDC reader

The event NATS serves both environments. Its users are named after their stream, so the users of the two environments do not clash. Write the WMS users with tenant `wms` and the WMS manifest:

```bash
cargo run -p wamn-test-infrastructure --example event_broker_files -- \
  $P/evt nats://evt-nats.platform.svc.cluster.local:4222 dkk wms dev wms 1 "$PWD/apps/wamn_wms/wamn.json"
E=$P/evt/event-nats
```

The program derives a consumer from each registration of the manifest, handlers and workflows. WMS has one, `mat_wms_wamn_wms_movement_label` on `evt.dkk.wms.dev.inventory_movement.>`, from the workflow `movement_label`. The program writes its configuration into `consumers.jsonl` for `enable-cdc-project-env`. The first run on 2026-09-27 took handlers only, so the WMS materializer user had no consumer. The program was corrected and run again, and the users were replaced.

Read the current `authorization.conf` from the Secret `evt-nats-authorization` into a mode 0600 file. Keep its Receiving users and `tap-admin`, add the five WMS users of `$E/authorization.conf` without their `tap-admin`, and apply the result. Make the WMS user Secrets. Restart the broker and run the tap-stream Job again as in section 3.5:

```bash
kubectl -n platform create secret generic evt-nats-authorization \
  --from-file=authorization.conf=$P/evt/merged-authorization.conf --dry-run=client -o yaml | kubectl apply -f -
for role in provisioning publisher observer; do
  kubectl -n platform create secret generic evt-nats-wms-$role --from-file=username=$E/$role-username --from-file=password=$E/$role-password
done
kubectl -n hosts create secret generic wamn-event-nats-wms \
  --from-file=username=$E/runtime-username --from-file=password=$E/runtime-password \
  --from-literal=org=dkk --from-literal=project=wms --from-literal=environment=dev \
  --from-literal=stream_replicas=1 --from-literal=dup_window_secs=120
kubectl -n hosts create secret generic wamn-materializer-nats-wms --from-file=binding.json=$E/binding.json
```

The restart stops the Receiving event connections for a few seconds. The Receiving CDC reader logged `connected successfully` 11 seconds after the restart, and the host warnings stopped at the same time. After a broker restart, restart the port-forward of section 3.18 too, because it still points at the old pod.

Enable CDC for WMS as in section 3.18, with `--project wms --schema wms --stream EVT_3_dkk_3_wms_3_dev`, the WMS provisioning user, and `--consumer-config "$(cat $P/evt/consumers.jsonl)"`. Apply its role SQL, its CDC SQL in `wamn-db-dkk--wms--dev--bnarqpnc`, and its Secret. Prepare the WMS registry-reader credential as in section 3.19 and apply it. Then deploy the reader of [cdc-reader-wms.yaml](../../deploy/gcp/cdc-reader-wms.yaml):

```bash
kubectl apply -f deploy/gcp/cdc-reader-wms.yaml
kubectl -n platform rollout status deploy/cdc-reader-wms --timeout=180s
```

On 2026-09-27 the Secrets took 6 seconds, the restart and the Job 41 seconds, `enable-cdc-project-env` 3 seconds, its apply 5 seconds, the registry-reader credential 18 seconds and the reader rollout 3 seconds. The reader logged `registration loaded`, the three `preflight` lines and `walsender session open`.

### 5.5 WMS host group and workloads

The host values program renders the WMS overlay as host group `wms` next to Receiving. It names the WMS guest, event NATS and materializer Secrets and the instance suffix `bnarqpnc`. It removes the object-store credentials of the kind overlay, because the `gcs` store needs none. Render with both release digests and upgrade the host:

```bash
cargo run -p wamn-test-infrastructure --example host_values_files -- deploy/gcp \
  us-central1-docker.pkg.dev/wamn-dev/wamn/releases <Receiving manifest digest> <WMS manifest digest>
helm upgrade wamn-host oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.10.0 -n hosts \
  -f deploy/gcp/values-host-base.yaml -f deploy/gcp/values-host.yaml --wait --timeout 6m
```

Push the HTTP ingress and the materializer of the current build with `wash`, as in section 3.20, under the tags `wms-flow-http` and `wms-materializer`. Render the four workloads and apply the WMS ones:

```bash
cargo run -p wamn-test-infrastructure --example workload_files -- deploy/gcp \
  $R@<Receiving flow-http digest> $R@<Receiving materializer digest> $R@<WMS flow-http digest> $R@<WMS materializer digest>
kubectl apply -f deploy/gcp/wms-flow-http.yaml -f deploy/gcp/wms-materializer.yaml
kubectl -n hosts wait --for=condition=Ready workloaddeployment/wms-flow-http workloaddeployment/wms-materializer --timeout=240s
```

| Component | File SHA-256 | Pushed digest |
| --- | --- | --- |
| `wms-flow-http` (`http_route.wasm`) | `09e74ca57c22b7f396f1f07adb7cbcfa4d65ae4e37dcb1285fc97d8e82dd88a9` | `sha256:923f270bb7425cad48e46a12fa3bb5843f9b1bd174851298ee68485cc8deea7e` |
| `wms-materializer` | `7d78a4ad93a2484a3964e5d631e857f439316663f8227f4c08676659bf8f8153` | `sha256:581fd2ed52bf349d2adc6cfbad22d0b2c33b157c99f3ef1b3a61aa92bac6ca7a` |

On 2026-09-27 the upgrade took 17 seconds, and the WMS host loaded release 1 with 20 routes and 4 components. The push took 5 seconds, and the workloads were Ready 6 seconds after the apply. Through a port-forward to `hostgroup-wms`, `GET /pallet/query` with `Host: wms.wamn.dev` answered 401 and an unknown path 404.

Give the owner WMS access as in section 4.5: `grant-project-env-membership --project wms`, then `reconcile-run-plane --project wms --tenant wms`, then the temporary `route-caller` insert with tenant `wms` in the WMS database. Load the small WMS dataset inside the database pod, because the port-forward of section 3.6 drops after one connection at times:

```bash
kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d wamn-db-dkk--wms--dev--bnarqpnc \
  -v ON_ERROR_STOP=1 -q < apps/wamn_wms/tests/fixtures/wms-seed-small.sql
```

It writes 10 locations, 10 pallets, 10 products and 19 pallet quantities. On 2026-09-27 the grant took under 1 second, the reconcile 12 seconds, the role insert 1 second and the load 1 second.

### 5.6 WMS public edge

Upload the WMS client of release 1, as in section 4.2:

```bash
target/debug/wamn web upload apps/wamn_wms \
  --release sha256:6649148172e83eea8af3c9a5f133cb5f961d634de4f85244909da046475890a3 --bucket gs://wamn-dev-web/clients \
  --org dkk
```

[values-edge.yaml](../../deploy/gcp/values-edge.yaml) lists WMS with its host, `http://wms-flow-http.hosts.svc.cluster.local` and its bucket path. [url-map.yaml](../../deploy/gcp/url-map.yaml) has the host rule `wms.wamn.dev` and the path matcher `wms`, with the same rules as Receiving. Upgrade the edge, import the URL map and add the record:

```bash
helm upgrade wamn-edge deploy/platform/edge -n edge -f deploy/gcp/values-edge.yaml --wait --timeout 3m
gcloud compute url-maps import wamn-edge --global --project wamn-dev --source deploy/gcp/url-map.yaml --quiet
gcloud dns record-sets create wms.wamn.dev. --project wamn-dev --zone wamn-dev --type A --ttl 300 --rrdatas 8.232.230.139
```

The same certificate `*.wamn.dev` serves both hosts. On 2026-09-27 the upload took 14 seconds, and the upload again with `--org` took 12 seconds. The edge upgrade 12 seconds, the import 17 seconds and the record 1 second. Until the new host rule spreads, `wms.wamn.dev` reaches the default bucket, which answers with a listing of the whole web bucket (finding `wamn-uo2p`).
