# Application chart

Each application release records its top package OCI digest in the WorkloadDeployment annotation `wamn.package-digest`.
The annotation `wamn.release-revision` records the revision that rendered the live content.
When Helm rolls back to revision N, it restores N's saved manifest and annotations.
Status reports N after that rollback. Use the same number with `wamn rollback --revision N`.

Without an explicit revision, the worker selects the newest earlier revision whose manifest contains its own revision number.
It checks enabled workflows before it runs Helm rollback.
Application upgrades and rollbacks use `--wait=watcher`. The project operation Role already grants `list` and `watch` on this chart's ConfigMaps, Services, and WorkloadDeployments.
