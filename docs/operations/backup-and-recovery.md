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

## What recovery restores

CloudNativePG recovery is whole-cluster point-in-time recovery.
It restores a base backup and then replays WAL up to the point you name.
The restore always lands in a new cluster, and the source cluster is untouched.
The new Cluster recovers the project database and its roles at that instant.

## Select recovery

Use a separate release of the project chart. The source Cluster keeps serving its existing endpoint.
The new Cluster uses the source name with `-restore` appended.
It reads from the source ObjectStore and archives WAL to its own prefix.

Set `project_release` to the live project release for org `acme`, whose history is in `wamn-helm-acme`.
Export only the source `cluster` values. Review the instances and storage for the restore.
Set `recovery.targetTime` to the instant to recover.

```bash
helm get values "$project_release" --namespace wamn-helm-acme --output json |
  jq '{cluster: .cluster}' > restore-values.json
helm install recovery-acme-prod deploy/charts/project --namespace wamn-helm-acme --server-side=true \
  -f restore-values.json --set-string namespace=acme --set-string recovery.targetTime=2026-11-18T12:00:00Z

restored_cluster="$(jq -r '.cluster.name + "-restore"' restore-values.json)"
kubectl -n acme get cluster "$restored_cluster" -o wide
kubectl -n acme logs -l "cnpg.io/cluster=$restored_cluster" -c postgres --tail=50
```

## Connect to the restored cluster

The restored cluster carries the role names and passwords of the source.
Its read-write service is `<target>-rw.<org>.svc:5432`.
CloudNativePG creates the Secret `<target>-superuser` with a usable admin URI.

Application workloads still address the source cluster.
Nothing points at the restored cluster until you change a target yourself.

## Recover one database or one table

CloudNativePG restores a whole cluster, so there is no command that restores one database.
To recover one database or one table, restore the cluster and then copy the rows out.

1. Install the recovery release at the instant before the loss.
2. Read the superuser URI from the Secret `<target>-superuser`.
3. Copy the wanted database or table out of the restored cluster.
4. Load it into the live database under a new name, then move the rows.
5. When the copy is complete, delete the restored cluster.

```bash
kubectl -n acme get secret "${restored_cluster}-superuser" -o jsonpath='{.data.uri}' | base64 -d
pg_dump "<restored-superuser-uri>" --table=receiving.receipts --data-only --format=custom --file=/tmp/receipts.dump
pg_restore --dbname="<live-superuser-uri>" --data-only /tmp/receipts.dump
```

The restored cluster is a disposable copy, and the operator supplies its superuser URI.
That is the one case where a superuser dump is correct.
Row level security applies to every ordinary role, so a dump under an application role returns an error or no rows.
The platform mints no `BYPASSRLS` role for this work.

Warning: `pg_restore --data-only` into a live table adds rows, it does not replace them.
Load into a scratch table first, then write the rows you want with SQL you read before you run it.

## Keep or discard the restored cluster

Pending owner ruling (Epic 9, restore replacement): env apply regenerates the targets from the registry.

If the restore replaces the source, keep it.
It already archives to its own store, so its backups continue without more work.
Change the workload targets to the new cluster, then delete the source cluster.

If the restore was only an inspection, delete it and its resources.

```bash
helm uninstall recovery-acme-prod --namespace wamn-helm-acme
```

The objects under `s3://wamn-backups/wal/acme-prod-restore` remain after the delete.
When you no longer want them, remove them with the object-store client.

## Limits

A project without backups has no archived WAL for point-in-time recovery.
Helm deletes recovery Cluster resources when you uninstall the recovery release. Objects in the recovery prefix stay in the bucket.
