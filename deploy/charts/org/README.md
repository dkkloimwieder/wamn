# Org chart

The org chart declares the org namespace, base RBAC and default-deny ingress and egress.
The platform uses `wamn.dev/org` for org control and `wamn.dev/org-workloads` for services shared by org and env workloads.
The chart sets Pod Security enforcement and warnings to `restricted` in the org namespace.
Set `org` to the registry org slug. The platform, hosts, cert-manager, default, kube-* and wamn names are reserved.
`wamn env create` applies the resource quota and container defaults to each env namespace using the system database settings.
If the registry marks the env as prod, the command applies `wamn.dev/prod=true` to its workload namespace.

`wamn org create` finds the API server destinations and sets `networkPolicy.apiServer`.
It reads the required `registryEgress` list from `wamn-org-inputs` and sets `networkPolicy.registry`.
For direct Helm installs, set both lists with `cidr` and TCP `port` pairs for this installation.
Include addresses and ports used before and after Service translation where the network plugin requires them.
Environment and project releases supply the policy rules for their host groups, project databases, and CDC readers.

`wamn org create` creates and uses `wamn-helm-<org>` as the Helm release namespace.
Environment workers store their release records in their own Helm namespace.
Set `control.image` and `control.configuration` to deploy the org's control service and render its settings file. Pass the platform's `global.identityIssuer` and `global.nats.schedulerUrl` as `identity_issuer` and `nats_url`.
The control API route uses `<org>.<base>`. The `wamn org create` command reads the platform wildcard base from `wamn-org-inputs`.
The command also reads `appChart` from that ConfigMap and sets the `chart` value in the control configuration.
Set `control.configuration.registry` to the org origin for packages and application components.
Set `WAMN_REGISTRY` to that org origin when you publish. On kind, use `localhost:5001` from the publishing machine.
Set `control.configuration.platform_registry` to the platform origin for integrations.
`wamn org create` currently initializes both from `registry` in `wamn-org-inputs`.
Pin `control.image` by digest. Set `control.platformUi` to publish the UI release at that host.
The control Service publishes the route and UI pin in the `wamn.dev/web-release` annotation.
Control mounts its login from `wamn-control-db`. The chart creates the Secret with its public username.
After Helm installs the chart, the org SQL Job in `platform` writes the password.
`wamn org create` copies the platform's `identity.oauthEncryption` to `control.configuration.oauth_encryption`.
For local encryption, control reads `wamn-oauth` from the environment namespace.
trust-manager writes the public identity, NATS, and system database roots to ConfigMaps in the org namespace.
`wamn org create` waits for these ConfigMaps before it checks the control rollout.
