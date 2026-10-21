# Plans

Each page below is a plan with its status: done, live or deferred, and its bead. Current behavior belongs in [architecture](../architecture/README.md), and Beads records decisions and implementation status.

- [CDC reader slot](cdc-reader-slot.md): done, `wamn-59z6`.
- [Component pull](component-pull.md): done, `wamn-i87m`.
- [Delivery](delivery.md): deferred, no bead.
- [Environment teardown](environment-teardown.md): done, `wamn-psss`.
- [Generated operations](generated-operations.md): done, `wamn-iowb`.
- [Identity](identity.md): deferred, `wamn-jpx2`.
- [Integrations](integrations.md): live, `wamn-7e39m` (Epic 3); Epics 1 `wamn-jstjf` and 2 `wamn-5i3v9` are done.
- [Manifest authoring](manifest-authoring.md): done, `wamn-j1ti`.
- [NATS outage](nats-outage.md): deferred, `wamn-gdex` and `wamn-ifh7`.
- [Operator image](operator-image.md): done, `wamn-n5d1` and `wamn-lo7z`.
- [Operator UI](operator-ui.md): deferred by owner ruling, `wamn-z7j99`. The [web client](web-operator-client.md) is the only operator client plan.
- [Package upgrade](package-upgrade.md): live, `wamn-eycl` (Epic 4); Epic 3 `wamn-5ihq` is deferred.
- [Platform deploy](platform-deploy.md): done, `wamn-jqdja`.
- [Platform install](platform-install.md): live, `wamn-whdih` (E1).
- [Platform UI](platform-ui.md): done, `wamn-a40n`.
- [Platform UI application](platform-ui-app.md): draft, `wamn-n8q9m`.
- [Request round-trips](request-round-trips.md): live, `wamn-stxy9`.
- [Routes, workflows, and the router](routes-router.md): done, `wamn-upl3`.
- [Table data loading](table-adv-data.md): done, `wamn-3nsf`.
- [Table presentation](table-adv-ui.md): done, `wamn-8iul`.
- [Transactional composition](trx-pkg-plan.md): done, `wamn-14bx8`, `wamn-lqtai`, `wamn-ponya` and `wamn-aemxz`.
- [Web deployment](web-deployment.md): done, `wamn-xyxj`.
- [Web operator client](web-operator-client.md): deferred, item 9 of §7 has no bead.
- [Workflow feature](workflow-feature.md): done, `wamn-upl3`.
- [Write log](write-log.md): done, `wamn-an24`.

## Moved to history

- [Google Cloud deployment](../history/gcp-deployment.md): done, `wamn-ghx2`; [platform deploy](platform-deploy.md) replaced it.
- [Edge](../history/edge.md): done, `wamn-e5in`; its status section is now [architecture](../architecture/edge.md).
- [Upgrades](../history/upgrades.md): done, `wamn-orb5`.
- [Upgrade environment](../history/upgrade-environment.md): done, `wamn-m511`; replaced by [platform deploy](platform-deploy.md) §10.
- [Schema upgrade](../history/schema-upgrade.md): done, `wamn-o8b9`.
- [Kind to type](../history/kind-to-type.md): done, `wamn-sfea`.
- [Application simplification](../history/application-simplification-plan.md): superseded; [platform deploy](platform-deploy.md) is the one authority.
- [Host-owned transaction](../history/host-transaction.md): done, `wamn-4afx`.
- [Operation ids](../history/operation-ids.md): done, `wamn-cuqc`.
- [User identity and login](../history/identity-plan.md): done, `wamn-a045` and `wamn-k3mu`; open work is [identity](identity.md).
- [Platform UI history](../history/platform-ui-history.md): sections 0, 1 and 3 to 9 of platform UI; section 5 is replaced by [platform deploy](platform-deploy.md) §12.
- [Release qualification](../history/release-qualification.md): done, `wamn-ld93.33`; it names deleted verbs, and [platform deploy](platform-deploy.md) §15 holds the current behavior.
