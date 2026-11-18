# Project chart

The chart creates the project database, publication, login Secrets, and the project operation account.
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
