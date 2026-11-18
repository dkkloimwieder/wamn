# Project chart

The chart creates the project database, publication, login Secrets, and the project operation account.
It saves the applied environment YAML in the project's operations ConfigMap. Later Jobs mount this file.

Set `cdcReader.pause` to stop the CDC reader without removing its configuration.
The recovery Job pauses the reader before repairing the replication slot.
The close Job records resynchronization before it starts the reader again.
Deployment keeps the reader paused while the registry records an open gap.
The chart issues a separate client certificate for project recovery.
