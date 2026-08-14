# Environment credential resolution

Secret material is environment-owned authority. An authored flow never names a
credential and never carries secret material. It declares a typed portable
connection requirement, and a node selects that requirement through its
`connection` field. The target environment binds the requirement to a
connection instance whose immutable generation selects an opaque credential-set
handle.

This ownership split keeps the same flow artifact portable across environments:

```text
flow artifact                 environment                     run worker
-------------                 -----------                     ----------
connection requirement  --->  release binding          --->  verified plan facts
node.connection               instance generation            credential-set handle
                               secret source             --->  trusted host adapter
```

The flow artifact and execution plan may retain requirement, binding, instance,
and generation identities. They never retain secret bytes. Durable effect facts
record the credential generation that authorized an occurrence, not the value.

## Mounted secret source

The initial host-owned source is the optional file mounted by
`deploy/platform/runner.yaml` at `/etc/wamn/credentials/credentials.json`. Its
JSON shape is `{project: {handle: secret}}`, following the project-file pattern.
`deploy/platform/runner-credentials.example.yaml` is a deployment example; real
entries are provisioned separately so applying the runner manifest cannot
replace them with sample values.

The runtime credential plugin parses that file and resolves handles within the
host-injected project identity. A missing optional file is an empty source; a
malformed file is a startup error. Resolution is fail-closed and audit logging
records only project, handle, and outcome. Secret values must not enter logs,
flow data, run input/output, node records, or error details.

Only the trusted connection adapter may request the environment-selected handle
for the active occurrence. Authored node configuration cannot select another
handle, and a sibling occurrence receives no ambient credential access.

## Egress boundary

Credential resolution does not authorize a destination. Outbound HTTP must pass
both the connection-derived authority for the resolved instance generation and
the trusted platform host policy supplied to the run worker. The platform list
is configured with `--allowed-hosts` / `WAMN_ALLOWED_HOSTS`; an empty list denies
all. Cluster network policy remains an independent outer ceiling.

The host returns a typed denial instead of trapping the component when either
authority check fails. Rotation creates a new credential generation; it does not
change the portable flow artifact.

## Verification

The current local gates cover strict mounted-file parsing and project-scoped
lookup in `crates/platform/runtime/src/plugins/wamn_credentials.rs`, plus the
standard HTTP adapter’s injection and error classification. Commands are listed
in [build-and-test.md](../build-and-test.md) under `[5.9]`.
