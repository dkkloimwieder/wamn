# Receiving change walkthrough

This walkthrough uses an installed `kind` platform. Follow the [development loop](../../../docs/operations/development-loop.md) to prepare publication credentials, the build database, and `environment.k`.
Name the document's selected application `wamn_receiving`. Use the platform's identity service for browser sign-in.

## Prepare a release

From the repository root, publish and apply the initial schema. The publish command prints the release digest.

```bash
"$CARGO_TARGET_DIR/debug/wamn" schema publish apps/wamn_receiving --env environment.k
"$CARGO_TARGET_DIR/debug/wamn" schema apply wamn_receiving --release <published-digest> --env environment.k
"$CARGO_TARGET_DIR/debug/wamn" dev --config environment.k --overlay-root apps/wamn_receiving --watch
```

The watcher builds immutable prerelease packages, publishes them, and requests deployment through control.
It watches the package and its build dependencies. Saving unchanged content keeps its coordinate.
A failed build leaves the deployed release available.

## Change a response

Change a response value in `apps/wamn_receiving/component/src/reads.rs`.
Save the file and wait for deployment to finish.
Request the affected operation through the installed `/api/wamn_receiving` route and check the changed value.
Restore the source file and wait for the next deployment.
Use the product schema commands before deploying a change to application SQL.
The watcher keeps application data between saves.

The location and purchase-order screens need imported application data.
The missing product import route is tracked by `wamn-eulfr.12.9`.
Their automated assertions stop at that missing step; they do not create data through a private database provisioner.

## Open the browser

In a second terminal, run the browser client against the installed application URL.

```bash
cd apps/wamn_receiving/web
WAMN_ROUTE_URL=https://receiving.example.test pnpm run dev
```

Open Vite's address and sign in with the platform account.
Vite reads configuration from the installed web service and preserves `/api` when it forwards requests.
See the [web README](../web/README.md) for the screens and data prerequisites.

## Measure a change

Use the three separate build, publication, and deployment commands in the [measurement procedure](../../../docs/operations/development-loop.md#measure-one-change).
Record the source diff, platform identity, resulting release digest, and each duration.
Run the measurement when the machine is idle. Earlier native-process measurements do not describe this deployment path.
