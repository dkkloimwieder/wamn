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
For an ignored case, add `--include-ignored`.
For a database-backed case, supply its declared URL variable with `--database-url-env`.
The [owned PostgreSQL runner](running-tests.md#test-database-isolation) creates that database and supplies the URL.
A change result records its source state and cannot substitute for release qualification.

## Candidate qualification

Build the selected application artifacts through the existing [build owners](building.md).
Mint its release through the existing release command.
Upload native images as inactive artifacts when the disposable cluster needs registry access.
Record their immutable `repository@sha256:digest` references.

Capture the minted manifest and explicit artifact locations:

```bash
wamn-ctl prepare-release \
  --database-url "$DELIVERY_DATABASE_URL" --tenant "$DELIVERY_TENANT" \
  --effective-release-id "$DELIVERY_RELEASE_ID" --artifact-base "$DELIVERY_ARTIFACT_BASE" \
  --target-directory "$DELIVERY_TARGET" \
  --host-image "$DELIVERY_HOST_IMAGE" --gates-image "$DELIVERY_GATES_IMAGE" \
  --executor-image "$DELIVERY_EXECUTOR_IMAGE" \
  --manifest-output "$DELIVERY_MANIFEST" --candidate-output "$DELIVERY_CANDIDATE"
```

Use unused absolute paths for both output files.
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
Qualification reconstructs fresh application schemas, compares generated output, and uses SQLx CLI 0.9.0 with the actual verifier targets.
It rebuilds through Cargo and the existing Docker targets, then refuses artifacts that differ from the candidate.
Native comparison builds use the existing source commit and release profile labels.
The application cases consume the supplied artifacts without replacement builds.

The first disposable targets use the existing Receiving and Acme baseline release or the existing WMS route release.
Receiving runs command histories and baseline overlay compatibility against the same manifest.
WMS runs its released routes against its own manifest.
Both require exact canonical release bytes, executed success, unchanged artifacts, and successful cleanup.
A supplied executor image also runs the existing idle lifecycle assertions.
The gates assertions inspect its image and shared host layers.

Qualification writes pass or fail with the source commit, command results, release inputs, and artifact hashes.
It creates temporary application evidence beneath the repository evidence directory and removes its own files after the cases.
Keep the qualification result and candidate files until publication and deployment finish.
No source-host or CI-provider API is required.

## Owned application acceptance

Use a clean integrated checkout for the complete release command tests.
Set `CARGO_TARGET_DIR` to the absolute build directory for that checkout.
Run the existing Receiving and WMS fixtures with separate unused evidence directories:

```bash
tools/delivery-owned receiving "$PWD/evidence/delivery-receiving"
tools/delivery-owned wms "$PWD/evidence/delivery-wms"
```

Each fixture creates its own services, database, kind cluster, and native image registry.
It keeps the minted release store until preparation, qualification, publication, selection, and deployment finish.
The application owner supplies the authenticated request and expected response.
Receiving also supplies an executor image, while WMS uses the host image.
The fixture reports failure if an operation or cleanup fails.
It removes only its own resources and writes the command results beneath the chosen evidence directory.

## Qualified publication

A qualification result binds executed application checks to exact release files and one source commit.
Keep that result and its files available until deployment finishes.
The publication command reads the existing immutable catalog snapshot and requires the same tenant, release ID, and canonical manifest bytes.
Every required check must pass, and every recorded artifact must retain its digest.

Use the existing project and control database credentials for an authorized operator.
Keep registry credentials in the private file accepted by `--registry-auth-file`.
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
  --tenant "$DELIVERY_TENANT"
  --effective-release-id "$DELIVERY_RELEASE_ID"
  --artifact-base "$DELIVERY_ARTIFACT_BASE"
  --registry-auth-file "$DELIVERY_REGISTRY_AUTH_FILE"
)
wamn-ctl publish-qualified-release \
  --qualification "$DELIVERY_QUALIFICATION" "${release_args[@]}"
```

If the registry certificate chains to a private CA, add `--oci-ca-path` with that CA to `release_args`, or set `WASH_OCI_CA_PATHS`.
If the registry uses HTTP, add `--insecure-registry` for that registry.
The existing publisher preserves an exact retry and refuses conflicting content or source attribution.
Different release bytes require a different identity through the existing release creation command.

## Selection and deployment

Select an already published release before starting its deployment:

```bash
wamn-ctl select-release \
  --qualification "$DELIVERY_QUALIFICATION" "${release_args[@]}"
```

Selection updates the existing `catalog.effective_release_heads` row for the release tenant and environment.
Release IDs are scoped identities, and their numeric magnitude does not set deployment order.
A deployment never selects itself again.
If another release is already selected, the deployment refuses before changing workloads.

Include the rendered host Deployment JSON and the application request and expected response files in `candidate.deployment_files` before qualification.
Use absolute paths for these files.
The Deployment must name the explicit namespace and contain one container with the qualified immutable host image.
Its `--release-artifact-base` and `--release-manifest-digest` arguments must match the published release.
For fresh nodes, set `imagePullPolicy: IfNotPresent` so the node can fetch that exact image from its registry.
The owned application fixtures set this policy for qualified candidates.
The command permits Kubernetes defaults but refuses changes to supplied container fields or additional container images.

If the candidate includes an executor image, include its Deployment JSON and pass `--executor-deployment`.
Its `WAMN_RELEASE_ARTIFACT_BASE` and `WAMN_RELEASE_MANIFEST_DIGEST` environment values must match the published release.
If the deployment also replaces identity, include its qualified Deployment JSON and pass `--identity-deployment`.
The gates image belongs to qualification and does not require a deployed workload.
The target must already provide its database, bindings, credentials, ingress, and other required infrastructure.

Run the deployment against the explicit Kubernetes target:

```bash
wamn-ctl deploy-release \
  --qualification "$DELIVERY_QUALIFICATION" "${release_args[@]}" \
  --kubeconfig "$DELIVERY_KUBECONFIG" --context "$DELIVERY_CONTEXT" \
  --namespace "$DELIVERY_NAMESPACE" --host-deployment "$DELIVERY_HOST_JSON" \
  --http-workload "$DELIVERY_HTTP_WORKLOAD" \
  --principal "$DELIVERY_OPERATOR" \
  --interaction-url "$DELIVERY_INTERACTION_URL" --route-host "$DELIVERY_ROUTE_HOST" \
  --request-body "$DELIVERY_REQUEST_JSON" --expected-response "$DELIVERY_RESPONSE_JSON" \
  --bearer-file "$DELIVERY_CALLER_TOKEN_FILE"
```

The interaction URL must reach a POST operation in this deployment through its configured ingress.
The selected manifest must permit a platform access token for that route.
Keep the token in the private bearer file, outside qualified artifacts.
If `--http-workload` is supplied, the command waits for that WorkloadDeployment to become ready before the request.
The command requires an HTTP success response and an exact JSON result.
It refuses redirects and does not repeat the application request automatically.

Deployment pulls the published manifest by digest and uses the supplied immutable images without rebuilding.
It holds the existing selection row lock through workload readiness, the authenticated operation, and the activation commit.
It also holds the package owner's lineage locks while comparing the selected and installed migration sequences.
Identical applied migrations permit code replacement across package versions.
A mismatch requires a fresh target and refuses an unsupported change to a database with retained data.

The command bounds deployment to 15 minutes and each Kubernetes rollout wait to 5 minutes.
Failure or interruption leaves the activation transaction uncommitted and reports failure.
Workload changes and an application mutation can already exist when failure occurs.
Inspect that state before another authorized attempt because the command does not reset the database or roll back workloads automatically.
The deployment pull uses the same `--oci-ca-path` roots from `release_args`.
