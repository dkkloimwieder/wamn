# Repository delivery

## Release qualification

A package schema change requires a new release and a reset of the application tables.
The [owner ruling](../audit/README.md) defines this baseline policy.

Publish the release first with `publish-release`, as [deployment](deployment.md#publish-a-release) shows.
It pushes the release manifest to the repository that `WAMN_RELEASE_ARTIFACT_BASE` names, and it prints the release digest.
Then qualify that release:

```bash
wamn-ctl qualify-release \
  --release-digest "$RELEASE_DIGEST" --release-chart "$WAMN_RELEASE_CHART" \
  --artifact-base "$WAMN_RELEASE_ARTIFACT_BASE" \
  --registry-auth-file "$REGISTRY_AUTH_FILE"
```

`--release-digest` is the digest that `publish-release` printed.
`--release-chart` is the release chart that `wamn-ctl build-platform` stamped.
Its default values name the platform image set: the host image and the `http-route` and `materializer` role components, each by its `@sha256` digest.
They also name the runner, gates and identity images, outside the qualification key.
The command line does not repeat these digests.
`--artifact-base` is the release repository, the same repository that `WAMN_RELEASE_ARTIFACT_BASE` names.

Keep registry credentials in the private file that `--registry-auth-file` names.
For a registry without authentication, write an explicit empty entry, `{"auths":{"<registry>":{}}}`, and the registry access is anonymous.
A missing entry still refuses with `registry-credentials-not-found`.
If the registry certificate chains to a private CA, add `--oci-ca-path` with that CA, or set `WASH_OCI_CA_PATHS`.
If the registry uses HTTP, add `--insecure-registry` for that registry.

DEP2-02 removed the duplicate application installers and their cluster cases.
The runner retains repository inputs and pinned tools, but it carries no application case executables.
Receiving, WMS, and Acme declare empty case sets, so qualification refuses their empty union.

Run the command from the repository checkout or one of its subdirectories.
The command reads application declarations from that checkout and builds nothing.
It pulls the release by its digest.
It pulls the host, gates, identity and runner images by digest.
For each declared case, it runs the named executable under `/cases` in the runner image with `docker run`.
The runner uses the Docker daemon of this machine through its socket and the host network.

Each package declares its cases in `tests/qualification.json` beside its tests.
The application owns this declaration. No approver or platform case list exists.
The current `apps/wamn_receiving/tests/qualification.json` contains:

```json
{
  "cases": []
}
```

`executable` names a test executable under `/cases` in the runner image.
`name` names the exact ignored test. `evidence_env` names the variable for its evidence directory.

The verb reads package ids from the pulled release manifest.
The build owner's `build_record::source_root` maps each id to its checkout directory, including packages under `apps/platform`.
The verb reads each declaration from that directory.
An unfamiliar package with a valid declaration needs no platform change.

The release runs the union of its packages' declarations.
The verb removes duplicate entries and sorts by executable, exact case name, then evidence variable, in lexical order.
A missing declaration refuses and names the package. A malformed declaration refuses and names its file and field.
An explicit `{"cases": []}` contributes no cases. A release with an empty union refuses.

Every declared case must appear in its executable's listing and execute exactly one passing test without an explicit skip.

The cases write their results in a temporary directory under the system temporary directory.
Qualification removes that directory after the cases pass.
If a case fails, qualification keeps that directory and names it in the cause of the failed check.
Read the case results there, then remove the directory.
No source-host or CI-provider API is required.

## The qualification artifact

When every case passes, `qualify-release` pushes a qualification artifact beside the release and prints its digest.
A failed qualification pushes nothing.
The command writes no result file and no database row.

The artifact is an OCI image manifest with the artifact type `application/vnd.wamn.release-qualification.v1`.
Its `subject` is the release manifest.
Its one layer is canonical JSON with payload format 2.
It names the release digest, the platform image set, and the names of the cases that passed. It names no runner and no chart version.
A run again on the same inputs reuses that artifact and runs no case, as [reuse](#reuse-of-a-qualification) describes.
The artifact goes into the release repository under the immutable tag `qualification-<release sha256 hex>-<first 32 hex of the image set sha256>`.
An OCI tag holds at most 128 characters, so the tag carries only part of the image set digest. The reader compares the whole image set in the layer.

An environment can use the qualification when its platform image set is the one that the artifact names, whatever chart version selected that image set.
A release on a new host image or role component needs a new qualification.
The gates and identity images are not part of the key.

### Reuse of a qualification

`qualify-release` reads the qualification tag before it runs a case.
The tag comes from the release digest and the digests of the host image and the two role components.
It does not come from the image tags, the registry names or the chart version.
A platform version can copy an unchanged host image under a new tag. The digest stays the same, so the tag stays the same.
If the tag holds a qualification, the command checks it as `apply` does.
If the qualification passed the cases that the release runs, the command runs no case.
It names the qualification, its digest, the image digests and the cases on standard error.
It prints the qualification digest and `Reused` on standard output.
If the qualification passed other cases, the command refuses and runs nothing, because the tag is immutable.
If the tag holds an artifact that fails a check, the command refuses.
A qualification of an earlier payload format refuses in the same way.
A new release digest, a new host digest or a new role component digest gives a new tag, so the cases run.

## Deployment

`wamn-ctl env apply` deploys a release: the environment document names its digest.
`apply` derives the qualification tag from the release digest and the image set of the release chart, and reads the artifact from the registry.
If the tag does not exist, `apply` refuses and names the tag.
See [deployment](deployment.md) for the environment verbs.
