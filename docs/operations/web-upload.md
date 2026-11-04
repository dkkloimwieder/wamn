# Web clients

`wamn web upload` writes built browser files to an S3 or Google Cloud Storage bucket.
It uploads the platform UI from the local repository.
Application uploads pull a pinned standalone web artifact from the registry.
It does not build files or change edge routing.
[services/ctl/src/web.rs](../../services/ctl/src/web.rs) defines the arguments and output.
[crates/control/lib/src/web_artifact.rs](../../crates/control/lib/src/web_artifact.rs) owns the upload behavior.

## Build and push

`wamn web build` builds one package's browser files with Vite through `pnpm run build`.
First, run `wamn build` for that package to generate its contract and client bindings.
Run `pnpm install` at the repository root before the web build.
Then run these commands from the repository root:

```bash
wamn web build --package apps/wamn_receiving
wamn web push --package apps/wamn_receiving \
  --artifact-base registry.example/wamn/packages \
  --registry-auth-file /path/to/dockerconfig.json
```

The build writes `apps/target/wamn/<package>/web-artifact/dist/` and `web.json` beside it.
The source package's compiled `wamn.json` must hash to `build.json`'s `outputs.manifest`.
The build refuses a different digest or a package without `client_package`, and names the package.
The build refuses a missing or empty `version` in the package's `web/package.json`, and names that file.
`web.json` contains four fields:

- `client_package`: The client name from the compiled manifest.
- `version`: The version from the package's `web/package.json`.
- `package`: `<id>@<package version>` from the build record.
- `contract_digest`: `outputs.manifest`, the digest of the canonical package manifest.

Web source edits need only another `wamn web build` against that recorded contract.
This command leaves the package build record unchanged.
`wamn build` builds package components without Vite.
Package artifacts contain no web layer or browser files.

The push packs `dist/` and canonical `web.json` into one tar layer in an OCI artifact.
The tar sorts paths and fixes file modes, timestamps, and owners.
It includes no host names and refuses links and special files.
The OCI artifact type is `application/vnd.wamn.web.v1`.
The layer media type is `application/vnd.wamn.web.v1.tar`, and the configuration is the OCI empty JSON object.
The artifact digest is the SHA-256 of the canonical OCI image manifest.

The tag is `<package id>-web-<web version>` under `--artifact-base`.
The package id comes from `web.json`'s `package`, and the web version comes from its `version`.
An existing tag with the same digest returns `AlreadyPresent` without writes.
An existing tag with different content refuses and names the tag and both digests.
For a UI change, change only the version in `web/package.json` to an unused web version before the build.
The push uses the existing registry client and Docker credential reader.
The registry arguments match `apply-package`: `--artifact-base`, `--registry-auth-file`, `--insecure-registry`, and repeatable `--oci-ca-path`.
For an anonymous push, the credential file must explicitly contain `{"auths":{"<registry>":{}}}`.

`wamn web upload --platform-ui` still reads the local platform UI.

## Arguments

The [wamn binary](../../services/ctl/src/bin/wamn.rs) exposes these two forms:

```text
wamn web upload --web sha256:<64 lowercase hex digits> --org <org> --bucket <bucket URL> --artifact-base <registry/repository> --registry-auth-file <file>
wamn web upload --platform-ui --org <org> [--bucket <bucket URL>]
```

| Input | Package clients | Platform UI |
| --- | --- | --- |
| `--web` | Required digest of the web artifact | Refused |
| Registry arguments | Same arguments as `web push`, required | Refused |
| `--bucket` | Required | Defaults to `gs://wamn-dev-web` |
| `--platform-ui` | Omit | Required |
| `--org` | Required, nonempty organization name | Required, nonempty organization name |

A bucket URL uses `s3://<bucket>[/<prefix>]` or `gs://<bucket>[/<prefix>]`.
The optional prefix adds a directory before the upload path.
`Sink::store` in [web_artifact.rs](../../crates/control/lib/src/web_artifact.rs) reads `AWS_*` variables for S3.
For Google Cloud Storage, it reads `GOOGLE_*` variables and Application Default Credentials, the credentials that Google client libraries use.
The caller needs access to create objects in the bucket.
The upload does not create the bucket or grant public access.
[Deployment](deployment.md#web-client-files) owns serving and routing prerequisites.

## Platform UI

Run this mode from the repository root that contains `apps/platform_ui/dist`.
Use the stamped chart and matching local files from [the platform build](building.md#platform-ui).
The upload reads `Chart.yaml` and `platform-build.json` from that chart directory.

1. Set `WAMN_RELEASE_CHART` to the stamped chart directory.
2. Make sure that the local `apps/platform_ui/dist` matches the files recorded in that chart.
3. Make sure that bucket credentials are available to the upload process.
4. Run the upload with the target organization.

```bash
export WAMN_RELEASE_CHART=/path/to/stamped-chart
wamn web upload --platform-ui --org dkk
```

For another bucket or prefix, pass `--bucket`:

```bash
wamn web upload --platform-ui --org dkk --bucket gs://example-web/clients
```

The uploader requires the chart version to equal the version in `deploy/platform/release/Chart.yaml`, which it reads at run time.
It also requires the local file digest to match `PlatformBuildRecord.platform_ui` before it writes any object.
[platform_ui_source](../../crates/control/lib/src/web_artifact.rs) applies these rules.
[platform_ui_digest](../../crates/control/lib/src/platform_build.rs) includes each relative file path and file digest, and requires `index.html`.

The destination is `<prefix>/platform-ui/<platform version>/`.
The default example prints `platform-ui gs://wamn-dev-web/platform-ui/<platform version>/ written` after success.
Its `config.json` contains only `{"org":"dkk"}`, as [web_scope.rs](../../crates/control/lib/src/web_scope.rs) defines.
This mode requires no environment coordinate, release selection, or package artifact.
A repeated platform upload attempts the writes again. It keeps equal objects and refuses objects with other bytes.

## Package clients

Upload the artifact digest that the environment document pins in `client.web`.
Use the same artifact repository and registry credentials as `wamn web push`:

```bash
wamn web upload --web sha256:<64 lowercase hex digits> --org acme \
  --bucket gs://example-web/prefix \
  --artifact-base registry.example/wamn/packages \
  --registry-auth-file /path/to/dockerconfig.json
```

The uploader makes sure that the manifest and tar layer match their digests before any bucket write.
It refuses an artifact with another type or layer media type.
The package id comes from `web.json`'s `package`, which contains `<id>@<version>`.
The destination is `<prefix>/clients/<package id>/<web digest hex>/`.
The hexadecimal portion excludes `sha256:`.
No environment coordinate or release digest selects the upload root.
Two environments with the same web digest use the same root.

The uploader derives the project from `web.json`'s `client_package`, which must use `@wamn/<project>-client`.
It writes `config.json` with the supplied org and that project.
The edge adds the live release and web digests when it serves this file.

The command prints `<package> <bucket URL>/<root>/ written` after the first complete upload.
An existing `index.html` marks a complete upload.
Another upload prints `present` and writes nothing.

## Object writes and errors

Both upload modes write assets first, generated `config.json` next, and `index.html` last.
Each write creates a new object and never replaces an existing object.
A failed upload can leave objects behind. The command performs no rollback.
A retry after a partial upload keeps each existing object whose bytes are equal, and refuses an object with other bytes.

[write_files](../../crates/control/lib/src/web_artifact.rs) accepts `index.html`, the service worker `sw.js`, and files under `assets/`.
Assets carry `Cache-Control: public, max-age=31536000, immutable`.
The index, `sw.js` and generated `config.json` carry `Cache-Control: no-cache`.
The accepted extensions are `html`, `js`, `css`, `json`, `map`, `svg`, `png`, `ico`, `woff2`, and `woff`.

The [CLI](../../services/ctl/src/web.rs) and [upload implementation](../../crates/control/lib/src/web_artifact.rs) report these errors:

| Error text | Cause or action |
| --- | --- |
| `set WAMN_RELEASE_CHART to the stamped platform chart` | Set the variable before a platform upload. |
| ``read <path>; run `wamn-ctl build-platform` `` | The platform build record is absent or unreadable. Use the matching stamped chart. |
| `the stamped chart version <version> differs from platform <version>` | Use a stamped chart that matches `deploy/platform/release/Chart.yaml`. |
| `the platform UI build <path> differs from the platform build record <path>` | Restore the matching local build before upload. |
| `--bucket must be s3://<bucket>[/<prefix>] or gs://<bucket>[/<prefix>]` | Use one of the two supported bucket schemes. |
| `the build wrote <path>, which has no declared cache rule` | Keep built files under `assets/`, except for `index.html` and `sw.js`. The uploader generates `config.json`. |
| `the build wrote <path>, which has no declared content type` | Use a supported file extension. |
| `write <key>; an existing object with other bytes is never replaced` | An earlier upload wrote other bytes under this upload path. Inspect the object before you delete it. |
| `write <key>; an existing object is never replaced` | Inspect the underlying storage error. Missing access or a failed storage request can cause this error. |

Argument errors and upload failures return a nonzero exit status.
The command has no overwrite, delete, or resume flag.
[Platform UI deployment](deployment.md#platform-ui-files) owns the public host and control routing after upload.
