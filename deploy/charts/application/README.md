# Application chart

Each application release records its top package OCI digest in the WorkloadDeployment annotation `wamn.package-digest`.
The annotation `wamn.release-revision` records the revision that rendered the live content.
When Helm rolls back to revision N, it restores N's saved manifest and annotations.
Status reports N after that rollback. Use the same number with `wamn rollback --revision N`.

Without an explicit revision, the worker selects the newest earlier revision whose manifest contains its own revision number.
It checks enabled workflows before it runs Helm rollback.
Application upgrades and rollbacks use `--wait=watcher`. The project operation Role already grants `list` and `watch` on this chart's ConfigMaps, Services, and WorkloadDeployments.

The control deployment's operation Role lets it create the ConfigMap that carries each Job's input.
The chart can grant named reads for the control database credentials during the transition to mounted database credentials.

Each release with wasm integration authorities adds one egress policy for its environment's host group.
The deploy resolves each authority to its explicit TCP port or the default socket port for its scheme.
Policies add up across releases and Helm removes a release's policy on uninstall.
The public IPv4 rule excludes fixed private and special-use ranges. wash-runtime enforces each integration's `allowedHosts`.
