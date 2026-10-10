# Repository delivery

## Change checks

Run the existing exact tests that cover the changed behavior before integration.
The command compiles the selected target with locked dependencies and then executes each named case.
An empty selection, failure, ignored case, or reported skip cannot establish success.

```bash
wamn-ctl check-changes --repository "$SOURCE" \
  --package wamn-client-tui --test screen \
  --case unchanged_activation_preserves_state_but_same_schema_target_replacement_resets_everything \
  --result "$CHANGE_RESULT"
```

Replace the package, target, and case with the existing test for the change.
The package must be a member of exactly one of the root and apps workspaces.
The command builds and runs the test in that workspace.
For an ignored case, add `--include-ignored`.
A case that uses PostgreSQL takes its database from the [test server](running-tests.md#test-database-isolation) and needs no database input.
A change result records its source state and cannot substitute for release qualification.

## Candidate qualification

For a package successor with application migrations, first follow [package upgrades](deployment.md#package-upgrades).
`qualify-upgrade` proves the database transition before application. It does not qualify a release or replace the candidate qualification below.

Build the selected application artifacts through the existing [build owners](building.md).
Build native delivery binaries with `tools/delivery-owned build-native "$DELIVERY_TARGET"` before preparing the candidate.
The application build owners and qualification use this same command.
It selects the host and four helper packages together, with locked dependencies, default features, and the development profile.
It fixes compiler flags and the build environment, disables incremental compilation, and normalizes output-directory paths.
Reproduction requires the same clean checkout path, pinned Rust version, native tools, and Cargo configuration.
Publish its release through the existing release command.
Upload native images as inactive artifacts when the disposable cluster needs registry access.
Record their immutable `repository@sha256:digest` references.

Capture the published manifest and explicit artifact locations:

```bash
wamn-ctl prepare-release \
  --database-url "$DELIVERY_DATABASE_URL" --org "$DELIVERY_ORG" --project "$DELIVERY_PROJECT" \
  --environment "$DELIVERY_ENVIRONMENT" --tenant "$DELIVERY_TENANT" \
  --route-host "$DELIVERY_ROUTE_HOST" \
  --release-digest "$DELIVERY_RELEASE_DIGEST" --artifact-base "$DELIVERY_ARTIFACT_BASE" \
  --target-directory "$DELIVERY_TARGET" \
  --host-image "$DELIVERY_HOST_IMAGE" \
  --manifest-output "$DELIVERY_MANIFEST" --candidate-output "$DELIVERY_CANDIDATE"
```

Use unused absolute paths for both output files.
The candidate records the org, the project, the environment, the tenant and the route host, because the manifest does not name them.
The qualification fixtures take the org, the project, the tenant, the environment, the route host and the packages from the candidate.
Add each deployment document, application request, and expected response with `--deployment-file` before qualification.
For an owned registry reached through another address inside kind, add `--native-registry-endpoint HOST:PORT`.
If that endpoint uses HTTP, also add `--native-registry-insecure`.
The mapping applies only inside the newly created nodes.
Every native image reference must name the same explicit registry authority.

Select a clean integrated checkout and run qualification:

```bash
wamn-ctl qualify-release --repository "$SOURCE" --revision main \
  --candidate "$DELIVERY_CANDIDATE" --result "$DELIVERY_QUALIFICATION"
```

The checkout must match the selected revision and stay clean through all checks.
A release tag or explicit revision can replace `main`.
Qualification reconstructs fresh application schemas, compares generated output, and uses SQLx CLI 0.9.0 through the shared platform verifier.
It refuses committed SQLx metadata that is missing, changed, or holds query files that no query uses.
It rebuilds through Cargo, then refuses artifacts that differ from the candidate.
It pulls each image by the digest that the candidate names and records its image ID. It builds no image.
The application cases consume the supplied artifacts without replacement builds.

The package set of the candidate selects the cases exactly. Any other set fails.
A Receiving release with Acme runs command histories, baseline overlay compatibility, and durable queue recovery after interruption of its combined host.
A Receiving release without Acme runs command histories, which calls no Acme route.
Durable queue recovery starts its item from a wiring, and Receiving alone has none.
WMS runs its released routes, partial-label completion, and restart/cache cases against the same supplied manifest and host image.
Both require exact canonical release bytes, executed success, unchanged artifacts, and successful cleanup.
Qualification writes pass or fail with the source commit, command results, release inputs, and artifact hashes.
It creates temporary application results in the system temporary directory and removes them after the cases pass.
If a case fails, qualification keeps that directory and names it in the cause of the failed check.
Read the case results there, then remove the directory.
Keep the qualification result and candidate files until publication and deployment finish.
No source-host or CI-provider API is required.

## Qualified publication

A qualification result binds executed application checks to exact release files and one source commit.
Keep that result and its files available until deployment finishes.
The publication command reads the existing immutable catalog snapshot and requires the same tenant, release ID, and canonical manifest bytes.
Every required check must pass, and every recorded artifact must retain its digest.

Use the existing project and control database credentials for an authorized operator.
Keep registry credentials in the private file accepted by `--registry-auth-file`.
For a registry without authentication, write an explicit empty entry, `{"auths":{"<registry>":{}}}`, and the push is anonymous.
A missing entry still refuses with `registry-credentials-not-found`.
Keep credentials outside the candidate files and qualification output.
The command uses the existing OCI publisher and records the qualification source commit with its upload record.
An upload record establishes publication, while the deployment command separately requires readiness and an authenticated application result.

Set the existing publication scope once:

```bash
release_args=(
  --database-url "$DELIVERY_DATABASE_URL"
  --control-database-url "$DELIVERY_CONTROL_DATABASE_URL"
  --org "$DELIVERY_ORG"
  --project "$DELIVERY_PROJECT"
  --environment "$DELIVERY_ENVIRONMENT"
  --tenant "$DELIVERY_TENANT"
  --release-digest "$DELIVERY_RELEASE_DIGEST"
  --artifact-base "$DELIVERY_ARTIFACT_BASE"
  --registry-auth-file "$DELIVERY_REGISTRY_AUTH_FILE"
)
wamn-ctl publish-qualified-release \
  --qualification "$DELIVERY_QUALIFICATION" \
  --release-chart "$WAMN_RELEASE_CHART" "${release_args[@]}"
```

If the registry certificate chains to a private CA, add `--oci-ca-path` with that CA to `release_args`, or set `WASH_OCI_CA_PATHS`.
If the registry uses HTTP, add `--insecure-registry` for that registry.
The existing publisher preserves an exact retry and refuses conflicting content or source attribution.
Different release bytes require a different identity through the existing release creation command.

## Qualification record and deployment

`publish-qualified-release` records the passing qualification file in `catalog.qualifications` in the control database, after the push.
The key is the package set of the release, its `(package_id, version, component_digest)` triples, and the platform image set of the stamped release chart that `--release-chart` names: the `@sha256` digests of the host image and of each role component (R12).
The verb refuses a chart whose host image is not the one the qualification ran on.
The row also records the gates and identity digests the qualification ran with; they are not part of the key.
`wamn-ctl env apply` deploys a release: the environment document names its digest, and `apply` refuses a release with no recorded qualification on the image set of the release chart.
See [deployment](deployment.md) for the environment verbs.

Receiving and WMS qualification require a supplied identity image through `--identity-image`, because their published application routes accept sessions.
The owned fixtures start that image and supply issuer trust to their application hosts.
