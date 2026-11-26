# Development loop

Local development runs on `kind` through the deployed product path.
Helm installs the platform once. The native control service owns changes to deployed state.
The development loop watches source inputs, builds packages, publishes artifacts, and requests deployment through authenticated control routes.
The deployed release stays available after the command exits.

## Prepare the platform

Use the [deployment procedure](deployment.md) to install a disposable development platform.
Apply its authored `env.yaml` through `wamn env apply`.
Make the registry and control endpoint reachable from the development machine.
Use the platform's own credential and identity services for connections and sign-in.

Set `WAMN_CONTROL_URL` to the control service URL and `WAMN_TOKEN` to the caller's token.
If SQL or the verification schema changes, refresh the [committed describe evidence](building.md#wamn-build).
Pass `--refresh-describe` to a dev invocation to refresh with the dev base pins.
Set `DATABASE_URL` or pass `--database-url` for that invocation.
Set `WAMN_REGISTRY` to the org registry origin for publication.
Set `WAMN_REGISTRY_AUTH_FILE` to the registry credential file.
`WAMN_INGRESS_COMPONENT` selects the stamped HTTP route component and its adjacent palette.
If the registry needs extra trust roots, set `WAMN_OCI_CA_PATHS`.
The platform and local publication inputs must use the same stamped artifacts.

## Build and deploy

The environment document selects one application for the development session.
The source package and its declared bases supply the package closure.
The loop builds each base before the overlay.

```bash
"$CARGO_TARGET_DIR/debug/wamn" publish apps/platform_fixture --env env.yaml
"$CARGO_TARGET_DIR/debug/wamn" schema apply platform_fixture --release <published-digest> --env env.yaml
"$CARGO_TARGET_DIR/debug/wamn" dev --config env.yaml --overlay-root apps/platform_fixture --watch
```

Without `--watch`, the command builds, publishes, and deploys once.
The watcher includes the source closure, shared platform sources, shared WIT sources, and the environment document.
It excludes generated output and Git-ignored directories.
Changes that arrive during a deployment wait for the next pass.
A failed pass reports its error. A later source change retries the product operation.
Interrupting the watcher waits for the active deployment to finish.
The package SQL step applies pending migrations and keeps the SQLx checksum of each installed migration.
The deploy worker applies SQL before it installs the new code, so a separate schema apply is optional.

Package tags point to the latest digest published under each version.
A replacement must preserve every migration and use higher numbers for additions.
The development builder assigns each source snapshot a content-derived prerelease tag.
For example, `0.1.0-dev.h0123456789ab` uses the first 12 hexadecimal digits of its content digest.
The prefix `h` keeps all-digit digests valid in semantic versions.
The digest includes package sources, shared build inputs, and built bases.
A repeated save uses the same coordinate. Changed content gets a new coordinate.
The builder records the matching base versions and component digests in overlays.
Refresh appends generated migrations to the package’s existing `history/` directory.
Each dev pass carries those files into the package it publishes.
Only dev package manifests carry the OCI publication timestamp `org.opencontainers.image.created`.
A repeat publication under the same tag keeps the original timestamp.

## Measure one change

Use an existing disposable platform when the machine has no other build, test, or cluster load.
Record the source commit, source diff, stamped platform identity, and published digest with the result.
Make one fixture change that changes its HTTP response.
The selected package coordinate must be unpublished.
Run the three phases separately:

```bash
/usr/bin/time -f 'build %e seconds' "$CARGO_TARGET_DIR/debug/wamn" build apps/platform_fixture
/usr/bin/time -f 'publication %e seconds' "$CARGO_TARGET_DIR/debug/wamn" publish apps/platform_fixture --env env.yaml
/usr/bin/time -f 'deployment %e seconds' "$CARGO_TARGET_DIR/debug/wamn" deploy --app platform_fixture --release <published-digest> --env env.yaml
```

The publish command reads existing build output and prints the top package OCI digest.
The deploy command waits for the control operation to finish.
Request the fixture's changed HTTP route and compare its returned value with the source change.
Record build, publication, and deployment times separately.
Stop the measurement after a second failure.
