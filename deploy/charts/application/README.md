# Application chart

Set `namespace` to the org namespace. Helm uses server-side apply and skips CRD installation for application installs and upgrades.

Each application release has one route workload. Its ingress configuration comes from `route.config`.

Each application release records its top package OCI digest in the WorkloadDeployment annotation `wamn.package-digest`.
The annotation `wamn.release-revision` records the revision that rendered the live content.
When Helm rolls back to revision N, it restores N's saved manifest and annotations.
Status reports N after that rollback. Use the same number with `wamn rollback --revision N`.

Without an explicit revision, the worker selects the newest earlier revision whose manifest contains its own revision number.
The worker reads `route.config` from the target revision's saved values to check enabled workflows before it runs Helm rollback.
Application upgrades and rollbacks use `--wait=watcher`. The org worker Role grants `list` and `watch` on the non-Secret resources the watcher observes.

Deploy uses the `workflow` NATS binding for the workflow service. It adds the `ingress` binding when the application declares Data change handlers.

Each release with wasm integration authorities or bound host HTTP connections adds one egress policy for its environment's host group.
The deploy resolves each authority to its explicit TCP port or the default socket port for its scheme.
For a host HTTP connection, `connectionPorts` holds the ports of its primary authority and its OAuth `token_authority`.
Policies add up across releases and Helm removes a release's policy on uninstall.
The public IPv4 rule excludes fixed private and special-use ranges. wash-runtime enforces each integration's `allowedHosts`.
