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

The `credentials` values name the empty Secret for the project release and the control ServiceAccount that can read and patch it.
Control writes data keys after install. Helm uses server-side apply and keeps the Secret on uninstall.
The project NetworkPolicies allow its host group, control, the worker Jobs, and its CDC reader to reach the project database.
Set `networkPolicy.objectStorage` to the CIDR and TCP port pairs that the database and backup process use.

Project upgrades omit the Helm wait flag. The worker reads the Database, certificates, and CDC reader Deployment by name.
The worker checks the publication in SQL before it creates the replication slot. The reader waits up to 120 seconds for its slot before it starts replication.

Set `cdcReader.pause` to stop the CDC reader without removing its configuration.
The recovery Job pauses the reader before repairing the replication slot.
The close Job records resynchronization before it starts the reader again.
Deployment keeps the reader paused while the registry records an open gap.
The chart issues a separate client certificate for project recovery.
