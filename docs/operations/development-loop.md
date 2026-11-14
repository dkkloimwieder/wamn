# Development loop

Local development runs on `kind` through the deployed product path.
Helm installs the platform once. The native control service owns changes to deployed state.
The development loop watches source inputs, builds packages, publishes artifacts, and requests deployment through authenticated control routes.
The deployed release stays available after the command exits.

## Prepare the platform

Use the [deployment procedure](deployment.md) to install a disposable development platform.
Apply its authored `environment.k` through `wamn env apply`.
Make the registry and control endpoint reachable from the development machine.
Use the platform's own credential and identity services for connections and sign-in.

Set `WAMN_CONTROL_URL` to the control service URL and `WAMN_TOKEN` to the caller's token.
Set `DATABASE_URL` to the PostgreSQL server that the package builder uses for disposable build databases.
Set the existing publication inputs: `WAMN_PACKAGE_ARTIFACT_BASE`, `WAMN_COMPONENT_ARTIFACT_BASE`, `WAMN_RELEASE_ARTIFACT_BASE`, and `WAMN_REGISTRY_AUTH_FILE`.
`WAMN_FLOW_HTTP_COMPONENT` selects the stamped HTTP route component and its adjacent palette.
If the registry needs extra trust roots, set `WAMN_OCI_CA_PATHS`.
The platform and local publication inputs must use the same stamped artifacts.

## Build and deploy

The environment document selects one application for the development session.
The source package and its declared bases supply the package closure.
Build the bases before the overlay when you prepare their build records.
[Building](building.md) describes package output and its source records.

```bash
"$CARGO_TARGET_DIR/debug/wamn" schema publish apps/platform_fixture --env environment.k
"$CARGO_TARGET_DIR/debug/wamn" schema apply platform_fixture --release <published-digest> --env environment.k
"$CARGO_TARGET_DIR/debug/wamn" dev --config environment.k --overlay-root apps/platform_fixture --watch
```

Without `--watch`, the command builds, publishes, and deploys once.
The watcher includes the source closure, shared platform sources, shared WIT sources, and the environment document.
It excludes generated output and Git-ignored directories.
Changes that arrive during a deployment wait for the next pass.
A failed pass reports its error. A later source change retries the product operation.
Interrupting the watcher waits for the active deployment to finish.
Use the product schema commands when application SQL needs to change.
The watcher does not reset application data on each save.

Package tags are immutable.
Publication refuses changed bytes for an existing package coordinate.
The development builder assigns each source snapshot an immutable prerelease coordinate.
For example, `0.1.0-dev.h0123456789ab` uses the first 12 hexadecimal digits of its content digest.
The prefix `h` keeps all-digit digests valid in semantic versions.
The digest includes package sources, shared build inputs, and built bases.
A repeated save uses the same coordinate. Changed content gets a new coordinate.
The builder records the matching base versions and component digests in overlays.
It writes development SQL history to build output without changing authored history.

## Measure one change

Use an existing disposable platform when the machine has no other build, test, or cluster load.
Record the source commit, source diff, stamped platform identity, and published digest with the result.
Make one fixture change that changes its HTTP response.
The selected package coordinate must be unpublished.
Run the three phases separately:

```bash
/usr/bin/time -f 'build %e seconds' "$CARGO_TARGET_DIR/debug/wamn" build apps/platform_fixture
/usr/bin/time -f 'publication %e seconds' "$CARGO_TARGET_DIR/debug/wamn" publish apps/platform_fixture --env environment.k
/usr/bin/time -f 'deployment %e seconds' "$CARGO_TARGET_DIR/debug/wamn" deploy --application platform_fixture --release <published-digest> --env environment.k
```

The publish command reads existing build output and prints its application and digest.
The deploy command waits for the control operation to finish.
Request the fixture's changed HTTP route and compare its returned value with the source change.
Record build, publication, and deployment times separately.
Stop the measurement after a second failure.
