# Project chart

The chart creates the project's CloudNativePG Cluster, Database, login Secrets, publication, and operation account.
CloudNativePG creates the database at bootstrap. The Database resource installs the extensions.
`env apply` derives the Cluster name and login Secret names from the registered project generation.
It also sets limits on the five managed logins from the host replica count.
The environment file provides sizing. The installation values provide the image and object-storage connection.
If backups are configured, Helm creates an ObjectStore and ScheduledBackup for the project generation.
Helm keeps the live Cluster and ObjectStore unless `env delete --data` removes them.
To restore into a separate release, see [Backup and recovery](../../../docs/operations/backup-and-recovery.md).
It saves the applied environment YAML in the project's operations ConfigMap. Later Jobs mount this file.
The CDC reader projects its replication login from `operations.captureSecret` and the `ca.crt` key from the project's CA Secret under `/etc/wamn/db/<login-secret>/`.
Operation Jobs fetch their named database Secrets and stage the same files in their own volume.

Project upgrades omit the Helm wait flag. The worker reads the Database, certificates, and CDC reader Deployment by name.
The worker checks the publication in SQL before it creates the replication slot. The reader waits up to 120 seconds for its slot before it starts replication.

Set `cdcReader.pause` to stop the CDC reader without removing its configuration.
The recovery Job pauses the reader before repairing the replication slot.
The close Job records resynchronization before it starts the reader again.
Deployment keeps the reader paused while the registry records an open gap.
The chart issues a separate client certificate for project recovery.
