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

The source is in [deploy/gcp/guard](../../deploy/gcp/guard). Run its test first:

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
  --billing-project=wamn-dev --display-name="wamn-dev" --budget-amount=50USD \
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
