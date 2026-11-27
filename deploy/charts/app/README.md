# Application chart

Set `namespace` to the environment namespace from the registry. Helm uses server-side apply and skips CRD installation for application installs and upgrades.

Each application release has one route workload. Its ingress configuration comes from `route.config`.
The HTTP Service publishes the app and project names, release digest, and optional browser pin in the `wamn.dev/web-release` annotation.
The workload uses the HTTP Service DNS name as its Host binding.

Each application release records its top package OCI digest in the WorkloadDeployment annotation `wamn.package-digest`.
The annotation `wamn.release-revision` records the revision that rendered the live content.
When Helm rolls back to revision N, it restores N's saved manifest and annotations.
Status reports N after that rollback. Use the same number with `wamn rollback --revision N`.

Without an explicit revision, the worker selects the newest earlier revision whose manifest contains its own revision number.
The worker reads `route.config` from the target revision's saved values to check enabled workflows before it runs Helm rollback.
Application upgrades and rollbacks use `--wait=watcher`. The environment worker Role grants `list` and `watch` on the non-Secret resources the watcher observes.
Application upgrades also use `--rollback-on-failure`. Automatic and manual rollback both keep committed SQL migrations.
A revision must include a mounted app route on its HTTP Service to serve as an upgrade or rollback baseline.
After Helm returns success, the worker checks that each workload's current replica set serves the selected template and reports Ready for its generation. It uses the time left in the Helm phase for this check.
If the upgraded template does not serve in time, the worker rolls back and checks the restored template. If the release was a first install, the worker uninstalls it. The environment worker Role grants `get` on WorkloadReplicaSets for these checks.
At the start of a deploy or rollback, the worker checks the deployed Helm revision. If it is unready, the worker restores the newest earlier successful revision or uninstalls an interrupted first install.

Deploy uses the `workflow` NATS binding for the workflow service. It adds the `ingress` binding when the application declares Data change handlers.

Each release with wasm integration authorities or bound host HTTP connections adds one egress policy for its environment's host group.
The deploy resolves each authority to its explicit TCP port or the default socket port for its scheme.
For a host HTTP connection, `connectionPorts` holds the ports of its primary authority and its OAuth `token_authority`.
Policies add up across releases and Helm removes a release's policy on uninstall.
The public IPv4 rule excludes fixed private and special-use ranges. wash-runtime enforces each integration's `allowedHosts`.
