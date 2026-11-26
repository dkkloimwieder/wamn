# Backup and recovery

The project chart owns the PostgreSQL Cluster and its backup resources.
CloudNativePG uses the Barman Cloud plugin to archive write-ahead logs (WAL).
WAL records changes between base backups.

## Configure backups

Set `database.backup.cadence` in the environment YAML to a six-field CNPG schedule.
Set `database.backup.retention` to the recovery window, such as `14d`.
Each project's ScheduledBackup uses that cadence. Each ObjectStore uses that retention window.
Omit `database.backup` to leave backups disabled for the environment.

Set `postgres.backup.endpoint`, `postgres.backup.bucket`, and `postgres.backup.secret` in the installation values before enabling backups.
These values have no defaults. Epic 10 supplies them during installation.
The named Secret must be present in the project's org namespace with keys `ACCESS_KEY_ID` and `ACCESS_SECRET_KEY`.
Clusters share the bucket. Each project generation uses its own prefix at `s3://<bucket>/wal/<cluster>`.

## Restore to a separate Cluster

CloudNativePG loads a base backup and replays WAL to the time you choose.
This recovers the project database and PostgreSQL roles in a separate Cluster.
During a row copy, the live endpoint stays at `<cluster>-rw.<org>.svc.cluster.local:5432`.

Use the project chart version from the live release.
In this example, the org is `acme` and the environment is `prod`.
Helm stores the release in `wamn-helm-acme`. The chart puts the resources in `acme`.
Set `project_release` to the live project release name, which starts with `e-prod-`.
Read the project database name from `database.name` and the Cluster name from `cluster.name`.
The Cluster name is `wamn-pg-` followed by the first 32 hex characters of the database name's SHA-256 hash.
Pass only the source `cluster` values to the recovery release.

```bash
set -euo pipefail
umask 077
org=acme
helm_namespace="wamn-helm-${org}"
project_release='<live-project-release>'
helm get values "$project_release" --namespace "$helm_namespace" --output json > project-values.json
project_database="$(jq -er '.database.name' project-values.json)"
jq -e '{cluster: .cluster} | select(.cluster.backup != null)' project-values.json > restore-values.json
cluster="$(jq -er '.cluster.name' restore-values.json)"
restored_cluster="${cluster}-restore"
restore_release="$restored_cluster"
```

Make sure that `<cluster>-store` still points to `s3://<bucket>/wal/<cluster>`.
The recovery Cluster reads that ObjectStore with `<cluster>` as the Barman server name.
The chart creates `<cluster>-restore-store` at `s3://<bucket>/wal/<cluster>-restore`.
It also creates `<cluster>-restore-backup` as the ScheduledBackup for the recovery Cluster.
Make sure that the recovery prefix is empty before you reuse the recovery Cluster name.

The recovery release supplies no NetworkPolicies.
In an org with default-deny rules, arrange temporary network access for the recovery pods before you install the release.
Allow access to object storage.
Allow access from the CloudNativePG operator.
If the recovery Cluster has multiple instances, allow replication between its pods.

Review the instance count and storage size in `restore-values.json`.
Set `recovery.targetTime` to the recovery time in UTC.
Wait for the recovery Cluster to report Ready.

```bash
helm install "$restore_release" deploy/charts/project --namespace "$helm_namespace" --server-side=true \
  -f restore-values.json --set-string "namespace=$org" \
  --set-string recovery.targetTime=2026-11-18T12:00:00Z
kubectl -n "$org" wait --for=condition=Ready "cluster/$restored_cluster" --timeout=30m
```

## Copy rows back to the live database

Read the `uri` key from the Secret `<cluster>-restore-superuser`.
The URI connects as `postgres` through the recovery Cluster's `<cluster>-restore-rw` service. [CloudNativePG connection Secrets](https://cloudnative-pg.io/docs/1.28/applications/)

Use a local port forward to reach that service.
In a second terminal, set `org` and `restored_cluster` to the same values.
Leave the port forward running while you dump the data.

```bash
kubectl -n "$org" port-forward "svc/${restored_cluster}-rw" 15432:5432
```

In your first terminal, use the project database name in the URI.

```bash
restored_uri="$(kubectl -n "$org" get secret "${restored_cluster}-superuser" \
  -o jsonpath='{.data.uri}' | base64 -d)"
restored_userinfo="${restored_uri#*://}"
restored_userinfo="${restored_userinfo%@*}"
restored_db_uri="postgresql://${restored_userinfo}@127.0.0.1:15432/${project_database}?sslmode=require"
```

Dump the selected table as the restored superuser to read all rows.
Use `pg_restore` to extract its table data as SQL.
This example uses `receiving.receipts`. [PostgreSQL pg_dump and row security](https://www.postgresql.org/docs/18/app-pgdump.html)

```bash
pg_dump --dbname="$restored_db_uri" --format=custom --data-only \
  --table=receiving.receipts --file=receipts.dump
pg_restore --data-only --schema=receiving --table=receipts --file=receipts.sql receipts.dump
```

Load the rows into a temporary table first. A direct data-only load adds rows and can hit duplicate keys.
In `receipts.sql`, change the `COPY` target from `receiving.receipts` to `pg_temp.recovery_receipts`.
Write the SQL that moves the wanted rows into `receiving.receipts` in `merge-receipts.sql`.
Review that SQL for conflicts with existing rows and for trigger effects.
Pause writes to the affected tables before you run the copy.

If the table has audit triggers, start `merge-receipts.sql` with the statement below.
Use the operator's principal ID from the live `platform."user"` table.

```sql
INSERT INTO platform.tx_identity (principal_id, role, application, operation)
VALUES ('<operator-principal-id>'::uuid, '', '', 'admin:restore');
```

The live Cluster disables password access for `postgres`.
Its database owner is `wamn_migration`, which authenticates over the network with a client certificate.
Run the copy through the local PostgreSQL socket on the live primary as `postgres`.
The temporary load and the merge run in one transaction. [CloudNativePG local authentication](https://cloudnative-pg.io/docs/1.28/bootstrap/)

```bash
cat receipts.sql merge-receipts.sql |
  kubectl -n "$org" exec -i "svc/${cluster}-rw" -c postgres -- \
    psql -X --username=postgres --dbname="$project_database" --set=ON_ERROR_STOP=1 \
    --single-transaction \
    --command='CREATE TEMP TABLE recovery_receipts AS SELECT * FROM receiving.receipts WITH NO DATA' \
    --file=-
```

Make sure that the live database contains the expected rows before you resume writes.

## Recover after deactivation

`wamn env deactivate-project ORG/ENV/PROJECT` keeps the Cluster and ObjectStore for a manual dump.
The next `wamn env apply` creates a new database generation with a new Cluster.
When you no longer need the kept data, run `wamn env deactivate-project ORG/ENV/PROJECT --data`.
The command removes the kept resources and lists each one it deletes.
If you already applied the environment again, `--data` also deletes the new generation.

If the Cluster is already gone, the command cannot find its ObjectStore, so delete it by name with `kubectl -n "$org" delete objectstore.barmancloud.cnpg.io "${cluster}-store" --ignore-not-found`.

## Recover after total loss of the live Cluster

1. Follow [Restore to a separate Cluster](#restore-to-a-separate-cluster) to recover the archive into `<cluster>-restore`.
2. Read the restored superuser URI as shown above. Dump the entire project database.

   ```bash
   pg_dump --dbname="$restored_db_uri" --format=custom --file=project.dump
   ```

3. After the restore and dump finish, delete the old project generation. This deletes the old ObjectStore.
   With `--data`, deactivation accepts an unreachable project database and reports that it skipped the database-side steps.
   Set `coordinate` to the project's org, environment, and project. Confirm the coordinate when prompted.

   ```bash
   coordinate='<org>/<env>/<project>'
   wamn env deactivate-project "$coordinate" --data
   ```

4. Apply the environment file. This creates a new generation with a new database, Cluster, archive prefix, login Secrets, and CDC slot.

   ```bash
   wamn env apply env.yaml
   ```

5. Load the dump into the new project database with `pg_restore`.
   Connect as `wamn_migration`, the database owner.
   Read the database, Cluster, and login certificate Secret names from the Helm release values.
   Save the client certificate, private key, and server CA certificate in a private directory.

   ```bash
   new_values="$(helm get values "$project_release" --namespace "$helm_namespace" --output json)"
   new_database="$(jq -er '.database.name' <<< "$new_values")"
   new_cluster="$(jq -er '.cluster.name' <<< "$new_values")"
   new_user="$(jq -er '.database.owner' <<< "$new_values")"
   new_login_secret="$(jq -er --arg user "$new_user" '.cluster.roles[] | select(.name == $user) | .certificateSecret' <<< "$new_values")"
   new_tls="$(mktemp -d)"
   trap 'rm -r -- "$new_tls"' EXIT
   (
     umask 077
     kubectl -n "$org" get secret "$new_login_secret" --output jsonpath='{.data.tls\.crt}' | base64 -d > "$new_tls/tls.crt"
     kubectl -n "$org" get secret "$new_login_secret" --output jsonpath='{.data.tls\.key}' | base64 -d > "$new_tls/tls.key"
     kubectl -n "$org" get secret "${new_cluster}-ca" --output jsonpath='{.data.ca\.crt}' | base64 -d > "$new_tls/ca.crt"
   )
   new_db_uri="host=${new_cluster}-rw.${org}.svc.cluster.local hostaddr=127.0.0.1 port=15433 dbname=$new_database user=$new_user sslmode=verify-full sslcert='$new_tls/tls.crt' sslkey='$new_tls/tls.key' sslrootcert='$new_tls/ca.crt'"
   ```

   In a second terminal, set `org` and `new_cluster` to the same values.
   Leave the port forward running until the load finishes.

   ```bash
   kubectl -n "$org" port-forward "svc/${new_cluster}-rw" 15433:5432
   ```

   Run the load in the first terminal.

   ```bash
   pg_restore --list project.dump > project.list
   grep -vE ' EXTENSION - (btree_gist|pg_stat_statements) *$| COMMENT - EXTENSION (btree_gist|pg_stat_statements) *$' project.list > load.list
   pg_restore --dbname="$new_db_uri" --clean --if-exists --no-owner --single-transaction --exit-on-error --use-list=load.list project.dump
   ```

   The list excludes `btree_gist` and `pg_stat_statements` because the Database resource installs them.
   The `wamn_migration` login cannot drop those extensions.
   Each of those `pg_restore -l` entries ends with a space. The pattern ` *$` matches that space.
   The `--single-transaction` and `--exit-on-error` flags load all the data or none of it.
   The `--clean --if-exists` flags replace objects that `env apply` installed in the new generation.
   This replaces the seeded rows in `platform."user"` and `platform.login_principal`.
   After the load, `wamn_migration` owns every restored relation.
   The two existing `pg_stat_statements` views belong to the extension.

The project is down from the loss until the load finishes.
Writes after the last archived WAL segment are lost.
The new generation uses new logins and a new CDC slot.
The old archive prefix stays in the bucket.
After you account for the recovered data, remove the old prefix by hand.

## Remove the recovery Cluster

After you confirm the copy, uninstall the recovery release.

```bash
helm uninstall "$restore_release" --namespace "$helm_namespace"
```

Remove the temporary NetworkPolicies you added for the recovery pods.
Helm removes the recovery Cluster, ObjectStore, and ScheduledBackup.
Objects under `s3://<bucket>/wal/<cluster>-restore` stay in the bucket.
If you no longer need those objects, remove them with the object-store client.

## Limits

Each project database has its own live CloudNativePG Cluster and, when configured, its own backup prefix.
Each recovery uses one project's backup. It leaves the system database and other projects outside the recovery.
A project needs a base backup and archived WAL through the target time for point-in-time recovery.
