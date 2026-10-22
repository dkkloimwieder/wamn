# Web upload

`wamn web upload` writes built browser files to an S3 or Google Cloud Storage bucket.
It uploads application clients from package artifacts or the platform UI from the local repository.
It does not build files or change edge routing.
[services/ctl/src/web.rs](../../services/ctl/src/web.rs) defines the arguments and output.
[crates/control/lib/src/web_upload.rs](../../crates/control/lib/src/web_upload.rs) owns the upload behavior.

## Arguments

The [wamn binary](../../services/ctl/src/bin/wamn.rs) exposes these two forms:

```text
wamn web upload <org>/<project>/<env> --release sha256:<64 lowercase hex digits> --bucket <bucket URL>
wamn web upload --platform-ui --org <org> [--bucket <bucket URL>]
```

| Input | Package clients | Platform UI |
| --- | --- | --- |
| `<org>/<project>/<env>` | Required environment coordinate | Refused |
| `--release` | Required manifest digest of the served release | Refused |
| `--bucket` | Required | Defaults to `gs://wamn-dev-web` |
| `--platform-ui` | Omit | Required |
| `--org` | Refused | Required, nonempty organization name |

A bucket URL uses `s3://<bucket>[/<prefix>]` or `gs://<bucket>[/<prefix>]`.
The optional prefix adds a directory before the upload path.
`Sink::store` in [web_upload.rs](../../crates/control/lib/src/web_upload.rs) reads `AWS_*` variables for S3.
For Google Cloud Storage, it reads `GOOGLE_*` variables and Application Default Credentials, the credentials that Google client libraries use.
The caller needs access to create objects in the bucket. Package uploads also read object metadata.
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

The uploader requires the chart version to equal the version compiled into `wamn`.
It also requires the local file digest to match `PlatformBuildReceipt.platform_ui` before it writes any object.
[platform_ui_source](../../crates/control/lib/src/web_upload.rs) applies these rules.
[platform_ui_digest](../../crates/control/lib/src/platform_build.rs) includes each relative file path and file digest, and requires `index.html`.

The destination is `<prefix>/platform-ui/<platform version>/`.
The default example prints `platform-ui gs://wamn-dev-web/platform-ui/<platform version>/ written` after success.
Its `config.json` contains only `{"org":"dkk"}`, as [web_scope.rs](../../crates/control/lib/src/web_scope.rs) defines.
This mode requires no environment coordinate, release selection, or package artifact.
A repeated platform upload attempts the writes again and refuses an existing object.

## Package clients

Use the environment configuration from [deployment ordering](deployment.md#deployment-ordering).
Make sure that `helm` is available on the upload host and can read the selected cluster.
[Platform::from_env](../../crates/control/lib/src/environment/mod.rs) reads that configuration before the upload starts.
It requires these inputs even when the upload does not use their database or broker connections:

| Input | Meaning |
| --- | --- |
| `KUBECONFIG` | File with the target cluster and a current context |
| `WAMN_RELEASE_NAMESPACE` | Namespace of the release chart, default `wamn-system` |
| `WAMN_CONTROL_DATABASE_URL` | Required control database URL |
| `WAMN_RELEASE_CHART` | Required stamped chart directory |
| `WAMN_RELEASE_ARTIFACT_BASE` | Required registry repository for release manifests |
| `WAMN_PACKAGE_ARTIFACT_BASE` | Required registry repository for package artifacts |
| `WAMN_REGISTRY_AUTH_FILE` | Required registry authentication file |
| `WAMN_OCI_CA_PATHS` | Optional colon-separated paths to registry CA certificates |
| `WAMN_AUTH_NATS_URL` | Required URL for the authorization broker |
| `WAMN_AUTH_NATS_TLS_CA`, `WAMN_AUTH_NATS_TLS_CERT`, `WAMN_AUTH_NATS_TLS_KEY` | Required paths for the broker CA, client certificate, and key |
| `WAMN_AUTH_NATS_TLS_SERVER_NAME` | Optional broker TLS server name |

[AuthNats::from_env](../../crates/identity/platform/src/auth_changes.rs) defines the broker inputs.
`Platform::from_env` also parses the optional database, identity, and event configuration used by environment commands.
If `WAMN_EVENT_NATS_URL` is set, it also requires `WAMN_EVENT_NATS_USERNAME` and `WAMN_EVENT_NATS_PASSWORD_FILE`.

1. Load the environment configuration above with the intended cluster context and release namespace.
2. Make sure that the package artifacts include the web files from [wamn build](building.md#wamn-build).
3. Read the manifest digest from the newest successful Helm revision of the environment release.
4. Set `WAMN_UPLOAD_RELEASE` to that full `sha256:` digest.
5. Run the upload with the environment coordinate and bucket URL.

```bash
wamn web upload acme/receiving/dev --release "$WAMN_UPLOAD_RELEASE" --bucket gs://example-web/clients
```

[deployed_release](../../crates/control/lib/src/environment/analyse.rs) reads Helm history and values for the environment.
The uploader refuses a different release digest, then pulls the release manifest and each named package artifact.
The web layer of each artifact supplies its built files, as [package_artifact.rs](../../crates/control/lib/src/package_artifact.rs) defines.
It skips packages without `client_package.name`.
For each client, it requires a name of the form `@wamn/<project>-client` and built files that include `index.html`.
The upload does not read or build application files from the working tree.

The destination is `<prefix>/<package id>/<release digest without sha256:>/`.
Its `config.json` contains the coordinate's organization and the project from the client package name.
For example, `@wamn/receiving-client` produces `{"org":"acme","project":"receiving"}` for the coordinate above.
The command prints one line per client: `<package id> <bucket URL>/<upload path>/ written`.
If the destination already has `index.html`, it skips that client and prints `present` instead of `written`.
It prints no line for a package without a client.

## Object writes and errors

Both modes write assets first, generated `config.json` next, and `index.html` last.
Each write creates a new object and never replaces an existing object, even when its bytes match.
A failed upload can leave objects behind. The command performs no rollback.
A retry after a partial upload can therefore refuse an existing asset before it reaches the index.

[write_files](../../crates/control/lib/src/web_upload.rs) accepts `index.html` and files under `assets/`.
Assets carry `Cache-Control: public, max-age=31536000, immutable`.
The index and generated `config.json` carry `Cache-Control: no-cache`.
The accepted extensions are `html`, `js`, `css`, `json`, `map`, `svg`, `png`, `ico`, `woff2`, and `woff`.

The [CLI](../../services/ctl/src/web.rs) and [upload implementation](../../crates/control/lib/src/web_upload.rs) report these errors:

| Error text | Cause or action |
| --- | --- |
| `set WAMN_RELEASE_CHART to the stamped platform chart` | Set the variable before a platform upload. |
| ``read <path>; run `wamn-ctl build-platform` `` | The platform build record is absent or unreadable. Use the matching stamped chart. |
| `the stamped chart version <version> differs from platform <version>` | Use a `wamn` binary that matches the chart. |
| `the platform UI build <path> differs from the platform build receipt <path>` | Restore the matching local build before upload. |
| `set <variable> for wamn-ctl env` | Supply the required environment configuration for a package upload. |
| `coordinate <value> is not org/project/env` | Pass exactly three slash-separated parts. The coordinate also must pass the platform naming rules. |
| `--release must be sha256: and 64 lowercase hexadecimal digits` | Pass the full manifest digest. |
| `--bucket must be s3://<bucket>[/<prefix>] or gs://<bucket>[/<prefix>]` | Use one of the two supported bucket schemes. |
| `<coordinate> serves no release` | The environment has no successful revision with a release digest. |
| `release <requested> is not the release <deployed> that <coordinate> serves` | Use the digest from the newest successful revision. |
| `the package artifact <tag> holds no built web client` | Publish the package with its built web files. |
| `the package artifact <tag> holds no web client index.html` | Include the built index in the package artifact. |
| `the build wrote <path>, which has no declared cache rule` | Keep built files under `assets/`, except for `index.html`. The uploader generates `config.json`. |
| `the build wrote <path>, which has no declared content type` | Use a supported file extension. |
| `write <key>; an existing object is never replaced` | Inspect the underlying storage error. An existing object, missing access, or a failed storage request can cause this error. |

Argument errors and upload failures return a nonzero exit status.
The command has no overwrite, delete, or resume flag.
[Platform UI deployment](deployment.md#platform-ui-files) owns the public host and control routing after upload.
