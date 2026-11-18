# Org chart

The org chart declares the tenant namespace, base RBAC, resource quota, and default-deny ingress and egress.
Set `org` to the registry org slug. The platform, hosts, cert-manager, default, kube-* and wamn names are reserved.
The quota caps object counts and storage requests. Set individual limits in `quota`.

Set `networkPolicy.apiServer` and `networkPolicy.registry` to lists of CIDR and TCP port pairs for this installation.
Include addresses and ports used before and after Service translation where the network plugin requires them.
Environment and project releases supply the policy rules for their host groups, project databases, and CDC readers.

The operation worker keeps the Helm Secret permissions while releases and payloads share the org namespace.
