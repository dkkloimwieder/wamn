# Environment chart

Set `namespace` to the org namespace. The runtime host group's `namespace` must match.
Helm uses server-side apply and skips CRD installation. The installation chart provides the runtime resource types.

The environment chart supplies policy rules for its host group and disables the runtime-operator chart's host policy.
The host accepts ingress from the edge on the same HTTP port that its host group declares.
The lifecycle worker supplies `networkPolicy.projectClusters` from the project's database placement.
Application releases supply public integration egress rules for the host group.
