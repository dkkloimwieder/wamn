# Org chart

The org chart declares the tenant namespace, base RBAC, resource quota, and default-deny ingress and egress.
The platform selects the namespace by its `wamn.dev/org` label.
Set `org` to the registry org slug. The platform, hosts, cert-manager, default, kube-* and wamn names are reserved.
The quota caps object counts and storage requests. Set individual limits in `quota`.

Set `networkPolicy.apiServer` and `networkPolicy.registry` to lists of CIDR and TCP port pairs for this installation.
Include addresses and ports used before and after Service translation where the network plugin requires them.
Environment and project releases supply the policy rules for their host groups, project databases, and CDC readers.

Create `wamn-helm-<org>` before installing the org chart, and use it as the Helm release namespace.
The trusted install entry point in Epic 10 will create it.
The chart grants the org worker access to Helm release Secrets in that namespace. Keep payload and credential Secrets in their resource namespaces.
Set `control.image` and `control.configuration` to deploy the org's control service.
Set `control.routeHost` and pin `control.image` by digest to publish its API route. Set `control.platformUi` to publish the UI release at that host.
Control mounts its database login and system migration credential from named Secrets. Supply both Secrets before installing control.
Pass the public CAs through `identityCa` and `postgresCa`. Set `control.certificateIssuer` to the existing namespaced Issuer for the control client certificate.
