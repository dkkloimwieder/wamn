# Edge box

These commands operate `wamn-edge` on its box. [Building](building.md#the-edge-box-binary) builds the binary, and [the edge architecture](../architecture/edge.md) holds its behavior.

## Serve

Serve the release that the configuration names. The listener must use a loopback address:

```bash
wamn-edge --config <path>
```

Without `--config`, `WAMN_EDGE_CONFIG` names the file. [`edge.example.toml`](../../services/edge/edge.example.toml) shows every key.
The `[target]` keys `org`, `project`, and `env` name the environment of the box.
The device also accepts `WAMN_ORG` and `WAMN_PROJECT` for these two keys.
The environment has no variable. The edge reads it only from the file.
The release names no environment key.
The edge derives `project_env_key` as `<org>--<env>--<project>` from that coordinate.

## Read the status of a running edge

Run this command on the box while the edge runs:

```bash
wamn-edge --config <path> status
```

The command reads `status.sock` in the directory of the run-state file. It prints the start time, the dropped frames, the credential failures of the forward, whether the forward stopped, the pending sample count, and one line per refused sample. If the forward stopped, renew the token file and restart the edge.

A process on the box can also read the JSON answer. With the run-state path of `edge.example.toml`, the command is:

```bash
curl --unix-socket /var/lib/wamn-edge/status.sock http://localhost/status
```

## Resolve refused samples

Stop the edge first, because a running edge holds the run-state file.
After opening the store, the command waits for its SQLite connection to close before it returns.

```bash
wamn-edge --config <path> samples list
wamn-edge --config <path> samples resolve <sample_key> <basis>
```

`<basis>` is `external-evidence`, `counterparty-confirmation` or `operator-judgment`.

## Serve the cluster web edge

The `edge-web` image runs `wamn-web`, a separate service from the device binary.
Its JSON settings name the web bucket, identity HTTPS issuer and `trusted_proxy_hops`. The service listens on port 8080 and reads the identity CA and CSRF key from mounted files.
Count trusted proxies from the direct peer toward the client. Zero uses the direct peer.
Direct requests without a forwarded header also use the peer, including Kubernetes port forwards.
The installer selects two for GCP: its load balancer appends the client and its own address to `X-Forwarded-For`.
Local ingress uses one. The front forwards only the selected client address for throttling.
TLS for browser traffic terminates at ingress.

```bash
wamn-web --settings <path>
wamn-web --settings <path> probe
```

The service discovers hosts from release ConfigMaps.
The probe requests `/` and `/config.json` through the local listener for each declared host.
It fails if a response is not HTTP 200.
