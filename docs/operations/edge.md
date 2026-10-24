# Edge box

These commands operate `wamn-edge` on its box. [Building](building.md#the-edge-box-binary) builds the binary, and [the edge architecture](../architecture/edge.md) holds its behavior.

## Serve

Serve the release that the configuration names:

```bash
wamn-edge --config <path>
```

Without `--config`, `WAMN_EDGE_CONFIG` names the file. [`edge.example.toml`](../../services/edge/edge.example.toml) shows every key.
The `[scope]` keys name the org, the project and the environment of the box.
The org and the project also take the variables `WAMN_ORG` and `WAMN_PROJECT` that the cloud host reads.
The environment has no variable. The edge reads it only from the file.
The release names no tenant, so the edge takes the tenant `<org>--<project>--<environment>` of that coordinate.

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

## Resolve refused samples and uncertain intents

Stop the edge first, because a running edge holds the run-state file.

```bash
wamn-edge --config <path> samples list
wamn-edge --config <path> samples resolve <sample_key> <basis>
wamn-edge --config <path> intents list
wamn-edge --config <path> intents resolve <id> <basis>
```

`<basis>` is `external-evidence`, `counterparty-confirmation` or `operator-judgment`.

## Serve the cluster web edge

The `edge-web` image builds `wamn-edge` with the `web` feature. The box binary does not have these two subcommands. Both require `--config`, a JSON file of the web edge.

```bash
wamn-edge --config <path> web
wamn-edge --config <path> web-check
```

`web` serves the bucket files of the releases that the file names. `web-check` requests `/` and `/config.json` from `wamn-edge-web.edge.svc.cluster.local` for each host in the file. It fails if one response is not 200.
