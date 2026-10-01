# Plans

These pages describe unbuilt work and its limits. Current behavior belongs in [architecture](../architecture/README.md), and Beads records decisions and implementation status.

- [Component pull](component-pull.md): Design for the GKE host to pull with a Workload Identity token in place of the registry token CronJob (finding `wamn-i87m`).
- [Delivery](delivery.md): Deferred CI-provider configuration and conditional delivery extensions.
- [Environment teardown](environment-teardown.md): Design for `delete-project-env`, the verb that deletes one project environment (finding `wamn-psss`).
- [Identity](identity.md): External login providers within the existing identity authority.
- [Edge](edge.md): Epic 19 scope for `wamn-edge`, one application and one device loop on a small aarch64 box.
- [Generated operations](generated-operations.md): Draft design for generating the handler, data function, world export, route entry and declaration entry of each generated operation.
- [Google Cloud deployment](gcp-deployment.md): Epic 23 proposal for the first deployment to the `wamn-dev` project, with costs, a limited mode and shutdown commands.
- [Host-owned transaction](host-transaction.md): Design for one idempotency path, in which the host owns the transaction of an operation and the engine runs the intent rules around every call.
- [Human identity and login](identity-plan.md): Incremental password login, later account lifecycle, and PAT-free sensitive operations with planning at each epic start.
- [NATS outage](nats-outage.md): Design for the host and the runtime operator to stay up and connect again when the scheduler NATS stops or moves.
- [Operation ids](operation-ids.md): Design for authoring the package version once, in `wamn.json`, and deriving each sealed operation id from it.
- [Operator image](operator-image.md): Design for the image and the Jobs that run the verbs that call identity inside the cluster (findings `wamn-n5d1` and `wamn-lo7z`).
- [Operator UI](operator-ui.md): Deferred client and screen capabilities.
- [Release qualification](release-qualification.md): Draft design for `qualify-release` of any environment from its candidate (finding `wamn-ld93.33`).
- [Schema upgrade](schema-upgrade.md): Design for `upgrade-schema`, the verb that applies a platform schema change to an installed database (finding `wamn-o8b9`).
- [Upgrades](upgrades.md): Design for changing a schema after installation.
- [Web cache invalidation](web-cache-invalidation.md): A write marks stale only the stored reads whose relations it writes (`wamn-fjdo`).
- [Web deployment](web-deployment.md): Epic 20 rules for the web client host, and its remaining real deployment.
- [Web operator client](web-operator-client.md): Generated browser UIs from the release contract, scoped one epic at a time.
- [Write log](write-log.md): Epic 24 design for one idempotency record per database, `app_system.write_log`, in place of the claim tables.
- [Workflow feature](workflow-feature.md): Epic 22 scope for the workflow contract, an event trigger, a JSONata node, and the WMS label workflow.
