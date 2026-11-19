# Environment chart

Set `org` and `env` to identify the environment. The chart renders the host group from the selected image, placement, `drainBoundSeconds`, shared endpoints and eligible `projects`. With no eligible projects, the chart omits the host group.
Helm uses server-side apply and skips CRD installation. The infrastructure chart provides the runtime resource types.

The chart mounts one host settings file and the project database credentials. The host listens on port 9191. The environment chart allows traffic on that port from the edge and uses each project's database cluster name to allow database egress.
Application releases supply public integration egress rules for the host group.
