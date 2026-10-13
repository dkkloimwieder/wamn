# Repository delivery

## Release qualification

For a package successor with application migrations, first follow [package upgrades](deployment.md#package-upgrades).
`qualify-upgrade` proves the database transition before application. It does not qualify a release or replace the release qualification below.

Publish the release first with `publish-release`, as [deployment](deployment.md#publish-and-select-a-release) shows.
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

Receiving and WMS start the chart's identity image, because their published application routes accept sessions, and give issuer trust to their application hosts.
When the chart's images are in a registry at a loopback port, the kind nodes reach it through the container that publishes that port on the `kind` network.

The command needs no checkout and builds nothing.
It pulls the release by its digest.
It pulls the host, gates, identity and runner images by digest.
It runs each case in the chart's runner image with `docker run`: the runner carries the case executables, the repository files they read, and pinned `docker`, `kind`, `kubectl` and `helm`.
The runner uses the Docker daemon of this machine through its socket and the host network.
Each case runs on its own kind cluster with the image set of the chart.
Each case installs the candidate's packages from the package repository beside the release repository through the package stage of `apply`, and publishes nothing.
Each case installs its environment with the release chart, as `env apply` does.
It copies the release, its components and the role components by digest into its own registry and stamps a copy of the chart with the same image set.

The package set of the release selects the cases exactly. Any other set fails.
A Receiving release with Acme runs command histories and the baseline overlay with materializer progress.
A Receiving release without Acme runs command histories, which calls no Acme route.
WMS runs its released routes, partial-label completion, and restart/cache cases against the same release and host image.

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
A run again on the same inputs writes the same bytes, and the command reports that the artifact is already present.
The artifact goes into the release repository under the immutable tag `qualification-<release sha256 hex>-<first 32 hex of the image set sha256>`.
An OCI tag holds at most 128 characters, so the tag carries only part of the image set digest. The reader compares the whole image set in the layer.

An environment can use the qualification when its platform image set is the one that the artifact names, whatever chart version selected that image set.
A release on a new host image or role component needs a new qualification.
The gates and identity images are not part of the key.

## Deployment

`wamn-ctl env apply` deploys a release: the environment document names its digest.
`apply` derives the qualification tag from the release digest and the image set of the release chart, and reads the artifact from the registry.
If the tag does not exist, `apply` refuses and names the tag.
See [deployment](deployment.md) for the environment verbs.
