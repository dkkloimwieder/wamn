# Org chart

The org chart declares the tenant namespace, base RBAC, resource quota, and default-deny ingress and egress.
The platform selects the namespace by its `wamn.dev/org` label.
Set `org` to the registry org slug. The platform, hosts, cert-manager, default, kube-* and wamn names are reserved.
The quota caps object counts and storage requests. Set individual limits in `quota`.

Set `networkPolicy.apiServer` and `networkPolicy.registry` to lists of CIDR and TCP port pairs for this installation.
Include addresses and ports used before and after Service translation where the network plugin requires them.
Environment and project releases supply the policy rules for their host groups, project databases, and CDC readers.

`wamn org create` creates and uses `wamn-helm-<org>` as the Helm release namespace.
The chart grants the org worker access to Helm release Secrets in that namespace. Keep payload and credential Secrets in their resource namespaces.
Set `control.image` and `control.configuration` to deploy the org's control service.
Set `control.routeHost` and pin `control.image` by digest to publish its API route. Set `control.platformUi` to publish the UI release at that host.
Control mounts its database login and system migration credential from named Secrets. The chart creates the control login Secret with its public username. The platform SQL Job writes its password after Helm installs the chart.
Pass the public CAs through `identityCa` and `postgresCa`. Set `control.certificateIssuer` to the existing namespaced Issuer for the control client certificate.
