# Web clients

`wamn web upload` writes built browser files to an S3 or Google Cloud Storage bucket.
It pulls the platform UI from its pinned registry artifact.
Application uploads pull a pinned standalone web artifact from the registry.
It does not build files or change edge routing.
[services/ctl/src/web.rs](../../services/ctl/src/web.rs) defines the arguments and output.
[crates/control/package/src/web_artifact.rs](../../crates/control/package/src/web_artifact.rs) owns the upload behavior.

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

`wamn web upload --platform-ui` pulls the separate platform UI artifact by digest.

## Arguments

The [wamn binary](../../services/ctl/src/bin/wamn.rs) exposes these two forms:

```text
wamn web upload --web sha256:<64 lowercase hex digits> --org <org> --bucket <bucket URL> --artifact-base <registry/repository> --registry-auth-file <file>
wamn web upload --platform-ui sha256:<64 lowercase hex digits> --org <org> [--bucket <bucket URL>] --artifact-base <registry/repository> --registry-auth-file <file>
```

| Input | Package clients | Platform UI |
| --- | --- | --- |
| `--web` | Required digest of the web artifact | Refused |
| Registry arguments | Same arguments as `web push`, required | Same arguments, required |
| `--bucket` | Required | Defaults to `gs://wamn-dev-web` |
| `--platform-ui` | Omit | Required digest of the platform UI artifact |
| `--org` | Required, nonempty organization name | Required, nonempty organization name |

A bucket URL uses `s3://<bucket>[/<prefix>]` or `gs://<bucket>[/<prefix>]`.
The optional prefix adds a directory before the upload path.
`Sink::store` in [web_artifact.rs](../../crates/control/package/src/web_artifact.rs) reads `AWS_*` variables for S3.
For Google Cloud Storage, it reads `GOOGLE_*` variables and Application Default Credentials, the credentials that Google client libraries use.
The caller needs access to create objects in the bucket.
The upload does not create the bucket or grant public access.
[Deployment](deployment.md#web-client-files) owns serving and routing prerequisites.

## Platform UI

Use the `platformUi` manifest digest from the prepared chart's `values.yaml`.
The platform build publishes this artifact in `<registry>/wamn/platform-ui`.
Supply registry credentials and bucket credentials to the upload process.
The upload needs no checkout, chart directory, pnpm installation, or local build output.

```bash
wamn web upload --platform-ui sha256:<64 lowercase hex digits> --org dkk \
  --artifact-base registry.example/wamn/platform-ui \
  --registry-auth-file /path/to/dockerconfig.json
```

The default bucket is `gs://wamn-dev-web`.
For another bucket or prefix, pass `--bucket gs://example-web/prefix`.
The destination is `<prefix>/platform-ui/sha256-<hex>/`.
The artifact contains UI files and UI metadata only.
The uploader makes sure that the manifest and tar layer match their digests before any bucket write.
It refuses application artifacts and invalid UI metadata.

The uploader writes `{"org":"dkk"}` to `<prefix>/platform-ui/orgs/dkk/config.json`.
No organization configuration enters the shared artifact prefix.
An existing index marks a complete shared upload.
A repeated upload keeps equal organization configuration and prints `present` for the shared files.
A different artifact digest selects a different shared prefix.

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

Both upload modes write `index.html` last.
Application uploads write assets first and generated `config.json` next.
Platform uploads keep organization configuration outside the shared files.
Each write creates a new object and never replaces an existing object.
A failed upload can leave objects behind. The command performs no rollback.
A retry after a partial upload keeps each existing object whose bytes are equal, and refuses an object with other bytes.

[The web archive reader](../../crates/control/package/src/web_artifact/plan.rs) accepts `index.html`, the service worker `sw.js`, and files under `assets/`.
Assets carry `Cache-Control: public, max-age=31536000, immutable`.
The index, `sw.js` and generated `config.json` carry `Cache-Control: no-cache`.
The accepted extensions are `html`, `js`, `css`, `json`, `map`, `svg`, `png`, `ico`, `woff2`, and `woff`.

The [CLI](../../services/ctl/src/web.rs) and [upload implementation](../../crates/control/package/src/web_artifact.rs) report these errors:

| Error text | Cause or action |
| --- | --- |
| `--bucket must be s3://<bucket>[/<prefix>] or gs://<bucket>[/<prefix>]` | Use one of the two supported bucket schemes. |
| `the build wrote <path>, which has no declared cache rule` | Keep built files under `assets/`, except for `index.html` and `sw.js`. The uploader generates `config.json`. |
| `the build wrote <path>, which has no declared content type` | Use a supported file extension. |
| `write <key>; an existing object with other bytes is never replaced` | An earlier upload wrote other bytes under this upload path. Inspect the object before you delete it. |
| `write <key>; an existing object is never replaced` | Inspect the underlying storage error. Missing access or a failed storage request can cause this error. |

Argument errors and upload failures return a nonzero exit status.
The command has no overwrite, delete, or resume flag.
[Platform UI deployment](deployment.md#platform-ui-files) owns the public host and control routing after upload.
