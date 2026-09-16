# Backup and recovery

This page covers the backup of an org Postgres cluster and its recovery.
Backup and recovery run through CloudNativePG and its Barman Cloud plugin.
The platform renders the manifests and the operator applies them.

## What the platform backs up

`provision-org` renders one cluster for each recovery domain of an org.
Each backed cluster carries the Barman Cloud plugin as its WAL archiver.
The plugin ships every WAL segment to `s3://wamn-backups/wal/<cluster>` in the shared object store.
A `ScheduledBackup` takes a base backup at the cadence of the env policy.
Two policy fields size the recovery window.
`wal_retention` sets how far back a restore can reach.
`backup_cadence` sets how often a base backup is taken.

An env policy with an empty `backup_cadence` has no backup at all.
The shipped `dev` policy is such a policy, and the shipped `prod` policy keeps 14 days.
An environment with no backup has no WAL stream, so it cannot be recovered.

Each cluster writes under its own prefix, so two recovery domains never share a WAL stream.

## What recovery restores

CloudNativePG recovery is whole-cluster point-in-time recovery.
It restores a base backup and then replays WAL up to the point you name.
The restore always lands in a new cluster, and the source cluster is untouched.
The new cluster holds every database and every role the source held at that instant.

## Render the recovery manifests

Run the command with the same `--template` that `provision-org` was run with.
The template supplies the env policy that sizes the restored cluster.

```bash
wamn-ctl-ops recover-org-cluster \
  --org acme --template standard --owner prod \
  --at 2026-09-16T14:31:00Z \
  --emit-object-store /tmp/restore-store.json \
  --emit-cluster /tmp/restore-cluster.json \
  --emit-scheduled-backup /tmp/restore-backup.json
```

`--owner` names the recovery domain, so the source cluster is `<org>-<owner>`.
`--at` is an RFC3339 timestamp, and the replay stops there.
If you omit `--at`, the replay applies every archived WAL segment.
`--target` names the new cluster, and the default is `<org>-<owner>-restore`.
The command refuses a target equal to the source.
Such a manifest addresses the live cluster instead of a new one.

The command reads no database and applies nothing.
Read the rendered `Cluster` before you apply it, and compare its `storage` and `instances` with the source.
If the org customized its policy after provisioning, edit the rendered values to match the source.

## Apply the recovery manifests

1. Apply the `ObjectStore` of the restored cluster.
2. Apply the recovery `Cluster`.
3. Wait for the cluster to report ready.
4. Apply the `ScheduledBackup` of the restored cluster.

```bash
kubectl -n wamn-system apply -f /tmp/restore-store.json
kubectl -n wamn-system apply -f /tmp/restore-cluster.json
kubectl -n wamn-system wait --for=condition=Ready cluster/acme-prod-restore --timeout=30m
kubectl -n wamn-system apply -f /tmp/restore-backup.json
```

The restored cluster archives WAL to its own store, under the prefix of its own name.
It can never write into the stream it was restored from.
Its `externalClusters` entry names the source store, and that entry is a read path only.

To watch the restore, read the cluster status and the operator logs.

```bash
kubectl -n wamn-system get cluster acme-prod-restore -o wide
kubectl -n wamn-system logs -l cnpg.io/cluster=acme-prod-restore -c postgres --tail=50
```

## Connect to the restored cluster

The restored cluster carries the role names and passwords of the source.
Its read-write service is `<target>-rw.wamn-system.svc:5432`.
CloudNativePG creates the Secret `<target>-superuser` with a usable admin URI.

Application workloads still address the source cluster.
Nothing points at the restored cluster until you change a target yourself.

## Recover one database or one table

CloudNativePG restores a whole cluster, so there is no command that restores one database.
The retired dump path restored one database, and this path does not.
To recover one database or one table, restore the cluster and then copy the rows out.

1. Render and apply the recovery with `--at` set to the instant before the loss.
2. Read the superuser URI from the Secret `<target>-superuser`.
3. Copy the wanted database or table out of the restored cluster.
4. Load it into the live database under a new name, then move the rows.
5. When the copy is complete, delete the restored cluster.

```bash
kubectl -n wamn-system get secret acme-prod-restore-superuser -o jsonpath='{.data.uri}' | base64 -d
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

If the restore replaces the source, keep it.
It already archives to its own store, so its backups continue without more work.
Change the workload targets to the new cluster, then delete the source cluster.

If the restore was only an inspection, delete it and its resources.

```bash
kubectl -n wamn-system delete scheduledbackup acme-prod-restore-backup
kubectl -n wamn-system delete cluster acme-prod-restore
kubectl -n wamn-system delete objectstore acme-prod-restore-store
```

The objects under `s3://wamn-backups/wal/acme-prod-restore` remain after the delete.
When you no longer want them, remove them with the object-store client.

## Limits

- An env policy with an empty `backup_cadence` has no WAL stream and cannot be recovered.
- A pooled org owns no cluster, so `recover-org-cluster` refuses it.
- The rendered size comes from the template, not from the recorded policy of the org.
- Recovery restores a whole cluster, and every database in that cluster comes back together.
