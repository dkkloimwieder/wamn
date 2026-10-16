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

1. The agent identity. `CLOUDSDK_CONFIG` holds the login of `wamn-agent@wamn-dev.iam.gserviceaccount.com` and application default credentials, which `wamn web upload` reads. `KUBECONFIG` names a file of the agent's own. The cluster create of section 4.2 writes the cluster into it.
2. The Resend key, as the Secret `identity-resend` in namespace `identity`, key `api-key`. The owner makes it after section 4.4 makes the namespace and before section 4.12. Write the key to a mode 0600 file. Identity refuses whitespace in the key (`Resend configuration refused`). Most editors end a file with a line end, so the command removes all whitespace:

   ```bash
   kubectl -n identity create secret generic identity-resend --from-literal=api-key="$(tr -d '[:space:]' < <file>)"
   ```

3. The Workload Identity binding of the host service account. The Kubernetes service account `hosts/wamn-host` acts as the Google service account `wamn-blob`. A teardown keeps this binding, because it is IAM. If the binding is missing, the owner runs:

   ```bash
   gcloud iam service-accounts add-iam-policy-binding wamn-blob@wamn-dev.iam.gserviceaccount.com --project wamn-dev \
     --role roles/iam.workloadIdentityUser --member "serviceAccount:wamn-dev.svc.id.goog[hosts/wamn-host]"
   ```

4. The telemetry bucket and its writer (`docs/plan/observability.md` R1, R7). The bucket `wamn-dev-otel` deletes an object 30 days after it lands. The Google service account `wamn-otel` writes to it and reads its metadata, because the collector exporter reads the bucket at start (`storage.buckets.get`). The Kubernetes service account `platform/otel-collector` acts as `wamn-otel`. A teardown keeps all three, because they are a bucket and IAM:

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

5. The approval of the teardown of section 3. The teardown deletes every database.

## 2. Project resources

The teardown keeps these resources, and the install expects them. [Install history](../history/gcp-install-history.md) sections 1, 2.2, 2.3, 4.3, 4.3.1 and 5.1 hold the commands that made them:

- The APIs, the quotas and the daily guard that sets pool `main` to 0 nodes at 03:00 New York time.
- The Artifact Registry repositories `wamn` and `wamn-kind`, with immutable tags. `wamn` holds the platform images, the packages, the components and the releases.
- The buckets `wamn-dev-web`, `wamn-dev-backups` and `wamn-dev-labels`.
- The VPC `wamn`, its subnet `wamn-us-central1` and the node service account `wamn-nodes`.
- The Cloud DNS zone `wamn-dev`, its records, and the `roles/dns.admin` binding of the cert-manager principal on the zone.
- The Google service account `wamn-blob`, its bucket binding and its `roles/artifactregistry.reader` binding on `wamn`.
- The load balancer: address, health check, firewall rule, backend bucket, backend service, URL map, proxy, forwarding rule and certificate entry. Also the role `wamnEdgeCert` and its binding.

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

The network endpoint group `wamn-edge` stays, because the backend service `wamn-edge-platform` names it. Section 4.20 replaces it.

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
cargo build --locked -p wamn-ctl --bin wamn --bin wamn-ctl
cargo build --locked -p wamn-scenario-worker
```

### 4.2 Cluster

gcloud cannot name the first pool of a new cluster. The cluster starts with a temporary `default-pool`, and the pool `main` of 1 Spot node replaces it. The pool `pg` holds one node for PostgreSQL alone: its taint keeps every other pod off, and the `Cluster` selects it (section 4.5). The pool `bench` holds one Spot node for the Receiving host alone: its taint keeps every other pod off, and an admission policy places the host there (section 4.9). The 8-vCPU project quota and the SSD quota of 0 set its size and disk. The create writes the cluster into `KUBECONFIG` and makes it the current context:

```bash
NSA=wamn-nodes@wamn-dev.iam.gserviceaccount.com
gcloud container clusters create wamn --project wamn-dev --zone us-central1-a \
  --release-channel regular --network wamn --subnetwork wamn-us-central1 \
  --enable-ip-alias --cluster-secondary-range-name pods \
  --services-secondary-range-name services --workload-pool wamn-dev.svc.id.goog \
  --logging=SYSTEM --monitoring=SYSTEM --no-enable-managed-prometheus \
  --service-account $NSA --disk-type pd-standard --disk-size 30 \
  --num-nodes 1 --machine-type e2-standard-2 --spot
gcloud container node-pools create main --cluster wamn --project wamn-dev \
  --zone us-central1-a --machine-type e2-standard-2 --spot --num-nodes 1 \
  --disk-type pd-standard --disk-size 30 --service-account $NSA \
  --max-surge-upgrade 0 --max-unavailable-upgrade 1
gcloud container node-pools create bench --cluster wamn --project wamn-dev \
  --zone us-central1-a --machine-type e2-standard-2 --spot --num-nodes 1 \
  --disk-type pd-standard --disk-size 30 --service-account $NSA \
  --max-surge-upgrade 0 --max-unavailable-upgrade 1 \
  --node-labels role=bench-host --node-taints role=bench-host:NoSchedule
gcloud container node-pools create pg --cluster wamn --project wamn-dev \
  --zone us-central1-a --machine-type e2-standard-2 --num-nodes 1 \
  --disk-type pd-standard --disk-size 50 --service-account $NSA \
  --node-labels role=postgres --node-taints role=postgres:NoSchedule
gcloud container node-pools delete default-pool --cluster wamn --project wamn-dev --zone us-central1-a --quiet
gcloud container clusters get-credentials wamn --project wamn-dev --zone us-central1-a
kubectl get nodes
```

The pools have a fixed size and no autoscaler, so a scale to 0 holds. If `kubectl get nodes` lists no node after 03:00 New York time, run section 5.1.

### 4.3 cert-manager and the edge certificate

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
helm upgrade --install --namespace platform wamn oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.10.3 \
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

[deploy/gcp/cnpg-cluster.yaml](../../deploy/gcp/cnpg-cluster.yaml) is one instance that holds the system database `wamn_system` and every environment database. CloudNativePG keeps the superuser login in the Secret `wamn-pg-superuser`:

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

`wamn-ctl build-platform` builds the host, runner, gates and identity images and the `http` and `materializer` role components. When every build succeeded, it pushes them with the tag of the chart version and stamps their digests into the release chart ([delivery](delivery.md#release-qualification)). The repository `wamn` has immutable tags, so a version that is already in the registry cannot be built again. A reinstall at an unchanged version uses the registry (`docs/plan/platform-deploy.md` R15). With `--stamp-only`, the command builds nothing. It reads the registry digests of the six images of the version and stamps them into the release chart. It refuses if one of them is missing.

The build pushes for longer than one hour, and a `docker login` token expires after one hour. Give Docker a configuration of its own with the gcloud credential helper, which gets a new token for each push. The file holds no credential. Then build if the host image of the version is absent, and stamp only if it is present:

```bash
W=$HOME/.cache/wamn-install
mkdir -m 700 $W/docker
echo '{"credHelpers":{"us-central1-docker.pkg.dev":"gcloud"}}' > $W/docker/config.json
V=$(sed -n 's/^pub const CHART_VERSION: &str = "\(.*\)";/\1/p' crates/control/lib/src/release_chart.rs)
if gcloud artifacts docker images describe us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-host:$V >/dev/null 2>&1; then
  STAMP=--stamp-only
else
  STAMP=
fi
echo "platform $V ${STAMP:-build}"
DOCKER_CONFIG=$W/docker target/debug/wamn-ctl build-platform $STAMP \
  --registry us-central1-docker.pkg.dev/wamn-dev/wamn --out $W/release-chart
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

`authorization.conf` is `authorization: ` followed by JSON. Merge the two files: the Receiving users, its `tap-admin`, and the WMS users without their `tap-admin`:

```bash
W=$HOME/.cache/wamn-install; E=$W/evt-receiving/event-nats; F=$W/evt-wms/event-nats
(umask 077; python3 - $E/authorization.conf $F/authorization.conf > $W/merged-authorization.conf <<'PY'
import json, sys
def users(path):
    return json.loads(open(path).read().split(':', 1)[1])['users']
merged = users(sys.argv[1]) + [u for u in users(sys.argv[2]) if u['user'] != 'tap-admin']
print('authorization: ' + json.dumps({'users': merged}))
PY
)
```

Make the broker Secrets, install the broker, and make the user Secrets:

```bash
W=$HOME/.cache/wamn-install; E=$W/evt-receiving/event-nats; F=$W/evt-wms/event-nats
kubectl -n platform create secret generic evt-nats-authorization --from-file=authorization.conf=$W/merged-authorization.conf
kubectl -n platform create secret generic evt-nats-bootstrap --from-file=context.json=$E/context.json
kubectl apply -f deploy/gcp/nats-jetstream.yaml
kubectl -n platform rollout status statefulset/evt-nats --timeout=300s
kubectl -n platform wait --for=condition=complete job/evt-nats-tap-stream --timeout=180s
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

Make sure that the stream `WAMN_TAP` exists. The log shows `"config":{"name":"WAMN_TAP"`:

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

The package artifacts `wamn_receiving-2.1.0` and `wamn_wms-2.1.0` are in the registry. If a tag is missing, push that package. The tags are immutable, so a push of other bytes under a present tag fails:

```bash
. $HOME/.cache/wamn-install/platform.sh
registry_token
for project in receiving wms; do
  gcloud artifacts docker images describe $WAMN_PACKAGE_ARTIFACT_BASE:wamn_$project-2.1.0 --format="value(image_summary.digest)" \
  || target/debug/wamn-ctl push-package --package apps/wamn_$project --artifact-base $WAMN_PACKAGE_ARTIFACT_BASE \
       --component-artifact-base us-central1-docker.pkg.dev/wamn-dev/wamn/components --registry-auth-file $WAMN_REGISTRY_AUTH_FILE
done
```

Install each package into its environment. `reconcile-replica-identity` runs for WMS, because the workflow `movement_label` compares the old location of `wms.packaging`:

```bash
. $HOME/.cache/wamn-install/platform.sh
R="--artifact-base $WAMN_PACKAGE_ARTIFACT_BASE --registry-auth-file $WAMN_REGISTRY_AUTH_FILE"
for project in receiving wms; do
  target/debug/wamn-ctl apply-package --package-artifact wamn_$project-2.1.0 $R \
    --database-url "$(db_url $project)" --tenant dkk--$project--dev
  target/debug/wamn-ctl reconcile-package-data-access --package-artifact wamn_$project-2.1.0 $R \
    --database-url "$(db_url $project)" --tenant dkk--$project--dev
done
target/debug/wamn-ctl reconcile-replica-identity --admin-database-url "$(db_url wms)" --package apps/wamn_wms
```

### 4.14 Releases

`publish-release` prints the digest of the release manifest. Publish Receiving, which has no wiring:

```bash
. $HOME/.cache/wamn-install/platform.sh
registry_token
RELEASE_receiving=$(target/debug/wamn-ctl publish-release --database-url "$(db_url receiving)" \
  --control-database-url "$WAMN_SYSTEM_ADMIN_URL" --org dkk --tenant dkk--receiving--dev --environment dev \
  --verified-publisher-principal wamn-management-author-dkk--receiving--dev \
  --run-schema wamn_run --package wamn_receiving@2.1.0 \
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
cargo run --locked -p wamn-test-infrastructure --example gate_request -- wamn_wms 2.1.0 wms dev \
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
  --tenant dkk--wms--dev --package-id wamn_wms --package-version 2.1.0 \
  --wiring-document apps/wamn_wms/publication/wirings/inventory_move_and_label.json \
  --artifact-base $WAMN_PACKAGE_ARTIFACT_BASE --registry-auth-file $WAMN_REGISTRY_AUTH_FILE
RELEASE_wms=$(target/debug/wamn-ctl publish-release --database-url "$(db_url wms)" \
  --control-database-url "$WAMN_SYSTEM_ADMIN_URL" --org dkk --tenant dkk--wms--dev --environment dev \
  --verified-publisher-principal wamn-management-author-dkk--wms--dev \
  --run-schema wamn_run --package wamn_wms@2.1.0 --wiring "wamn_wms@2.1.0::inventory_move_and_label=2" \
  --artifact-base $WAMN_PACKAGE_ARTIFACT_BASE --registry-auth-file $WAMN_REGISTRY_AUTH_FILE)
echo "$RELEASE_wms" > $W/release-wms
shred -u $G/management-author-pat.json
```

### 4.15 Qualification

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

Each environment document names its release digest. The edge values and the URL map name the bucket path of each web client, which holds the digest hex. Write the new digests into the four files:

```bash
. $HOME/.cache/wamn-install/platform.sh
for project in receiving wms; do
  NEW=$(cat $W/release-$project)
  OLD=$(sed -n 's/^    release = "\(sha256:[0-9a-f]*\)"/\1/p' deploy/gcp/environments/dkk--$project--dev.k)
  sed -i "s/$OLD/$NEW/" deploy/gcp/environments/dkk--$project--dev.k
  sed -i "s/${OLD#sha256:}/${NEW#sha256:}/g" deploy/gcp/values-edge.yaml deploy/gcp/url-map.yaml
done
git diff --stat
```

Commit every change of `deploy/gcp` from sections 4.6, 4.11 and 4.16 in one commit, and push it.

### 4.17 Apply with the release

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

The CDC readers run in `hosts`. `env apply` wrote their replication Secrets and their registry-reader Secrets there, and recorded `hosts` on each reader row. Each reader reads its registration with the registry-reader login:

```bash
kubectl apply -f deploy/gcp/cdc-reader.yaml -f deploy/gcp/cdc-reader-wms.yaml
kubectl -n hosts rollout status deploy/cdc-reader deploy/cdc-reader-wms --timeout=180s
kubectl -n hosts logs deploy/cdc-reader --tail=20
kubectl -n hosts logs deploy/cdc-reader-wms --tail=20
```

Each log shows `registration loaded`, the three `preflight` lines and `walsender session open`.

### 4.19 Web clients

`wamn web upload` reads the built client from the package artifact and writes it to `gs://wamn-dev-web/clients/<package>/<digest hex>/` with the application default credentials. It reads the platform inputs of `env apply`. With `WAMN_EVENT_NATS_URL` set, it also asks for a NATS user, so unset that variable:

```bash
. $HOME/.cache/wamn-install/platform.sh
registry_token
unset WAMN_EVENT_NATS_URL
for project in receiving wms; do
  target/debug/wamn web upload dkk/$project/dev --release "$(cat $W/release-$project)" --bucket gs://wamn-dev-web/clients
done
```

### 4.20 Edge and load balancer

The edge chart annotates its Service with `neg: true`, so GKE makes the network endpoint group `wamn-edge`. The group of the deleted cluster carries the old cluster UID, and the new cluster's controller refuses it (`found conflicting description in neg wamn-edge`). Install the edge, take the old group off the backend service, delete it, wait for the new group, and add it back. The new group exists some seconds before it is ready, and `add-backend` refuses it until then (`is not ready`), so the add repeats:

```bash
helm install wamn-edge deploy/platform/edge -n edge -f deploy/gcp/values-edge.yaml
gcloud compute backend-services remove-backend wamn-edge-platform --project wamn-dev --global \
  --network-endpoint-group wamn-edge --network-endpoint-group-zone us-central1-a
gcloud compute network-endpoint-groups delete wamn-edge --zone us-central1-a --project wamn-dev --quiet
until gcloud compute network-endpoint-groups describe wamn-edge --zone us-central1-a --project wamn-dev >/dev/null 2>&1; do sleep 10; done
until gcloud compute backend-services add-backend wamn-edge-platform --project wamn-dev --global \
  --network-endpoint-group wamn-edge --network-endpoint-group-zone us-central1-a \
  --balancing-mode RATE --max-rate-per-endpoint 100; do sleep 10; done
kubectl -n edge rollout status deploy/wamn-edge --timeout=600s
gcloud compute url-maps import wamn-edge --global --project wamn-dev --source deploy/gcp/url-map.yaml --quiet
```

The CronJob `edge/edge-cert` copies a renewed certificate to the load balancer every day ([edge-cert.yaml](../../deploy/gcp/edge-cert.yaml)). Its role and binding stay after a teardown. Install it and run it once. The run uploads the certificate of section 4.3 and moves the proxy to it:

```bash
kubectl apply -f deploy/gcp/edge-cert.yaml
kubectl -n edge create job edge-cert-1 --from=cronjob/edge-cert
kubectl -n edge wait job/edge-cert-1 --for=condition=Complete --timeout=300s
kubectl -n edge logs job/edge-cert-1
```

### 4.21 Check

Run these from this machine. The load balancer can take some minutes to use a new group and a new URL map:

```bash
for h in receiving.wamn.dev wms.wamn.dev; do
  curl -s -o /dev/null -w "$h / %{http_code}\n" https://$h/
  curl -s -o /dev/null -w "$h /config.json %{http_code}\n" https://$h/config.json
done
curl -s -w ' %{http_code}\n' https://receiving.wamn.dev/api/purchase_order/query
curl -s -w ' %{http_code}\n' -X POST -H 'content-type: application/json' -d '{}' https://wms.wamn.dev/api/inventory/move
openssl s_client -connect receiving.wamn.dev:443 -servername receiving.wamn.dev </dev/null 2>/dev/null | openssl x509 -noout -serial -enddate
```

`/` and `/config.json` answer `200` on both hosts. The two API calls answer `401` with `{"error":{"code":"unauthorized"}}`, because they carry no session. The serial is the serial of the Secret `edge/wamn-edge-tls`. The install is done. To stop the cost of the nodes, run section 5.2.

## 5. Operations

### 5.1 Pool after the daily guard

The guard sets pool `main` to 0 nodes every day. Scale `pg`, `main` and `bench` back to 1 node each. The `Cluster` keeps its old `Ready` condition while its pod is gone, so wait for the pod `wamn-pg-1`. The event NATS keeps `WAMN_TAP` in memory, so run the tap-stream Job again after every restart of the broker:

```bash
gcloud container clusters resize wamn --node-pool pg --num-nodes 1 --zone us-central1-a --project wamn-dev --quiet
gcloud container clusters resize wamn --node-pool main --num-nodes 1 --zone us-central1-a --project wamn-dev --quiet
gcloud container clusters resize wamn --node-pool bench --num-nodes 1 --zone us-central1-a --project wamn-dev --quiet
until kubectl -n platform get pod wamn-pg-1 >/dev/null 2>&1; do sleep 5; done
kubectl -n platform wait pod/wamn-pg-1 --for=condition=Ready --timeout=900s
kubectl -n platform rollout status statefulset/evt-nats --timeout=600s
kubectl -n platform delete job evt-nats-tap-stream --ignore-not-found
kubectl apply -f deploy/gcp/nats-jetstream.yaml
kubectl -n platform wait --for=condition=complete job/evt-nats-tap-stream --timeout=300s
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

The admin that `provision-org --admin` made has no password, so `/password/recover` answers `202` and sends no mail. Invite that user with the Job [invite.yaml](../../deploy/gcp/operator/invite.yaml). It runs in namespace `identity` with the mounted operator certificate. `__GRANT_FLAGS__` takes `--membership <project>/<env>` or `--org-admin`:

```bash
. $HOME/.cache/wamn-install/platform.sh
(umask 077; sed -e 's/__EMAIL__/<email>/' -e 's/__DISPLAY_NAME__/<name>/' -e 's/__ORG__/dkk/' \
  -e 's|__GRANT_FLAGS__|--membership receiving/dev|' deploy/gcp/operator/invite.yaml > $W/invite.yaml)
kubectl apply -f $W/invite.yaml
rm $W/invite.yaml
JOB_UID=$(kubectl -n identity get job invite -o jsonpath='{.metadata.uid}')
printf 'postgresql://postgres:%s@wamn-pg-rw.platform.svc.cluster.local:5432/wamn_system' "$PW" |
  kubectl -n identity create secret generic wamn-system-admin --from-file=url=/dev/stdin --dry-run=client -o json |
  jq --arg uid "$JOB_UID" '.metadata.ownerReferences = [{apiVersion: "batch/v1", kind: "Job", name: "invite", uid: $uid}]' |
  kubectl create -f -
kubectl -n identity wait --for=condition=complete job/invite --timeout=120s
kubectl -n identity logs job/invite
kubectl -n identity delete job invite
```

Identity answers `201 {"status":"accepted_for_delivery"}`. Then apply the environment, which writes the user's tenant `users` row, and give the user a role in it ([deployment](deployment.md#user-roles)):

```bash
. $HOME/.cache/wamn-install/platform.sh
use_env receiving
export WAMN_RELEASE_HOST_GROUP=deploy/gcp/host-group-receiving.yaml
target/debug/wamn-ctl env apply deploy/gcp/environments/dkk--receiving--dev.k
target/debug/wamn-ctl grant-role --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$(db_url receiving)" \
  --org dkk --project receiving --env dev --tenant dkk--receiving--dev --user <email> --role admin
```

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

The tier 2 benchmark runs from `wamn-bench-client` in project `wamn-dev`, zone `us-central1-a`.
The Receiving host runs alone on pool `bench`, and PostgreSQL runs alone on pool `pg`.
Every other pod runs on pool `main`.
Prepare the VM and its `~/wamn/target/release/wamn-bench` binary with [install history](../history/gcp-install-history.md#64-bench-client-and-pat), section 6.4.
Use the 1000-order Receiving seed from [install history](../history/gcp-install-history.md#61-morning-start-and-the-1000-seed), section 6.1.
Use the operator PAT from section 4.14 and the platform file from section 4.10.
Install `psql` and Python 3 on the repository machine.
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
