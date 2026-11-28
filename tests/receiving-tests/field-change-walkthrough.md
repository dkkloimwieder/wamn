# Receiving change walkthrough

This walkthrough uses an installed `kind` platform. Follow the [development loop](../../docs/operations/development-loop.md) to prepare publication credentials, the build database, and `env.yaml`.
Name the selected application `wamn-receiving` in the environment file.
Use the platform's identity service for browser sign-in.

## Prepare a release

From the repository root, publish and apply the initial schema. The publish command prints the release digest.

```bash
"$CARGO_TARGET_DIR/debug/wamn" schema publish apps/wamn_receiving --env env.yaml
"$CARGO_TARGET_DIR/debug/wamn" schema apply wamn-receiving --release <published-digest> --env env.yaml
"$CARGO_TARGET_DIR/debug/wamn" dev --config env.yaml --overlay-root apps/wamn_receiving --watch
```

The watcher builds immutable prerelease packages, publishes them, and requests deployment through control.
It watches the package and its build dependencies. Saving unchanged content keeps its coordinate.
A failed build leaves the deployed release available.

## Change a response

Change a response value in `apps/wamn_receiving/component/src/reads.rs`.
Save the file and wait for deployment to finish.
Request the affected operation under `/<project>/wamn-receiving/api/`.
Make sure that the response contains the changed value.
Restore the source file and wait for the next deployment.
Use the product schema commands before deploying a change to application SQL.
The watcher keeps application data between saves.

The location and purchase-order screens need imported application data.
The missing product import route is tracked by `wamn-eulfr.12.9`.
Their automated assertions stop at that missing step; they do not create data through a private database provisioner.

## Open the browser

The installed application URL ends in `/<project>/wamn-receiving/`.
Use that URL for `WAMN_ROUTE_URL` if you run Receiving through Vite.
See the [web README](../../apps/wamn_receiving/web/README.md) for the screens and data prerequisites.

## Measure a change

Use the three separate build, publication, and deployment commands in the [measurement procedure](../../docs/operations/development-loop.md#measure-one-change).
Record the source diff, platform identity, resulting release digest, and each duration.
Run the measurement when the machine is idle. Earlier native-process measurements do not describe this deployment path.
