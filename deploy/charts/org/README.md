# Org chart

The org chart declares the tenant namespace, base RBAC, resource quota, and default-deny ingress and egress.
The platform selects the namespace by its `wamn.dev/org` label.
Set `org` to the registry org slug. The platform, hosts, cert-manager, default, kube-* and wamn names are reserved.
The quota caps object counts and storage requests. Set individual limits in `quota`.

`wamn org create` finds the API server destinations and sets `networkPolicy.apiServer`.
It reads the required `registryEgress` list from `wamn-org-inputs` and sets `networkPolicy.registry`.
For direct Helm installs, set both lists with `cidr` and TCP `port` pairs for this installation.
Include addresses and ports used before and after Service translation where the network plugin requires them.
Environment and project releases supply the policy rules for their host groups, project databases, and CDC readers.

`wamn org create` creates and uses `wamn-helm-<org>` as the Helm release namespace.
The chart grants the org worker access to Helm release Secrets in that namespace. Keep payload and credential Secrets in their resource namespaces.
Set `control.image` and `control.configuration` to deploy the org's control service and render the settings file shared with its worker. Pass the platform's `global.identityIssuer` and `global.nats.schedulerUrl` as `identity_issuer` and `nats_url`.
The control API route uses `<org>.<base>`. The `wamn org create` command reads the platform wildcard base from `wamn-org-inputs`.
The command also reads `applicationChart` from that ConfigMap and sets the `chart` value in the control configuration.
Set `control.configuration.registry` to the org origin for packages and application components.
Set `WAMN_REGISTRY` to that org origin when you publish. On kind, use `localhost:5001` from the publishing machine.
Set `control.configuration.platform_registry` to the platform origin for integrations.
`wamn org create` currently initializes both from `registry` in `wamn-org-inputs`.
Pin `control.image` by digest. Set `control.platformUi` to publish the UI release at that host.
Control mounts the org's control and apply logins from named Secrets. The chart creates both Secrets with their public usernames.
After Helm installs the chart, the org SQL Job in `platform` writes the passwords.
`wamn org create` copies the platform's `identity.oauthEncryption` to `control.configuration.oauth_encryption`.
For local encryption, the chart creates an empty `wamn-oauth` Secret in the org namespace and grants the org SQL Job access to it.
The Job writes a random 32-byte `key` only when the entry is absent. Control and the environment hosts use that key.
Cloud KMS uses no org OAuth Secret.
Pass the public CA certificates as PEM through `identityCa`, `natsCa`, and `postgresCa`.
Read `tls.crt` from `platform/wamn-identity-ca` and `platform/wasmcloud-ca`, and `ca.crt` from `platform/wamn-pg-ca`.
